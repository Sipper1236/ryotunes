# Free Spotify Account Support — Implementation Summary (Updated)

## Completed (Phases 1, 2 & 3)

### Phase 1: Core Provider Changes (`crates/spotify/`)

#### `auth.rs`
- Added `FULL_SCOPES` (includes `"streaming"`) and `METADATA_SCOPES` (excludes `"streaming"`)
- Added `metadata_only` field to `AuthConfig`
- Conditional scopes in `authorize()` based on `metadata_only`
- Conditional `premium()` check in `restore()` and `sign_in()` — skipped when `metadata_only`
- Added test: `metadata_scopes_exclude_streaming`

#### `provider.rs`
- Added `metadata_only` field to `SpotifyConfig`
- Propagates `metadata_only` to `AuthConfig` and `Client::new()`

#### `client.rs`
- Added `metadata_only` field and `is_metadata_only()` method
- `Engine` (librespot playback) only created when `!metadata_only`
- `stream()` returns clear error when called in metadata-only mode

#### `lib.rs`
- Exported `FULL_SCOPES`, `METADATA_SCOPES`, `DEFAULT_CLIENT_ID`, `DEFAULT_REDIRECT_URI`

### Phase 2: Daemon Integration (`crates/core/src/`)

#### `spotify.rs`
- Added `metadata_only` field to `SpotifyState`
- Updated constructor to accept `metadata_only` and create `SpotifyConfig` with it
- Added `is_metadata_only()` method
- Reads `spotify_metadata_only` setting from database on startup

#### `state.rs`
- Added `spotify_metadata_only` to `UI_SETTINGS` allowlist
- Modified `resolve()` to detect `spotify:track:` IDs in metadata-only mode
- Added `resolve_spotify_via_youtube()` method:
  1. Fetches track metadata from Spotify (name, artists, duration)
  2. Builds search query: `"track_name artist1 artist2"`
  3. Searches YouTube Music via existing `orchestrator.resolve()`
  4. Verifies duration proximity (within 10 seconds) to reduce wrong matches
  5. Returns YouTube `PlaybackData` with `stream_client = "youtube-fallback"`
- Uses existing `parse_duration_ms()` for duration comparison

### Phase 3: UI Configuration (`ui/src/lib/components/SettingsDialog.svelte`)

#### New "Providers" Tab
- Added "Providers" tab between "Playback" and "Data & storage"
- Provider selector: YouTube Music, Spotify, SoundCloud, Tidal
- Shows current provider with descriptive subtitle
- Spotify-specific section appears when Spotify is selected:
  - **Metadata-only mode toggle** — enables free account support
  - Explanatory text about the mode
  - Note about YouTube Music fallback for audio

#### SettingsDialog Changes
- Added `currentProvider`, `spotifyMetadataOnly`, `providersLoaded` state
- Added `loadProviders()` function — loads provider + metadata-only setting
- Added `$effect` to auto-load when Providers tab opens
- Added `setProvider()` — calls `set_provider` Tauri command
- Added `setSpotifyMetadataOnly()` — toggles `spotify_metadata_only` setting
- Updated `settingsArt`, `settingsCaption`, `settingsReadout` for providers tab

#### API Layer (`ui/src/lib/api.ts`)
- Added `ProviderInfo` interface and `getProvider()` function
- Added `setProvider()` function

#### Rust Settings Allowlist (`crates/core/src/state.rs`)
- Added `"spotify_metadata_only"` to `UI_SETTINGS` array

## Architecture

