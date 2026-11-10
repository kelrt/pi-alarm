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
