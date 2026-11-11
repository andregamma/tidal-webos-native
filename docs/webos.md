# Building and running on the TV (M4)

Target tested here: LG C4 (webOS 24/26 firmware line). The app has since been built, installed and run on a
real TV; the session and the settings file do not survive a restart there, and Back in the player closes the app
(see README, *Known issues*). Everything else below is still unverified on hardware.
What *was* verified in the build sandbox: SDL-webOS cross-compiles with the NDK, and the Rust app
cross-compiles and links for `armv7-unknown-linux-gnueabi` (see "Build status").

## How the build works
* **NDK**: the x86_64 Linux buildroot SDK from `openlgtv/buildroot-nc4` (tag `webos-b17b4cc`; GCC 12.2,
  glibc 2.12 sysroot, default `-mcpu=cortex-a9 -mfloat-abi=softfp`). `ci/setup-ndk.sh` downloads and relocates it.
* **SDL**: built from `webosbrew/SDL-webOS` (SDL 2.30.12 + webOS Wayland driver + remote-key scancodes) with
  `SDL_WEBOS_BROKEN_ABI=OFF` and **bundled inside the .ipk** (`lib/libSDL2-2.0.so.0`, found through RPATH `$ORIGIN/lib`).
  Reason: some LG firmwares ship an SDL whose event structs differ from upstream (`inputSource` field); bundling
  our own removes that ABI question for the `sdl2` crate. Audio drivers inside: pulseaudio, alsa (loaded at runtime).
* **Rust**: target `armv7-unknown-linux-gnueabi`, linker = NDK gcc. The NDK's glibc 2.12 lacks `getauxval`
  (needed by std and `ring`): `ci/shims/getauxval.c` provides a weak fallback.
* **Package**: `packaging/mkipk.py` writes the `.ipk` (ar of `debian-binary`, `control.tar.gz`, `data.tar.gz`;
  `usr/palm/applications/<id>/` + `usr/palm/packages/<id>/packageinfo.json`). `packaging/appinfo.json`
  holds the app id (`dev.tidalwebos.app`; change it) and metadata.

## Get an .ipk
**GitHub Actions** (works from Windows): push the repo, open *Actions -> webOS build -> Run workflow*,
download the `tidal-webos-ipk` artifact. It contains the real Tidal backend (`tidlers`); the TLS/C
dependencies (`aws-lc`) are the risky part of the cross-build.
**Locally** (Linux x86_64 or arm64 with cmake, ninja, python3, Rust + the `armv7-unknown-linux-gnueabi` target):
```
ci/setup-ndk.sh                       # once
ci/webos-build.sh                     # -> dist/tidal-webos.ipk
# without rustup's prebuilt std: BUILD_STD=1 (nightly features via RUSTC_BOOTSTRAP, needs rust-src)
```

## Install (LG Developer Mode)
1. TV: install *Developer Mode* from the LG Content Store, sign in with a LG developer account, turn
   *Dev Mode Status* and *Key Server* on. The session expires periodically; renew it or LG removes the app.
2. PC: `npm i -g @webos-tools/cli`, then
   ```
   ares-setup-device --add tv -i "host=<TV IP>" -i "port=9922" -i "username=prisoner"
   ares-novacom --device tv --getkey        # passphrase shown in the Developer Mode app
   ares-install --device tv dist/tidal-webos.ipk
   ares-launch  --device tv dev.tidalwebos.app
   ```
   (or the webOS Dev Manager / VS Code extension: same steps in a GUI.)

## First run on the TV: what to look at
* The app starts with the **HUD on** (fps, draw calls, `key <scancode>`); the **red button** toggles it.
  Press every remote key and tell me which scancode shows up for OK, arrows and Back.
  Known from SDL-webOS: Back = 482, Home = 384, Exit = 505, Red = 486; Play/Stop/Prev/Next arrive as the
  standard audio keys; Rew/FF = 285/286.
* **Log**: `/media/developer/tidal-webos/tide.log` (the previous run is `tide.log.old`; falls back to `<app dir>/tide-data`, see `docs/auth.md`). Fetch with
  `ares-pull --device tv /media/developer/tidal-webos/tide.log .`. It records the SDL video/audio driver chosen,
  panics and errors.
* **Credentials**: nothing to configure; sign-in uses the `tidlers` library's defaults (`docs/auth.md`).
* The backend is always `tidlers`; there is no backend switch.

## Things most likely to go wrong (in order)
1. **Black screen / immediate exit**: the log shows why (SDL video init, GLES context, missing lib). GLES 2.0 is requested.
2. **Keys**: OK/arrows should work as standard keys; Back needs the raw watch (`remote.rs`).
3. **No sound**: look for `audio: using driver ...` in the log; set `SDL_AUDIODRIVER` if SDL picked the wrong one.
4. **Out of memory** on long lossless tracks (the stream is held in RAM; `requiredMemory` in appinfo is a guess).
5. **`tidlers` / TLS**: `aws-lc` or glibc-2.12 symbol problems (more shims like `getauxval` may be needed),
   or runtime TLS/certificate paths on the TV.

## Build status (verified in the build sandbox, x86_64 Linux, Rust 1.91 + `-Z build-std`)
| | Result |
|---|---|
| SDL-webOS (NDK, `WEBOS=ON`, `BROKEN_ABI=OFF`) | builds; 1.3 MB stripped; audio drivers alsa + pulseaudio, video wayland |
| `tidal-webos` (reqwest + rustls + aws-lc) for `armv7-unknown-linux-gnueabi` | links; needs only glibc symbols up to 2.12; 10 MB (4.3 MB ipk) |
| `.ipk` packaging | produced; structure checked with `ar`/`tar` only |
| Running on a TV | runs; session/settings persistence and Back-in-player fail (README, *Known issues*) |

Needed fixes found along the way: `getauxval` shim (glibc 2.12), and the shim must be `-fPIC` (the binary is PIE).
GitHub Actions uses rustup's prebuilt std instead of `-Z build-std`; that path was not exercised here.
