# Desktop simulator (needs libsdl2-dev + a GLES2-capable driver, e.g. Mesa).
sim:            ## run the simulator (keyboard = remote, F3 = fps HUD)
	cargo run --release -p tide-sdl -- --screen=home --hud
sim-login:      ## start at the login screen
	cargo run --release -p tide-sdl -- --screen=login
sim-tidlers:    ## POC backend on the tidlers crate (see docs/tidlers-poc.md)
	cargo run --release -p tide-sdl --features tidlers -- --screen=login --backend=tidlers
sim-4k:         ## render at 3840x2160 to compare sharpness / cost
	cargo run --release -p tide-sdl -- --screen=home --size=3840x2160 --hud --no-vsync
check:          ## tests + lints (headless, no GPU needed for tide-core)
	cargo test --workspace
	cargo clippy --workspace --all-targets
shots:          ## headless screenshots into ./shots (needs xvfb)
	mkdir -p shots
	LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a -s "-screen 0 1920x1080x24" cargo run -q -p tide-sdl -- --screen=home --screenshot=shots/home.png
	LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a -s "-screen 0 1920x1080x24" cargo run -q -p tide-sdl -- --screen=home --keys=right,ok --screenshot=shots/player.png
.PHONY: sim sim-login sim-tidlers sim-4k check shots
# TODO(M4): `tv` / `ipk` targets using the webOS NDK. See ARCHITECTURE.md §6.
