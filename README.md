# pi-alarm

A bedroom wake-up alarm. A Raspberry Pi Zero W with a CC1101 radio replays a
ceiling fan/light remote's signals on a schedule; a phone app sets the alarms
over Bluetooth Low Energy.

**Start with [HANDOFF.md](HANDOFF.md).**

| Document | What it is for |
|---|---|
| [HANDOFF.md](HANDOFF.md) | Status, open questions, next steps |
| [WIRING.md](WIRING.md) | Connecting the CC1101 to the Pi (3.3 V only) |
| [HARDWARE.md](HARDWARE.md) | Bring-up order, antenna, troubleshooting |
| [BUILD.md](BUILD.md) | Building, cross-compiling, deploying, the phone app |
| [PROTOCOL.md](PROTOCOL.md) | The BLE messages between phone and Pi |
| [DECISIONS.md](DECISIONS.md) | Choices made along the way, with reasons |
| [LEARNING.md](LEARNING.md) | Rust concepts per module, and features to build yourself |

## Layout

```
src/              the daemon and CLI (Rust)
tests/            end-to-end tests of the CLI and of PROTOCOL.md's examples
flipper_files/    the three original Flipper captures (test fixtures; do not edit)
deploy/           example config and systemd unit for the Pi
scripts/          check.sh (lint + test), build-pi.sh (ARMv6 cross-compile)
app/              the Flutter phone app
pi-alarm.dev.toml config for trying things on a computer with no radio
```

## Try it in a minute (no hardware)

```sh
cargo test
cargo run -- --config pi-alarm.dev.toml check
cargo run -- --config pi-alarm.dev.toml send fan_3
cargo run -- inspect flipper_files/3_fan.sub
```
