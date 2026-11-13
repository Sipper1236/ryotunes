//! Spotify metadata client used by metadata-only accounts.
//!
//! librespot's Connect session is Premium-only, including for catalogue requests. This client
//! keeps Spotify's web-session token directly and reads catalogue metadata through Pathfinder. It
//! deliberately has no playback path; Ryotunes resolves its track IDs through YouTube Music when
//! metadata-only mode is enabled.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use hmac::{Hmac, Mac};
use oauth2::basic::BasicClient;
use oauth2::{ClientId, RefreshToken, TokenResponse, TokenUrl};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::Sha1;
use tokio::sync::Mutex;

use crate::{
    Album, AlbumDetail, Artist, ArtistProfile, ArtistRef, Playlist, PlaylistDetail, ReleaseType,
    Track, UserProfile,
};

const API: &str = "https://api.spotify.com/v1";
const PATHFINDER: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const WEB_TOKEN: &str = "https://open.spotify.com/api/token";
const SERVER_TIME: &str = "https://open.spotify.com/api/server-time";
const NUANCES: &str =
    "https://gist.githubusercontent.com/raw/22ed9c6ba463899e933427f7de1f0eef/nuances.json";
const TOKEN: &str = "https://accounts.spotify.com/api/token";
const TOKEN_FILE: &str = "web-api-token.json";
const PAGE_SIZE: u32 = 50;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub session_cookie: Option<String>,
    pub expires_at: u64,
}

impl Tokens {
    pub(crate) fn from_oauth<T: TokenResponse>(response: &T) -> Option<Self> {
        Some(Self {
            access_token: response.access_token().secret().to_owned(),
            refresh_token: Some(response.refresh_token()?.secret().to_owned()),
            session_cookie: None,
            expires_at: now().saturating_add(response.expires_in()?.as_secs()),
        })
    }
}

pub(crate) struct WebApiClient {
    client_id: String,
    token_path: PathBuf,
    http: reqwest::Client,
    tokens: Mutex<Tokens>,
}

impl WebApiClient {
    pub(crate) fn new(client_id: String, cache_dir: &Path, tokens: Tokens) -> Result<Self> {
        let token_path = cache_dir.join(TOKEN_FILE);
        persist(&token_path, &tokens)?;
        Ok(Self { client_id, token_path, http: reqwest::Client::new(), tokens: Mutex::new(tokens) })
    }

    /// Authenticate a free account using the first-party Spotify web session cookie.
    pub(crate) async fn from_session_cookie(
        client_id: String,
        cache_dir: &Path,
        session_cookie: String,
    ) -> Result<Self> {
        let http = reqwest::Client::new();
        let (access_token, expires_at) = web_token(&http, &session_cookie).await?;
        Self::new(
            client_id,
            cache_dir,
            Tokens {
                access_token,
                refresh_token: None,
                session_cookie: Some(session_cookie),
                expires_at,
            },
        )
    }

    pub(crate) async fn restore(client_id: String, cache_dir: &Path) -> Result<Option<Self>> {
        let token_path = cache_dir.join(TOKEN_FILE);
        let bytes = match std::fs::read(&token_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("cannot read Spotify Web API credentials"),
        };
        let mut tokens: Tokens =
            serde_json::from_slice(&bytes).context("cannot decode Spotify Web API credentials")?;
        // Tokens written by the earlier PKCE metadata implementation cannot access the free
        // account path. Discard them and ask for first-party web sign-in instead of silently
        // falling back to the Premium-gated public Web API.
        if tokens.session_cookie.is_none() {
            crate::credentials::remove(&token_path);
            return Ok(None);
        }
        let client = Self {
            client_id,
            token_path,
            http: reqwest::Client::new(),
            tokens: Mutex::new(tokens.clone()),
        };
        if tokens.expires_at <= now().saturating_add(30) {
            tokens = client.refresh(&tokens).await?;
            persist(&client.token_path, &tokens)?;
            *client.tokens.lock().await = tokens;
        }
        Ok(Some(client))
    }

    pub(crate) fn stored(cache_dir: &Path) -> bool {
        cache_dir.join(TOKEN_FILE).is_file()
    }

    pub(crate) fn forget(cache_dir: &Path) {
        crate::credentials::remove(&cache_dir.join(TOKEN_FILE));
    }

    pub(crate) fn alive(&self) -> bool {
        self.token_path.is_file()
    }

    async fn access_token(&self) -> Result<String> {
        let mut tokens = self.tokens.lock().await;
        if tokens.expires_at <= now().saturating_add(30) {
            *tokens = self.refresh(&tokens).await?;
            persist(&self.token_path, &tokens)?;
        }
        Ok(tokens.access_token.clone())
    }

