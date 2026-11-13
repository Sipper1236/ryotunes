//! The signed-in Spotify client. Adapted from nolight132/sonora `spotify/client.rs`: Sonora's
//! `impl MusicApi for LibrespotClient` becomes inherent async methods on [`Client`] (Ryotunes has no
//! `MusicApi` trait), and the client now owns the one librespot player/FIFO ([`stream::Engine`]) that
//! streams every track into mpv.

use std::collections::HashSet;

use anyhow::{Context as _, Result};
use librespot_core::Session;
use librespot_protocol::playlist4_external::SelectedListContent as RootList;
use protobuf::Message as _;

use crate::stream::{self, StreamHandle};
use crate::web_api::WebApiClient;
use crate::{
    Album, AlbumDetail, Artist, ArtistProfile, ArtistRef, Genre, GenreDetail, HomeFeed, Lyrics,
    MediaKind, Playlist, PlaylistDetail, SavedArtist, Track, UserDetail, UserProfile, albums,
    artists, collection, collection2, lyrics, pathfinder, playlists, profiles, radio, search, wire,
};

const MADE_FOR_YOU: &str = "0JQ5DAt0tbjZptfcdMSKl3";
/// How many liked tracks make up one [`Client::liked`] page.
const LIKED_PAGE: u32 = 100;
/// The page size used by Spotify's playlist browse endpoint in metadata-only mode.
const PLAYLIST_PAGE: usize = 50;

/// The four result lists a query returns. `artists` is derived from the track hits' artist refs
/// (Spotify's Pathfinder search covers albums and playlists but not artists); see the README.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<ArtistRef>,
    pub playlists: Vec<Playlist>,
}

pub struct Client {
    session: Option<Session>,
    web_api: Option<WebApiClient>,
    engine: Option<stream::Engine>,
    metadata_only: bool,
}

impl Client {
    pub(crate) fn new(session: Session, metadata_only: bool) -> Result<Self> {
        let engine = if metadata_only {
            None
        } else {
            Some(
                stream::Engine::new(session.clone())
                    .context("cannot start the Spotify playback engine")?,
            )
        };
        Ok(Self { session: Some(session), web_api: None, engine, metadata_only })
    }

    pub(crate) fn new_web_api(client: WebApiClient) -> Result<Self> {
        Ok(Self { session: None, web_api: Some(client), engine: None, metadata_only: true })
    }

    /// The underlying librespot session, for callers that need it directly.
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Whether the session is still connected.
    pub fn alive(&self) -> bool {
        self.web_api.as_ref().map_or_else(
            || self.session.as_ref().is_some_and(|session| !session.is_invalid()),
            WebApiClient::alive,
        )
    }

    /// Whether this client is in metadata-only mode (free account, no streaming).
    pub fn is_metadata_only(&self) -> bool {
        self.metadata_only
    }

    /// A public `open.spotify.com` URL for a piece of media.
    pub fn share_url(&self, kind: MediaKind, id: &str) -> String {
        let kind = match kind {
            MediaKind::Track => "track",
            MediaKind::Album => "album",
            MediaKind::Artist => "artist",
            MediaKind::Playlist => "playlist",
        };
        format!("https://open.spotify.com/{kind}/{id}")
    }

    // -- Playback -----------------------------------------------------------------------------

    /// Start streaming `track_id` (bare id or `spotify:track:` URI) through the FIFO mpv reads.
    /// Returns an error if the client is in metadata-only mode (free account).
    pub fn stream(&self, track_id: &str) -> Result<StreamHandle> {
        if self.metadata_only {
            anyhow::bail!(
                "Cannot stream audio in metadata-only mode (free account). Use YouTube Music fallback."
            );
        }
        self.engine
            .as_ref()
            .context("Playback engine not available in metadata-only mode")?
            .stream(track_id)
    }

    // -- Profile / library --------------------------------------------------------------------

    pub async fn profile(&self) -> Result<UserProfile> {
        if let Some(client) = &self.web_api {
            return client.profile().await;
        }
        let session = self.librespot_session()?;
        let username = session.username();
        let body = session.spclient().get_user_profile(&username, None, None).await?;

        let profile: wire::Named = serde_json::from_slice(&body).unwrap_or_default();
        Ok(UserProfile {
            display_name: profile.label().unwrap_or(&username).to_owned(),
            id: username,
        })
    }

