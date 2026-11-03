# Proof of concept: backend on `tidlers`

Run (no credentials to configure, see `docs/auth.md`):
```
make sim-tidlers
# = cargo run --release -p tide-sdl --features tidlers -- --screen=login --backend=tidlers
```
Code: `crates/tide-api/src/tidlers_backend/` (`mod.rs` adapter, `map.rs` pure mappings).
The hand-written `real` backend was removed once this one worked; only `mock` and `tidlers` remain.

## What the adapter does
| Cmd | Implementation |
|---|---|
| `Boot` | Read saved refresh token → one **forced** refresh (also fills the user info the library needs for favourites). Rejected (400/401/403) → delete + `LoginRequired`; other errors → `LoginFailed`. |
| `StartLogin` | `get_oauth_link()` → `Evt::LoginCode` (QR) → `wait_for_oauth(...)`. Library timeout → new code automatically (max 6). |
| `LoadLiked` | `get_collection_track_favorites(300, 0)` → `Track` (artists joined, version appended). **First page only.** |
| `LoadHome` | `get_home_feed(40)` → horizontal lists → shelves. |
| `LoadLyrics` | `get_track_lyrics` → LRC parser. Timestamps ⇒ synced; otherwise "unsynced" mode (static text, Up/Down scroll). No lyrics / error ⇒ empty list. |
| `Logout` | Delete session file. |

Core changes this required: `Card.play` is now `Option<Track>` (+ `art_url`), `Evt::Lyrics`
gained `synced`, `Action::Toast`, and the player has an unsynced lyrics mode.

## Findings from reading the crate (0.5.0)
1. **It embeds default client credentials** (base64-obfuscated in `auth/credentials.rs`). `TidalAuth::with_oauth()`
   uses them and this app no longer overrides or configures them (decision taken after the POC; it is subject to the
   service's terms and the library can change them).
2. `TidalClient` is `Serialize` **including `client_secret` and tokens**: never dump it to
   disk. The adapter persists only the refresh token (`tidlers-session.json`, mode 0600).
3. `wait_for_oauth` treats `slow_down` as fatal (the hand-written backend backs off instead).
   Expect an occasional "try again" on slow polling; fixing it means patching/forking.
4. Scope is hard-coded to `r_usr w_usr w_sub` (matches what we used).
5. Home feed models have many **required** fields. A real response may fail to
   deserialize; then you get a toast "Could not load Home" and the skeleton stays.
   Home items are mixes/playlists/albums/artists: those cards are **not playable yet**
   (toast). They need a collection screen: new core work for M2.
6. `LyricsResponse` has a single `lyrics: String`. Synced lyrics only work if that text is
   LRC; if the service sends timestamps in another field the crate does not expose it.
7. TLS: `reqwest 0.13` default features ⇒ rustls + **aws-lc-rs** (C build). It compiles
   natively here. Cross-compiling to the webOS ARM target is **not tested** and is the
   main M4 risk. The crate is edition 2024 (Rust ≥ 1.85; our lockfile needs ≥ 1.88).

## Verification status
| | Status |
|---|---|
| Builds with `--features tidlers` | yes (native x86_64) |
| Unit tests (LRC parsing, Track mapping from JSON, credential override, config gating, rejection classification) | 33 pass |
| Home mapping | **not unit-tested** (needs huge fixtures); compiled only |
| Login / refresh / favourites / lyrics against the real service | **never run** |
| Cross-compile for webOS | **not tried** |

## Decision checklist (to choose between `real` and `tidlers`)
1. Does the first real login work with your credentials? (`make sim-tidlers`)
2. Do Liked songs and Lyrics load? Does Home deserialize? (toasts tell you)
3. Does the crate cross-compile with the NDK (aws-lc)? If not: fork with another TLS backend,
   or keep `real` for auth and use `tidlers` models only.
4. If all pass: delete `crates/tide-api/src/real/{auth,http,store}.rs` (about 450 lines + tests).