    async fn refresh(&self, previous: &Tokens) -> Result<Tokens> {
        if let Some(cookie) = &previous.session_cookie {
            let (access_token, expires_at) = web_token(&self.http, cookie).await?;
            return Ok(Tokens {
                access_token,
                refresh_token: None,
                session_cookie: Some(cookie.clone()),
                expires_at,
            });
        }
        let refresh_token = previous
            .refresh_token
            .as_deref()
            .ok_or_else(|| anyhow!("Spotify session is missing refresh credentials"))?;
        let client = BasicClient::new(ClientId::new(self.client_id.clone()))
            .set_token_uri(TokenUrl::new(TOKEN.to_owned()).context("invalid Spotify token URL")?);
        let response = client
            .exchange_refresh_token(&RefreshToken::new(refresh_token.to_owned()))
            .request_async(&self.http)
            .await
            .context("Spotify access token refresh failed")?;
        Ok(Tokens {
            access_token: response.access_token().secret().to_owned(),
            refresh_token: Some(
                response
                    .refresh_token()
                    .map(|token| token.secret().to_owned())
                    .unwrap_or_else(|| refresh_token.to_owned()),
            ),
            session_cookie: None,
            expires_at: now().saturating_add(
                response.expires_in().map(|expires| expires.as_secs()).unwrap_or(3600),
            ),
        })
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let url = format!("{API}{path}");
        let token = self.access_token().await?;
        let mut response = self.send(&method, &url, &token, body.as_ref()).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let mut tokens = self.tokens.lock().await;
            *tokens = self.refresh(&tokens).await?;
            persist(&self.token_path, &tokens)?;
            response = self.send(&method, &url, &tokens.access_token, body.as_ref()).await?;
        }
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            anyhow::bail!("Spotify Web API returned {status}: {}", safe_error(&message));
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        response.json().await.context("Spotify Web API returned invalid JSON")
    }

    async fn send(
        &self,
        method: &reqwest::Method,
        url: &str,
        token: &str,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let mut request = self.http.request(method.clone(), url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(body);
        }
        request.send().await.context("cannot reach Spotify Web API")
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None).await
    }

    async fn write(&self, method: reqwest::Method, path: &str, body: Value) -> Result<Value> {
        self.request(method, path, Some(body)).await
    }

    async fn pathfinder(&self, operation: &str, hash: &str, variables: Value) -> Result<Value> {
        let url = PATHFINDER;
        let payload = json!({
            "operationName": operation,
            "variables": variables,
            "extensions": { "persistedQuery": { "version": 1, "sha256Hash": hash } }
        });
        let cookie = self.tokens.lock().await.session_cookie.clone();
        let token = self.access_token().await?;
        anyhow::ensure!(cookie.is_some(), "Spotify internal metadata requires a web session");
        let send = |token: &str| {
            self.http
                .post(url)
                .bearer_auth(token)
                .header(
                    reqwest::header::COOKIE,
                    format!("sp_dc={}", cookie.as_deref().unwrap_or_default()),
                )
                .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
                .json(&payload)
                .send()
        };
        let mut response = send(&token).await.context("cannot reach Spotify metadata service")?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let mut tokens = self.tokens.lock().await;
            *tokens = self.refresh(&tokens).await?;
            persist(&self.token_path, &tokens)?;
            response = send(&tokens.access_token).await?;
        }
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let error = serde_json::from_str(&body).unwrap_or(Value::Null);
            let message = pathfinder_error_message(&error, &token, cookie.as_deref().unwrap_or(""));
            anyhow::bail!("Spotify metadata service returned {status}: {message}");
        }
        let value: Value =
            response.json().await.context("Spotify metadata response was invalid JSON")?;
        if value.pointer("/errors/0").is_some() {
            let message = pathfinder_error_message(&value, &token, cookie.as_deref().unwrap_or(""));
            anyhow::bail!("Spotify metadata query failed: {message}");
        }
        Ok(value)
    }

    pub(crate) async fn profile(&self) -> Result<UserProfile> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self
                .pathfinder(
                    "profileAttributes",
                    "53bcb064f6cd18c23f752bc324a791194d20df612d8e1239c735144ab0399ced",
                    json!({}),
                )
                .await?;
            let profile = response
                .pointer("/data/me/profile")
                .ok_or_else(|| anyhow!("Spotify profile is unavailable"))?;
            return Ok(UserProfile {
                id: profile.get("username").and_then(Value::as_str).unwrap_or_default().to_owned(),
                display_name: profile
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        let value = self.get("/me").await?;
        Ok(UserProfile {
            id: string(&value, "id"),
            display_name: value
                .get("display_name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| string(&value, "id")),
        })
    }

    pub(crate) async fn search(&self, query: &str) -> Result<crate::SearchResults> {
        if query.trim().is_empty() {
            return Ok(crate::SearchResults::default());
        }
        let internal = self.tokens.lock().await.session_cookie.is_some();
        if internal {
            let response = self
                .pathfinder(
                    "searchDesktop",
                    "d9f785900f0710b31c07818d617f4f7600c1e21217e80f5b043d1e78d74e6026",
                    json!({ "searchTerm": query, "offset": 0, "limit": 10,
                    "numberOfTopResults": 5, "includeAudiobooks": true,
                    "includeArtistHasConcertsField": false, "includePreReleases": true,
                    "includeLocalConcertsField": false, "includeAuthors": false }),
                )
                .await?;
            let search = response.pointer("/data/searchV2").unwrap_or(&Value::Null);
            let track_rows = search
                .pointer("/tracksV2/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.pointer("/item/data"));
            let tracks: Vec<_> = track_rows.map(track_from_pathfinder).collect();
            let albums = search
                .pointer("/albumsV2/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("data"))
                .map(album_from_pathfinder)
                .collect();
            let playlists = search
                .pointer("/playlists/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("data"))
                .map(playlist_from_pathfinder)
                .collect();
            let mut artists: Vec<ArtistRef> = search
                .pointer("/artists/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("data"))
                .map(|artist| ArtistRef {
                    name: artist
                        .pointer("/profile/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    id: artist
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(|uri| uri.rsplit(':').next())
                        .map(str::to_owned),
                })
                .collect();
            for track in &tracks {
                for artist in &track.artist_refs {
                    if !artists.iter().any(|existing: &ArtistRef| existing.id == artist.id) {
                        artists.push(artist.clone());
                    }
                }
            }
            return Ok(crate::SearchResults { tracks, artists, albums, playlists });
        }
        let query =
            url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect::<String>();
        let value = self
            .get(&format!("/search?q={query}&type=track,album,artist,playlist&limit=10"))
            .await?;
        let tracks = items(&value, "tracks").iter().map(track_from).collect::<Vec<_>>();
        Ok(crate::SearchResults {
            artists: items(&value, "artists")
                .iter()
                .map(|artist| ArtistRef {
                    name: string(artist, "name"),
                    id: optional_string(artist, "id"),
                })
                .collect(),
            albums: items(&value, "albums").iter().map(album_from).collect(),
            playlists: items(&value, "playlists").iter().map(playlist_from).collect(),
            tracks,
        })
    }

    pub(crate) async fn search_albums(&self, query: &str) -> Result<Vec<Album>> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self
                .pathfinder(
                    "searchAlbums",
                    "a71d2c993fc98e1c880093738a55a38b57e69cc4ce5a8c113e6c5920f9513ee2",
                    search_variables(query),
                )
                .await?;
            return Ok(response
                .pointer("/data/searchV2/albumsV2/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("data"))
                .map(album_from_pathfinder)
                .collect());
        }
        let query =
            url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect::<String>();
        let value = self.get(&format!("/search?q={query}&type=album&limit=10")).await?;
        Ok(items(&value, "albums").iter().map(album_from).collect())
    }

    pub(crate) async fn search_playlists(&self, query: &str) -> Result<Vec<Playlist>> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self
                .pathfinder(
                    "searchPlaylists",
                    "fc3a690182167dbad20ac7a03f842b97be4e9737710600874cb903f30112ad58",
                    search_variables(query),
                )
                .await?;
            return Ok(response
                .pointer("/data/searchV2/playlists/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("data"))
                .map(playlist_from_pathfinder)
                .collect());
        }
        let query =
            url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect::<String>();
        let value = self.get(&format!("/search?q={query}&type=playlist&limit=10")).await?;
        Ok(items(&value, "playlists").iter().map(playlist_from).collect())
    }

    pub(crate) async fn track(&self, id: &str) -> Result<Track> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self
                .pathfinder(
                    "getTrack",
                    "612585ae06ba435ad26369870deaae23b5c8800a256cd8a57e08eddc25a37294",
                    json!({ "uri": format!("spotify:track:{id}") }),
                )
                .await?;
            let track = response
                .pointer("/data/trackUnion")
                .ok_or_else(|| anyhow!("Spotify track was not found"))?;
            return Ok(track_from_pathfinder(track));
        }
        Ok(track_from(&self.get(&format!("/tracks/{id}")).await?))
    }

    pub(crate) async fn liked(&self, page: u32) -> Result<Vec<Track>> {
        let offset = page.saturating_mul(PAGE_SIZE);
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self
                .pathfinder(
                    "fetchLibraryTracks",
                    "087278b20b743578a6262c2b0b4bcd20d879c503cc359a2285baf083ef944240",
                    json!({"offset": offset, "limit": PAGE_SIZE}),
                )
                .await?;
            let rows = response
                .pointer("/data/me/library/items")
                .or_else(|| response.pointer("/data/me/library/tracks/items"))
                .or_else(|| response.pointer("/data/me/library/tracks"))
                .and_then(Value::as_array);
            return Ok(rows.into_iter().flatten().map(library_track_from_pathfinder).collect());
        }
        let value = self.get(&format!("/me/tracks?limit={PAGE_SIZE}&offset={offset}")).await?;
        Ok(library_items(&value).into_iter().map(track_from).collect())
    }

    pub(crate) async fn saved_tracks(&self, limit: u32) -> Result<Vec<Track>> {
        let mut all = Vec::new();
        for page in 0..limit.div_ceil(PAGE_SIZE) {
            let mut found = self.liked(page).await?;
            all.append(&mut found);
        }
        all.truncate(limit as usize);
        Ok(all)
    }

    pub(crate) async fn playlists(&self, limit: u32) -> Result<Vec<Playlist>> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let rows = self.library_v3("Playlists", limit).await?;
            return Ok(rows
                .iter()
                .filter_map(|row| row.pointer("/item/data"))
                .map(playlist_from_pathfinder)
                .collect());
        }
        let current_user = self.profile().await?.id;
        let mut output = Vec::new();
        while output.len() < limit as usize {
            let take = PAGE_SIZE.min(limit.saturating_sub(output.len() as u32));
            let value =
                self.get(&format!("/me/playlists?limit={take}&offset={}", output.len())).await?;
            let batch = items(&value, "items")
                .iter()
                .map(|item| {
                    let mut playlist = playlist_from(item);
                    playlist.owned = playlist.owner_id == current_user;
                    playlist
                })
                .collect::<Vec<_>>();
            let count = batch.len();
            output.extend(batch);
            if count < take as usize {
                break;
            }
        }
        Ok(output)
    }

    pub(crate) async fn saved_albums(&self, limit: u32) -> Result<Vec<Album>> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let rows = self.library_v3("Albums", limit).await?;
            return Ok(rows
                .iter()
                .filter_map(|row| row.pointer("/item/data"))
                .map(album_from_pathfinder)
                .collect());
        }
        let mut output = Vec::new();
        while output.len() < limit as usize {
            let take = PAGE_SIZE.min(limit.saturating_sub(output.len() as u32));
            let value =
                self.get(&format!("/me/albums?limit={take}&offset={}", output.len())).await?;
            let batch = library_items(&value).into_iter().map(album_from).collect::<Vec<_>>();
            let count = batch.len();
            output.extend(batch);
            if count < take as usize {
                break;
            }
        }
        Ok(output)
    }

    pub(crate) async fn saved_artists(&self, limit: u32) -> Result<Vec<crate::SavedArtist>> {
        if self.tokens.lock().await.session_cookie.is_none() {
            return Ok(Vec::new());
        }
        let rows = self.library_v3("Artists", limit).await?;
        Ok(rows
            .iter()
            .filter_map(|row| row.pointer("/item/data"))
            .map(saved_artist_from_pathfinder)
            .collect())
    }

    async fn library_v3(&self, filter: &str, limit: u32) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let first_take = PAGE_SIZE.min(limit);
        let mut output;
        let mut offset;
        if limit > PAGE_SIZE {
            let second_take = PAGE_SIZE.min(limit - PAGE_SIZE);
            let (first, second) = first_two_library_pages(
                self.library_v3_page(filter, 0, first_take),
                self.library_v3_page(filter, PAGE_SIZE, second_take),
            )
            .await?;
            let first_full = first.len() >= first_take as usize;
            output = first;
            offset = output.len() as u32;
            if first_full {
                if let Some(second) = second {
                    let second_full = second.len() >= second_take as usize;
                    output.extend(second);
                    offset = output.len() as u32;
                    if !second_full {
                        output.truncate(limit as usize);
                        return Ok(output);
                    }
                }
            } else {
                output.truncate(limit as usize);
                return Ok(output);
            }
        } else {
            output = self.library_v3_page(filter, 0, first_take).await?;
            if output.len() < first_take as usize {
                output.truncate(limit as usize);
                return Ok(output);
            }
            offset = output.len() as u32;
        }

        while output.len() < limit as usize {
            let take = PAGE_SIZE.min(limit.saturating_sub(offset));
            let batch = self.library_v3_page(filter, offset, take).await?;
            let count = batch.len();
            output.extend(batch);
            offset = output.len() as u32;
            if count < take as usize {
                break;
            }
        }
        output.truncate(limit as usize);
        Ok(output)
    }

    async fn library_v3_page(&self, filter: &str, offset: u32, limit: u32) -> Result<Vec<Value>> {
        let response = self
            .pathfinder(
                "libraryV3",
                "390c78e5b951029bad359785e69b07b536a509c581cbcd0aded5e5067f187455",
                json!({ "filters": [filter], "order": null, "textFilter": "", "features": [],
                    "limit": limit, "offset": offset, "flatten": true,
                    "expandedFolders": [], "folderUri": null, "includeFoldersWhenFlattening": true }),
            )
            .await?;
        Ok(response
            .pointer("/data/me/libraryV3/items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) async fn album(&self, id: &str) -> Result<AlbumDetail> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let mut album_data = None;
            let mut tracks = Vec::new();
            let mut offset = 0usize;
            loop {
                let response = self.pathfinder("getAlbum",
                    "b9bfabef66ed756e5e13f68a942deb60bd4125ec1f1be8cc42769dc0259b4b10",
                    json!({"uri": format!("spotify:album:{id}"), "locale": "", "offset": offset, "limit": PAGE_SIZE}),
                ).await?;
                let Some(data) = response.pointer("/data/albumUnion") else {
                    anyhow::bail!("Spotify album was not found");
                };
                if album_data.is_none() {
                    album_data = Some(data.clone());
                }
                let page = data
                    .pointer("/tracksV2/items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let count = page.len();
                tracks.extend(
                    page.iter().filter_map(|item| item.get("track")).map(track_from_pathfinder),
                );
                offset += count;
                let total = data
                    .pointer("/tracksV2/totalCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(offset as u64) as usize;
                if count < PAGE_SIZE as usize || offset >= total {
                    break;
                }
            }
            let data = album_data.ok_or_else(|| anyhow!("Spotify album was not found"))?;
            let album = album_from_pathfinder(&data);
            let tracks = tracks
                .into_iter()
                .map(|mut track| {
                    track.album = album.name.clone();
                    track.album_id = Some(album.id.clone());
                    track.cover = album.cover.clone();
                    track
                })
                .collect();
            return Ok(AlbumDetail { album, tracks });
        }
        let value = self.get(&format!("/albums/{id}")).await?;
        let album = album_from(&value);
        let first_page = value.get("tracks").unwrap_or(&Value::Null);
        let mut tracks = items_value(first_page)
            .iter()
            .map(|track| track_from_album(track, &album))
            .collect::<Vec<_>>();
        let mut has_more = first_page.get("next").and_then(Value::as_str).is_some();
        while has_more {
            let offset = tracks.len();
            let page =
                self.get(&format!("/albums/{id}/tracks?limit={PAGE_SIZE}&offset={offset}")).await?;
            let batch = items_value(&page)
                .iter()
                .map(|track| track_from_album(track, &album))
                .collect::<Vec<_>>();
            let count = batch.len();
            tracks.extend(batch);
            has_more = page.get("next").and_then(Value::as_str).is_some();
            if count < PAGE_SIZE as usize {
                break;
            }
        }
        Ok(AlbumDetail { album, tracks })
    }

    pub(crate) async fn artist(&self, id: &str) -> Result<Artist> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self.pathfinder("queryArtistOverview", "7f86ff63e38c24973a2842b672abe44c910c1973978dc8a4a0cb648edef34527",
                json!({"uri": format!("spotify:artist:{id}"), "locale": "", "preReleaseV2": false})).await?;
            let data = response
                .pointer("/data/artistUnion")
                .ok_or_else(|| anyhow!("Spotify artist was not found"))?;
            let profile = artist_profile_pathfinder(data);
            let albums = data
                .pointer("/discography/popularReleasesAlbums/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(album_from_pathfinder)
                .collect();
            let top_tracks = data
                .pointer("/discography/topTracks/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("track"))
                .map(track_from_pathfinder)
                .collect();
            return Ok(Artist {
                name: profile.name,
                cover_large: profile.cover_large,
                biography: profile.biography,
                monthly_listeners: data.pointer("/stats/monthlyListeners").and_then(Value::as_u64),
                top_tracks,
                albums,
            });
        }
        let value = self.get(&format!("/artists/{id}")).await?;
        let artist = artist_profile(&value);
        let albums = self
            .get(&format!("/artists/{id}/albums?include_groups=album,single,compilation&limit=50"))
            .await?;
        Ok(Artist {
            name: artist.name,
            cover_large: artist.cover_large,
            biography: None,
            monthly_listeners: None,
            // Spotify removed this endpoint from Web API; the rest of the artist catalogue stays browsable.
            top_tracks: Vec::new(),
            albums: items(&albums, "items").iter().map(album_from).collect(),
        })
    }

    pub(crate) async fn artist_profile(&self, id: &str) -> Result<ArtistProfile> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self.pathfinder("queryArtistOverview", "7f86ff63e38c24973a2842b672abe44c910c1973978dc8a4a0cb648edef34527",
                json!({"uri": format!("spotify:artist:{id}"), "locale": "", "preReleaseV2": false})).await?;
            let artist = response
                .pointer("/data/artistUnion")
                .ok_or_else(|| anyhow!("Spotify artist was not found"))?;
            return Ok(artist_profile_pathfinder(artist));
        }
        Ok(artist_profile(&self.get(&format!("/artists/{id}")).await?))
    }

    pub(crate) async fn playlist(&self, id: &str) -> Result<PlaylistDetail> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let mut offset = 0usize;
            let mut playlist_data = None;
            let mut tracks = Vec::new();
            loop {
                let response = self.pathfinder("fetchPlaylist", "cd2275433b29f7316176e7b5b5e098ae7744724e1a52d63549c76636b3257749",
                    json!({"uri": format!("spotify:playlist:{id}"), "offset": offset, "limit": PAGE_SIZE, "enableWatchFeedEntrypoint": true})).await?;
                let Some(data) = response.pointer("/data/playlistV2") else {
                    anyhow::bail!("Spotify playlist was not found");
                };
                if playlist_data.is_none() {
                    playlist_data = Some(data.clone());
                }
                let page = data
                    .pointer("/content/items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let count = page.len();
                tracks.extend(
                    page.iter()
                        .filter_map(|item| item.pointer("/itemV2/data"))
                        .map(track_from_pathfinder),
                );
                offset += count;
                let total = data
                    .pointer("/content/totalCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(offset as u64) as usize;
                if count < PAGE_SIZE as usize || offset >= total {
                    break;
                }
            }
            let data = playlist_data.ok_or_else(|| anyhow!("Spotify playlist was not found"))?;
            let playlist = playlist_from_pathfinder(&data);
            return Ok(PlaylistDetail { playlist, tracks });
        }
        let value = self.get(&format!("/playlists/{id}")).await?;
        let current_user = self.profile().await?.id;
        let mut playlist = playlist_from(&value);
        playlist.owned = playlist.owner_id == current_user;
        let mut tracks = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self
                .get(&format!("/playlists/{id}/items?limit={PAGE_SIZE}&offset={offset}"))
                .await?;
            let rows = items(&page, "items");
            let count = rows.len();
            tracks.extend(
                rows.iter()
                    .filter_map(|row| row.get("item"))
                    .filter(|item| {
                        item.get("type").and_then(Value::as_str).unwrap_or("track") == "track"
                    })
                    .map(track_from),
            );
            offset += count;
            if count < PAGE_SIZE as usize || page.get("next").and_then(Value::as_str).is_none() {
                break;
            }
        }
        Ok(PlaylistDetail { playlist, tracks })
    }

    /// Fetch a single playlist page for the metadata-only browse path.
    pub(crate) async fn playlist_page(&self, id: &str, page: u32) -> Result<PlaylistDetail> {
        let offset = page.saturating_mul(PAGE_SIZE) as usize;
        if self.tokens.lock().await.session_cookie.is_some() {
            let response = self
                .pathfinder(
                    "fetchPlaylist",
                    "cd2275433b29f7316176e7b5b5e098ae7744724e1a52d63549c76636b3257749",
                    json!({
                        "uri": format!("spotify:playlist:{id}"),
                        "offset": offset,
                        "limit": PAGE_SIZE,
                        "enableWatchFeedEntrypoint": true
                    }),
                )
                .await?;
            let data = response
                .pointer("/data/playlistV2")
                .ok_or_else(|| anyhow!("Spotify playlist was not found"))?;
            let tracks = data
                .pointer("/content/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.pointer("/itemV2/data"))
                .map(track_from_pathfinder)
                .collect();
            return Ok(PlaylistDetail { playlist: playlist_from_pathfinder(data), tracks });
        }

        let value = self.get(&format!("/playlists/{id}")).await?;
        let current_user = self.profile().await?.id;
        let mut playlist = playlist_from(&value);
        playlist.owned = playlist.owner_id == current_user;
        let page =
            self.get(&format!("/playlists/{id}/items?limit={PAGE_SIZE}&offset={offset}")).await?;
        let tracks = items(&page, "items")
            .iter()
            .filter_map(|row| row.get("item"))
            .filter(|item| item.get("type").and_then(Value::as_str).unwrap_or("track") == "track")
            .map(track_from)
            .collect();
        Ok(PlaylistDetail { playlist, tracks })
    }

    pub(crate) async fn set_track_saved(&self, id: &str, saved: bool) -> Result<()> {
        let method = if saved { reqwest::Method::PUT } else { reqwest::Method::DELETE };
        self.update_library_item(method, &format!("spotify:track:{id}")).await
    }

    pub(crate) async fn set_album_saved(&self, id: &str, saved: bool) -> Result<()> {
        let method = if saved { reqwest::Method::PUT } else { reqwest::Method::DELETE };
        self.update_library_item(method, &format!("spotify:album:{id}")).await
    }

    async fn update_library_item(&self, method: reqwest::Method, uri: &str) -> Result<()> {
        if self.tokens.lock().await.session_cookie.is_some() {
            let operation =
                if method == reqwest::Method::PUT { "addToLibrary" } else { "removeFromLibrary" };
            self.pathfinder(
                operation,
                "a3c1ff58e6a36fec5fe1e3a193dc95d9071d96b9ba53c5ba9c1494fb1ee73915",
                json!({"uris": [uri]}),
            )
            .await?;
            return Ok(());
        }
        let query =
            url::form_urlencoded::Serializer::new(String::new()).append_pair("uris", uri).finish();
        self.request(method, &format!("/me/library?{query}"), None).await?;
        Ok(())
    }

    pub(crate) async fn create_playlist(&self, name: &str) -> Result<String> {
        if self.tokens.lock().await.session_cookie.is_some() {
            anyhow::bail!(
                "Creating playlists is not supported by the current Spotify web-session integration"
            );
        }
        let value = self
            .write(reqwest::Method::POST, "/me/playlists", json!({ "name": name, "public": false }))
            .await?;
        value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Spotify did not return the created playlist ID"))
    }

    pub(crate) async fn rename_playlist(&self, id: &str, name: &str) -> Result<()> {
        if self.tokens.lock().await.session_cookie.is_some() {
            anyhow::bail!(
                "Renaming playlists is not supported by the current Spotify web-session integration"
            );
        }
        self.write(reqwest::Method::PUT, &format!("/playlists/{id}"), json!({ "name": name }))
            .await?;
        Ok(())
    }

    pub(crate) async fn set_playlist_public(&self, id: &str, public: bool) -> Result<()> {
        if self.tokens.lock().await.session_cookie.is_some() {
            anyhow::bail!(
                "Changing playlist visibility is not supported by the current Spotify web-session integration"
            );
        }
        self.write(reqwest::Method::PUT, &format!("/playlists/{id}"), json!({ "public": public }))
            .await?;
        Ok(())
    }

    pub(crate) async fn add_track(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        if self.tokens.lock().await.session_cookie.is_some() {
            anyhow::bail!(
                "Editing playlists is not supported by the current Spotify web-session integration"
            );
        }
        self.write(
            reqwest::Method::POST,
            &format!("/playlists/{playlist_id}/items"),
            json!({ "uris": [format!("spotify:track:{track_id}")] }),
        )
        .await?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct ServerTime {
    #[serde(rename = "serverTime")]
    server_time: u64,
}

#[derive(Deserialize)]
struct Nuance {
    v: u32,
    s: String,
}

#[derive(Deserialize)]
struct WebToken {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "accessTokenExpirationTimestampMs")]
    expires_at_ms: u64,
}

