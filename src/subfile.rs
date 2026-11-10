//! Parser for Flipper Zero `.sub` (Sub-GHz) files.
//!
//! A `.sub` file is plain text, one `Key: value` pair per line:
//!
//! ```text
//! Filetype: Flipper SubGhz RAW File
//! Version: 1
//! Frequency: 304250000
//! Preset: FuriHalSubGhzPresetOok270Async
//! Protocol: RAW
//! RAW_Data: 433 -306 833 -716 ...
//! ```
//!
//! There are two flavours:
//!
//! * **RAW** files (`Protocol: RAW`) hold the recorded signal as a list of
//!   durations in microseconds. A positive number means "carrier on for this
//!   long", a negative number means "carrier off for this long".
//! * **Key** files (any other protocol, e.g. `Princeton`) hold a decoded
//!   button code: `Bit`, `Key` and usually `TE` (the base pulse length).
//!
//! This module only *reads* the file. Turning the timings into something we
//! can transmit happens in `signal.rs`.

use std::fmt;
use std::path::Path;

/// The radio preset the Flipper used while recording.
///
/// The preset decides modulation (OOK vs FSK) and receiver bandwidth. We only
/// know how to transmit the two OOK presets, but we keep the name of any
/// other preset so error messages can show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preset {
    /// `FuriHalSubGhzPresetOok270Async`: on/off keying, 270 kHz bandwidth.
    Ook270Async,
    /// `FuriHalSubGhzPresetOok650Async`: on/off keying, 650 kHz bandwidth.
    Ook650Async,
    /// Anything else (2-FSK presets, custom presets, ...).
    Other(String),
}

impl Preset {
    /// Convert the name used in the file into a `Preset`.
    pub fn from_name(name: &str) -> Preset {
        match name {
            "FuriHalSubGhzPresetOok270Async" => Preset::Ook270Async,
            "FuriHalSubGhzPresetOok650Async" => Preset::Ook650Async,
            other => Preset::Other(other.to_string()),
        }
    }

    /// The name as it appears in a `.sub` file.
    pub fn name(&self) -> &str {
        match self {
            Preset::Ook270Async => "FuriHalSubGhzPresetOok270Async",
            Preset::Ook650Async => "FuriHalSubGhzPresetOok650Async",
            // `name` is a `String`; returning `&str` borrows from it, which is
            // fine because the borrow cannot outlive `self`.
            Preset::Other(name) => name,
        }
    }

    /// True for the on/off-keying presets, the only ones we can transmit.
    pub fn is_ook(&self) -> bool {
        matches!(self, Preset::Ook270Async | Preset::Ook650Async)
    }
}

/// A decoded button code from a "Key" file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedKey {
    /// Number of meaningful bits in `key`.
    pub bits: u32,
    /// The key bytes exactly as written in the file (most significant first).
    pub key: Vec<u8>,
    /// Base pulse length in microseconds, if the file has a `TE` line.
    pub te_us: Option<u32>,
    /// Any other `Name: value` lines we did not recognise, in file order.
    pub extra: Vec<(String, String)>,
}

impl DecodedKey {
    /// The key as one number. Returns `None` if it does not fit in 64 bits.
    pub fn value(&self) -> Option<u64> {
        if self.key.len() > 8 {
            return None;
        }
        let mut value: u64 = 0;
        for byte in &self.key {
            value = (value << 8) | u64::from(*byte);
        }
        Some(value)
    }
}

/// What a `.sub` file carries: recorded timings or a decoded key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// Durations in microseconds. Positive = carrier on, negative = off.
    Raw(Vec<i32>),
    /// A decoded key (protocol name is in `SubFile::protocol`).
    Key(DecodedKey),
}

/// A parsed `.sub` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubFile {
    pub frequency_hz: u32,
    pub preset: Preset,
    /// `"RAW"` or the name of a decoded protocol such as `"Princeton"`.
    pub protocol: String,
    pub payload: Payload,
}