    pub async fn user(&self, user_id: &str) -> Result<UserDetail> {
        profiles::profile(self.librespot_session()?, user_id).await
    }

    /// One page of the user's liked (saved) tracks, newest first. Spotify's collection API pages by
    /// an opaque continuation token rather than an offset, so page `n` is served by reading the first
    /// `(n + 1) * LIKED_PAGE` saved tracks and returning the tail slice.
    pub async fn liked(&self, page: u32) -> Result<Vec<Track>> {
        if let Some(client) = &self.web_api {
            return client.liked(page).await;
        }
        let limit = page.saturating_add(1).saturating_mul(LIKED_PAGE);
        let mut tracks = collection::saved_tracks(self.librespot_session()?, limit).await?;
        let skip = (page.saturating_mul(LIKED_PAGE)) as usize;
        if skip >= tracks.len() {
            return Ok(Vec::new());
        }
        Ok(tracks.split_off(skip))
    }

    pub async fn saved_tracks(&self, limit: u32) -> Result<Vec<Track>> {
        if let Some(client) = &self.web_api {
            return client.saved_tracks(limit).await;
        }
        collection::saved_tracks(self.librespot_session()?, limit).await
    }

    pub async fn set_track_saved(&self, track_id: &str, saved: bool) -> Result<()> {
        if let Some(client) = &self.web_api {
            return client.set_track_saved(track_id, saved).await;
        }
        collection2::set_track_saved(self.librespot_session()?, track_id, saved).await
    }

    pub async fn saved_albums(&self, limit: u32) -> Result<Vec<Album>> {
        if let Some(client) = &self.web_api {
            return client.saved_albums(limit).await;
        }
        albums::saved_albums(self.librespot_session()?, limit).await
    }

    pub async fn set_album_saved(&self, album_id: &str, saved: bool) -> Result<()> {
        if let Some(client) = &self.web_api {
            return client.set_album_saved(album_id, saved).await;
        }
        collection2::set_album_saved(self.librespot_session()?, album_id, saved).await
    }

    pub async fn saved_artists(&self, limit: u32) -> Result<Vec<SavedArtist>> {
        if let Some(client) = &self.web_api {
            return client.saved_artists(limit).await;
        }
        artists::saved_artists(self.librespot_session()?, limit).await
    }

    pub async fn set_artist_saved(&self, artist_id: &str, saved: bool) -> Result<()> {
        collection2::set_artist_saved(self.librespot_session()?, artist_id, saved).await
    }

    // -- Tracks / albums / artists ------------------------------------------------------------

    pub async fn track(&self, track_id: &str) -> Result<Track> {
        if let Some(client) = &self.web_api {
            return client.track(track_id).await;
        }
        collection::track(self.librespot_session()?, track_id).await
    }

    pub async fn track_playcount(&self, track_id: &str) -> Result<Option<u64>> {
        if self.web_api.is_some() {
            return Ok(None);
        }
        pathfinder::track(self.librespot_session()?, track_id).await
    }

    pub async fn track_radio(&self, track_id: &str) -> Result<Vec<Track>> {
        if self.web_api.is_some() {
            return Ok(Vec::new());
        }
        radio::track_radio(self.librespot_session()?, track_id).await
    }

    pub async fn lyrics(&self, track_id: &str) -> Result<Option<Lyrics>> {
        if self.web_api.is_some() {
            return Ok(None);
        }
        lyrics::lyrics(self.librespot_session()?, track_id).await
    }

    pub async fn album(&self, album_id: &str) -> Result<AlbumDetail> {
        if let Some(client) = &self.web_api {
            return client.album(album_id).await;
        }
        albums::album(self.librespot_session()?, album_id).await
    }

    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>> {
        if self.web_api.is_some() {
            return Ok(self.album(album_id).await?.tracks);
        }
        albums::album_tracks(self.librespot_session()?, album_id).await
    }

    pub async fn artist(&self, artist_id: &str) -> Result<Artist> {
        if let Some(client) = &self.web_api {
            return client.artist(artist_id).await;
        }
        artists::artist(self.librespot_session()?, artist_id).await
    }

    pub async fn artist_profile(&self, artist_id: &str) -> Result<ArtistProfile> {
        if let Some(client) = &self.web_api {
            return client.artist_profile(artist_id).await;
        }
        artists::profile(self.librespot_session()?, artist_id).await
    }

