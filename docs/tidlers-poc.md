# Backend on `tidlers`

The `tidlers` crate is the **only** backend: every build and every platform runs it. The mock
backend and the hand-written `real` module were removed, and there is no cargo feature or
`--backend` switch to disable it.

Run (`TIDAL_CLIENT_ID`/`TIDAL_CLIENT_SECRET` are optional, see `docs/auth.md`):
```
make sim
# = cargo run --release -p tide-sdl   (boots, then Home or sign-in)
```
Code: `crates/tide-api/src/tidlers_backend/` (`mod.rs` adapter, `map.rs` pure mappings).

## What the adapter does
| Cmd | Implementation |
|---|---|
| `Boot` | Read saved refresh token → one **forced** refresh (also fills the user info the library needs for favourites). Rejected (400/401/403) → delete + `LoginRequired`; other errors → `LoginFailed`. |
| `StartLogin` | PKCE: the relay builds the sign-in page around the crate's own client and `redirect_uri` → `Evt::PkceLogin` (two QR codes) → the phone signs in and pastes the resulting address on the relay → `finish_pkce_login(...)`, then `subscription()` for the quality. Any failure → `Evt::FallbackOffered`. |
| `StartLoginFallback` | `get_oauth_link()` → `Evt::LoginCode` (QR) → `wait_for_oauth(...)`. Library timeout → new code automatically (max 6). |
| `CancelLogin` | Taken inside the PKCE wait: stops polling the relay without waiting for the timeout. |
| `LoadLiked` | `get_collection_track_favorites(300, 0)` → `Track` (artists joined, version appended). **First page only.** |
| `LoadHome` | `get_home_feed(40)` → horizontal lists → shelves. |
| `LoadLyrics` | `get_track_lyrics` → LRC parser. Timestamps ⇒ synced; otherwise "unsynced" mode (static text, Up/Down scroll). No lyrics / error ⇒ empty list. |
| `Logout` | Delete session file. |

Core changes this required: `Card.play` is now `Option<Track>` (+ `art_url`), `Evt::Lyrics`
gained `synced`, `Action::Toast`, and the player has an unsynced lyrics mode.

## Findings from reading the crate (0.5.0)
1. **It embeds default client credentials** (base64-obfuscated in `auth/credentials.rs`), and two different
   pairs: one for `with_oauth()` (device code) and one inside `PkceConfig::try_default()` (PKCE). The device
   flow keeps the embedded pair. The PKCE flow is switched to the client named by `TIDAL_CLIENT_ID` /
   `TIDAL_CLIENT_SECRET` in `auth_for`, because the code is authorized for the relay's client and every later
   refresh has to authenticate as that same client (see `docs/auth.md` §Credentials); without the variables
   the embedded PKCE pair is used and a notice says so in `tide.log`.
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
8. **PKCE**: `initiate_pkce_login()` / `finish_pkce_login()` exist, but `PkceConfig::try_default()`
   pins `redirect_uri` to `https://tidal.com/android/login/auth` and the URL carries no `state`;
   this app overrides the redirect (to the TV's LAN address) and appends its own `state`, because
   both live outside the library's API. `client_unique_key` is regenerated on every
   `TidalAuth::with_pkce()`, and `refresh_access_token` only uses the PKCE credentials while
   `pkce_login` is set — hence the flow is saved with the token (`docs/auth.md`).
9. **DASH manifests (HiRes)**: the crate parses the MPD with quick-xml but keeps the *raw*
   attribute values, so the URLs arrive with `&amp;` still in them and relative to `<BaseURL>`;
   `get_init_url()` also falls back to `urls[0]`, which is that `<BaseURL>` (a directory).
   Fetching either one earns `HTTP 403`. `tide-api` unescapes the entities, joins the relative
   paths and never fetches a bare directory (`dash_url`/`dash_base` in `tidlers_backend`).
10. **DASH means FLAC-in-fMP4**: for these tracks the service answers `audio/mp4` with
    `codecs="flac"` (init segment + `moof`/`mdat` fragments) even at `LOSSLESS`. The crate has no
    stream helper at all, so the app concatenates init + segments and symphonia's `isomp4`
    demuxes them. A `MediaSource` must not report `is_seekable` while the length is unknown —
    `isomp4` fails the whole stream with `seek error: stream is not seekable`. TidaLuna does the
    same by hand (`parseDasha(xml, "https://sp-ad-cf.audio.tidal.com")` plus a segment list with
    `init` flags), so this is not something the API gives us for free.

## Verification status
| | Status |
|---|---|
| Builds (native x86_64) | yes |
| Unit tests (LRC parsing, Track mapping, rejection classification, session file, **PKCE server**: state, 404/405, `/auth`, one-shot, timeout, request cap) | 31 pass |
| PKCE sign-in against the real service | **never run**; depends on the service accepting the LAN `redirect_uri` (else the fallback is used) |
| Home mapping | **not unit-tested** (needs huge fixtures); compiled only |
| Login / refresh / favourites / lyrics against the real service | **never run** |
| Cross-compile for webOS | **not tried** |

## Next steps
1. Confirm the first real login works with your credentials (`make sim`).
2. Confirm Liked songs and Lyrics load and Home deserializes (toasts tell you).
3. Confirm the crate cross-compiles with the NDK (aws-lc, `docs/webos.md`). If it does not, fork it
   with another TLS backend rather than reintroducing a local-only backend.