/// Everything that can go wrong while parsing.
///
/// A hand-written error enum (instead of a string) lets callers and tests
/// match on the exact problem. `line` numbers start at 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubFileError {
    /// The first line is not a Flipper Sub-GHz `Filetype`.
    NotASubFile,
    /// A required field such as `Frequency` is missing.
    MissingField(&'static str),
    /// A line is not of the form `Name: value`.
    BadLine { line: usize },
    /// A field's value could not be understood.
    BadValue {
        line: usize,
        field: String,
        value: String,
    },
    /// The file says `Protocol: RAW` but has no usable `RAW_Data`.
    EmptyRawData,
}

impl fmt::Display for SubFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubFileError::NotASubFile => {
                write!(
                    f,
                    "not a Flipper Sub-GHz file (bad or missing Filetype line)"
                )
            }
            SubFileError::MissingField(name) => write!(f, "missing required field `{name}`"),
            SubFileError::BadLine { line } => {
                write!(f, "line {line}: expected `Name: value`")
            }
            SubFileError::BadValue { line, field, value } => {
                write!(f, "line {line}: bad value for `{field}`: `{value}`")
            }
            SubFileError::EmptyRawData => write!(f, "RAW file contains no RAW_Data timings"),
        }
    }
}

// Implementing the standard `Error` trait lets `?` convert this type into
// `anyhow::Error` automatically in the rest of the program.
impl std::error::Error for SubFileError {}

impl SubFile {
    /// Read and parse a `.sub` file from disk.
    pub fn load(path: &Path) -> anyhow::Result<SubFile> {
        use anyhow::Context;
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let sub =
            SubFile::parse(&text).with_context(|| format!("cannot parse {}", path.display()))?;
        Ok(sub)
    }

    /// Parse the text of a `.sub` file.
    pub fn parse(text: &str) -> Result<SubFile, SubFileError> {
        // Each of these starts as `None` and is filled in when its line
        // appears. At the end we check that the required ones are present.
        let mut filetype_seen = false;
        let mut frequency_hz: Option<u32> = None;
        let mut preset: Option<Preset> = None;
        let mut protocol: Option<String> = None;
        let mut raw: Vec<i32> = Vec::new();
        let mut bits: Option<u32> = None;
        let mut key: Option<Vec<u8>> = None;
        let mut te_us: Option<u32> = None;
        let mut extra: Vec<(String, String)> = Vec::new();

        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // `split_once` returns the text before and after the first ':'.
            let Some((name, value)) = line.split_once(':') else {
                return Err(SubFileError::BadLine { line: line_number });
            };
            let name = name.trim();
            let value = value.trim();

            // The very first real line must identify the file type.
            if !filetype_seen {
                if name != "Filetype" || !value.starts_with("Flipper SubGhz") {
                    return Err(SubFileError::NotASubFile);
                }
                filetype_seen = true;
                continue;
            }

            // A small closure so every arm below builds the same error.
            let bad_value = || SubFileError::BadValue {
                line: line_number,
                field: name.to_string(),
                value: shorten(value),
            };

            match name {
                "Version" => {
                    // We accept any version number but it must be a number.
                    value.parse::<u32>().map_err(|_| bad_value())?;
                }
                "Frequency" => {
                    frequency_hz = Some(value.parse::<u32>().map_err(|_| bad_value())?);
                }
                "Preset" => preset = Some(Preset::from_name(value)),
                "Protocol" => protocol = Some(value.to_string()),
                "RAW_Data" => {
                    // A file has many RAW_Data lines; they simply continue
                    // one another, so we append to one long list.
                    for token in value.split_whitespace() {
                        let duration: i32 = token.parse().map_err(|_| bad_value())?;
                        if duration == 0 {
                            return Err(bad_value());
                        }
                        raw.push(duration);
                    }
                }
                "Bit" => bits = Some(value.parse::<u32>().map_err(|_| bad_value())?),
                "Key" => key = Some(parse_hex_bytes(value).ok_or_else(bad_value)?),
                "TE" => te_us = Some(value.parse::<u32>().map_err(|_| bad_value())?),
                _ => extra.push((name.to_string(), value.to_string())),
            }
        }

