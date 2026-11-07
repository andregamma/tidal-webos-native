# AGENTS.md

Guidance for coding agents working in this repository. Read `README.md` and
`ARCHITECTURE.md` first: they describe the milestones, the crate seams and the
known gaps. This file only adds what an agent needs to change code without
breaking those seams.

## What this is

`tide-tv` is a native (no Chromium) music client for LG webOS TVs: Rust + SDL2 +
OpenGL ES 2.0, drawn directly on the GPU. It is a Cargo workspace of binaries and
libraries; the desktop SDL build is the primary development target and the webOS
`.ipk` is cross-built in CI.

## Layout

| path | contents |
|---|---|
| `crates/tide-core` | UI state, routes, focus, layout, animation, theme, i18n, DrawList. No GL, no I/O, no SDL. |
| `crates/tide-gfx` | GLES2 renderer (SDF rects, glyph atlas, scissor clips). |
| `crates/tide-img` | image decoding. |
| `crates/tide-player` | audio decode + PCM queue. |
| `crates/tide-api` | backend thread implementing `Cmd -> Evt` (mock and real/tidlers). |
| `crates/tide-sdl` | window, GL context, input mapping, main loop, screenshots, theme reload — the only SDL-aware crate. |
| `crates/tide-relay` | Cloudflare Worker PKCE relay. **Excluded from the workspace**, has its own manifest. |
| `assets/` | theme JSON, fonts. `docs/` design notes. `ci/`, `packaging/` webOS build + ipk. |

## Commands

```bash
make sim          # simulator, Home, F3 = fps HUD
make sim-login    # start at the login flow
make check        # cargo test --workspace + cargo clippy --workspace --all-targets
make shots        # headless screenshots into ./shots (needs xvfb + Mesa llvmpipe)
make relay-test   # relay unit tests (host)
make relay-check  # relay type-check for wasm32-unknown-unknown
```

* Host deps: `libsdl2-dev`, `libgles2-mesa-dev`, Rust >= 1.88.
* Headless visual check without `make shots`:
  `cargo run -p tide-sdl -- --screen=home --keys=down,right,ok --screenshot=out.png`
* Useful flags: `--screen=`, `--keys=`, `--screenshot=`, `--size=3840x2160`,
  `--hud`, `--no-vsync`, `--backend=tidlers` (with `--features tidlers`).
* There is no formatter/rustfmt config; match the surrounding style (4 spaces,
  ~100 columns, doc comments in English).

## Architectural rules (do not break these)

* Dependencies point inward: `tide-core` knows nothing external; only `tide-sdl`
  knows SDL; only `tide-api` does HTTP. The TV build swaps only `tide-sdl`'s
  window/input bits, so platform code must not leak into other crates.
* The UI never blocks and never sees HTTP. New backend work is a `Cmd` in
  `tide-core/src/services.rs`; results come back as `Evt` through the mpsc pump.
* `update(dt, keys)` mutates state; `view()` is a pure function of state that
  appends to a `DrawList`. There is no retained widget tree: screens are small
  structs that draw themselves.
* Screens should not mutate the app: return an `Action`
  (`FocusSidebar`, `FocusBar`, `Play(track)`, `Back`, `OpenQuality`, ...).
* Anything that moves is a `Smooth` (frame-rate independent), never a raw lerp per
  frame. Lists cull off-screen items before emitting commands.
* The logical canvas is fixed at 1920x1080; `Renderer::set_drawable_size` scales
  and letterboxes. Do not hardcode pixel sizes that assume the framebuffer size.
* Rendering is one shader, one dynamic VBO, one A8 atlas; draw calls only break at
  `Clip`. Avoid adding per-frame state changes or extra shaders without need.
* Text is rasterised by `fontdue` per (char, physical px): no kerning, shaping,
  RTL or emoji. Don't assume they work.

## Protocol and backend changes

* Changing `Cmd`/`Evt` means updating **both** implementations: the mock
  (`tide-api/src/mock.rs`) and the real/tidlers backend
  (`tide-api/src/tidlers_backend/`), plus every `match` in `tide-sdl`/`App`.
* Settings (audio quality, locale) are persisted by `tide_api::settings` into
  `tide-settings.json` and survive logout; quality is a ceiling request capped by
  the account's `highestSoundQuality`, and `TIDAL_QUALITY` overrides both.
* `tidlers` is pinned (`=0.5.0`); don't bump it casually.
* `crates/tide-relay` must stay out of the root workspace: it is a wasm/Workers
  build and would break host builds.

## Configuration and secrets

* `.env` (git-ignored) holds `TIDAL_CLIENT_ID`, `TIDAL_CLIENT_SECRET` and the
  optional `TIDE_*` variables. Cargo and a bare `cargo run` do **not** read it;
  the `Makefile` includes and exports only those names, so use the `make`
  targets when credentials matter.
* Never commit `.env`, `tidlers-session.json`, `tide-settings.json`, tokens, or
  logs. Never print secret values into code, tests, docs or logs.
* CI builds two matrix entries (`""` and `tidlers`) and the tidlers one may fail
  on TLS/C deps; keep the basic build green.

## Testing and honesty about status

* `tide-core` is GPU-free on purpose: state, navigation, text fitting and route
  flow are unit-tested headlessly (`App::view` with `MonoMetrics`). Keep new core
  logic testable without a window.
* Run `make check` for UI/logic changes. For renderer or visual changes, produce
  a `--screenshot` and compare it; the simulator proves look and logic, not TV
  performance.
* Auth, the relay redirect and everything webOS/TV-specific are **not verified
  against real hardware or the real service**. Don't claim they work; if you
  change status, update README / `ARCHITECTURE.md` / the relevant `docs/*.md`
  (including the "Not done / unverified" list).
* Docs are in English and are current: when behaviour changes, fix the doc that
  describes it (`docs/auth.md`, `docs/audio.md`, `docs/relay.md`,
  `docs/webos.md`, `docs/covers-and-quality.md`, `docs/tidlers-poc.md`).
* Commits follow Conventional Commits with a scope, e.g.
  `feat(settings): add settings page`, `fix(home): fix home structure and parsing`.
  `Cargo.lock` is committed on purpose.
