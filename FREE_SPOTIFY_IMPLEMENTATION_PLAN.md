# Free Spotify Account Support in Ryotunes — Implementation Plan

## Executive Summary

**Goal**: Enable Ryotunes users with free (non-Premium) Spotify accounts to browse the Spotify catalog and play tracks via YouTube Music fallback.

**Core Constraint**: Spotify's protocol **fundamentally requires Premium for audio streaming** — no librespot/Soloist backend can stream audio on free accounts. The only viable path is **metadata-only Spotify + YouTube Music audio fallback**.

---

## Architecture Analysis (Current State)

### Spotify Provider Stack (`crates/spotify/`)

| File | Role | Premium Dependency |
|------|------|-------------------|
| `auth.rs` | OAuth PKCE flow, session restore, **Premium gate** | `SCOPES` includes `"streaming"` (line 38); `premium()` check (lines 225-242) hard-rejects free accounts |
| `client.rs` | Signed-in client: metadata APIs + `stream::Engine` | `Engine::new()` creates librespot `Player` with `pipe` backend → FIFO → mpv |
| `stream.rs` | Audio streaming via librespot `pipe` backend | **Requires Premium** — audio keys only issued to Premium sessions |
| `provider.rs` | Entry point: `restore()`, `sign_in()`, `sign_out()` | Calls `auth::sign_in()` which enforces Premium |

### Daemon Integration (`crates/core/src/`)

| File | Role |
|------|------|
| `state.rs:959-997` | `resolve()` detects `spotify:track:` IDs → calls `client.stream()` → FIFO → mpv |
| `spotify.rs` | `SpotifyState` wrapper: holds `SpotifyProvider`, manages client lifecycle, `selected` catalogue |
| `spotify_bridge.rs` | Maps Spotify models → YouTube-shaped UI structs (pure, no network) |

### Key Data Flow

```
User selects Spotify provider
       ↓
OAuth PKCE (scopes include "streaming") → Spotify Auth
       ↓
librespot Session.connect() → Session established
       ↓
premium() check: session.user_data().attributes["type"] == "premium"?
       ↓ NO (free account) → SignInFailure(Premium) → REJECT
       ↓ YES (Premium) → Client created with Engine (librespot Player + FIFO)
       ↓
User plays spotify:track:xxx
       ↓
resolve() → client.stream(tid) → StreamHandle (FIFO) → mpv loadfile with rawaudio demuxer
```

---

## Implementation Paths Evaluated

### ❌ Path 1: Remove Premium Check Only
**Change**: Remove `premium()` call in `auth.rs`, remove `"streaming"` from `SCOPES`
**Result**: Auth succeeds, but `librespot_playback::Player::load()` fails — Spotify returns 403 for audio keys on free accounts
**Verdict**: **Dead end** — protocol enforces this server-side

### ❌ Path 2: Web Playback SDK Backend
**Change**: Embed WebView, implement Spotify Web Playback SDK (JS), route audio through WebView
**Effort**: Very high (new architecture, WebView management, DRM handling)
**Result**: Works for free accounts (ad-supported), but completely different stack
**Verdict**: **Too invasive** — would require separate player instance, breaks Ryotunes' single-mpv model

### ❌ Path 3: go-librespot / Alternative librespot Fork
**Change**: Use different librespot fork that claims free support
**Reality**: Music Assistant's go-librespot only works for "Premium-family accounts created before Dec 2024" — not general free accounts
**Verdict**: **Doesn't solve the problem**

### ✅ Path 4: Metadata-Only Spotify + YouTube Fallback (CHOSEN)
**Change**: 
1. Add `metadata_only` mode to Spotify provider config
2. Conditional scopes: omit `"streaming"` when metadata-only
3. Skip `premium()` check when metadata-only
4. On play: search YouTube Music for track metadata → play YouTube result
**Result**: Free accounts can browse Spotify catalog, play via YouTube (existing pipeline)
**Effort**: Medium — touches auth, provider, daemon resolve, UI config
**Verdict**: **Only viable path** — leverages existing YouTube Music pipeline

---

## Detailed Implementation Plan

### Phase 1: Core Provider Changes (`crates/spotify/`)

#### 1.1 `auth.rs` — Conditional Scopes & Premium Check
- Add `metadata_only: bool` to `AuthConfig`
- Split `SCOPES` into `FULL_SCOPES` (with streaming) and `METADATA_SCOPES` (without)
- Make `premium()` check conditional on `!metadata_only`
- Update `restore()` and `sign_in()` to pass config through

#### 1.2 `client.rs` — Metadata-Only Client
- Add `metadata_only: bool` field to `Client`
- Conditionally create `Engine` only when `!metadata_only`
- `stream()` method: return error when `metadata_only` (caller must use YouTube fallback)

#### 1.3 `provider.rs` — Config Propagation
- Add `metadata_only` to `SpotifyConfig`
- Pass through to `AuthConfig` and `Client::new()`

#### 1.4 `stream.rs` — Graceful Degradation
- `Engine::new()` returns `Result<Option<Engine>>` — `None` when metadata-only
- `Client::stream()` returns `Err` with clear message when metadata-only

### Phase 2: Daemon Integration (`crates/core/src/`)

#### 2.1 `spotify.rs` — SpotifyState Metadata Mode
- Add `metadata_only: bool` field to `SpotifyState`
- Expose `is_metadata_only()` method
- Update `restore()` / `sign_in()` to respect mode
- Persist mode in settings (daemon-owned)

