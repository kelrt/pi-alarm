//! Turning recorded timings into something worth transmitting.
//!
//! A Flipper RAW capture is *everything* the receiver heard: radio noise, then
//! the remote's message repeated a few times, then more noise. Replaying all
//! of it would mostly transmit noise. So this module finds the part that
//! repeats (one "frame") and throws the rest away.
//!
//! How the search works (`extract_frame`):
//!
//! 1. Cut the recording into segments at every long pause. A remote leaves a
//!    pause of several milliseconds between repeats of its message.
//! 2. Keep the segments that look like a frame: they start and end with the
//!    carrier on, and are long enough to hold some bits.
//! 3. Group segments that have the same length and nearly the same timings.
//!    Noise never repeats itself, the remote's frame does, so the biggest
//!    group is the signal.
//! 4. Use the median timing at every position. This averages away the jitter
//!    of the individual copies.

use std::fmt;

/// One stretch of time with the transmitter either on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pulse {
    /// `true` = carrier on (the Flipper writes this as a positive number).
    pub high: bool,
    /// How long this level lasts, in microseconds.
    pub micros: u32,
}

impl Pulse {
    pub fn high(micros: u32) -> Pulse {
        Pulse { high: true, micros }
    }

    pub fn low(micros: u32) -> Pulse {
        Pulse {
            high: false,
            micros,
        }
    }
}

/// Convert Flipper timings (positive = on, negative = off) into pulses.
///
/// Neighbouring values with the same sign are merged into one pulse, so the
/// result always alternates between on and off.
pub fn pulses_from_raw(raw: &[i32]) -> Vec<Pulse> {
    let mut pulses: Vec<Pulse> = Vec::with_capacity(raw.len());
    for &duration in raw {
        if duration == 0 {
            continue;
        }
        let high = duration > 0;
        let micros = duration.unsigned_abs();
        // `last_mut` gives a mutable reference to the last element, if any.
        match pulses.last_mut() {
            Some(previous) if previous.high == high => {
                previous.micros = previous.micros.saturating_add(micros);
            }
            _ => pulses.push(Pulse { high, micros }),
        }
    }
    pulses
}

/// Total length of a pulse list in microseconds.
pub fn total_micros(pulses: &[Pulse]) -> u64 {
    pulses.iter().map(|pulse| u64::from(pulse.micros)).sum()
}

/// Settings for `extract_frame`. `Default` gives values that suit ordinary
/// fixed-code remotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractOptions {
    /// A pulse at least this long separates two frames.
    pub gap_min_us: u32,
    /// Segments with fewer pulses than this are ignored.
    pub min_frame_pulses: usize,
    /// Two segments are "the same frame" if every timing differs by at most
    /// this many percent.
    pub tolerance_percent: u32,
    /// How many matching copies we need before we believe it is a signal.
    pub min_copies: usize,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            gap_min_us: 3_000,
            min_frame_pulses: 16,
            tolerance_percent: 35,
            min_copies: 2,
        }
    }
}

/// The repeating message found in a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The frame's timings. Starts and ends with the carrier on.
    pub pulses: Vec<Pulse>,
    /// The silence the remote leaves between two copies, in microseconds.
    pub gap_us: u32,
    /// How many clean copies of the frame the capture contained.
    pub copies_found: usize,
}

/// Why no frame could be extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    /// The capture has no segment that looks like a frame at all.
    NoCandidates,
    /// There were candidates, but nothing repeated often enough.
    NoRepeat {
        candidates: usize,
        best_copies: usize,
        needed: usize,
    },
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExtractError::NoCandidates => {
                write!(f, "no frame-like section found in the capture")
            }
            ExtractError::NoRepeat {
                candidates,
                best_copies,
                needed,
            } => write!(
                f,
                "no repeating frame found: {candidates} candidate section(s), \
                 the best one appears {best_copies} time(s), need {needed}"
            ),
        }
    }
}

impl std::error::Error for ExtractError {}

/// A piece of the capture between two long pauses.
struct Segment {
    pulses: Vec<Pulse>,
    /// The long pause that ended this segment (`None` at the end of the file).
    separator: Option<Pulse>,
}