    pub async fn artist_images(
        &self,
        ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, String>> {
        if let Some(client) = &self.web_api {
            let mut images = std::collections::HashMap::new();
            for id in ids {
                if let Ok(profile) = client.artist_profile(&id).await {
                    if let Some(image) = profile.cover_large {
                        images.insert(id, image);
                    }
                }
            }
            return Ok(images);
        }
        artists::images(self.librespot_session()?, &ids).await
    }

    // -- Playlists ----------------------------------------------------------------------------

    pub async fn playlist(&self, playlist_id: &str) -> Result<PlaylistDetail> {
        if let Some(client) = &self.web_api {
            return client.playlist(playlist_id).await;
        }
        let session = self.librespot_session()?;
        let mut detail = playlists::playlist(session, playlist_id).await?;
        let owner = detail.playlist.owner_id.clone();
        if !owner.is_empty() {
            let names = profiles::display_names(session, HashSet::from([owner.clone()])).await;
            if let Some(name) = names.get(&owner) {
                detail.playlist.owner = name.clone();
            }
        }
        Ok(detail)
    }

    /// Fetch one playlist page. Metadata-only browse uses this so a large playlist can render its
    /// first screen without waiting for every subsequent Pathfinder request.
    pub async fn playlist_page(&self, playlist_id: &str, page: u32) -> Result<PlaylistDetail> {
        if let Some(client) = &self.web_api {
            return client.playlist_page(playlist_id, page).await;
        }
        let mut detail = self.playlist(playlist_id).await?;
        let start = (page as usize).saturating_mul(PLAYLIST_PAGE);
        if start >= detail.tracks.len() {
            detail.tracks.clear();
        } else {
            detail.tracks = detail.tracks.into_iter().skip(start).take(PLAYLIST_PAGE).collect();
        }
        Ok(detail)
    }

    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        Ok(self.playlist(playlist_id).await?.tracks)
    }

    pub async fn playlist_covers(&self, playlist_id: &str, wanted: usize) -> Result<Vec<String>> {
        if self.web_api.is_some() {
            let tracks = self.playlist(playlist_id).await?.tracks;
            let mut covers = Vec::new();
            for cover in tracks.into_iter().filter_map(|track| track.cover) {
                if !covers.contains(&cover) {
                    covers.push(cover);
                    if covers.len() >= wanted {
                        break;
                    }
                }
            }
            return Ok(covers);
        }
        playlists::covers(self.librespot_session()?, playlist_id, wanted).await
    }

    pub async fn playlists(&self, limit: u32) -> Result<Vec<Playlist>> {
        if let Some(client) = &self.web_api {
            return client.playlists(limit).await;
        }
        let session = self.librespot_session()?;
        let body = session.spclient().get_rootlist(0, Some(limit as usize)).await?;

        let rootlist =
            RootList::parse_from_bytes(&body).context("cannot decode the rootlist protobuf")?;
        let mut playlists = wire::playlists_from(&rootlist);

        let owners = playlists
            .iter()
            .map(|playlist| playlist.owner_id.clone())
            .filter(|owner| !owner.is_empty())
            .collect();
        let ids = playlists.iter().map(|playlist| playlist.id.clone()).collect();
        let (names, stamps) = tokio::join!(
            profiles::display_names(session, owners),
            playlists::modified(session, ids)
        );

        for playlist in &mut playlists {
            playlist.owned = playlist.owner_id == session.username();
            if let Some(name) = names.get(&playlist.owner_id) {
                playlist.owner = name.clone();
            }
            playlist.modified_at = stamps.get(&playlist.id).copied();
        }

        Ok(playlists)
    }

    pub async fn create_playlist(&self, name: &str) -> Result<String> {
        if let Some(client) = &self.web_api {
            return client.create_playlist(name).await;
        }
        playlists::create(self.librespot_session()?, name).await
    }

    pub async fn rename_playlist(&self, playlist_id: &str, name: &str) -> Result<()> {
        if let Some(client) = &self.web_api {
            return client.rename_playlist(playlist_id, name).await;
        }
        playlists::rename(self.librespot_session()?, playlist_id, name).await
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        if self.web_api.is_some() {
            anyhow::bail!("Deleting Spotify playlists is not supported in metadata-only mode");
        }
        playlists::delete(self.librespot_session()?, playlist_id).await
    }

    pub async fn remove_playlist_from_library(&self, playlist_id: &str) -> Result<()> {
        if self.web_api.is_some() {
            anyhow::bail!("Removing Spotify playlists is not supported in metadata-only mode");
        }
        playlists::remove_from_library(self.librespot_session()?, playlist_id).await
    }

