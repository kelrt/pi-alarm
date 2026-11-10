#!/usr/bin/env bash
# Cross-compile the daemon for the Raspberry Pi Zero W (ARMv6, hard-float).
# One-time setup is described in BUILD.md ("Cross-compiling for the Pi").
set -euo pipefail
cd "$(dirname "$0")/.."

# cargo-zigbuild and zig live in a Python virtual environment in .tools/.
if [ ! -x .tools/venv/bin/cargo-zigbuild ]; then
    echo "cargo-zigbuild not found in .tools/venv. See BUILD.md for the setup." >&2
    exit 1
fi
export PATH="$PWD/.tools/venv/bin:$PATH"

# arm-unknown-linux-gnueabihf is ARMv6 + hard-float: right for the Zero W.
# (armv7-unknown-linux-gnueabihf would build fine and then crash on the Pi
# with "Illegal instruction".)
# The ".2.28" suffix makes the binary need glibc 2.28 or newer, which covers
# Raspberry Pi OS from Buster (2019) onwards.
TARGET=arm-unknown-linux-gnueabihf
cargo zigbuild --release --target "$TARGET.2.28"

BIN="target/$TARGET/release/pi-alarm"
echo
echo "Built $BIN"
file "$BIN"
# Proof that no ARMv7 code slipped in.
readelf -A "$BIN" | grep -E "Tag_CPU_arch:|Tag_FP_arch:" || true