/// Find the repeating frame in a RAW capture. See the module comment.
pub fn extract_frame(raw: &[i32], options: &ExtractOptions) -> Result<Frame, ExtractError> {
    let pulses = pulses_from_raw(raw);
    let segments = split_at_gaps(&pulses, options.gap_min_us);

    // Step 2: keep only segments that could be a frame.
    let candidates: Vec<&Segment> = segments
        .iter()
        .filter(|segment| looks_like_a_frame(segment, options))
        .collect();
    if candidates.is_empty() {
        return Err(ExtractError::NoCandidates);
    }

    // Step 3: group similar candidates. Each group is a list of indexes into
    // `candidates`; a candidate joins the first group whose first member it
    // resembles, or starts a new group.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let mut joined = false;
        for group in groups.iter_mut() {
            let reference = candidates[group[0]];
            if similar(
                &reference.pulses,
                &candidate.pulses,
                options.tolerance_percent,
            ) {
                group.push(index);
                joined = true;
                break;
            }
        }
        if !joined {
            groups.push(vec![index]);
        }
    }

    // The biggest group wins. On a tie we keep the earliest one, which is why
    // this is a plain loop with `>` rather than `max_by_key` (that returns
    // the *last* maximum).
    let mut best = &groups[0];
    for group in &groups {
        if group.len() > best.len() {
            best = group;
        }
    }
    if best.len() < options.min_copies {
        return Err(ExtractError::NoRepeat {
            candidates: candidates.len(),
            best_copies: best.len(),
            needed: options.min_copies,
        });
    }

    // Step 4: the median timing at every position.
    let frame_length = candidates[best[0]].pulses.len();
    let mut frame_pulses = Vec::with_capacity(frame_length);
    for position in 0..frame_length {
        let mut values: Vec<u32> = best
            .iter()
            .map(|&index| candidates[index].pulses[position].micros)
            .collect();
        frame_pulses.push(Pulse {
            high: candidates[best[0]].pulses[position].high,
            micros: median(&mut values),
        });
    }

    // The gap between copies: the pauses that followed our frames. The last
    // copy is usually followed by a very long silence (the button was
    // released), so ignore pauses much longer than the shortest one.
    let mut gaps: Vec<u32> = best
        .iter()
        .filter_map(|&index| candidates[index].separator)
        .map(|separator| separator.micros)
        .collect();
    let shortest = gaps.iter().copied().min().unwrap_or(options.gap_min_us);
    gaps.retain(|&gap| gap <= shortest.saturating_mul(3));
    let gap_us = if gaps.is_empty() {
        options.gap_min_us
    } else {
        median(&mut gaps)
    };

    Ok(Frame {
        pulses: frame_pulses,
        gap_us,
        copies_found: best.len(),
    })
}

/// Step 1: cut the pulse list at every pulse of `gap_min_us` or longer.
fn split_at_gaps(pulses: &[Pulse], gap_min_us: u32) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current: Vec<Pulse> = Vec::new();
    for &pulse in pulses {
        if pulse.micros >= gap_min_us {
            if !current.is_empty() {
                segments.push(Segment {
                    // `std::mem::take` moves the Vec out and leaves an empty
                    // one behind, so we can keep using `current`.
                    pulses: std::mem::take(&mut current),
                    separator: Some(pulse),
                });
            }
        } else {
            current.push(pulse);
        }
    }
    if !current.is_empty() {
        segments.push(Segment {
            pulses: current,
            separator: None,
        });
    }
    segments
}

fn looks_like_a_frame(segment: &Segment, options: &ExtractOptions) -> bool {
    let pulses = &segment.pulses;
    if pulses.len() < options.min_frame_pulses {
        return false;
    }
    let starts_high = pulses[0].high;
    let ends_high = pulses[pulses.len() - 1].high;
    // A frame is followed by silence (carrier off), or by the end of the file.
    let followed_by_silence = match segment.separator {
        Some(separator) => !separator.high,
        None => true,
    };
    starts_high && ends_high && followed_by_silence
}

/// True if both pulse lists have the same shape and every timing is within
/// `tolerance_percent` of its partner.
fn similar(a: &[Pulse], b: &[Pulse], tolerance_percent: u32) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // `zip` pairs up the elements of the two lists.
    a.iter().zip(b.iter()).all(|(x, y)| {
        if x.high != y.high {
            return false;
        }
        let longer = u64::from(x.micros.max(y.micros));
        let difference = u64::from(x.micros.abs_diff(y.micros));
        difference * 100 <= longer * u64::from(tolerance_percent)
    })
}

/// Median of a list. Sorts the list in place, which is why it takes `&mut`.
/// For an even count this returns the lower of the two middle values.
fn median(values: &mut [u32]) -> u32 {
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}

