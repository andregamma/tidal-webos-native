# tide-tv (M0 skeleton + M1 auth)

Native (no Chromium) music-client UI for LG webOS TVs: Rust + SDL2 + OpenGL ES 2.0,
drawn directly on the GPU. **M0 = UI skeleton with mock data**: login screen,
Home, Liked songs, sidebar, player with synced lyrics (simulated playback).
**M1** adds real device-code sign-in (QR, code, countdown), token refresh and a
persisted session: see `docs/auth.md`. Sign-in uses the `tidlers` library's own client credentials: there is nothing to configure. **First cut of audio** (`tide-player`, SDL output): see `docs/audio.md`. TV build + `.ipk` packaging: see `docs/webos.md`.

![home](docs/home.png)

## Run the simulator
```
sudo apt install libsdl2-dev libgles2-mesa-dev   # Debian/Ubuntu (+ Rust >= 1.88 via rustup)
make sim            # Home screen, F3 = fps HUD
make sim-login      # starts at the login flow (mock "approves" after ~5 s)
make check          # unit tests (core is GPU-free)
make sim-tidlers    # real service via the `tidlers` crate: no credentials to configure (see docs/auth.md)
```
Keys: arrows = D-pad, Enter = OK, Esc/Backspace = Back, Space/P = play/pause, F3 = HUD.
Edit `assets/theme.json` while the simulator runs: colours reload live.

Headless screenshots (CI / quick visual diff):
`cargo run -p tide-sdl -- --screen=home --keys=down,right,ok --screenshot=out.png`

## Layout
See ARCHITECTURE.md. Short version: `tide-core` (pure UI logic, testable) →
`DrawList` → `tide-gfx` (GLES renderer); `tide-api` (backend thread, mock now);
`tide-sdl` (window/input/loop; the only SDL-aware crate).

## Not done / unverified (be honest with yourself)
* Everything TV-specific: NDK cross-build, `.ipk`, SDL on webOS, remote key codes,
  `packaging/appinfo.json` (copy the real fields from PlxNative's `ipkroot`/appinfo).
* Real fps on TV hardware. The simulator proves logic and look, not performance.
* Authentication has only been tested against fakes, never the real service (see `docs/auth.md`).
* Content (M2) and audio (M3). Check the service's terms before shipping.
* Font is DejaVu Sans (free licence, see `assets/fonts`). Pick your own later.

Covers and the quality indicator: see `docs/covers-and-quality.md`.
