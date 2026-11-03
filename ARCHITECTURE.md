# Architecture

```
            ┌────────────────────────── tide-sdl (platform) ───────────────────────────┐
 remote ──► │ SDL events ─► map_key ─► Key ─┐                                          │
            │                               ▼                                          │
            │   loop:  app.update(dt, keys) ── pump Evt ◄───────────────┐              │
            │          app.view(&mut DrawList, &metrics)                │              │
            │          renderer.render(&DrawList)  ─► GLES2 ─► swap     │              │
            └───────────────┬───────────────────────────────────────────┼──────────────┘
                            │                                           │ mpsc
                  tide-core │  App ─ Sidebar/Home/Liked/Player/Login    │ Cmd ▲ ▼ Evt
                  (no GL,   │  Focus · Smooth anims · DrawList          │
                   no I/O)  └───────────────────────────────────────────┼──────────────┐
                                                                         │ tide-api    │
                                                              backend thread (mock → real)
```

## 1. Crates
| crate | responsibility | knows about |
|---|---|---|
| `tide-core` | state, navigation/focus, layout, animation, theme; emits `DrawList` | nothing external |
| `tide-gfx` | GLES2 renderer: SDF rounded rects, glyph atlas, scissor clips | `glow`, `fontdue` |
| `tide-api` | backend thread implementing `Cmd → Evt` (mock today) | `tide-core` types |
| `tide-sdl` | window, GL context, input mapping, main loop, screenshots, theme hot-reload | SDL2 |

Rule: dependencies point inward. The TV build swaps only `tide-sdl`'s window/input bits.

## 2. Frame loop
`update(dt, keys)` mutates state and advances animations; `view()` is a pure
function of state that appends to a `DrawList`; the renderer consumes it.
No retained widget tree: screens are small structs that draw themselves.
Why: trivial to reason about, allocation-light, easy to test headlessly
(`App::view` with `MonoMetrics` in `app.rs` tests).

## 3. Focus & motion (the part that makes it feel smooth)
* Three focus zones: **Sidebar**, **Content** and the **mini bar**. Screens return an `Action`
  (`FocusSidebar`, `FocusBar`, `Play(track)`, `Back`) instead of mutating the app.
* Every moving value is a `Smooth` (exponential smoothing, frame-rate
  independent): scroll offsets, per-card focus (ring + 5% scale), sidebar
  expansion, highlight bar, lyric scroll, route fade.
* Sidebar expands as an **overlay**, so content never re-layouts.
* Lists cull off-screen rows/cards before emitting commands.

### Mini bar (persistent "now playing")
`widgets::minibar::MiniBar` is a component, not a page: it takes a borrowed `model::NowPlaying`, returns a
`BarAction` and knows nothing about routes or the backend. `App` draws it above the page (below the sidebar
overlay and the toast) on every route except Login and the full player, and acts on its actions
(play/pause, open the full player). Playback survives navigation: Back from the player no longer sends
`PlayerCmd::Stop`, and `PlayerScreen` keeps its clock running on every route.

To use it from a new page: (1) call `set_bottom_inset(minibar::RESERVED)`-style handling like `HomeScreen`
does, so content stays clear of the bar; (2) return `Action::FocusBar` when Down runs off the end of the page.
The bar is reached **only** that way (or via the sidebar's "Reproduzindo agora" entry), so the arrow keys keep
their normal meaning everywhere else. Up/Back leaves the bar; Left/Right pick play/pause or the chevron.
A finished or failed stream is flagged (`PlayerScreen::is_dead`): play then reloads it with a new `Cmd::Play`,
because a `Resume` would reach no stream. Selecting the track that is already playing only shows the player.

## 4. Rendering
* Logical canvas is 1920×1080; `Renderer::set_drawable_size` scales it
  (letterboxed). Rendering at 3840×2160 only changes the framebuffer and glyph
  raster size, not the layout: `make sim-4k` lets you measure the cost.
* One shader, one dynamic VBO, one A8 atlas ⇒ draw calls only break at `Clip`.
  Check `--hud` for the count.
* Text: `fontdue` rasterises per (char, physical px) into the atlas; glyphs are
  pixel-snapped. No kerning/shaping yet (no RTL, no ligatures, no emoji).
* Known cheap-but-suboptimal spots (fix when profiling says so): `String`
  per text command, whole-atlas re-upload when dirty, `fit_text` recomputed per
  frame, 6 vertices per quad instead of indices/instancing.

## 5. Service seam
Protocol (M1): `Cmd::{Boot, StartLogin, Logout, LoadHome, LoadLiked, LoadLyrics, LoadAccount, SetLocale}`,
`Evt::{LoginRequired, LoginCode, LoggedIn, LoginFailed, Home, Liked, Account, Lyrics, Error}`.
`tide-api::tidlers_backend` adapts the `tidlers` crate (async, own tokio runtime) to this protocol.

`services.rs` defines `Cmd` (UI → backend) and `Evt` (backend → UI). The UI
never blocks and never sees HTTP. The real backend is `tide-api::tidlers_backend::run(BackendSide)`
with the same contract (device-code login, token refresh/storage, home, favourites,
lyrics), and choose mock vs real in `tide-sdl` (e.g. `--backend=mock`).

Alternative backend (POC, cargo feature `tidlers`): `tide-api::tidlers_backend` adapts the
`tidlers` crate to the same protocol; see `docs/tidlers-poc.md` for findings and status.

Settings & language: the Settings page (`screens/settings.rs`, with the reusable `widgets/select.rs`) shows the
account (`Evt::Account`: name, e-mail, plan) and a language select fed by `tide_core::i18n::LANGUAGES` (codes like
`pt-BR`). Picking one sends `Cmd::SetLocale`; the real backend stores it, sets `client.session.locale` on the live
client and in `new_client` for every later one, and persists it through `tide_api::settings` (`tide-settings.json`,
same multi-location storage as the session, kept across logout). `tide-sdl` loads it at start and gives it to
`App::with_locale`; `App::locale()` is the key for the app's own i18n texts.

Audio: `tide-player` (decode + PCM queue) with SDL output in `tide-sdl`; see `docs/audio.md`.

## 6. Roadmap
* **M1 Auth (done, untested against the real service)**: device-code flow, real QR,
  token persistence + refresh. See `docs/auth.md`.
* **M2 Data**: real Home/Liked; async image loading → textures (add an `Image`
  command + texture cache with LRU; replace `ui::art`).
* **M3 Player (first cut done, see docs/audio.md)**: new `tide-player` crate/thread (HTTP range streamer + decoder
  + audio out). It owns the clock and sends `Evt::Position`; `PlayerScreen`
  stops simulating. Add `Cmd::Play/Pause/Seek`.
* **M4 TV (build + ipk done, see docs/webos.md; nothing run on a TV yet)**: webOS NDK cross-build, SDL/GL context on the TV, key mapping, `.ipk`,
  fps/memory measurements on real hardware, decide default interface resolution.

## 7. Testing
* `cargo test --workspace`: navigation, text fitting, headless frame + route flow.
* Visual: `--screenshot` (xvfb + Mesa llvmpipe works; see `make shots`).
