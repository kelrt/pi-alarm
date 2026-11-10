# Decisions

Choices made while you were away, where the brief was open or something had to
give. Each has the reason and what to change if you disagree.

## Signal handling

### D1. Replay the clean repeating frame, not the whole RAW capture
The three `.sub` files are RAW recordings and are mostly noise: `0_fan.sub`
holds 2.8 s of receiver noise around 0.15 s of real signal. Replaying
everything would transmit noise for seconds. The daemon instead finds the
frame that repeats in the capture (`src/signal.rs`), takes the median timing
at each position across the clean copies, and transmits that frame N times
with the gap the remote used.

- The timings sent are the *recorded* ones (median of the copies). They are
  not rounded to an idealised 1:2 ratio, so what goes on air is as close as
  possible to what the Flipper would replay.
- If a future capture has no repeating frame (rolling code, single-shot
  remote), set `mode = "raw"` on that action to replay the capture verbatim.

### D2. 8 repeats per transmission by default
A short press of the real remote produced 4-5 frames in each capture. 8 gives
margin for a weaker transmitter and takes about 0.3 s on air. Change
`[radio] repeats`, or `repeats` on a single action.

### D3. Decoded-key `.sub` files are parsed but cannot be transmitted
The parser reads both RAW and decoded-key files, as asked. Transmitting a
decoded key needs a per-protocol encoder (Princeton, CAME, ...), and none of
your files use one. Loading a key file as an action gives a clear error
telling you to re-capture with **Read RAW**. Writing a Princeton encoder is
one of the suggested exercises in LEARNING.md.

### D4. Only the two OOK presets are supported
`FuriHalSubGhzPresetOok270Async` (your files) and `Ook650Async`. 2-FSK and
custom presets are rejected at load time with a clear error.

## Radio

### D5. CC1101 is configured exactly like the Flipper preset
The register table in `src/cc1101.rs` is copied from the Flipper firmware's
`subghz_device_cc1101_preset_ook_270khz_async_regs` (checked against the
firmware source during this session, not written from memory), and the
frequency word is computed with the same rounding. The transmit sequence also
follows the Flipper: idle, load preset, drive GDO0 low, enter TX, toggle GDO0,
back to idle.

### D6. GDO0 is high-impedance on the chip side whenever we are not transmitting
After a reset the CC1101 outputs a clock signal on GDO0. The driver switches
that off (IOCFG0 = 0x2E) immediately after reset and again after every
transmission, and the Pi only drives the line during a transmission. So the
two sides should never drive the wire against each other. WIRING.md still
suggests an optional series resistor as cheap insurance.

### D7. Timing: busy-wait with absolute deadlines, real-time priority if allowed
See the comment on `play_pulses` in `src/cc1101.rs`. Gaps longer than 3 ms
are mostly slept so the single-core Zero is not frozen for the whole burst.
Real-time priority (SCHED_FIFO) is requested per transmission and silently
skipped if the process lacks CAP_SYS_NICE; the systemd unit grants it.

### D8. A transmission is capped at 5 seconds
Protects against a huge capture in `mode = "raw"` freezing the Pi at
real-time priority. Constant `MAX_ON_AIR_MICROS` in `src/radio.rs`.

### D9. The example config uses the Pi's system time zone
I do not know your time zone, so `timezone` is commented out in the example
and the daemon falls back to the system zone. The phone app warns if the Pi's
UTC offset differs from the phone's (see PROTOCOL.md, `set_time`).

## Alarms and scheduling

### D10. Alarm shape
`id`, `label`, `hour`, `minute`, `days` (non-empty list of `mon`..`sun`),
`enabled`, `steps` (each `offset_s` in seconds relative to the alarm time,
plus an action name). Offsets are in seconds, not minutes, so "light on, fan
30 seconds later" is possible; the app's editor works in minutes. Limits: 20
alarms, 8 steps each, offsets within 6 hours either way.

### D11. Alarm ids are chosen by the client, and saving is "put"
The phone picks a random id and sends the whole alarm. Sending the same id
again replaces the alarm. That makes retries harmless, which the app's
offline queue depends on: a save that is sent twice has the same result as
one sent once.

### D12. No one-shot alarms
Your brief describes alarms as time + days of week, so `days` must not be
empty. A "tomorrow only" alarm is one of the suggested exercises in
LEARNING.md.

### D13. Days of week refer to the alarm time, not the step
A Monday 00:05 alarm with a step at -10 min fires that step on Sunday at
23:55. It is still Monday's alarm.

### D14. Daylight saving
- Spring forward: a time that does not exist that night (02:30) fires an hour
  later on the new clock (03:30), once.
- Fall back: a time that happens twice (01:30) fires at the first one only.
Tested for the US, UK, Lord Howe Island's 30-minute shift, and a full-year
"exactly once per day" check.

### D15. Late steps: 5 minutes of grace, then skipped
If a step could not run on time (daemon restarting, clock corrected), it
still runs when it is under `late_grace_secs` (default 300) late. Anything
older is skipped and logged as missed. This is what stops a clock that jumps
forward by hours from firing a backlog of alarms at once.

### D16. A step never runs twice for the same scheduled time
The scheduler keeps a small record of what it has handed out and saves it to
`scheduler-state.json`. That covers restarts, small backward clock steps (NTP)
and a power cut that resets the clock to an earlier time. The one deliberate
exception: if the clock is moved back by more than about 11 minutes *while the
daemon is running*, the earlier firing is treated as having happened at a
wrong time and the alarm runs again at the corrected time.

