# Spotify metadata mode for free accounts

Metadata mode opens Spotify in the system default browser, watches that browser's cookie store for
the first-party `sp_dc` session cookie, exchanges it for a short-lived token, and uses Spotify's
internal Pathfinder API for catalog data. It does not create a librespot session. Track playback
continues through Ryotunes' YouTube Music resolver. Spotify can change these internal endpoints
without notice; this integration is not a supported Spotify developer API.

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
- Spotify track playback through a duration-checked YouTube Music match
- Saved library lists for tracks, albums, artists and playlists

Metadata mode currently maps catalog search, track, album, artist and playlist detail, saved tracks,
saved albums, saved artists, and playlists through Pathfinder. Saving and removing tracks or albums uses the
Pathfinder library mutation. Playlist creation, renaming, visibility changes and track edits return
an explicit unsupported-operation error in metadata mode. These internal calls still need a live
free-account check. The Spotify home feed and lyrics are not implemented. Direct Spotify playback
remains available in normal mode with Premium.

## Credentials

The `sp_dc` cookie is read only from Spotify-domain rows in discovered Firefox-family and supported
Linux Chromium cookie stores; unrelated browser cookies are not imported. Chromium cookies using a
keyring-backed `v11` encryption key cannot currently be read. Refresh credentials and the web
session cookie are stored under Ryotunes' Spotify data directory in `web-api-token.json`, with
owner-only file permissions. Signing out or changing metadata mode removes the saved credentials
and requires another sign-in. Never share this file; the cookie grants access to the Spotify web
session.
