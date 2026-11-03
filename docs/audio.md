# Audio / player (M3 first cut)

```
UI ──Cmd::Play──► backend ──resolve stream──► PlayerCmd::Load ─┐
UI ──PlayerCmd::{Pause,Resume,Seek,Stop}───────────────────────┤
                                                                ▼
                 player thread ─► decode thread: HTTP (progressive) → symphonia → f32 PCM
                       ▲ Evt::{Position,TrackEnded,PlaybackError}        │
                       └────────────────────────────────────────  PcmPort (2 s queue)
                                                                         │ fill() in the audio callback
                                                       tide-sdl: SDL2 audio device (desktop; TV if SDL provides a driver)
```
* `tide-core::services`: `StreamSpec`/`StreamSource`/`PlayerCmd`, `Evt::{Position, TrackEnded, PlaybackError}`.
  Transport goes UI → player directly, so pause/seek never wait behind backend network calls.
* `tide-player`: `source.rs` (progressive HTTP, whole stream in RAM), `engine.rs` (symphonia
  decode, seek, position, end of track), `pcm.rs` (queue + device negotiation; no audio API inside).
* `tide-sdl/src/audio.rs`: opens/reopens an SDL device when the decoder asks for a new rate.
* `tidlers_backend::play`: `get_track_postpaywall_playback_info` → `Single(url)` (JSON manifest,
  LOW/HIGH/LOSSLESS) or `Segmented{init, $Number$ template}` (DASH, HIRES).
* Quality: `TIDE_QUALITY=low|high|lossless|hires` (default `lossless`).
* The mock backend uses `StreamSource::Simulated` (silent fake clock), so the UI keeps working without a service.

## Protected streams are not played
The service can mark a stream as encrypted (`encryptionType` other than `NONE`). The reference
client (TidaLuna) decrypts those with a key embedded in its source. **This project does not implement
or ship that**: such tracks fail with "This stream is protected and cannot be played here". Only clear
streams play.

## Try it without any service
```
python3 -m http.server 8099            # in a folder with a .flac/.m4a/.wav
cargo run --release -p tide-sdl -- --play-url=http://127.0.0.1:8099/song.flac --mime=audio/flac
SDL_AUDIODRIVER=<pulseaudio|alsa|...>  # override SDL's pick; the chosen driver is printed
```
Playback stops when you leave the player screen (Back). There is no mini-player yet.

## webOS
* SDL audio uses whatever driver the TV's SDL build ships. **Not verified on a TV.** If it opens, the
  desktop code path is the TV code path. Run with `SDL_AUDIODRIVER` set and read the "audio: using driver" line.
* The system-native route is the webOS media pipeline (what PlxNative uses for video via LG's
  Starfish/`libplayerAPIs`): it decodes in the platform and bypasses `tide-player`'s decoder. It relies on
  reverse-engineered, per-firmware private symbols, so it needs a real TV to build and test; the seam is
  `PlayerCmd`/`Evt` (implement a second player behind the same channels).

## Verified / not verified
| | |
|---|---|
| Builds (desktop), existing tests pass | yes |
| End to end with a local WAV (HTTP → decode → SDL `disk` driver): a 4 s 440 Hz tone arrived with the right amplitude and length | yes |
| Pause, resume, seek, end-of-track event | written, **not exercised** |
| FLAC / AAC (M4A) / DASH HiRes from the real service | **not tested** (needs your account) |
| SDL audio on webOS | **not tested** |

## Known limits
* Whole stream is buffered in RAM; seeking past the downloaded part waits (no HTTP Range yet).
* More than 2 channels: only the first two are played. No resampling (the device is opened at the track's rate).
* DASH segment count is estimated from the track length; the first failing segment ends the stream.
* No gapless/crossfade, no replay gain, no "next track" queue, no buffering indicator.
