#!/usr/bin/env bash
# Cross-compile the daemon for the Raspberry Pi Zero W (ARMv6, hard-float).
# One-time setup is described in BUILD.md ("Cross-compiling for the Pi").
#
#   ./scripts/build-pi.sh            normal build, uses the Pi's own C library
#   ./scripts/build-pi.sh --static   fully static build, needs nothing on the Pi
set -euo pipefail
cd "$(dirname "$0")/.."

# cargo-zigbuild and zig live in a Python virtual environment in .tools/.
if [ ! -x .tools/venv/bin/cargo-zigbuild ]; then
    echo "cargo-zigbuild not found in .tools/venv. See BUILD.md for the setup." >&2
    exit 1
fi
export PATH="$PWD/.tools/venv/bin:$PATH"

# Both targets are ARMv6 + hard-float, which is what the Zero W needs.
# (armv7-unknown-linux-gnueabihf would build fine and then crash on the Pi
# with "Illegal instruction".)
if [ "${1:-}" = "--static" ]; then
    # musl: the C library is compiled into the binary.
    TARGET=arm-unknown-linux-musleabihf
    BUILD_TARGET=$TARGET
else
    # glibc: uses the C library installed on the Pi. The ".2.28" suffix makes
    # the binary need glibc 2.28 or newer, which covers Raspberry Pi OS from
    # Buster (2019) onwards.
    TARGET=arm-unknown-linux-gnueabihf
    BUILD_TARGET=$TARGET.2.28
fi

if ! rustup target list --installed | grep -qx "$TARGET"; then
    echo "Rust target $TARGET is not installed. Run: rustup target add $TARGET" >&2
    exit 1
fi

cargo zigbuild --release --target "$BUILD_TARGET"

BIN="target/$TARGET/release/pi-alarm"
echo
echo "Built $BIN"
file "$BIN"
# Proof that no ARMv7 code slipped in.
readelf -A "$BIN" | grep -E "Tag_CPU_arch:|Tag_FP_arch:" || true