        if !filetype_seen {
            return Err(SubFileError::NotASubFile);
        }
        let frequency_hz = frequency_hz.ok_or(SubFileError::MissingField("Frequency"))?;
        let preset = preset.ok_or(SubFileError::MissingField("Preset"))?;
        let protocol = protocol.ok_or(SubFileError::MissingField("Protocol"))?;

        let payload = if protocol == "RAW" {
            if raw.is_empty() {
                return Err(SubFileError::EmptyRawData);
            }
            Payload::Raw(raw)
        } else {
            Payload::Key(DecodedKey {
                bits: bits.ok_or(SubFileError::MissingField("Bit"))?,
                key: key.ok_or(SubFileError::MissingField("Key"))?,
                te_us,
                extra,
            })
        };

        Ok(SubFile {
            frequency_hz,
            preset,
            protocol,
            payload,
        })
    }

    /// The RAW timings, or `None` for a decoded-key file.
    pub fn raw_timings(&self) -> Option<&[i32]> {
        match &self.payload {
            Payload::Raw(timings) => Some(timings),
            Payload::Key(_) => None,
        }
    }
}

/// Parse `"00 00 A2 E8"` into bytes. Returns `None` on any bad token.
fn parse_hex_bytes(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for token in text.split_whitespace() {
        bytes.push(u8::from_str_radix(token, 16).ok()?);
    }
    if bytes.is_empty() {
        None
    } else {
        Some(bytes)
    }
}

