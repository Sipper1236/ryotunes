//! Spotify provider state, held for the daemon.
//!
//! A thin wrapper around [`ryotunes_spotify::SpotifyProvider`] plus the currently signed-in
//! [`ryotunes_spotify::Client`], behind a [`tokio::sync::RwLock`] so many readers (playback,
//! browsing) share one client. The daemon adds the actual command methods on top of this (see the
//! `## Daemon wiring` section of `crates/spotify/README.md`); this file deliberately wires none.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::{Mutex as ParkingMutex, RwLock as ParkingRwLock};
use ryotunes_spotify::{Client, SpotifyProvider, SpotifyConfig, DEFAULT_CLIENT_ID, DEFAULT_REDIRECT_URI};
use tokio::sync::RwLock;

/// Which catalogue the browsing commands (search, home, library, album, artist, playlist) read
/// from. YouTube Music and Spotify are peers: the queue may hold tracks from both, told apart by
/// their ids (`spotify:track:…` vs a video id), and playback of either goes through the one mpv.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Youtube,
    Spotify,
    Soundcloud,
    Tidal,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Youtube => "youtube",
            Provider::Spotify => "spotify",
            Provider::Soundcloud => "soundcloud",
            Provider::Tidal => "tidal",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "youtube" => Some(Provider::Youtube),
            "spotify" => Some(Provider::Spotify),
            "soundcloud" => Some(Provider::Soundcloud),
            "tidal" => Some(Provider::Tidal),
            _ => None,
        }
    }
}

pub const SPOTIFY_TRACK_PREFIX: &str = "spotify:track:";

/// True for any id the daemon minted for a Spotify item (`spotify:<kind>:<base62>`).
pub fn is_spotify_id(id: &str) -> bool {
    id.starts_with("spotify:")
}

/// The bare base62 id of a Spotify track id, or None for anything else.
pub fn spotify_track_id(id: &str) -> Option<&str> {
    id.strip_prefix(SPOTIFY_TRACK_PREFIX).filter(|s| !s.is_empty())
}

pub const SC_TRACK_PREFIX: &str = "sc:track:";
pub const SC_USER_PREFIX: &str = "sc:user:";
pub const SC_PLAYLIST_PREFIX: &str = "sc:playlist:";
pub const SC_SYSTEM_PREFIX: &str = "sc:system:";

/// True for any id the daemon minted for a SoundCloud item (`sc:<kind>:<num>`). SoundCloud tracks
/// stream over plain HTTPS (an HLS m3u8), not the Spotify FIFO — see `AppState::resolve`.
pub fn is_sc_id(id: &str) -> bool {
    id.starts_with("sc:")
}

/// The numeric id of a SoundCloud track id (`sc:track:<num>`), or None for anything else.
pub fn sc_track_id(id: &str) -> Option<u64> {
    id.strip_prefix(SC_TRACK_PREFIX)?.parse().ok()
}

/// The numeric id of a SoundCloud user id (`sc:user:<num>`).
pub fn sc_user_id(id: &str) -> Option<u64> {
    id.strip_prefix(SC_USER_PREFIX)?.parse().ok()
}

/// The numeric id of a SoundCloud playlist/album id (`sc:playlist:<num>`).
pub fn sc_playlist_id(id: &str) -> Option<u64> {
    id.strip_prefix(SC_PLAYLIST_PREFIX)?.parse().ok()
}

/// The permalink of a SoundCloud system-playlist id (`sc:system:<permalink>`) — a curated/charts
/// playlist keyed by permalink rather than a numeric id. `None` for anything else.
pub fn sc_system_id(id: &str) -> Option<&str> {
    id.strip_prefix(SC_SYSTEM_PREFIX).filter(|s| !s.is_empty())
}

pub const TIDAL_TRACK_PREFIX: &str = "tidal:track:";
pub const TIDAL_ARTIST_PREFIX: &str = "tidal:artist:";
pub const TIDAL_ALBUM_PREFIX: &str = "tidal:album:";
pub const TIDAL_PLAYLIST_PREFIX: &str = "tidal:playlist:";

