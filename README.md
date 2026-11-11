# tidal-webos

A native (no Chromium) music client for LG webOS TVs: Rust + SDL2 + OpenGL ES 2.0, drawn
directly on the GPU. Sign-in, Home, Liked songs, a persistent player with lyrics, and a
Settings page for audio quality and language. Every screen talks to the real service through the
`tidlers` crate, the only backend (there is nothing to fall back to and no mock mode).

![home](docs/home.png)

## About this project

This is a **personal project**. I was inspired by the native Rust applications that have been
coming out lately and decided to try building one myself: a TIDAL client for the TV, without a
browser engine in the middle.

### Disclaimer

This repository is in a **preliminary stage of development** and is being built with the help of
AI tools. I am a professional software developer, but I have **no previous experience with Rust**,
and none with native/GPU application development either. Expect rough edges, changing structure and
code that a seasoned Rust developer would write differently.

Because of that, **constructive criticism, suggestions and pull requests are very welcome** — they
are the main reason this is public.

## Status: what works today

* **Sign-in** — PKCE authorization code through a small Worker relay (QR code on the TV, code
  pasted back from the phone), with the device-code flow as a CD-quality fallback, plus token
  refresh and a persisted session. See `docs/auth.md` and `docs/relay.md`.
* **Library** — Home shelves and Liked songs come from the real API. Cover art is downloaded and
  decoded on worker threads into a small GPU texture cache (LRU); see `docs/covers-and-quality.md`.
* **Player** — `tidal-player` streams the track over HTTP, decodes it with symphonia and feeds a
  PCM queue that `tidal-sdl` plays through SDL. Play/pause, seek, a quality badge with the stream's
  real specs (`FLAC · 24-bit / 96 kHz`) and lyrics. The lyrics are **plain text you scroll with
  Up/Down**: the player does parse LRC timestamps and follows the position when they are present,
  but the text the backend receives from the service carries none, so the synced mode does not
  engage in practice (`docs/tidlers-poc.md`). A mini bar keeps "now playing" visible on every
  screen. See `docs/audio.md`.
* **Settings** — account (name, e-mail, plan), audio quality (Low / High / Lossless / Max, capped by
  what the account can stream) and interface language. `Q` (the remote's green button) jumps
  straight to the quality field.
* **TV build** — NDK cross-build and `.ipk` packaging are automated in CI. See `docs/webos.md`.

## Roadmap

- [ ] **Albums, playlists and mixes**: browse and open album pages, playlists and the mixes the
      service recommends.
- [ ] **UI/UX fixes and improvements**: focus/navigation polish, spacing, transitions and the
      overall feel of every screen.
- [ ] **Synced lyrics**: make the lyrics scroll with the track. The LRC parser and the scrolling
      code exist, but the service delivers untimed text, so today the player shows a static list and
      asks you to scroll with Up/Down. Also polish that layout (the header line collides with the
      text — see *Known issues*).
- [ ] **Quality switch in the player**: change the audio quality without leaving the player
      (`Q` currently opens Settings on the quality field).
- [ ] **Playback queue**: continuous playback across a list, plus manual "add to queue".
- [ ] **i18n from the service's own files**: replace the hand-kept `i18n::LANGUAGES` list with the
      language JSON the service already ships.

## Known issues

These are reproduced on a real TV, so they are the current priority:

* **Sign-in is not persisted on the TV** (real device): the saved session is not read back on the
  next launch, so the app asks to sign in again.
* **Settings are not persisted on the TV either**: audio quality and language fall back to their
  defaults on every launch.
* **Overlapping elements**: the quality badge and the elapsed time collide in the mini bar, the
  quality badge overlaps other content in the player, and the lyrics area collides with the header
  text at the top of the player.
* **Back in the player closes the app**: pressing Back while the player is open should only leave
  the player and return to the previous screen.
* **Lyrics do not follow playback**: the player only auto-scrolls when the lines carry LRC
  timestamps, and the service sends none, so the screen shows "Lyrics are not time-synced ·
  Up/Down to scroll" and stays still until you scroll it yourself.

## Run the simulator

```
sudo apt install libsdl2-dev libgles2-mesa-dev   # Debian/Ubuntu (+ Rust >= 1.88 via rustup)
make sim            # boots: restored session goes to Home, otherwise sign-in; F3 = fps HUD
make check          # unit tests (core is GPU-free)
```

`TIDAL_CLIENT_ID`/`TIDAL_CLIENT_SECRET` in `.env` are optional: without them the client embedded
in `tidlers` is used (see `docs/auth.md`).

Keys: arrows = D-pad, Enter = OK, Esc/Backspace = Back, Space/P = play/pause, N/B = next/previous,
Q = audio quality, F3 = HUD. On the TV the red button toggles the HUD.
Edit `assets/theme.json` while the simulator runs: colours reload live.

## Screenshots

`--screen=` draws one route without the boot sequence, so a screenshot needs no account:

```
cargo run -p tidal-sdl -- --screen=home --screenshot=out.png
cargo run -p tidal-sdl -- --screen=home --keys=down,right,ok --screenshot=out.png
```

The image is the layout only — empty shelves instead of real content, and no cover art.
`make shots` does the same for Home on a headless Linux box (xvfb + Mesa llvmpipe).
`docs/home.png`, `docs/liked.png` and `docs/login.png` were captured this way; `docs/player.png` and
`docs/login_failed.png` are older, from a build that still had a mock backend, and show content that
no longer exists (they will be replaced once a signed-in capture is possible).

## Layout

See ARCHITECTURE.md. Short version: `tidal-core` (pure UI logic, testable) →
`DrawList` → `tidal-gfx` (GLES renderer); `tidal-api` (backend thread on `tidlers`);
`tidal-sdl` (window/input/loop; the only SDL-aware crate). The sidebar logo is uploaded by
`tidal-sdl` from `assets/logo` (wordmark when the rail is open, icon when it is collapsed).

## Docs

`docs/auth.md` (sign-in), `docs/relay.md` (the Worker), `docs/audio.md` (player and quality),
`docs/covers-and-quality.md`, `docs/tidlers-poc.md`, `docs/webos.md` (build, package, install).

## Not done / unverified (be honest with yourself)

* Everything TV-specific beyond a first install: key scancodes for every remote button, fps and
  memory on real hardware, and `packaging/appinfo.json` (copy the real fields from PlxNative's
  `ipkroot`/appinfo).
* The relay (`crates/tidal-relay`) builds and is unit-tested, but whether the service accepts its
  redirect still depends on the client id you use (see `docs/relay.md`).
* Protected streams are not played: tracks the service marks as encrypted fail with a message
  instead of being decrypted (see `docs/audio.md`).
* No albums, playlists, mixes, search or queue yet: Home and Liked are the whole library.
* Font is Inter (SIL OFL, see `assets/fonts`), with DejaVu Sans kept as a fallback for the few
  symbols Inter lacks (the sidebar's gear). No shaping, kerning, RTL or emoji.

Check the service's terms before shipping anything built from this.