### D17. Alarms fire even when the clock has not been confirmed
After a power cut with no Wi-Fi and no phone connection, the Pi's clock is
stale (Raspberry Pi OS restores the last saved time, so it is typically
minutes to an hour behind). Alarms still fire by that clock, so they will be
late by however far it is behind. The alternative was not firing at all, and
late seemed better for a wake-up alarm. The status message reports
`synced: false` in that state so the app can show it.

### D18. Storage: JSON, atomic writes, a lock file
`alarms.json` is written to a temporary file, flushed, then renamed into
place, so a power cut cannot leave half a file. A lock file stops the daemon
and the CLI from overwriting each other's changes. A damaged file is reported
as an error and never overwritten.

## Protocol, BLE and service

### D19. JSON messages with a one-byte chunk header
JSON is easy to read in a BLE debugging app and trivial in Dart. A BLE write
or notification can be as small as 20 bytes, so every message is cut into
chunks with a one-byte header (first/last flags). See PROTOCOL.md.

### D20. Request/response over two characteristics
The phone writes request chunks to one characteristic and receives response
chunks as notifications on another. Every request gets exactly one response,
matched by an `id` the phone chooses.

### D21. The phone's time always wins when the difference is 2 s or more
`set_time` sets the system clock if it differs from the phone by at least
2 seconds, in either direction, even if NTP has synchronised the Pi. Simple
and predictable; the phone's clock is network-synchronised too. Smaller
differences are left alone.

### D22. No BLE pairing or encryption yet (please read)
Anyone within Bluetooth range who knows the protocol could trigger an action
or edit alarms. For a fan and a light in a bedroom I judged that acceptable
for a first version, and BLE bonding between Android and BlueZ is a common
source of connection trouble that I could not test here. "Remember the Pi"
is done in the app by storing the Pi's Bluetooth address. Adding bonding
later is a contained change (`encrypt_authenticated_write` flags in
`src/ble.rs` plus a pairing agent). This is one of the questions in
HANDOFF.md.

### D23. The time zone is the Pi's, checked against the phone
Alarm times are wall-clock times in the daemon's zone (config `timezone`, or
the system zone). The phone sends its UTC offset with `set_time`; the Pi
answers whether its own offset matches (`tz_match`) and the app shows a
warning if not. I did not let the phone *set* the Pi's zone: a bare offset
is not enough to handle daylight saving, and sending a zone name would need
another Flutter plugin.

### D24. The CLI works directly on the files, not through the daemon
`pi-alarm send`, `list`, `add`, ... do not need the daemon to be running,
which matters for the very first wiring test. The daemon notices changes to
`alarms.json` within a second. Lock files keep the two from clashing over
the store or the radio. `pi-alarm request '<json>'` runs one protocol
request through the same handler the phone uses.

### D25. The daemon keeps scheduling when Bluetooth is unavailable
BLE start-up errors are logged and retried every 10 seconds. A missing or
broken CC1101, on the other hand, stops the daemon at start-up with a clear
error, because an alarm clock that cannot transmit is not worth running
silently. Use `--mock-radio` or `transmitter = "mock"` to run without one.

### D26. systemd: root with almost everything taken away
The service needs SPI, GPIO, BlueZ over D-Bus, clock setting and real-time
priority. Rather than a dedicated user plus four separate permission setups
(each a way to fail that I cannot test without the Pi), it runs as root with
only CAP_SYS_TIME and CAP_SYS_NICE left and a read-only filesystem apart
from `/var/lib/pi-alarm`.

### D27. Scheduler state is saved before transmitting
If the daemon dies in the 0.3 s between deciding to run a step and finishing
the transmission, the step is lost rather than repeated on restart. A lost
`fan_3` is bad; a repeated `light_toggle` turns the light back off, and a
crash that repeats on every restart would spam the radio.

## Build and tooling

### D28. libdbus is compiled from source into the binary
`bluer` needs libdbus. The dev machine has no dbus headers and the brief
forbids installing packages, so the `vendored-dbus` cargo feature (on by
default) builds it from source. This also makes cross-compiling simpler: no
ARM copy of libdbus is needed.

### D29. Cross-compiling uses Zig, installed inside the project
No ARM cross-compiler is installed. `cargo-zigbuild` and Zig were installed
with pip into `.tools/venv` (git-ignored, inside the project). The one thing
outside the project directory is the Rust target itself:
`rustup target add arm-unknown-linux-gnueabihf` added files under
`~/.rustup`. I judged that to be part of "try the ARMv6 cross-compile"; undo
it with `rustup target remove arm-unknown-linux-gnueabihf`.

### D30. The alarm delete command is `pi-alarm delete`, not `rm`
Matches the protocol's `delete` op.

### D31. The BLE server was smoke-tested on this computer's Bluetooth adapter
To check that BlueZ accepts the GATT service, I ran the daemon here with the
mock radio and BLE enabled, twice, for about 10 seconds each. During that
time this PC advertised a BLE device named `pi-alarm`. The adapter was
already powered on; its power, pairing and discoverable settings were not
changed, and the registration was removed on exit (checked: active
advertising instances back to 0).

### D32. The BLE server re-registers if BlueZ resets the adapter
If `bluetoothd` restarts (a package update, a crash), the GATT service
registration disappears. The server watches for the adapter being removed or
re-added and registers again after 10 seconds. This path is written but
could not be exercised here, since restarting bluetoothd needs sudo.
