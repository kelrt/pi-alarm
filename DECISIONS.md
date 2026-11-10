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