/// Exchange Spotify's first-party `sp_dc` cookie for a short-lived web-player token.
/// Spotify rotates the TOTP seed/version; refresh the public seed list once on failure.
async fn web_token(http: &reqwest::Client, session_cookie: &str) -> Result<(String, u64)> {
    anyhow::ensure!(
        !session_cookie.is_empty() && !session_cookie.contains([';', '\r', '\n']),
        "Spotify session cookie has an invalid format"
    );
    let server_time: ServerTime = http
        .get(SERVER_TIME)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("cannot read Spotify server time")?;
    match web_token_attempt(http, session_cookie, server_time.server_time, false).await {
        Ok(tokens) => return Ok(tokens),
        Err(_) => web_token_attempt(http, session_cookie, server_time.server_time, true)
            .await
            .context("Spotify web token exchange failed after refreshing its rotating parameters"),
    }
}

async fn web_token_attempt(
    http: &reqwest::Client,
    session_cookie: &str,
    server_time: u64,
    fresh: bool,
) -> Result<(String, u64)> {
    let mut request = http.get(NUANCES);
    if fresh {
        request = request.query(&[("t", now().to_string())]);
    }
    let nuances: Vec<Nuance> = request
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("cannot read Spotify web token parameters")?;
    let nuance = nuances
        .into_iter()
        .max_by_key(|n| n.v)
        .ok_or_else(|| anyhow!("Spotify did not provide web token parameters"))?;
    let totp = totp(&nuance.s, server_time / 30)?;
    let response = http
        .get(WEB_TOKEN)
        .query(&[
            ("reason", "transport"),
            ("productType", "web-player"),
            ("totp", &totp),
            ("totpServer", &totp),
            ("totpVer", &nuance.v.to_string()),
        ])
        .header(reqwest::header::COOKIE, format!("sp_dc={session_cookie}"))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("Spotify web token endpoint returned {status}");
    }
    let token: WebToken = response.json().await.context("Spotify returned an invalid web token")?;
    anyhow::ensure!(!token.access_token.is_empty(), "Spotify returned an empty web token");
    Ok((token.access_token, token.expires_at_ms / 1000))
}

