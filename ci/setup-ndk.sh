#!/usr/bin/env bash
# Download the x86_64 Linux webOS NDK (buildroot SDK from openlgtv/buildroot-nc4) and relocate it.
# ~310 MB download, ~1.5 GB on disk. Same release SDL-webOS's own CI uses.
set -euo pipefail
DEST=${1:-$HOME/webos-ndk}
TAG=${NDK_TAG:-webos-b17b4cc}
mkdir -p "$DEST"
if [ ! -x "$DEST/arm-webos-linux-gnueabi_sdk-buildroot/bin/arm-webos-linux-gnueabi-gcc" ]; then
  curl -fL -o "$DEST/ndk.tar.gz" "https://github.com/openlgtv/buildroot-nc4/releases/download/$TAG/arm-webos-linux-gnueabi_sdk-buildroot.tar.gz"
  tar -xzf "$DEST/ndk.tar.gz" -C "$DEST"     # tar (not node-tar): keeps symlinks like libdl.so
  rm "$DEST/ndk.tar.gz"
  "$DEST/arm-webos-linux-gnueabi_sdk-buildroot/relocate-sdk.sh"
fi
echo "WEBOS_SDK=$DEST/arm-webos-linux-gnueabi_sdk-buildroot"
