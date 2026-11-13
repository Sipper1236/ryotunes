//! Official Spotify Web Player host over local Chrome DevTools Protocol (CDP).
//!
//! Spotify's page owns EME/Widevine audio and Spotify ads; Ryotunes drives its player service and
//! mirrors state. If the local browser lacks Widevine, `play` fails before changing the queue's
//! audio owner and AppState transparently resolves the same track through YouTube Music.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use ryotunes_core::host::{SpotifyWebPlayer, SpotifyWebPlayerSnapshot};

const PLAYER_URL: &str = "https://open.spotify.com/";
const BRIDGE: &str = include_str!("spotify/player-bridge.js");
const READY_TIMEOUT: Duration = Duration::from_secs(45);

pub struct CdpSpotifyWebPlayer {
    profile: PathBuf,
    session: tokio::sync::Mutex<Option<Arc<CdpSession>>>,
    child: Mutex<Option<Child>>,
    failed_widevine: std::sync::atomic::AtomicBool,
    http: reqwest::Client,
}

impl CdpSpotifyWebPlayer {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            profile: data_dir.join("spotify-web-player"),
            session: tokio::sync::Mutex::new(None),
            child: Mutex::new(None),
            failed_widevine: std::sync::atomic::AtomicBool::new(false),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .build()
                .expect("build Spotify Web Player HTTP client"),
        }
    }

    async fn session(&self) -> Result<Arc<CdpSession>, String> {
        if self.failed_widevine.load(Ordering::Acquire) {
            return Err("this browser does not provide a Widevine CDM for Spotify playback".into());
        }
        let mut current = self.session.lock().await;
        if let Some(session) = current.as_ref() {
            if session.connection.alive.load(Ordering::Acquire) {
                return if session.widevine {
                    Ok(session.clone())
                } else {
                    Err("this browser does not provide a Widevine CDM for Spotify playback".into())
                };
            }
        }

        self.launch_browser()?;
        let port_file = self.profile.join("DevToolsActivePort");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let port = loop {
            if let Ok(contents) = tokio::fs::read_to_string(&port_file).await {
                if let Some(port) =
                    contents.lines().next().and_then(|line| line.trim().parse::<u16>().ok())
                {
                    break port;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("Chromium did not publish its DevTools endpoint".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let base = format!("http://127.0.0.1:{port}");
        let targets: Vec<Value> = self
            .http
            .get(format!("{base}/json/list"))
            .send()
            .await
            .map_err(|e| format!("cannot inspect Chromium tabs: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Chromium DevTools refused tab inspection: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Chromium returned invalid tab data: {e}"))?;
        let target = targets
            .iter()
            .find(|target| {
                target.get("type").and_then(Value::as_str) == Some("page")
                    && target
                        .get("url")
                        .and_then(Value::as_str)
                        .is_some_and(|url| url.starts_with(PLAYER_URL))
            })
            .ok_or_else(|| "Chromium opened without a Spotify page".to_owned())?;
        let ws_url = target
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| "Chromium tab has no DevTools WebSocket".to_owned())?;
        let (socket, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| format!("cannot connect to Chromium DevTools: {e}"))?;
        let connection = Arc::new(CdpConnection::new(socket));
        connection.command("Runtime.enable", json!({})).await?;
        connection.command("Page.enable", json!({})).await?;
        connection.command("Network.enable", json!({})).await?;
        connection.command("Runtime.addBinding", json!({"name":"__mspHostPost"})).await?;

        // Transfer only Spotify's session cookie from the browser sign-in flow. It is sent to
        // Spotify's own Web Player origin and is never written to logs.
        let cookie =
            tokio::task::spawn_blocking(crate::browser_auth::read_spotify_cookie_from_browser)
                .await
                .ok()
                .flatten();
        if let Some(value) = cookie.filter(|value| !value.is_empty()) {
            connection
                .command(
                    "Network.setCookie",
                    json!({"name":"sp_dc","value":value,"url":PLAYER_URL,"secure":true,"httpOnly":true}),
                )
                .await?;
        }

        connection
            .command("Page.addScriptToEvaluateOnNewDocument", json!({"source":BRIDGE}))
            .await?;
        // The browser may have loaded its first document before the bridge was installed.
        connection.command("Page.navigate", json!({"url":PLAYER_URL})).await?;
        let session = Arc::new(CdpSession { connection, widevine: false });
        session.wait_ready().await?;
        let widevine = session.widevine_available().await?;
        if !widevine {
            self.failed_widevine.store(true, Ordering::Release);
            *current = Some(session);
            if let Some(mut child) = self.child.lock().take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err("this browser does not provide a Widevine CDM for Spotify playback".into());
        }
        let session =
            Arc::new(CdpSession { connection: session.connection.clone(), widevine: true });
        *current = Some(session.clone());
        Ok(session)
    }

    fn launch_browser(&self) -> Result<(), String> {
        if self.child.lock().as_mut().is_some_and(|child| child.try_wait().ok().flatten().is_none())
        {
            return Ok(());
        }
        std::fs::create_dir_all(&self.profile)
            .map_err(|e| format!("cannot create Spotify Web Player profile: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.profile, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot secure Spotify Web Player profile: {e}"))?;
        }
        let port_file = self.profile.join("DevToolsActivePort");
        let _ = std::fs::remove_file(port_file);
        let browser = std::env::var_os("RYOTUNES_SPOTIFY_BROWSER")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                ["chromium", "google-chrome-stable", "google-chrome", "brave-browser"]
                    .iter()
                    .find_map(|name| find_executable(name))
            })
            .ok_or_else(|| {
                "install Chromium/Chrome with Widevine, or set RYOTUNES_SPOTIFY_BROWSER".to_owned()
            })?;
        let child = Command::new(browser)
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--remote-debugging-port=0")
            .arg("--remote-allow-origins=*")
            .arg(format!("--user-data-dir={}", self.profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(PLAYER_URL)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot launch the Spotify Web Player browser: {e}"))?;
        *self.child.lock() = Some(child);
        Ok(())
    }

    async fn call(&self, method: &str, args: Value) -> Result<Value, String> {
        let session = self.session().await?;
        let encoded = serde_json::to_string(&args).map_err(|e| e.to_string())?;
        session
            .evaluate(&format!(
                "window.Myspoti.call('ryotunes',{},{});",
                js_string(method),
                encoded
            ))
            .await
    }
}

impl Drop for CdpSpotifyWebPlayer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[async_trait::async_trait]
impl SpotifyWebPlayer for CdpSpotifyWebPlayer {
    async fn play(&self, track_uri: &str) -> Result<(), String> {
        let id = track_uri.strip_prefix("spotify:track:").unwrap_or(track_uri);
        if id.len() != 22 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err("Spotify Web Player needs a valid Spotify track id".into());
        }
        let uri = format!("spotify:track:{id}");
        self.call("player.play", json!({"contextUri":uri,"trackUri":uri})).await?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        loop {
            match self.snapshot().await {
                Ok(snapshot)
                    if snapshot.track_uri.as_deref() == Some(uri.as_str())
                        && (snapshot.playing || snapshot.loading) =>
                {
                    return Ok(())
                }
                Ok(_) => {}
                Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
                Err(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("Spotify did not start this track; using YouTube Music instead".into());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn pause(&self) -> Result<(), String> {
        self.call("player.pause", json!({})).await.map(|_| ())
    }

    async fn resume(&self) -> Result<(), String> {
        self.call("player.resume", json!({})).await.map(|_| ())
    }

    async fn seek(&self, position: f64) -> Result<(), String> {
        self.call("player.seek", json!({"positionMs":(position.max(0.0) * 1000.0) as u64}))
            .await
            .map(|_| ())
    }

    async fn set_volume(&self, volume: i64) -> Result<(), String> {
        self.call("volume.set", json!({"level":(volume.clamp(0, 100) as f64) / 100.0}))
            .await
            .map(|_| ())
    }

    async fn stop(&self) -> Result<(), String> {
        // Keep the account page alive so another Spotify track can start without another page load.
        self.pause().await
    }

    async fn snapshot(&self) -> Result<SpotifyWebPlayerSnapshot, String> {
        let state = self.call("player.state", json!({})).await?;
        Ok(snapshot_from_state(&state))
    }
}

fn snapshot_from_state(state: &Value) -> SpotifyWebPlayerSnapshot {
    let track = state.get("track").filter(|track| !track.is_null());
    let track_uri =
        track.and_then(|track| track.get("uri")).and_then(Value::as_str).map(str::to_owned);
    let number = |key: &str| state.get(key).and_then(Value::as_f64).unwrap_or(0.0).max(0.0);
    SpotifyWebPlayerSnapshot {
        track_uri,
        position: number("positionMs") / 1000.0,
        duration: number("durationMs") / 1000.0,
        playing: state.get("isPaused").and_then(Value::as_bool) == Some(false),
        loading: state.get("isBuffering").and_then(Value::as_bool).unwrap_or(false),
        is_ad: track.and_then(|track| track.get("kind")).and_then(Value::as_str) == Some("ad"),
        error: None,
    }
}

struct CdpSession {
    connection: Arc<CdpConnection>,
    widevine: bool,
}

impl CdpSession {
    async fn wait_ready(&self) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            // The bridge installs before Spotify's React app. Wait for its service registry,
            // not merely the bridge global, or the first play races initialization and falls
            // back to YouTube even when this browser can play Spotify.
            if let Ok(Value::Bool(true)) = self
                .evaluate("window.Myspoti.call('ryotunes','player.state',{}).then(()=>true)")
                .await
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("Spotify Web Player did not expose its player controls".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn widevine_available(&self) -> Result<bool, String> {
        let result = self
            .evaluate(
                "navigator.requestMediaKeySystemAccess('com.widevine.alpha',[{initDataTypes:['cenc'],audioCapabilities:[{contentType:'audio/mp4; codecs=\"mp4a.40.2\"'}]}]).then(()=>true).catch(()=>false)",
            )
            .await?;
        Ok(result.as_bool() == Some(true))
    }

    async fn evaluate(&self, expression: &str) -> Result<Value, String> {
        self.connection
            .evaluate(expression)
            .await
            .map_err(|error| format!("Spotify Web Player command failed: {error}"))
    }
}

struct CdpConnection {
    outgoing: mpsc::UnboundedSender<Message>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: AtomicU64,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl CdpConnection {
    fn new(
        socket: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Self {
        let (mut sink, mut stream) = socket.split();
        let (outgoing, mut outgoing_rx) = mpsc::unbounded_channel::<Message>();
        let pending =
            Arc::new(Mutex::new(HashMap::<u64, oneshot::Sender<Result<Value, String>>>::new()));
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let writer_alive = alive.clone();
        tokio::spawn(async move {
            while let Some(message) = outgoing_rx.recv().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
            writer_alive.store(false, Ordering::Release);
        });

        let reader_pending = pending.clone();
        let reader_alive = alive.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let text = match message {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                let Some(id) = value.get("id").and_then(Value::as_u64) else { continue };
                if let Some(sender) = reader_pending.lock().remove(&id) {
                    let response = if let Some(error) = value.get("error") {
                        Err(error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("CDP command failed")
                            .to_owned())
                    } else {
                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(response);
                }
            }
            reader_alive.store(false, Ordering::Release);
            for (_, sender) in reader_pending.lock().drain() {
                let _ = sender.send(Err("Chromium DevTools connection closed".into()));
            }
        });

        Self { outgoing, pending, next_id: AtomicU64::new(1), alive }
    }

    async fn command(&self, method: &str, params: Value) -> Result<Value, String> {
        if !self.alive.load(Ordering::Acquire) {
            return Err("Chromium DevTools connection is closed".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        self.outgoing
            .send(Message::Text(json!({"id":id,"method":method,"params":params}).to_string()))
            .map_err(|_| "Chromium DevTools writer stopped".to_owned())?;
        tokio::time::timeout(Duration::from_secs(12), rx)
            .await
            .map_err(|_| format!("Chromium DevTools timed out on {method}"))?
            .map_err(|_| "Chromium DevTools dropped a command response".to_owned())?
    }

    async fn evaluate(&self, expression: &str) -> Result<Value, String> {
        let response = self
            .command(
                "Runtime.evaluate",
                json!({"expression":expression,"awaitPromise":true,"returnByValue":true,"userGesture":true}),
            )
            .await?;
        if response.get("exceptionDetails").is_some() {
            return Err(response
                .pointer("/exceptionDetails/exception/description")
                .and_then(Value::as_str)
                .or_else(|| response.pointer("/exceptionDetails/text").and_then(Value::as_str))
                .unwrap_or("Spotify Web Player JavaScript exception")
                .to_owned());
        }
        Ok(response.pointer("/result/result/value").cloned().unwrap_or(Value::Null))
    }
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(Path::new)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::{js_string, snapshot_from_state};
    use serde_json::json;

    #[test]
    fn player_snapshot_maps_tracks_and_ads_without_negative_times() {
        let song = snapshot_from_state(&json!({
            "track":{"uri":"spotify:track:abc","kind":"track"},
            "positionMs":12500,"durationMs":180000,"isPaused":false,"isBuffering":true
        }));
        assert_eq!(song.track_uri.as_deref(), Some("spotify:track:abc"));
        assert_eq!(song.position, 12.5);
        assert_eq!(song.duration, 180.0);
        assert!(song.playing);
        assert!(song.loading);
        assert!(!song.is_ad);

        let ad = snapshot_from_state(&json!({
            "track":{"uri":"spotify:ad:xyz","kind":"ad"},
            "positionMs":-1,"durationMs":0,"isPaused":true
        }));
        assert!(ad.is_ad);
        assert_eq!(ad.position, 0.0);
        assert_eq!(ad.duration, 0.0);
        assert!(!ad.playing);
    }

    #[test]
    fn javascript_string_arguments_are_encoded_as_literals() {
        assert_eq!(js_string("x'); alert(1); //"), r#""x'); alert(1); //""#);
        assert_eq!(js_string("line\nbreak"), r#""line\nbreak""#);
    }
}