fn totp(secret: &str, counter: u64) -> Result<String> {
    type HmacSha1 = Hmac<Sha1>;
    let key = data_encoding::BASE32_NOPAD
        .decode(secret.trim_end_matches('=').as_bytes())
        .context("Spotify provided an invalid TOTP secret")?;
    let mut mac =
        HmacSha1::new_from_slice(&key).map_err(|_| anyhow!("invalid Spotify TOTP key"))?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let value = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    Ok(format!("{:06}", value % 1_000_000))
}

fn persist(path: &Path, tokens: &Tokens) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("cannot create Spotify credentials directory")?;
    }
    let bytes = serde_json::to_vec(tokens).context("cannot encode Spotify Web API credentials")?;
    std::fs::write(path, bytes).context("cannot store Spotify Web API credentials")?;
    crate::credentials::secure(path);
    Ok(())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn safe_error(raw: &str) -> String {
    let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    value.pointer("/error/message").and_then(Value::as_str).unwrap_or("request failed").to_owned()
}

fn pathfinder_error_message(value: &Value, access_token: &str, session_cookie: &str) -> String {
    let raw = value
        .pointer("/errors/0/message")
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("request failed");
    let mut message = raw.to_owned();
    for secret in [access_token, session_cookie] {
        if !secret.is_empty() {
            message = message.replace(secret, "[redacted]");
        }
    }
    message.chars().take(300).collect()
}

