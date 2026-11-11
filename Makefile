# Local configuration, git-ignored: the TIDAL_CLIENT_ID/TIDAL_CLIENT_SECRET and this app's own
# TIDE_* variables. Neither Cargo nor a bare `cargo run` reads `.env`; these targets do, so the file
# is enough for `make sim`. Plain `KEY=value` lines, no quotes (make keeps them).
-include .env
# Only these reach the program: an included file must not export everything make happens to know.
export TIDAL_CLIENT_ID TIDAL_CLIENT_SECRET TIDAL_QUALITY TIDE_RELAY_BASE TIDE_DATA_DIR

# Desktop simulator (needs libsdl2-dev + a GLES2-capable driver, e.g. Mesa). Always on `tidlers`.
sim:            ## run the simulator (keyboard = remote, F3 = fps HUD)
	cargo run --release -p tide-sdl -- --hud
sim-4k:         ## render at 3840x2160 to compare sharpness / cost
	cargo run --release -p tide-sdl -- --size=3840x2160 --hud --no-vsync
relay-test:     ## relay unit tests (host; no wasm tooling needed)
	cargo test --manifest-path crates/tide-relay/Cargo.toml
relay-check:    ## type-check the relay for the Workers runtime
	cargo check --manifest-path crates/tide-relay/Cargo.toml --target wasm32-unknown-unknown
check:          ## tests + lints (headless, no GPU needed for tide-core)
	cargo test --workspace
	cargo clippy --workspace --all-targets
shots:          ## headless screenshot of the empty Home layout (needs xvfb)
	mkdir -p shots
	LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a -s "-screen 0 1920x1080x24" cargo run -q -p tide-sdl -- --screen=home --screenshot=shots/home.png
.PHONY: sim sim-4k check shots relay-test relay-check
# TODO(M4): `tv` / `ipk` targets using the webOS NDK. See ARCHITECTURE.md §6.
