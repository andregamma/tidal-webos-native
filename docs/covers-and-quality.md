# Cover images and the quality indicator

Written without compiling: the first build may report small errors; send them back.

## Covers
`tidlers_backend/map.rs` turns image ids into URL **templates** (`.../{px}x{px}.jpg`); `Track.art` / `Card.art`
carry them. A view calls `ui::cover(out, cx, rect, seed, art, px, radius)` with the size it needs
(160 liked list, 320 home cards, 640 player). If the texture is on the GPU it draws it (`Cmd::Image`); otherwise it draws
the old placeholder and, the first time, asks for the image (`DrawList::want`).
`tide-sdl` drains those requests into `tide-img` (2 worker threads: HTTP + JPEG decode to RGB8), uploads up to 2 finished
images per frame (`Renderer::upload_image`), and logs failures (`image 0x... failed: ...`) in `tide.log`.
The renderer keeps at most 96 textures (least recently used evicted). Each image is its own draw call
(the HUD's "calls" number goes up with visible covers); an atlas would batch them later.
Unverified: that the image CDN serves 160/320/640 for every cover; check `image ... failed` lines.

## Quality badge (player screen top left; the same one in the mini bar)
* One chip, showing what the decoder received: codec, bits (when the decoder knows them) and sample rate —
  e.g. `FLAC · 24-bit / 96 kHz`. No tier name is shown, like the official client.
* The chip's colour is the tier (`tide_core::quality::Quality::of_stream`): grey = Low, cyan = High, white =
  Lossless, amber = Max/HiRes. The official client merges Lossless and HiRes into MAX; here they stay separate.
* The device format is no longer drawn under the artist. It is logged instead:
  `audio: device opened at <rate> Hz, <ch> ch (asked ...)`. SDL (and the TV's mixer) may still convert to the
  hardware format afterwards; the app cannot see that. To see the real hardware format on the TV, over ssh during
  playback: `cat /proc/asound/card*/pcm*p/sub*/hw_params` (path not verified on webOS).
* Lossy codecs show no bit depth (none exists) and no bitrate (not measured yet).

## Also included in this drop (earlier requests)
Session persistence fixes (`paths.rs`, `diag.rs`, `tidlers_backend::boot/save`, TV data dir `/media/developer/tide-tv`),
Back-key fixes (400 ms guard in `App::on_key`, no key repeat for Back/OK/Play in `main.rs` and `remote.rs`).