#### 2.2 `state.rs` — YouTube Fallback in `resolve()`
- Detect `spotify:track:` ID when Spotify is in metadata-only mode
- Extract track metadata (title, artists, duration) from Spotify client
- Search YouTube Music via `orchestrator.resolve()` with YouTube video ID
- Return YouTube `PlaybackData` instead of Spotify FIFO

#### 2.3 Search Integration
- Add `search_youtube_for_spotify_track()` helper
- Use track name + artists for YouTube Music search
- Pick best match (duration proximity, official channel)

### Phase 3: UI Configuration

#### 3.1 Settings Schema
- Add `spotify_metadata_only` boolean to daemon settings
- Expose in Ryotunes Settings → Providers → Spotify

#### 3.2 Provider Selector UI
- Show "Metadata Only (Free Account)" badge when enabled
- Tooltip: "Browse Spotify catalog, play via YouTube Music"

### Phase 4: Testing & Validation

#### 4.1 Unit Tests
- `auth.rs`: metadata-only scopes, premium check skipped
- `client.rs`: metadata-only client lacks Engine, stream() errors
- `state.rs`: resolve() routes to YouTube for spotify:track: in metadata mode

#### 4.2 Integration Test (Manual)
- Sign in with free Spotify account
- Browse: search, library, playlists work
- Play: track plays via YouTube Music (verify stream_client = "WEB_REMIX" etc.)

---

## File-by-File Change Map

### `crates/spotify/src/auth.rs`
```rust
// ADD: metadata_only field to AuthConfig
// ADD: METADATA_SCOPES (SCOPES minus "streaming")
// MODIFY: authorize() uses conditional scopes
// MODIFY: premium() called only when !metadata_only
// MODIFY: restore(), sign_in() accept metadata_only
```

### `crates/spotify/src/client.rs`
```rust
// ADD: metadata_only field to Client
// MODIFY: new() conditionally creates Engine
// MODIFY: stream() returns Err when metadata_only
```

### `crates/spotify/src/provider.rs`
```rust
// ADD: metadata_only to SpotifyConfig
// MODIFY: new()/with_config() pass metadata_only
// MODIFY: restore()/sign_in() propagate metadata_only
```

### `crates/spotify/src/stream.rs`
```rust
// MODIFY: Engine::new() returns Result<Option<Engine>>
// MODIFY: Client::stream() checks metadata_only
```

### `crates/core/src/spotify.rs`
```rust
// ADD: metadata_only field to SpotifyState
// ADD: is_metadata_only() method
// MODIFY: new() accepts metadata_only
// MODIFY: restore()/sign_in() use metadata_only
// ADD: settings persistence for metadata_only
```

### `crates/core/src/state.rs`
```rust
// MODIFY: resolve() detects spotify:track: + metadata_only
// ADD: search_youtube_for_spotify_track()
// MODIFY: calls orchestrator.resolve() with YouTube video_id
```

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| YouTube search returns wrong track | Medium | User hears wrong song | Duration matching, official channel preference, user can skip |
| Spotify metadata API changes | Low | Browsing breaks | librespot maintenance, fallback to YouTube search |
| Free account OAuth scopes restricted further | Low | Metadata access lost | Monitor Spotify developer docs, issue tracker |
| User confusion: "I selected Spotify but it plays YouTube" | Medium | UX complaints | Clear UI labeling, "Metadata Only" badge, tooltip |

---

## Success Criteria

1. ✅ Free Spotify account can sign in (OAuth completes without Premium error)
2. ✅ Spotify catalog browsable: search, library, playlists, albums, artists
3. ✅ Playing a Spotify track → YouTube Music stream starts (MPRIS shows YouTube)
4. ✅ Queue mixes Spotify + YouTube tracks seamlessly
5. ✅ Premium account still works normally (metadata_only = false)
6. ✅ Switching providers mid-session works

---

## Estimated Effort

| Phase | Files | Lines Changed | Time |
|-------|-------|---------------|------|
| Phase 1: Provider Core | 4 | ~200 | 2-3 hours |
| Phase 2: Daemon Integration | 2 | ~150 | 2-3 hours |
| Phase 3: UI Config | 2-3 | ~100 | 1-2 hours |
| Phase 4: Testing | - | - | 1-2 hours |
| **Total** | **~10** | **~450** | **6-10 hours** |

---

## Next Steps

1. Start with Phase 1: Modify `auth.rs` to support metadata-only mode
2. Verify OAuth flow works with reduced scopes (test with free account)
3. Implement YouTube fallback in `state.rs::resolve()`
4. Add UI toggle
5. End-to-end test

---

## Appendix: Spotify OAuth Scopes Reference

| Scope | Required For | Premium? |
|-------|--------------|----------|
| `streaming` | Audio playback via librespot | **YES** |
| `user-read-playback-state` | Connect device state | YES |
| `user-read-email` | Profile email | NO |
| `user-read-private` | Profile details | NO |
| `playlist-read-*` | Library/playlists | NO* |
| `user-library-read` | Liked tracks/albums | NO* |
| `user-follow-read` | Followed artists | NO* |
| `user-top-read` | Top tracks/artists | NO* |
| `user-read-recently-played` | History | NO* |

*Some library scopes may be Premium-gated in practice — test required.

---

*Plan created: 2026-11-09*
*Target: Ryotunes main branch*
*Author: Needle (Ryoku resident agent)*