fn string(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    let Some(value) = value.get(key) else {
        return &[];
    };
    if let Some(items) = value.as_array() { items } else { items_value(value) }
}

fn items_value(value: &Value) -> &[Value] {
    value.get("items").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

fn library_items(value: &Value) -> Vec<&Value> {
    items(value, "items")
        .iter()
        .filter_map(|row| row.get("item").or_else(|| row.get("track")).or_else(|| row.get("album")))
        .collect()
}

fn image(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .and_then(|images| images.first())
        .and_then(|image| image.get("url"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn artists(value: &Value) -> (String, Vec<ArtistRef>) {
    let refs = value
        .get("artists")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|artist| ArtistRef { name: string(artist, "name"), id: optional_string(artist, "id") })
        .collect::<Vec<_>>();
    (refs.iter().map(|artist| artist.name.as_str()).collect::<Vec<_>>().join(", "), refs)
}

fn track_from(value: &Value) -> Track {
    let (artist_names, artist_refs) = artists(value);
    let album = value.get("album").unwrap_or(&Value::Null);
    Track {
        id: optional_string(value, "id"),
        name: string(value, "name"),
        playable: value.get("is_playable").and_then(Value::as_bool).unwrap_or(true),
        artists: artist_names,
        artist_refs,
        album: string(album, "name"),
        album_id: optional_string(album, "id"),
        cover: image(album, "images"),
        duration: Duration::from_millis(
            value.get("duration_ms").and_then(Value::as_u64).unwrap_or_default(),
        ),
        added_at: None,
        added_by: None,
        playcount: None,
        popularity: value.get("popularity").and_then(Value::as_u64).unwrap_or_default() as u32,
        explicit: value.get("explicit").and_then(Value::as_bool).unwrap_or(false),
        track_number: value.get("track_number").and_then(Value::as_u64).unwrap_or_default() as u32,
        disc_number: value.get("disc_number").and_then(Value::as_u64).unwrap_or(1) as u32,
        tags: Vec::new(),
        languages: Vec::new(),
        credits: Vec::new(),
    }
}

fn search_variables(query: &str) -> Value {
    json!({"searchTerm": query, "offset": 0, "limit": 30, "includePreReleases": false,
        "numberOfTopResults": 20, "includeAudiobooks": true, "includeAuthors": false})
}

fn pathfinder_artists(value: &Value) -> Vec<Value> {
    value.pointer("/artists/items").and_then(Value::as_array).into_iter().flatten().map(|artist| {
        json!({ "id": artist.get("id").and_then(Value::as_str).or_else(|| artist.get("uri").and_then(Value::as_str).and_then(|s| s.rsplit(':').next())),
            "name": artist.pointer("/profile/name").and_then(Value::as_str).or_else(|| artist.get("name").and_then(Value::as_str)).unwrap_or("") })
    }).collect()
}

fn cover_sources(value: &Value, pointer: &str) -> Vec<Value> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|source| source.get("url").and_then(Value::as_str))
        .map(|url| json!({"url": url}))
        .collect()
}

fn album_from_pathfinder(value: &Value) -> Album {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| value.get("uri").and_then(Value::as_str).and_then(|s| s.rsplit(':').next()))
        .unwrap_or_default();
    let date = value.pointer("/date/isoString").and_then(Value::as_str).unwrap_or_default();
    let album = json!({"id": id, "name": string(value,"name"), "artists": pathfinder_artists(value),
        "images": cover_sources(value,"/coverArt/sources"), "release_date": date,
        "album_type": value.get("albumType").or_else(|| value.get("type")).and_then(Value::as_str).unwrap_or("album"),
        "total_tracks": value.pointer("/tracks/totalCount").and_then(Value::as_u64).unwrap_or_default(),
        "label": value.get("label").and_then(Value::as_str).unwrap_or("")});
    album_from(&album)
}

