# Spotify metadata mode for free accounts

Metadata mode opens Spotify in the system default browser, watches that browser's cookie store for
the first-party `sp_dc` session cookie, exchanges it for a short-lived token, and uses Spotify's
internal Pathfinder API for catalog data. It does not create a librespot session. Playback offers
two modes under **Settings → Providers → Spotify free account (metadata only)**:

- **Automatic** tries Spotify's official Web Player in a private Chromium/Chrome profile when a
  local Widevine CDM is available. If the player or account refuses a track, Ryotunes resolves the
  same track through YouTube Music and checks the match duration.
- **YouTube only** skips the Spotify Web Player and uses the duration-checked YouTube Music match.

Spotify's own ads and free-account restrictions apply to its Web Player. A browser without
Widevine, a missing browser, player startup errors, or later playback failure use the YouTube
fallback. The daemon looks for Chromium, Chrome, or Brave; set `RYOTUNES_SPOTIFY_BROWSER` to an
executable path to select another Chromium-based browser. Spotify can change the internal
Pathfinder and Web Player interfaces without notice; neither is a supported Spotify developer
API.

## Spotify app setup

Metadata-only sign-in does not need a Spotify Developer app, Client ID, or Premium developer
account. Choose **Spotify free account (metadata only)** in Ryotunes settings, then sign in in your
default browser. Ryotunes detects the session automatically after sign-in. The Premium librespot
playback path continues to use the configured Spotify OAuth application and its redirect URI:

```text
http://127.0.0.1:8989/login
```

If configuring that Premium playback path, set the app's Client ID in the user systemd manager:

```sh
systemctl --user set-environment RYOTUNES_SPOTIFY_CLIENT_ID=<client-id>
systemctl --user restart ryotunesd.socket
systemctl --user restart ryotunesd.service
```

The Client ID is public; do not put a Client Secret in Ryotunes. The daemon reads this value when
it starts. To remove the override and use the built-in client ID again:

```sh
systemctl --user unset-environment RYOTUNES_SPOTIFY_CLIENT_ID
systemctl --user restart ryotunesd.service
```

Spotify's current Development Mode policy requires the app owner to have an active Premium
subscription. This restriction applies to the OAuth/Web API playback integration, not the
metadata-only web-session flow described above.

## What works in metadata mode

- Spotify catalog search for tracks, albums, artists and playlists through Pathfinder
- Track detail through Pathfinder
- Automatic playback through Spotify's official Web Player with a duration-checked YouTube Music
  fallback
- YouTube-only playback through a duration-checked YouTube Music match
- Saved library lists for tracks, albums, artists and playlists

Metadata mode currently maps catalog search, track, album, artist and playlist detail, saved tracks,
saved albums, saved artists, and playlists through Pathfinder. Saving and removing tracks or albums uses the
Pathfinder library mutation. Playlist creation, renaming, visibility changes and track edits return
an explicit unsupported-operation error in metadata mode. These internal calls still need a live
free-account check. The Spotify home feed and lyrics are not implemented. Direct Spotify playback
remains available in normal mode with Premium. The legacy Tauri host does not embed the Chromium
adapter and uses the YouTube fallback for metadata-mode playback.

## Credentials

The `sp_dc` cookie is read only from Spotify-domain rows in discovered Firefox-family and supported
Linux Chromium cookie stores; unrelated browser cookies are not imported. Chromium cookies using a
keyring-backed `v11` encryption key cannot currently be read. Refresh credentials and the web
session cookie are stored under Ryotunes' Spotify data directory in `web-api-token.json`, with
owner-only file permissions. Signing out or changing metadata mode removes the saved credentials
and requires another sign-in. Never share this file; the cookie grants access to the Spotify web
session.