/// Best-effort decoding of a frame into bits, for humans comparing captures.
///
/// It assumes the common "start pulse, then one off/on pair per bit" layout:
/// a bit is `1` when the carrier-on part is longer than the pause before it.
/// Returns `None` if the frame does not have that shape. This is only used
/// for display; transmission always replays the timings themselves.
pub fn decode_bits(frame: &[Pulse]) -> Option<String> {
    // Need a start pulse plus at least one (off, on) pair.
    if frame.len() < 3 || frame.len().is_multiple_of(2) || !frame[0].high {
        return None;
    }
    let mut bits = String::new();
    // `chunks(2)` walks the rest of the frame two pulses at a time.
    for pair in frame[1..].chunks(2) {
        let (off, on) = (pair[0], pair[1]);
        if off.high || !on.high {
            return None;
        }
        bits.push(if on.micros > off.micros { '1' } else { '0' });
    }
    Some(bits)
}

/// Write a bit string such as `"10100010"` as hex (`"A2"`).
/// Returns `None` unless the length is a multiple of 4.
pub fn bits_to_hex(bits: &str) -> Option<String> {
    if bits.is_empty() || !bits.len().is_multiple_of(4) {
        return None;
    }
    let mut hex = String::new();
    for nibble in bits.as_bytes().chunks(4) {
        let mut value = 0u32;
        for &bit in nibble {
            value = (value << 1) | u32::from(bit == b'1');
        }
        // `from_digit(value, 16)` turns 0..=15 into '0'..='f'.
        hex.push(char::from_digit(value, 16)?.to_ascii_uppercase());
    }
    Some(hex)
}