    pub async fn add_playlist_to_library(&self, playlist_id: &str) -> Result<()> {
        if self.web_api.is_some() {
            anyhow::bail!("Saving Spotify playlists is not supported in metadata-only mode");
        }
        playlists::add_to_library(self.librespot_session()?, playlist_id).await
    }

    pub async fn set_playlist_public(&self, playlist_id: &str, public: bool) -> Result<()> {
        if let Some(client) = &self.web_api {
            return client.set_playlist_public(playlist_id, public).await;
        }
        playlists::set_public(self.librespot_session()?, playlist_id, public).await
    }

    pub async fn add_track_to_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        if let Some(client) = &self.web_api {
            return client.add_track(playlist_id, track_id).await;
        }
        playlists::add_track(self.librespot_session()?, playlist_id, track_id).await
    }

    pub async fn remove_track_from_playlist(
        &self,
        playlist_id: &str,
        track_id: &str,
    ) -> Result<()> {
        if self.web_api.is_some() {
            anyhow::bail!(
                "Removing Spotify playlist tracks is not supported in metadata-only mode"
            );
        }
        playlists::remove_track(self.librespot_session()?, playlist_id, track_id).await
    }

    // -- Discovery ----------------------------------------------------------------------------

    /// Search tracks, albums, and playlists in one shot; artists are derived from the track hits. A
    /// track failure propagates; album/playlist failures degrade to empty (a stale Pathfinder hash
    /// should not sink the whole query).
    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        if let Some(client) = &self.web_api {
            return client.search(query).await;
        }
        let session = self.librespot_session()?;
        let (tracks, albums, playlists) = tokio::join!(
            search::search(session, query),
            pathfinder::search_albums(session, query),
            pathfinder::search_playlists(session, query),
        );

        let tracks = tracks?;
        let albums = albums.unwrap_or_else(|error| {
            log::warn!("search: cannot search albums: {error:#}");
            Vec::new()
        });
        let playlists = playlists.unwrap_or_else(|error| {
            log::warn!("search: cannot search playlists: {error:#}");
            Vec::new()
        });
        let artists = artists_of(&tracks);

        Ok(SearchResults { tracks, albums, artists, playlists })
    }

    pub async fn search_albums(&self, query: &str) -> Result<Vec<Album>> {
        if let Some(client) = &self.web_api {
            return client.search_albums(query).await;
        }
        pathfinder::search_albums(self.librespot_session()?, query).await
    }

    pub async fn search_playlists(&self, query: &str) -> Result<Vec<Playlist>> {
        if let Some(client) = &self.web_api {
            return client.search_playlists(query).await;
        }
        pathfinder::search_playlists(self.librespot_session()?, query).await
    }

    pub async fn home(&self) -> Result<HomeFeed> {
        if self.web_api.is_some() {
            return Ok(HomeFeed::default());
        }
        let session = self.librespot_session()?;
        let mut sections = pathfinder::genre(session, MADE_FOR_YOU).await?.sections;
        playlists::name_blanks(session, &mut sections).await;

        Ok(HomeFeed { sections, ..HomeFeed::default() })
    }

    pub async fn genres(&self) -> Result<Vec<Genre>> {
        if self.web_api.is_some() {
            return Ok(Vec::new());
        }
        pathfinder::genres(self.librespot_session()?).await
    }

    pub async fn genre(&self, genre_id: &str) -> Result<GenreDetail> {
        if self.web_api.is_some() {
            anyhow::bail!("Spotify genre browse is unavailable in metadata-only mode");
        }
        pathfinder::genre(self.librespot_session()?, genre_id).await
    }

    fn librespot_session(&self) -> Result<&Session> {
        self.session.as_ref().context("This operation requires a Premium Spotify session")
    }
}

/// The distinct artists across a set of track hits, in first-seen order — Spotify search has no
/// artist entity, so results surface the artists that its tracks credit.
fn artists_of(tracks: &[Track]) -> Vec<ArtistRef> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut artists = Vec::new();
    for artist in tracks.iter().flat_map(|track| track.artist_refs.iter()) {
        let key = artist.id.clone().unwrap_or_else(|| artist.name.clone());
        if seen.insert(key) {
            artists.push(artist.clone());
        }
    }
    artists
}
