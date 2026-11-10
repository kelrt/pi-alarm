# Hardware bring-up

Test in this order. Each step removes one unknown, so when something does not
work you know which part to look at.

1. Replay the captures **from the Flipper**. Proves the captures are good.
2. Check the **CC1101 wiring**. Proves the Pi can talk to the chip.
3. Send one action **from the Pi**. Proves the whole transmit chain.
4. Compare the Pi's signal to the original **using the Flipper as a
   receiver**. Only needed if step 3 fails.

## What the captures contain

All three files are RAW recordings at **304.25 MHz**, preset
`FuriHalSubGhzPresetOok270Async` (on/off keying). Inside the noise, each
holds the same kind of frame, repeated with a pause in between:

| | |
|---|---|
| Frame | 1 start pulse, then 24 bits. 49 on/off timings, about 28 ms |
| One bit | an off-time then an on-time, about 1.15 ms together |
| Bit value | 1 = short off (~0.37 ms) + long on (~0.8 ms); 0 = long off + short on |
| Pause between frames | about 10.3 ms |
| Repeats per button press | 4-5 in your recordings |

| File | Action | Code (24 bits) |
|---|---|---|
| `0_fan.sub` | fan off | `A2E8DF` |
| `3_fan.sub` | fan speed 3 | `A2E83F` |
| `Lite.sub` | light toggle | `A2E85F` |

The first 16 bits (`A2E8`) are the same in all three: that is your remote's
address. Only the last byte, the command, changes. The code never changes
between frames or between files, so this is a **fixed code** and replaying
it works; a rolling-code remote would not allow that.

`pi-alarm inspect <file.sub>` prints this for any capture.

## Step 1: replay from the Flipper

On the Flipper: **Sub-GHz → Saved →** pick `3_fan` **→ Send**, standing where
the Pi will live.

- The fan should go to speed 3. Then try `Lite` (light toggles) and `0_fan`
  (fan off).
- If the Flipper replay works, the captures and the fan's receiver are fine,
  and everything from here on is about the Pi.
- If it does **not** work from the Flipper, stop and re-capture: **Read RAW**
  at 304.25 MHz, AM270, hold the remote button for about a second, a hand's
  width from the Flipper. Then run `pi-alarm inspect` on the new file; it
  should report "found N clean copies" with N of 3 or more and the same code
  as above.
- If the Flipper refuses to transmit on this frequency, that is its regional
  setting and not a problem with the file.

## Step 2: check the wiring

Wire the module (WIRING.md), enable SPI, then:

```sh
pi-alarm check --radio
```

Expected: `CC1101 found: part 0x00, version 0x14`. WIRING.md has a table for
when it reports something else.

## Step 3: the one-button test

Stop the service first if it is running, so only one program uses the radio:

```sh
sudo systemctl stop pi-alarm      # skip if not installed yet
pi-alarm send light_toggle
```

The light should toggle. Run it again to toggle it back. Then:

```sh
pi-alarm send fan_3
pi-alarm send fan_off
```

`light_toggle` is the best first test because you see the result at once.

### If nothing happens

Work down this list; the likely causes come first.

1. **Antenna.** No antenna, or the stock 433 MHz coil, gives very little
   range at 304 MHz. See "Antenna" below. For a first test, hold the Pi
   within a metre of the fan.
2. **Timing.** Look at the program's output. If it warns
   `real-time priority not available`, the pulses may be ragged. Run the
   test as root once (`sudo pi-alarm send light_toggle`) to rule that out.
   The installed service always has the priority.
3. **More repeats.** Set `repeats = 16` under `[radio]` in the config.
4. **Compare signals** with step 4 below.

## Step 4: use the Flipper as a receiver

The Flipper can tell you what the Pi actually transmits.

- **Is anything on air?** Flipper: **Sub-GHz → Frequency Analyzer**. Run
  `pi-alarm send fan_3` with the antennas a few centimetres apart. It should
  show about **304.25 MHz**. Nothing at all means no carrier: check the
  antenna connection, power, and that `check --radio` still passes.
  A clearly different frequency (for example ~292 or ~316 MHz) suggests a
  module with a crystal that is not 26 MHz; set `crystal_hz` to the value
  printed on the crystal can.
- **Is it the right signal?** Flipper: **Read RAW** at 304.25 MHz, AM270.
  Start recording, run `pi-alarm send fan_3`, stop, save, copy the file to
  the computer and run:

  ```sh
  pi-alarm inspect pi_fan3.sub
  ```

  It should report a 49-pulse frame with code `A2E83F`, the same as the
  original. If the code matches, the Pi is sending the right thing and the
  problem is range (antenna, power). If no frame is found or the code
  differs, the timing is being disturbed; see "Timing" above.

## Antenna (304.25 MHz)

The wavelength at 304.25 MHz is 98.5 cm. The simple, good antenna is a
**quarter-wave wire: about 23.5 cm of straight wire** soldered to the
module's ANT pad (or screwed into its SMA socket), pointing straight away
from the board. That is 24.6 cm in theory, shortened about 5 % because the
wire is not infinitely thin and ends in free air.

- **Replace the antenna that came with the module.** Nearly all CC1101
  boards are sold for 433 MHz and ship with a short coil or a 17 cm whip
  tuned to that band. At 304 MHz it works poorly.
- Any solid wire will do. Length matters more than thickness. Cut it a
  little long (25 cm) and trim if you want to experiment; a centimetre either
  way makes little difference at room range.
- Keep it straight and away from the Pi's metal and from other wires.
  Vertical is a sensible default; if range is marginal, try horizontal, as
  the receiver antenna in the fan canopy may be either.
- **The module itself is tuned for 433 MHz too.** The small coils and
  capacitors between the chip and the antenna pad are chosen for one band.
  A "433 MHz" board still transmits at 304 MHz (the chip covers 300-348 MHz)
  but with reduced power. Across a bedroom that is normally enough. If range
  stays poor with a correct antenna, buy a module sold as **315 MHz**: its
  matching suits 304 MHz well.

## Power and rules

`pa_power = 0xC0` is the chip's maximum (roughly 10 mW at the chip, less
after a mismatched 433 MHz front end). If the fan responds reliably, you can
lower it (`0x50` is about a tenth of that) to be a better radio neighbour.
You are responsible for following your local rules for unlicensed
transmitters; in the US that is FCC Part 15. Each transmission here lasts
about 0.3 s, the same kind of signal the original remote sends.

## About timing jitter on the Pi

The Pi Zero is a single slow core running Linux, which is not a real-time
system, and each pulse here is only ~0.4 ms long. The transmitter does three
things about that (`src/cc1101.rs`, `play_pulses`):

- **Busy-waiting**: it spins reading the clock instead of sleeping, because
  a sleep can overshoot by a millisecond or more.
- **Absolute deadlines**: each edge is timed from the start of the burst, so
  one late edge does not delay the rest.
- **Real-time priority** (`SCHED_FIFO`) during the burst, so ordinary
  programs cannot interrupt it. This needs a privilege the systemd unit
  grants.

And the frame is sent 8 times; the fan only needs to hear one clean copy.

If you ever need better, in rising order of effort: raise `repeats`; stop
other busy services on the Pi; or move the pulse generation out of Linux
altogether (the Pi's SPI MOSI line, or a small microcontroller, can produce
the pulses with exact timing).