/// Build the full on-air pulse list: the frame `repeats` times, each copy
/// followed by `gap_us` of silence.
pub fn repeat_frame(frame: &[Pulse], gap_us: u32, repeats: u32) -> Vec<Pulse> {
    let mut pulses = Vec::with_capacity((frame.len() + 1) * repeats as usize);
    for _ in 0..repeats {
        pulses.extend_from_slice(frame);
        pulses.push(Pulse::low(gap_us));
    }
    pulses
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subfile::SubFile;

    const FAN_OFF: &str = include_str!("../flipper_files/0_fan.sub");
    const FAN_3: &str = include_str!("../flipper_files/3_fan.sub");
    const LIGHT: &str = include_str!("../flipper_files/Lite.sub");

    fn extract(text: &str) -> Frame {
        let sub = SubFile::parse(text).unwrap();
        extract_frame(sub.raw_timings().unwrap(), &ExtractOptions::default()).unwrap()
    }

    #[test]
    fn merges_neighbours_with_the_same_sign() {
        let pulses = pulses_from_raw(&[100, 50, -200, -25, 300]);
        assert_eq!(
            pulses,
            vec![Pulse::high(150), Pulse::low(225), Pulse::high(300)]
        );
        assert_eq!(total_micros(&pulses), 675);
    }

    #[test]
    fn real_captures_contain_a_49_pulse_frame() {
        // 1 start pulse + 24 bits x 2 pulses = 49.
        for (name, text, copies) in [
            ("0_fan", FAN_OFF, 4),
            ("3_fan", FAN_3, 3),
            ("Lite", LIGHT, 4),
        ] {
            let frame = extract(text);
            assert_eq!(frame.pulses.len(), 49, "{name}");
            assert_eq!(frame.copies_found, copies, "{name}");
            assert!(frame.pulses[0].high, "{name}");
            assert!(frame.pulses[48].high, "{name}");
        }
    }

    #[test]
    fn real_captures_decode_to_the_expected_codes() {
        // All three share the remote's address (A2E8); only the last byte,
        // the command, differs.
        let expected = [(FAN_OFF, "A2E8DF"), (FAN_3, "A2E83F"), (LIGHT, "A2E85F")];
        for (text, code) in expected {
            let frame = extract(text);
            let bits = decode_bits(&frame.pulses).unwrap();
            assert_eq!(bits.len(), 24);
            assert_eq!(bits_to_hex(&bits).unwrap(), code);
        }
    }

    #[test]
    fn real_captures_have_a_ten_millisecond_gap() {
        for text in [FAN_OFF, FAN_3, LIGHT] {
            let frame = extract(text);
            assert!(
                (9_500..=11_000).contains(&frame.gap_us),
                "gap was {} us",
                frame.gap_us
            );
        }
    }

    #[test]
    fn real_frame_timings_are_short_or_long() {
        // Every pulse is one or two "units" of roughly 380 us. The receiver
        // stretches on-pulses a little, hence the generous ranges.
        for text in [FAN_OFF, FAN_3, LIGHT] {
            let frame = extract(text);
            for pulse in &frame.pulses {
                let short = (280..=520).contains(&pulse.micros);
                let long = (650..=950).contains(&pulse.micros);
                assert!(short || long, "unexpected pulse {pulse:?}");
            }
            // One whole frame lasts about 28 ms.
            let length = total_micros(&frame.pulses);
            assert!((26_000..=31_000).contains(&length), "frame is {length} us");
        }
    }

    #[test]
    fn extraction_survives_noise_around_a_synthetic_signal() {
        // Build a fake capture: noise, three copies of a frame, noise.
        // Start pulse, then bits 1,0,1,0,0,0,1,0 (1 = short off + long on).
        let frame: Vec<i32> = vec![
            400, -400, 800, -800, 400, -400, 800, -800, 400, -800, 400, -800, 400, -400, 800, -800,
            400,
        ];
        let mut raw: Vec<i32> = vec![65, -130, 99, -66, 5000, -66, 131];
        for _ in 0..3 {
            raw.push(-9000);
            raw.extend_from_slice(&frame);
        }
        raw.extend_from_slice(&[-250_000, 97, -66, 65, -131]);

        let options = ExtractOptions::default();
        let found = extract_frame(&raw, &options).unwrap();
        assert_eq!(found.copies_found, 3);
        assert_eq!(found.gap_us, 9000);
        assert_eq!(found.pulses, pulses_from_raw(&frame));
        assert_eq!(decode_bits(&found.pulses).unwrap(), "10100010");
    }

    #[test]
    fn median_smooths_out_jitter_between_copies() {
        let copy_a: Vec<i32> = vec![
            400, -410, 790, -800, 400, -400, 800, -800, 400, -800, 400, -800, 800, -400, 800, -400,
            380,
        ];
        let copy_b: Vec<i32> = vec![
            420, -400, 800, -790, 400, -400, 800, -800, 400, -800, 400, -800, 800, -400, 800, -400,
            400,
        ];
        let copy_c: Vec<i32> = vec![
            440, -390, 810, -780, 400, -400, 800, -800, 400, -800, 400, -800, 800, -400, 800, -400,
            420,
        ];
        let mut raw = Vec::new();
        for copy in [&copy_a, &copy_b, &copy_c] {
            raw.extend_from_slice(copy);
            raw.push(-10_000);
        }
        let found = extract_frame(&raw, &ExtractOptions::default()).unwrap();
        assert_eq!(found.pulses[0], Pulse::high(420));
        assert_eq!(found.pulses[1], Pulse::low(400));
        assert_eq!(found.pulses[16], Pulse::high(400));
    }

    #[test]
    fn pure_noise_is_rejected() {
        // Short random-looking segments: nothing long enough to be a frame.
        let raw = [65, -130, 99, -66, 5000, -66, 131, -9000, 200, -100];
        assert_eq!(
            extract_frame(&raw, &ExtractOptions::default()),
            Err(ExtractError::NoCandidates)
        );
    }

    #[test]
    fn a_frame_seen_only_once_is_rejected() {
        let mut raw: Vec<i32> = Vec::new();
        for _ in 0..10 {
            raw.extend_from_slice(&[400, -400]);
        }
        raw.push(400);
        let result = extract_frame(&raw, &ExtractOptions::default());
        assert_eq!(
            result,
            Err(ExtractError::NoRepeat {
                candidates: 1,
                best_copies: 1,
                needed: 2
            })
        );
    }

    #[test]
    fn decode_bits_rejects_odd_shapes() {
        assert_eq!(decode_bits(&[]), None);
        // Even number of pulses: no start pulse + pairs layout.
        assert_eq!(decode_bits(&[Pulse::high(1), Pulse::low(1)]), None);
        // Starts with the carrier off.
        assert_eq!(
            decode_bits(&[Pulse::low(1), Pulse::high(1), Pulse::low(1)]),
            None
        );
    }

    #[test]
    fn bits_to_hex_needs_whole_nibbles() {
        assert_eq!(bits_to_hex("10100010").unwrap(), "A2");
        assert_eq!(bits_to_hex("101"), None);
        assert_eq!(bits_to_hex(""), None);
    }

    #[test]
    fn repeat_frame_adds_a_gap_after_every_copy() {
        let frame = [Pulse::high(400), Pulse::low(800), Pulse::high(400)];
        let burst = repeat_frame(&frame, 10_000, 3);
        assert_eq!(burst.len(), 12);
        assert_eq!(burst[3], Pulse::low(10_000));
        assert_eq!(burst[11], Pulse::low(10_000));
        assert_eq!(total_micros(&burst), 3 * (1_600 + 10_000));
    }
}