fn playlist_from_pathfinder(value: &Value) -> Playlist {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| value.get("uri").and_then(Value::as_str).and_then(|s| s.rsplit(':').next()))
        .unwrap_or_default();
    let owner = value.pointer("/ownerV2/data").unwrap_or(&Value::Null);
    let images = cover_sources(value, "/images/items/0/sources");
    Playlist {
        id: id.to_owned(),
        name: string(value, "name"),
        owner: owner.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        owner_id: owner
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|s| s.rsplit(':').next())
            .unwrap_or_default()
            .to_owned(),
        owned: value
            .pointer("/currentUserCapabilities/canEditItems")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        collaborative: false,
        blend: false,
        public: false,
        cover: images
            .first()
            .and_then(|image| image.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        track_count: value
            .pointer("/content/totalCount")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32,
        modified_at: None,
    }
}

fn track_from_pathfinder(value: &Value) -> Track {
    let artists = value.pointer("/artists/items").and_then(Value::as_array)
        .into_iter().flatten().map(|row| {
            let artist = row.get("profile").map(|_| row).or_else(|| row.get("data")).unwrap_or(row);
            json!({ "id": artist.get("id").and_then(Value::as_str).or_else(|| artist.get("uri").and_then(Value::as_str).and_then(|s| s.rsplit(':').next())),
                "name": artist.pointer("/profile/name").and_then(Value::as_str).or_else(|| artist.get("name").and_then(Value::as_str)).unwrap_or("") })
        }).collect::<Vec<_>>();
    let album = value.get("albumOfTrack").unwrap_or(&Value::Null);
    let images =
        album.pointer("/coverArt/sources").and_then(Value::as_array).cloned().unwrap_or_default();
    let album_value = json!({
        "id": album.get("id").and_then(Value::as_str).or_else(|| album.get("uri").and_then(Value::as_str).and_then(|s| s.rsplit(':').next())),
        "name": album.get("name").and_then(Value::as_str).unwrap_or(""),
        "images": images.iter().map(|image| json!({"url": image.get("url").and_then(Value::as_str).unwrap_or("")})).collect::<Vec<_>>()
    });
    let duration =
        value.pointer("/duration/totalMilliseconds").and_then(Value::as_u64).unwrap_or_default();
    let content_rating =
        value.pointer("/contentRating/label").and_then(Value::as_str).unwrap_or("");
    let uri_id = value
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| value.get("_uri").and_then(Value::as_str).and_then(|s| s.rsplit(':').next()))
        .or_else(|| value.get("uri").and_then(Value::as_str).and_then(|s| s.rsplit(':').next()));
    let mapped = json!({ "id": uri_id, "name": string(value, "name"), "artists": artists,
        "album": album_value, "duration_ms": duration, "explicit": content_rating == "EXPLICIT",
        "track_number": value.get("trackNumber").and_then(Value::as_u64).unwrap_or_default(),
        "disc_number": value.get("discNumber").and_then(Value::as_u64).unwrap_or(1),
        "is_playable": value.pointer("/playability/playable").and_then(Value::as_bool).unwrap_or(true) });
    track_from(&mapped)
}