```
Free User Flow (metadata_only = true):
┌─────────────────────────────────────────────────────────────┐
│  User opens Settings → Providers tab                        │
│       ↓                                                     │
│  Selects "Spotify" as active provider                       │
│       ↓                                                     │
│  Enables "Spotify metadata-only mode (free accounts)"       │
│       ↓                                                     │
│  OAuth PKCE (METADATA_SCOPES — no "streaming")              │
│       ↓                                                     │
│  Spotify Auth succeeds (no Premium check)                   │
│       ↓                                                     │
│  User browses: search, library, playlists work ✓            │
│       ↓                                                     │
│  User plays spotify:track:xxx                               │
│       ↓                                                     │
│  resolve() detects metadata_only → resolve_spotify_via_youtube() │
│       ↓                                                     │
│  Fetch track metadata from Spotify (name, artists, duration)│
│       ↓                                                     │
│  Search YouTube Music: "track_name artists"                 │
│       ↓                                                     │
│  YouTube Music stream → mpv (existing pipeline) ✓           │
└─────────────────────────────────────────────────────────────┘

Premium User Flow (metadata_only = false):
┌─────────────────────────────────────────────────────────────┐
│  User selects Spotify provider                              │
│       ↓                                                     │
│  OAuth PKCE (FULL_SCOPES — includes "streaming")            │
│       ↓                                                     │
│  Spotify Auth + Premium check passes                        │
│       ↓                                                     │
│  librespot Engine created (pipe backend → FIFO → mpv)       │
│       ↓                                                     │
│  User plays spotify:track:xxx → direct Spotify audio ✓      │
└─────────────────────────────────────────────────────────────┘
```

## Settings

- Database key: `spotify_metadata_only` (boolean string: `"true"` / `"false"`)
- Default: `false` (Premium mode)
- When `true`: Free account mode — metadata-only + YouTube fallback
- Provider key: `provider` (`"youtube" | "spotify" | "soundcloud" | "tidal"`)

## Testing

- All existing tests pass (42 spotify + 162 core)
- New test: `metadata_scopes_exclude_streaming` verifies scopes
- UI builds successfully (`pnpm run build`)
- Manual testing needed with actual free Spotify account

## Remaining (Phase 4)

### Phase 4: Testing & Validation
- Test with real free Spotify account
- Verify: sign-in, browse (search/library/playlists), play via YouTube
- Verify: queue mixing Spotify + YouTube tracks
- Verify: Premium account still works normally
- Verify: Provider switching persists across restarts

## Files Modified

| File | Lines Changed | Purpose |
|------|---------------|---------|
| `crates/spotify/src/auth.rs` | ~60 | Scopes, metadata_only config, conditional premium check |
| `crates/spotify/src/provider.rs` | ~15 | Config propagation |
| `crates/spotify/src/client.rs` | ~25 | metadata_only field, conditional Engine, stream() error |
| `crates/spotify/src/lib.rs` | ~5 | Exports |
| `crates/core/src/spotify.rs` | ~25 | SpotifyState metadata_only, settings integration |
| `crates/core/src/state.rs` | ~100 | UI_SETTINGS, resolve() branch, resolve_spotify_via_youtube() |
| `ui/src/lib/api.ts` | ~8 | Provider API functions |
| `ui/src/lib/components/SettingsDialog.svelte` | ~150 | Providers tab, metadata-only toggle, provider selector |
| **Total** | **~388** | |

## Key Design Decisions

1. **Minimal invasive changes** — Only 9 files touched, existing Premium flow unchanged
2. **Reuses existing YouTube pipeline** — No new audio backend, leverages `orchestrator.resolve()`
3. **Duration verification** — Reduces wrong-match risk (10-second tolerance)
4. **Clear error messages** — `stream()` explains metadata-only limitation
5. **Settings persistence** — Survives restarts, per-installation
6. **UI-first approach** — Added Providers tab with intuitive controls

## Risks Mitigated

| Risk | Mitigation |
|------|------------|
| YouTube returns wrong track | Duration check (10s tolerance), logs warning |
| Spotify metadata API breaks | Existing librespot maintenance; fallback to YouTube search |
| User confusion | Providers tab with clear labels, explanatory text |
| Free account OAuth scopes restricted | METADATA_SCOPES uses only non-streaming scopes |
| Provider state not persisted | Uses existing `provider` and `spotify_metadata_only` DB settings |

## Next Steps (Phase 4)

1. **Manual Testing** (1-2 hours): Real free account end-to-end
2. **Documentation** (15 min): Update README with free account instructions

---

*Implementation completed: 2026-11-09*
*Branch: feat/free-spotify-metadata-fallback*
*Author: Needle*