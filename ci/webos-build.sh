#!/usr/bin/env bash
# Cross-build tide-tv for webOS (ARMv7) and package an .ipk.
#
#   WEBOS_SDK   path to arm-webos-linux-gnueabi_sdk-buildroot (default ~/webos-ndk/...; see ci/setup-ndk.sh)
#   OUT         output .ipk (default dist/tide-tv.ipk)
#
# Steps: (1) build SDL-webOS from source (bundled with the app, so the SDL ABI is ours, not the
# firmware's), (2) cargo cross-build against it, (3) package. Nothing here was run on a TV.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
SDK=${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}
TARGET=armv7-unknown-linux-gnueabi
SDL_REV=${SDL_REV:-ba4c86af391d1524d3ff0cb61dc3b10035933239}   # webosbrew/SDL-webOS (SDL 2.30.12 + webOS patches)
BUILD=$ROOT/build/webos
mkdir -p "$BUILD" "$ROOT/dist"

[ -x "$SDK/bin/arm-webos-linux-gnueabi-gcc" ] || { echo "NDK not found at $SDK (run ci/setup-ndk.sh)"; exit 1; }

# ---- 1. SDL ---------------------------------------------------------------
if [ ! -f "$BUILD/sdl-dist/lib/libSDL2.so" ]; then
  [ -d "$BUILD/SDL-webOS" ] || git clone -q https://github.com/webosbrew/SDL-webOS.git "$BUILD/SDL-webOS"
  git -C "$BUILD/SDL-webOS" checkout -q "$SDL_REV"
  cmake -B "$BUILD/sdl" -S "$BUILD/SDL-webOS" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE="$SDK/share/buildroot/toolchainfile.cmake" \
    -DCMAKE_BUILD_TYPE=Release -DWEBOS=ON -DSDL_WEBOS_BROKEN_ABI=OFF \
    -DSDL_OFFSCREEN=OFF -DSDL_DUMMYVIDEO=OFF -DSDL_KMSDRM=OFF -DSDL_STATIC=OFF -DSDL_TEST=OFF
  cmake --build "$BUILD/sdl" --parallel
  cmake --install "$BUILD/sdl" --prefix "$BUILD/sdl-dist"
fi
SDL_LIB=$(ls "$BUILD"/sdl-dist/lib/libSDL2-2.0.so.0.* | head -1)
echo "SDL: $SDL_LIB"

# ---- 2. Rust --------------------------------------------------------------
GCC="$SDK/bin/arm-webos-linux-gnueabi-gcc"
export CC_armv7_unknown_linux_gnueabi="$GCC"
export AR_armv7_unknown_linux_gnueabi="$SDK/bin/arm-webos-linux-gnueabi-ar"
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_LINKER="$GCC"
# glibc 2.12 in the sysroot has no getauxval (std and ring need it): link a weak fallback.
"$GCC" -c -O2 -fPIC ci/shims/getauxval.c -o "$BUILD/getauxval.o"
export RUSTFLAGS="${RUSTFLAGS:-} -L native=$BUILD/sdl-dist/lib -C link-arg=-Wl,-rpath,\$ORIGIN/lib -C link-arg=$BUILD/getauxval.o"
# Prebuilt std is not available for every host: BUILD_STD=1 uses -Z build-std (needs rust-src).
EXTRA=()
if [ "${BUILD_STD:-0}" = 1 ]; then export RUSTC_BOOTSTRAP=1; EXTRA=(-Z build-std=std,panic_abort); fi
cargo build --release --target "$TARGET" -p tide-sdl "${EXTRA[@]}"
BIN=$ROOT/target/$TARGET/release/tide-tv
"$SDK/bin/arm-webos-linux-gnueabi-strip" -o "$BUILD/tide-tv.stripped" "$BIN"

# ---- 3. Package -----------------------------------------------------------
OUT=${OUT:-$ROOT/dist/tide-tv.ipk}
"$SDK/bin/arm-webos-linux-gnueabi-strip" -o "$BUILD/libSDL2-2.0.so.0" "$SDL_LIB"
python3 packaging/mkipk.py --binary "$BUILD/tide-tv.stripped" --lib "$BUILD/libSDL2-2.0.so.0" --out "$OUT" --icon "./packaging/icon.png" --large-icon "./packaging/largeIcon.png"
echo "done: $OUT"