fn library_track_from_pathfinder(row: &Value) -> Track {
    let data = row
        .pointer("/track/data")
        .or_else(|| row.pointer("/track"))
        .or_else(|| row.get("data"))
        .unwrap_or(row);
    let mut track = data.clone();
    if track.get("id").is_none() && track.get("uri").is_none() {
        if let Some(uri) = row.pointer("/track/_uri").and_then(Value::as_str) {
            track["uri"] = Value::String(uri.to_owned());
        }
    }
    track_from_pathfinder(&track)
}

fn track_from_album(value: &Value, album: &Album) -> Track {
    let mut track = track_from(value);
    track.album = album.name.clone();
    track.album_id = Some(album.id.clone());
    track.cover = album.cover.clone();
    if track.artist_refs.is_empty() {
        track.artist_refs = album.artist_refs.clone();
        track.artists = album.artists.clone();
    }
    track
}

fn album_from(value: &Value) -> Album {
    let (artists, artist_refs) = artists(value);
    let release_date = string(value, "release_date");
    let release_type = match string(value, "album_type").as_str() {
        "single" => ReleaseType::Single,
        "compilation" => ReleaseType::Compilation,
        "ep" => ReleaseType::Ep,
        _ => ReleaseType::Album,
    };
    Album {
        id: string(value, "id"),
        name: string(value, "name"),
        artists,
        artist_refs,
        cover: image(value, "images"),
        cover_large: value
            .get("images")
            .and_then(Value::as_array)
            .and_then(|images| images.last())
            .and_then(|image| image.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        release_type,
        year: release_date.get(..4).and_then(|year| year.parse().ok()).unwrap_or_default(),
        track_count: value.get("total_tracks").and_then(Value::as_u64).unwrap_or_default() as u32,
        release_date,
        label: string(value, "label"),
        copyrights: value
            .get("copyrights")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("text").and_then(Value::as_str).map(str::to_owned))
            .collect(),
        added_at: None,
    }
}

