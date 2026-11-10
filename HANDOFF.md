# Handoff

Everything asked for is written and committed locally (nothing pushed). The
Pi side is tested as far as it can be without the Pi; the phone app is
written but has never been compiled. **Nothing has touched real hardware
yet.**

## What the .sub files contain

All three are **RAW** recordings (not decoded keys), **304.25 MHz**, preset
`FuriHalSubGhzPresetOok270Async` (on/off keying), `Protocol: RAW`.

Each is mostly noise with a short burst of real signal inside. The signal is
a **24-bit fixed code**: one start pulse, then 24 bits of about 1.15 ms
each, a frame of about 28 ms repeated with a 10 ms pause.

| File | Action | Code | Clean frames in the file |
|---|---|---|---|
| `0_fan.sub` | fan off | `A2E8DF` | 4 |
| `3_fan.sub` | fan speed 3 | `A2E83F` | 3 |
| `Lite.sub` | light toggle | `A2E85F` | 4 |

`A2E8` is your remote's address; only the last byte changes. Fixed code
means replaying it works. The daemon sends the clean frame 8 times instead
of replaying the noise. Details: HARDWARE.md. The files are unmodified and
are used as test fixtures.

## What works (run and checked here)

- **191 automated tests pass** on this PC, with clippy clean: `.sub` parser,
  frame extraction (against your three files), scheduler, DST, storage,
  protocol, BLE message handling, CLI.
- **The daemon ran for real** with the mock radio: an alarm set a minute
  ahead fired both steps on the exact second and shut down cleanly.
- **BLE service registers with BlueZ.** Checked on this PC's Bluetooth
  adapter (it briefly advertised as `pi-alarm`; see DECISIONS D31).
- **ARMv6 cross-compile works** for the target you named. See the caveat
  below.
- **The ARM code runs on an emulated Pi Zero CPU** (`qemu-arm-static -cpu
  arm1176`), with one caveat: the last fix was not re-run there. See
  "Emulator log" below.

## What is mocked or unproven

| Thing | State |
|---|---|
| CC1101 radio | Driver tested against a **simulated chip** only. No real chip, no real transmission. |
| Phone ↔ Pi over BLE | **No phone has ever connected.** Only service registration is proven. |
| Flutter app | **Never compiled or run** (no SDK). Written against flutter_blue_plus 2.3.15's source. Expect some compile errors. |
| The `gnueabihf` binary | Built and inspected (ARMv6, hard-float), **never executed**: this PC has no ARM glibc. A static build of the same code is what ran under emulation. |
| Setting the clock | Logic tested with a fake clock. The real system call has never run (needs root). |
| systemd unit | Written, never loaded. |
| Recovery after BlueZ restarts | Written, could not be triggered here. |

## Emulator log (stopped early at your request)

Runs of a static ARMv6 build under `qemu-arm-static -cpu arm1176`, in order:

| Run | Code state | Result |
|---|---|---|
| CLI: `--version`, `inspect`, `check`, `send` (mock) | commit `6aa84c3` + portable libc fix | all correct, same output as on the PC |
| Full test suite | same | **182 of 182 passed** |
| Daemon with BLE against this PC's BlueZ | same | service registered, clean shutdown |
| Library tests, 3 runs | after the BLE refactor (170 tests) | 2 runs passed 170/170; **1 run failed 1 test** |
| CLI tests | after the BLE refactor | **not re-run** (my loop picked the wrong file) |
| Anything | after the timing fix below | **not run** |

The one failure: `cc1101::tests::pulses_are_never_cut_short`, "burst took
8.595268ms, requested 9000 us". Cause: the driver started its clock before
writing the first edge, and under emulation that first write was slow, so
the first pulse came out about 0.4 ms short. That can happen on a real Pi
too. Fix, in the final commit: the clock starts once the first edge is
written, and the calls are made once as a warm-up first. Two new tests cover
it and pass on the PC. The fix has **not** been run under the emulator.

**The ARM binaries in `target/` are older than the final code.** Rebuild
with `./scripts/build-pi.sh` before copying anything to the Pi.

## Key decisions

All 35 are in [DECISIONS.md](DECISIONS.md). The ones most worth your eye:

- **D1** send the clean frame, not the raw capture.
- **D15-D17** late alarms: up to 5 min late still fires, older is skipped; a
  step never runs twice; alarms fire even if the clock is unconfirmed.
- **D21** the phone's time wins whenever it differs by 2 s or more.
- **D22** no BLE pairing or encryption yet.
- **D26** the service runs as root with nearly all privileges removed.
- **D29, D34** two Rust targets were added under `~/.rustup`; the build
  tools live in `.tools/` inside the project.

## Questions for you

1. **Does replaying the captures from the Flipper work** from where the Pi
   will sit? (HARDWARE.md step 1. Everything else assumes yes.)
2. **Which CC1101 module do you have**, one sold for 433 MHz or for 315 MHz?
   A 433 MHz board works at 304 MHz with reduced range and needs a 23.5 cm
   wire antenna instead of its coil.
3. **What time zone should alarms use?** The daemon uses the Pi's system
   zone unless you set `timezone` in the config. This PC is on
   America/New_York; I did not assume the Pi is.
4. **Is "no BLE pairing" acceptable for now?** Anyone in Bluetooth range who
   knew the protocol could toggle your light. Say the word and bonding is
   the next change.
5. **After a power cut with no Wi-Fi and no phone**, the Pi's clock is
   stale, so alarms fire late by however far it is behind. Would you prefer
   that (current), or no alarm at all until the clock is confirmed?
6. **Do you want one-shot ("tomorrow only") alarms?** Left out because the
   brief described alarms as time + days of week. It is exercise 4 in
   LEARNING.md if you want to build it.

## Next steps, in order

1. **Wire the CC1101 and do the one-button test.**
   - Read the 3.3 V warning in [WIRING.md](WIRING.md), wire the seven
     connections, enable SPI.
   - Build and copy the binary ([BUILD.md](BUILD.md) sections 2-3).
   - `pi-alarm check --radio` should report `CC1101 found`.
   - `sudo pi-alarm send light_toggle` should toggle the light.
   - If not, [HARDWARE.md](HARDWARE.md) step 4 uses the Flipper to see what
     the Pi is really sending.
2. **Install the service** (BUILD.md section 3) and watch
   `journalctl -u pi-alarm -f`. Set an alarm from the CLI for two minutes
   ahead and see it fire.
3. **Build the phone app** (BUILD.md section 4): install Flutter, run
   `flutter create` in `app/`, add the manifest permissions, fix whatever
   the compiler finds, `flutter test`, then **Find Pi** in the app.
4. **Capture fan speeds 1 and 2** and add them to the config (LEARNING.md
   exercise 1; no code needed).
5. **Decide on BLE pairing** (question 4).

## Where things are

- [README.md](README.md) has the document index and the directory layout.
- Try it now, no hardware: `cargo run -- --config pi-alarm.dev.toml check`
- Before committing: `./scripts/check.sh`
- Pi binary: `./scripts/build-pi.sh` (add `--static` for the fallback build)