/// True for any id the daemon minted for a TIDAL item (`tidal:<kind>:<id>`). TIDAL tracks stream
/// over plain HTTPS, either a progressive file or a DASH presentation written to disk, so they take
/// the same mpv path SoundCloud does rather than the Spotify FIFO.
pub fn is_tidal_id(id: &str) -> bool {
    id.starts_with("tidal:")
}

/// The numeric id of a TIDAL track id (`tidal:track:<num>`), or None for anything else.
pub fn tidal_track_id(id: &str) -> Option<u64> {
    id.strip_prefix(TIDAL_TRACK_PREFIX)?.parse().ok()
}

/// The numeric id of a TIDAL artist id (`tidal:artist:<num>`).
pub fn tidal_artist_id(id: &str) -> Option<u64> {
    id.strip_prefix(TIDAL_ARTIST_PREFIX)?.parse().ok()
}

/// The numeric id of a TIDAL album id (`tidal:album:<num>`).
pub fn tidal_album_id(id: &str) -> Option<u64> {
    id.strip_prefix(TIDAL_ALBUM_PREFIX)?.parse().ok()
}

/// The uuid of a TIDAL playlist id (`tidal:playlist:<uuid>`). TIDAL keys playlists by uuid rather
/// than a number, so this one stays a string.
pub fn tidal_playlist_id(id: &str) -> Option<&str> {
    id.strip_prefix(TIDAL_PLAYLIST_PREFIX).filter(|s| !s.is_empty())
}

const RECOVERY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// Owns the Spotify sign-in, the current client and the catalogue selector. Construct once with the
/// daemon's data directory; `spotify` credentials land under `<data_dir>/spotify`.
pub struct SpotifyState {
    provider: ParkingRwLock<Arc<SpotifyProvider>>,
    client: RwLock<Option<Arc<Client>>>,
    last_recovery: ParkingMutex<Option<std::time::Instant>>,
    /// Set when the stored credentials were refused for a reason no retry fixes (no Premium,
    /// out of region, revoked). Recovery then stays off until a sign-in or sign-out, instead of
    /// reconnecting librespot once per cooldown for an account that can never play.
    refused: std::sync::atomic::AtomicBool,
    /// Serialises session restores so an on-demand recovery never runs a second librespot
    /// connect against the same cached credentials while the startup restore is mid-flight.
    recover_lock: tokio::sync::Mutex<()>,
    selected: ParkingRwLock<Provider>,
    /// When true, the provider operates in metadata-only mode (free account).
    metadata_only: std::sync::atomic::AtomicBool,
}

impl SpotifyState {
    pub fn new(data_dir: PathBuf, selected: Provider, metadata_only: bool) -> Self {
        let config = SpotifyConfig {
            client_id: DEFAULT_CLIENT_ID.to_owned(),
            redirect_uri: DEFAULT_REDIRECT_URI.to_owned(),
            metadata_only,
        };
        Self {
            provider: ParkingRwLock::new(Arc::new(SpotifyProvider::with_config(data_dir, config))),
            client: RwLock::new(None),
            last_recovery: ParkingMutex::new(None),
            refused: std::sync::atomic::AtomicBool::new(false),
            recover_lock: tokio::sync::Mutex::new(()),
            selected: ParkingRwLock::new(selected),
            metadata_only: std::sync::atomic::AtomicBool::new(metadata_only),
        }
    }

    /// The catalogue the browsing commands read from right now.
    pub fn selected(&self) -> Provider {
        *self.selected.read()
    }

    pub fn select(&self, p: Provider) {
        *self.selected.write() = p;
    }

    /// Whether browsing should go to Spotify: selected AND signed in. Selected without a session
    /// is the sign-in state the client shows, not a catalogue.
    pub async fn browsing_spotify(&self) -> bool {
        self.selected() == Provider::Spotify && self.status().await
    }