fn playlist_from(value: &Value) -> Playlist {
    let owner = value.get("owner").unwrap_or(&Value::Null);
    Playlist {
        id: string(value, "id"),
        name: string(value, "name"),
        owner: owner.get("display_name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        owner_id: string(owner, "id"),
        owned: false,
        collaborative: value.get("collaborative").and_then(Value::as_bool).unwrap_or(false),
        blend: false,
        public: value.get("public").and_then(Value::as_bool).unwrap_or(false),
        cover: image(value, "images"),
        track_count: value
            .pointer("/items/total")
            .or_else(|| value.pointer("/tracks/total"))
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32,
        modified_at: None,
    }
}

fn artist_profile(value: &Value) -> ArtistProfile {
    ArtistProfile {
        name: string(value, "name"),
        cover_large: value
            .get("images")
            .and_then(Value::as_array)
            .and_then(|images| images.last())
            .and_then(|image| image.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        biography: None,
    }
}

fn artist_profile_pathfinder(value: &Value) -> ArtistProfile {
    let images = cover_sources(value, "/visuals/avatarImage/sources");
    ArtistProfile {
        name: value.pointer("/profile/name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        cover_large: images
            .last()
            .and_then(|image| image.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        biography: value
            .pointer("/profile/biography/text")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn saved_artist_from_pathfinder(value: &Value) -> crate::SavedArtist {
    let id = value
        .get("uri")
        .and_then(Value::as_str)
        .and_then(|uri| uri.rsplit(':').next())
        .unwrap_or_default()
        .to_owned();
    let cover = cover_sources(value, "/visuals/avatarImage/sources")
        .last()
        .and_then(|image| image.get("url"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    crate::SavedArtist {
        id,
        name: value.pointer("/profile/name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        cover,
        added_at: None,
    }
}

async fn first_two_library_pages<F1, F2>(
    first: F1,
    second: F2,
) -> Result<(Vec<Value>, Option<Vec<Value>>)>
where
    F1: Future<Output = Result<Vec<Value>>>,
    F2: Future<Output = Result<Vec<Value>>>,
{
    tokio::pin!(first);
    tokio::pin!(second);
    tokio::select! {
        result = &mut first => {
            let first = result?;
            if first.len() < PAGE_SIZE as usize {
                return Ok((first, None));
            }
            Ok((first, Some(second.await?)))
        }
        result = &mut second => {
            let first = first.await?;
            if first.len() < PAGE_SIZE as usize {
                return Ok((first, None));
            }
            Ok((first, Some(result?)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn library_page_pair_fetches_concurrently_and_preserves_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        async fn page(
            offset: usize,
            active: &AtomicUsize,
            max_active: &AtomicUsize,
        ) -> Result<Vec<Value>> {
            let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now_active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            let count = if offset == 0 { PAGE_SIZE as usize } else { 10 };
            Ok((offset..offset + count).map(|n| json!(n)).collect())
        }

        let active = AtomicUsize::new(0);
        let max_active = AtomicUsize::new(0);
        let (first, second) = first_two_library_pages(
            page(0, &active, &max_active),
            page(PAGE_SIZE as usize, &active, &max_active),
        )
        .await
        .unwrap();

        assert_eq!(max_active.load(Ordering::SeqCst), 2);
        assert_eq!(first.len(), PAGE_SIZE as usize);
        assert_eq!(
            second.unwrap(),
            (0..PAGE_SIZE as usize + 10)
                .map(|n| json!(n))
                .skip(PAGE_SIZE as usize)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn short_first_library_page_discards_an_earlier_second_page() {
        let first = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok((0..10).map(|n| json!(n)).collect::<Vec<_>>())
        };
        let second =
            async { Ok((PAGE_SIZE as usize..PAGE_SIZE as usize + 10).map(|n| json!(n)).collect()) };

        let (first, second) = first_two_library_pages(first, second).await.unwrap();
        assert_eq!(first.len(), 10);
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn short_first_library_page_ignores_a_parallel_second_page_error() {
        let first = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok((0..10).map(|n| json!(n)).collect::<Vec<_>>())
        };
        let second = async { Err(anyhow!("unused page failed")) };

        let (first, second) = first_two_library_pages(first, second).await.unwrap();
        assert_eq!(first.len(), 10);
        assert!(second.is_none());
    }

    #[test]
    fn generates_six_digit_totp_from_the_time_counter() {
        // RFC 4226 test secret and counter 1, with the six digit truncation used by Spotify.
        assert_eq!(totp("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 1).unwrap(), "287082");
    }

    #[tokio::test]
    async fn discards_legacy_api_tokens_without_a_web_session_cookie() {
        let directory =
            std::env::temp_dir().join(format!("ryotunes-spotify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(TOKEN_FILE);
        std::fs::write(
            &path,
            br#"{"access_token":"old","refresh_token":"old","expires_at":9999999999}"#,
        )
        .unwrap();
        assert!(WebApiClient::restore("unused".into(), &directory).await.unwrap().is_none());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn stores_web_session_credentials_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("ryotunes-spotify-{}", uuid::Uuid::new_v4()));
        let path = directory.join(TOKEN_FILE);
        let tokens = Tokens {
            access_token: "test-access-token".into(),
            refresh_token: None,
            session_cookie: Some("test-session-cookie".into()),
            expires_at: u64::MAX,
        };

        persist(&path, &tokens).unwrap();

        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn rejects_invalid_session_cookie_before_network_access() {
        let error = web_token(&reqwest::Client::new(), "invalid;cookie").await.unwrap_err();
        assert_eq!(error.to_string(), "Spotify session cookie has an invalid format");
    }

    #[test]
    fn maps_web_api_track_metadata_to_the_shared_catalogue_shape() {
        let track = track_from(&json!({
            "id": "track-id",
            "name": "Song",
            "duration_ms": 194000,
            "popularity": 73,
            "explicit": true,
            "track_number": 4,
            "disc_number": 2,
            "artists": [{ "id": "artist-id", "name": "Artist" }],
            "album": {
                "id": "album-id",
                "name": "Record",
                "images": [{ "url": "https://example.test/cover.jpg" }]
            }
        }));

        assert_eq!(track.id.as_deref(), Some("track-id"));
        assert_eq!(track.artists, "Artist");
        assert_eq!(track.artist_refs[0].id.as_deref(), Some("artist-id"));
        assert_eq!(track.album, "Record");
        assert_eq!(track.album_id.as_deref(), Some("album-id"));
        assert_eq!(track.cover.as_deref(), Some("https://example.test/cover.jpg"));
        assert_eq!(track.duration, Duration::from_secs(194));
        assert!(track.explicit);
        assert_eq!(track.track_number, 4);
        assert_eq!(track.disc_number, 2);
    }

    #[test]
    fn maps_pathfinder_track_wrappers_to_catalogue_tracks() {
        let track = track_from_pathfinder(&json!({
            "uri": "spotify:track:track-1", "name": "Song",
            "duration": { "totalMilliseconds": 123000 },
            "trackNumber": 2, "discNumber": 1,
            "contentRating": { "label": "EXPLICIT" },
            "playability": { "playable": true },
            "artists": { "items": [{ "profile": { "name": "Artist" }, "uri": "spotify:artist:artist-1" }] },
            "albumOfTrack": { "uri": "spotify:album:album-1", "name": "Album",
                "coverArt": { "sources": [{ "url": "https://example.test/cover.jpg" }] } }
        }));
        assert_eq!(track.id.as_deref(), Some("track-1"));
        assert_eq!(track.artists, "Artist");
        assert_eq!(track.artist_refs[0].id.as_deref(), Some("artist-1"));
        assert_eq!(track.album_id.as_deref(), Some("album-1"));
        assert_eq!(track.cover.as_deref(), Some("https://example.test/cover.jpg"));
        assert_eq!(track.duration, Duration::from_secs(123));
        assert!(track.explicit);
    }

    #[test]
    fn maps_saved_track_library_rows_with_uri_wrapper() {
        let track = library_track_from_pathfinder(&json!({
            "addedAt": "2025-01-01T00:00:00Z",
            "track": {
                "_uri": "spotify:track:track-1",
                "data": {
                    "name": "Song",
                    "duration": { "totalMilliseconds": 123000 },
                    "artists": { "items": [{ "uri": "spotify:artist:artist-1", "profile": { "name": "Artist" } }] },
                    "albumOfTrack": { "uri": "spotify:album:album-1", "name": "Album" }
                }
            }
        }));

        assert_eq!(track.id.as_deref(), Some("track-1"));
        assert_eq!(track.name, "Song");
        assert_eq!(track.artist_refs[0].id.as_deref(), Some("artist-1"));
        assert_eq!(track.album_id.as_deref(), Some("album-1"));
    }

    #[test]
    fn maps_pathfinder_album_and_playlist_details() {
        let album = album_from_pathfinder(&json!({
            "uri": "spotify:album:album-1", "name": "Record", "albumType": "single",
            "date": {"isoString": "2024-03-01"}, "label": "Label",
            "artists": {"items": [{"uri": "spotify:artist:artist-1", "profile": {"name": "Artist"}}]},
            "coverArt": {"sources": [{"url": "https://example.test/album.jpg"}]},
            "tracks": {"totalCount": 1}
        }));
        assert_eq!(album.id, "album-1");
        assert_eq!(album.release_type, ReleaseType::Single);
        assert_eq!(album.artist_refs[0].id.as_deref(), Some("artist-1"));
        assert_eq!(album.cover.as_deref(), Some("https://example.test/album.jpg"));

        let playlist = playlist_from_pathfinder(&json!({
            "uri": "spotify:playlist:playlist-1", "name": "Mix",
            "ownerV2": {"data": {"uri": "spotify:user:user-1", "name": "Owner"}},
            "images": {"items": [{"sources": [{"url": "https://example.test/mix.jpg"}]}]},
            "content": {"totalCount": 12}, "currentUserCapabilities": {"canEditItems": true}
        }));
        assert_eq!(playlist.id, "playlist-1");
        assert_eq!(playlist.owner_id, "user-1");
        assert_eq!(playlist.track_count, 12);
        assert!(playlist.owned);
    }

    #[test]
    fn maps_pathfinder_saved_artist() {
        let artist = saved_artist_from_pathfinder(&json!({
            "uri": "spotify:artist:artist-1", "profile": {"name": "Artist"},
            "visuals": {"avatarImage": {"sources": [{"url": "https://example.test/artist.jpg"}]}}
        }));
        assert_eq!(artist.id, "artist-1");
        assert_eq!(artist.name, "Artist");
        assert_eq!(artist.cover.as_deref(), Some("https://example.test/artist.jpg"));
    }

    #[test]
    fn reads_saved_items_wrapped_by_the_web_api() {
        let saved_track = json!({ "track": { "id": "track-id" } });
        let saved_album = json!({ "album": { "id": "album-id" } });

        assert_eq!(library_items(&json!({ "items": [saved_track] }))[0]["id"], "track-id");
        assert_eq!(library_items(&json!({ "items": [saved_album] }))[0]["id"], "album-id");
    }

    #[test]
    fn pathfinder_errors_redact_session_secrets() {
        let error = json!({
            "errors": [{
                "message": "rejected token-access and cookie-session"
            }]
        });
        let message = pathfinder_error_message(&error, "token-access", "cookie-session");
        assert_eq!(message, "rejected [redacted] and [redacted]");
    }
}