/// RAW_Data lines are thousands of characters long; keep error messages short.
fn shorten(value: &str) -> String {
    const MAX_CHARS: usize = 40;
    if value.chars().count() <= MAX_CHARS {
        value.to_string()
    } else {
        let start: String = value.chars().take(MAX_CHARS).collect();
        format!("{start}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `include_str!` copies the file into the test binary at compile time, so
    // the tests always run against the real captures.
    const FAN_OFF: &str = include_str!("../flipper_files/0_fan.sub");
    const FAN_3: &str = include_str!("../flipper_files/3_fan.sub");
    const LIGHT: &str = include_str!("../flipper_files/Lite.sub");

    #[test]
    fn real_captures_have_the_expected_header() {
        for (name, text) in [("0_fan", FAN_OFF), ("3_fan", FAN_3), ("Lite", LIGHT)] {
            let sub = SubFile::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(sub.frequency_hz, 304_250_000, "{name}");
            assert_eq!(sub.preset, Preset::Ook270Async, "{name}");
            assert_eq!(sub.protocol, "RAW", "{name}");
            assert!(sub.preset.is_ook(), "{name}");
        }
    }

    #[test]
    fn real_captures_have_the_expected_number_of_timings() {
        // Counted independently with a script when the files were analysed.
        let counts = [(FAN_OFF, 8704), (FAN_3, 373), (LIGHT, 3538)];
        for (text, expected) in counts {
            let sub = SubFile::parse(text).unwrap();
            assert_eq!(sub.raw_timings().unwrap().len(), expected);
        }
    }

    #[test]
    fn real_capture_starts_and_ends_with_the_recorded_values() {
        let sub = SubFile::parse(FAN_3).unwrap();
        let raw = sub.raw_timings().unwrap();
        assert_eq!(&raw[..4], &[81889, -64, 1897, -98]);
        assert_eq!(raw[raw.len() - 1], 1013);
    }

    #[test]
    fn real_captures_alternate_between_on_and_off() {
        for text in [FAN_OFF, FAN_3, LIGHT] {
            let sub = SubFile::parse(text).unwrap();
            let raw = sub.raw_timings().unwrap();
            // `windows(2)` walks over every neighbouring pair.
            for pair in raw.windows(2) {
                assert_ne!(pair[0] > 0, pair[1] > 0, "two timings with the same sign");
            }
        }
    }

    #[test]
    fn parses_a_decoded_key_file() {
        let text = "Filetype: Flipper SubGhz Key File\n\
                    Version: 1\n\
                    Frequency: 433920000\n\
                    Preset: FuriHalSubGhzPresetOok650Async\n\
                    Protocol: Princeton\n\
                    Bit: 24\n\
                    Key: 00 00 00 00 00 95 D5 D4\n\
                    TE: 400\n\
                    Repeat: 3\n";
        let sub = SubFile::parse(text).unwrap();
        assert_eq!(sub.frequency_hz, 433_920_000);
        assert_eq!(sub.preset, Preset::Ook650Async);
        assert_eq!(sub.protocol, "Princeton");
        assert!(sub.raw_timings().is_none());

        let Payload::Key(key) = &sub.payload else {
            panic!("expected a decoded key");
        };
        assert_eq!(key.bits, 24);
        assert_eq!(key.value(), Some(0x95D5D4));
        assert_eq!(key.te_us, Some(400));
        assert_eq!(key.extra, vec![("Repeat".to_string(), "3".to_string())]);
    }

    #[test]
    fn raw_data_lines_are_joined_and_comments_skipped() {
        let text = "Filetype: Flipper SubGhz RAW File\n\
                    Version: 1\n\
                    # a comment\n\
                    Frequency: 315000000\n\
                    Preset: FuriHalSubGhzPresetOok650Async\n\
                    Protocol: RAW\n\
                    RAW_Data: 100 -200\n\
                    \n\
                    RAW_Data: 300 -400\r\n";
        let sub = SubFile::parse(text).unwrap();
        assert_eq!(sub.raw_timings().unwrap(), &[100, -200, 300, -400]);
    }

    #[test]
    fn unknown_presets_are_kept_by_name() {
        let preset = Preset::from_name("FuriHalSubGhzPreset2FSKDev238Async");
        assert!(!preset.is_ook());
        assert_eq!(preset.name(), "FuriHalSubGhzPreset2FSKDev238Async");
    }

    #[test]
    fn rejects_files_that_are_not_sub_files() {
        assert_eq!(SubFile::parse(""), Err(SubFileError::NotASubFile));
        assert_eq!(
            SubFile::parse("Filetype: Flipper NFC device\nVersion: 2\n"),
            Err(SubFileError::NotASubFile)
        );
        assert_eq!(
            SubFile::parse("Frequency: 304250000\n"),
            Err(SubFileError::NotASubFile)
        );
    }

    #[test]
    fn reports_missing_fields() {
        let no_frequency = "Filetype: Flipper SubGhz RAW File\nVersion: 1\n\
                            Preset: FuriHalSubGhzPresetOok270Async\nProtocol: RAW\nRAW_Data: 1 -1\n";
        assert_eq!(
            SubFile::parse(no_frequency),
            Err(SubFileError::MissingField("Frequency"))
        );

        let no_key = "Filetype: Flipper SubGhz Key File\nVersion: 1\nFrequency: 1\n\
                      Preset: X\nProtocol: Princeton\nBit: 24\n";
        assert_eq!(
            SubFile::parse(no_key),
            Err(SubFileError::MissingField("Key"))
        );
    }

    #[test]
    fn reports_bad_values_with_a_line_number() {
        let text = "Filetype: Flipper SubGhz RAW File\nVersion: 1\nFrequency: 304.25MHz\n";
        assert_eq!(
            SubFile::parse(text),
            Err(SubFileError::BadValue {
                line: 3,
                field: "Frequency".to_string(),
                value: "304.25MHz".to_string(),
            })
        );

        let zero = "Filetype: Flipper SubGhz RAW File\nVersion: 1\nFrequency: 1\n\
                    Preset: X\nProtocol: RAW\nRAW_Data: 100 0 -100\n";
        assert!(matches!(
            SubFile::parse(zero),
            Err(SubFileError::BadValue { line: 6, .. })
        ));

        assert_eq!(
            SubFile::parse("Filetype: Flipper SubGhz RAW File\nthis line has no colon\n"),
            Err(SubFileError::BadLine { line: 2 })
        );
    }

    #[test]
    fn raw_file_without_timings_is_an_error() {
        let text = "Filetype: Flipper SubGhz RAW File\nVersion: 1\nFrequency: 1\n\
                    Preset: X\nProtocol: RAW\n";
        assert_eq!(SubFile::parse(text), Err(SubFileError::EmptyRawData));
    }
}