    /// Whether browsing should go to SoundCloud. SoundCloud is guest-only (no account), so this
    /// is just the selection — there is no sign-in gate like [`Self::browsing_spotify`].
    pub fn browsing_soundcloud(&self) -> bool {
        self.selected() == Provider::Soundcloud
    }

    /// Whether browsing should go to TIDAL. Unlike SoundCloud there is no guest catalogue, so the
    /// caller still has to check that a session exists before it asks for anything.
    pub fn browsing_tidal(&self) -> bool {
        self.selected() == Provider::Tidal
    }

    /// Whether a credential file exists to restore from (no network).
    pub fn stored(&self) -> bool {
        self.provider.read().stored()
    }

    /// Whether the provider is in metadata-only mode (free account).
    pub fn is_metadata_only(&self) -> bool {
        self.metadata_only.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether a usable client exists.
    pub async fn status(&self) -> bool {
        self.client().await.is_some()
    }

    /// The signed-in client, cloned out for use without holding the lock.
    pub async fn client(&self) -> Option<Arc<Client>> {
        if let Some(client) = self.live_client().await {
            return Some(client);
        }
        if !self.provider.read().stored()
            || self.refused.load(std::sync::atomic::Ordering::Relaxed)
            || !self.may_recover()
        {
            return None;
        }
        match self.restore().await {
            Ok(true) => {
                tracing::info!("spotify: recovered the cached session on demand");
                self.live_client().await
            }
            Ok(false) => None,
            Err(e) => {
                tracing::warn!(error = %e, "spotify: on-demand session recovery failed");
                None
            }
        }
    }

    fn may_recover(&self) -> bool {
        let mut last = self.last_recovery.lock();
        if matches!(*last, Some(at) if at.elapsed() < RECOVERY_COOLDOWN) {
            return false;
        }
        *last = Some(std::time::Instant::now());
        true
    }

    /// A session that is both present and still connected; drops (and logs) one that died.
    async fn live_client(&self) -> Option<Arc<Client>> {
        let live = {
            let guard = self.client.read().await;
            match guard.as_ref() {
                Some(c) if c.alive() => Some(Arc::clone(c)),
                Some(_) => None,
                None => return None,
            }
        };
        if live.is_none() {
            // Drops a session librespot invalidated; `restore_locked` serialises actual
            // (re)connects so this never races a connect in flight.
            let dead = self.client.write().await.take();
            if dead.is_some() {
                tracing::warn!("spotify: the session was invalidated; treating as signed out");
            }
        }
        live
    }

    /// Restore a client from cached credentials. Returns whether one was restored (or was
    /// already live). Serialized on `recover_lock`: a startup restore still connecting and an
    /// on-demand recovery racing over one credential cache is how a good session gets
    /// invalidated — and under the lock the restore we waited for may already have landed, in
    /// which case this simply returns true.
    pub async fn restore(&self) -> Result<bool> {
        let _guard = self.recover_lock.lock().await;
        self.restore_locked().await
    }

    /// The caller holds `recover_lock`.
    async fn restore_locked(&self) -> Result<bool> {
        if self.live_client().await.is_some() {
            return Ok(true);
        }
        let provider = self.provider.read().clone();
        let restored = provider.restore().await.inspect_err(|e| {
            if retry_cannot_fix(e) {
                self.refused.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })?;
        match restored {
            Some(client) => {
                *self.client.write().await = Some(Arc::new(client));
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Run the OAuth flow. `on_url` receives the authorization URL to hand to the client; on success
    /// the resulting client is stored, and a fresh credential lifetime arms recovery again.
    pub async fn sign_in<F>(&self, on_url: F) -> Result<()>
    where
        F: FnOnce(String) + Send + 'static,
    {
        let provider = self.provider.read().clone();
        let client = provider.sign_in(on_url).await?;
        *self.client.write().await = Some(Arc::new(client));
        *self.last_recovery.lock() = None;
        self.refused.store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Forget the cached credentials and drop the current client.
    pub async fn sign_out(&self) {
        let provider = self.provider.read().clone();
        provider.sign_out();
        *self.client.write().await = None;
        *self.last_recovery.lock() = None;
        self.refused.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Update the metadata-only mode and force a re-restore with the new setting.
    /// Call this when the user toggles the "Spotify free account (metadata only)" setting.
    pub async fn set_metadata_only(&self, metadata_only: bool) {
        let current = self.metadata_only.load(std::sync::atomic::Ordering::Relaxed);
        if current == metadata_only {
            return;
        }
        self.metadata_only.store(metadata_only, std::sync::atomic::Ordering::Relaxed);
        // Update the provider's auth config so future restores/sign-ins use the new mode.
        {
            let provider = self.provider.write().clone();
            provider.auth.metadata_only.store(metadata_only, std::sync::atomic::Ordering::Relaxed);
        }
        // Drop the current client so it gets recreated with the new mode on next use.
        *self.client.write().await = None;
        *self.last_recovery.lock() = None;
        self.refused.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether a failed restore is about the account itself rather than the connection.
fn retry_cannot_fix(e: &anyhow::Error) -> bool {
    use ryotunes_spotify::{SignInFailure, SignInProblem};
    matches!(
        e.downcast_ref::<SignInFailure>(),
        Some(SignInFailure(
            SignInProblem::Premium | SignInProblem::Region | SignInProblem::Credentials
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_spent_recovery_re_arms_after_the_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("spotify");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("credentials.json"), b"{").unwrap();

        let state = SpotifyState::new(dir.path().to_path_buf(), Provider::Spotify, false);
        assert!(state.stored());
        assert!(state.client().await.is_none());
        assert!(!state.may_recover());

        *state.last_recovery.lock() = Some(std::time::Instant::now() - RECOVERY_COOLDOWN);
        assert!(state.may_recover());
    }

    #[test]
    fn only_account_refusals_stop_recovery() {
        use ryotunes_spotify::{SignInFailure, SignInProblem};
        let refused = |p| retry_cannot_fix(&anyhow::Error::new(SignInFailure(p)));

        assert!(refused(SignInProblem::Premium));
        assert!(refused(SignInProblem::Region));
        assert!(refused(SignInProblem::Credentials));
        assert!(!refused(SignInProblem::Network));
        assert!(!refused(SignInProblem::Refused));
        assert!(!retry_cannot_fix(&anyhow::anyhow!("Connection to server closed")));
    }

    #[test]
    fn spotify_state_defaults_to_full_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let state = SpotifyState::new(dir.path().to_path_buf(), Provider::Spotify, false);
        assert!(!state.is_metadata_only());
    }

    #[test]
    fn spotify_state_can_be_metadata_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = SpotifyState::new(dir.path().to_path_buf(), Provider::Spotify, true);
        assert!(state.is_metadata_only());
    }

    #[tokio::test]
    async fn set_metadata_only_updates_flag_and_clears_client() {
        let dir = tempfile::tempdir().unwrap();
        let state = SpotifyState::new(dir.path().to_path_buf(), Provider::Spotify, false);
        
        assert!(!state.is_metadata_only());
        
        state.set_metadata_only(true).await;
        
        assert!(state.is_metadata_only());
        // Client should be cleared so it gets recreated with new mode
        assert!(state.client().await.is_none());
    }

    #[tokio::test]
    async fn set_metadata_only_to_false_updates_flag() {
        let dir = tempfile::tempdir().unwrap();
        let state = SpotifyState::new(dir.path().to_path_buf(), Provider::Spotify, true);
        
        assert!(state.is_metadata_only());
        
        state.set_metadata_only(false).await;
        
        assert!(!state.is_metadata_only());
    }
}
