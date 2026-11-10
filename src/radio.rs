//! Named actions (`fan_3`, `light_toggle`, ...) and sending them.
//!
//! At start-up every action in the config is loaded: its `.sub` file is
//! parsed, the clean frame is extracted and the complete pulse list is built.
//! A broken capture is therefore reported immediately by `pi-alarm check`,
//! not at 6 in the morning when the alarm fires.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context};

use crate::cc1101;
use crate::config::{ActionConfig, ActionMode, Config};
use crate::signal::{
    bits_to_hex, decode_bits, extract_frame, pulses_from_raw, repeat_frame, ExtractOptions, Pulse,
};
use crate::subfile::{Payload, SubFile};
use crate::transmitter::{Transmission, Transmitter};

/// Longest transmission we are willing to make. The transmitter busy-waits
/// at high priority, so this also bounds how long it can hog the Pi.
const MAX_ON_AIR_MICROS: u64 = 5_000_000;

/// One ready-to-send action.
#[derive(Debug, Clone)]
pub struct Action {
    pub name: String,
    /// Button text for the phone app.
    pub label: String,
    /// The `.sub` file this came from.
    pub source: PathBuf,
    pub mode: ActionMode,
    /// The pulse list that goes on air, repeats included.
    pub transmission: Transmission,
    /// Details about the extracted frame, for `pi-alarm inspect`.
    /// `None` in raw mode.
    pub frame: Option<FrameInfo>,
}

/// What we learned about the repeating frame of a capture.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub pulses: Vec<Pulse>,
    pub gap_us: u32,
    /// Clean copies found in the capture.
    pub copies_found: usize,
    /// How many times we send it.
    pub repeats: u32,
    /// Best-effort decoding for humans, e.g. `"A2E83F"`. Not used to transmit.
    pub code_hex: Option<String>,
    pub code_bits: Option<String>,
}

impl Action {
    /// Build an action from a `.sub` file.
    pub fn load(name: &str, config: &ActionConfig, default_repeats: u32) -> anyhow::Result<Action> {
        let sub = SubFile::load(&config.file)?;
        Action::from_sub_file(name, config, default_repeats, &sub)
    }

    /// The same, from an already parsed file (handy in tests).
    pub fn from_sub_file(
        name: &str,
        config: &ActionConfig,
        default_repeats: u32,
        sub: &SubFile,
    ) -> anyhow::Result<Action> {
        if !sub.preset.is_ook() {
            bail!(
                "preset `{}` is not supported; only the OOK presets can be transmitted",
                sub.preset.name()
            );
        }
        cc1101::check_frequency(sub.frequency_hz)?;

        // `match` on a reference to an enum: `raw` borrows the Vec inside.
        let raw = match &sub.payload {
            Payload::Raw(raw) => raw,
            Payload::Key(_) => bail!(
                "this is a decoded `{}` key file, which cannot be transmitted yet. \
                 Capture the button again on the Flipper with Read RAW",
                sub.protocol
            ),
        };

        let (pulses, frame) = match config.mode {
            ActionMode::Frame => {
                let found = extract_frame(raw, &ExtractOptions::default()).context(
                    "could not find a repeating frame. Re-capture holding the button \
                     a little longer, or set `mode = \"raw\"` for this action",
                )?;
                let repeats = config.repeats.unwrap_or(default_repeats);
                let pulses = repeat_frame(&found.pulses, found.gap_us, repeats);
                let code_bits = decode_bits(&found.pulses);
                // `as_deref` turns `&Option<String>` into `Option<&str>`;
                // `and_then` runs `bits_to_hex` only if there are bits.
                let code_hex = code_bits.as_deref().and_then(bits_to_hex);
                let info = FrameInfo {
                    pulses: found.pulses,
                    gap_us: found.gap_us,
                    copies_found: found.copies_found,
                    repeats,
                    code_hex,
                    code_bits,
                };
                (pulses, Some(info))
            }
            ActionMode::Raw => {
                let once = raw_pulses_for_replay(raw);
                // In raw mode the capture is sent once unless the action
                // itself asks for more.
                let repeats = config.repeats.unwrap_or(1);
                let mut pulses = Vec::with_capacity(once.len() * repeats as usize);
                for _ in 0..repeats {
                    pulses.extend_from_slice(&once);
                }
                (pulses, None)
            }
        };

        let transmission = Transmission {
            frequency_hz: sub.frequency_hz,
            preset: sub.preset.clone(),
            pulses,
        };
        if transmission.pulses.is_empty() {
            bail!("nothing to transmit");
        }
        let duration = transmission.duration_micros();
        if duration > MAX_ON_AIR_MICROS {
            bail!(
                "transmission would last {:.1} s; the limit is {:.0} s",
                duration as f64 / 1e6,
                MAX_ON_AIR_MICROS as f64 / 1e6
            );
        }

        Ok(Action {
            name: name.to_string(),
            label: config.label.clone().unwrap_or_else(|| name.to_string()),
            source: config.file.clone(),
            mode: config.mode,
            transmission,
            frame,
        })
    }
}

/// Prepare a whole capture for verbatim replay: drop a leading pause (there
/// is nothing to wait for) and make sure it ends with the carrier off.
fn raw_pulses_for_replay(raw: &[i32]) -> Vec<Pulse> {
    let mut pulses = pulses_from_raw(raw);
    if let Some(first) = pulses.first() {
        if !first.high {
            pulses.remove(0);
        }
    }
    if let Some(last) = pulses.last() {
        if last.high {
            pulses.push(Pulse::low(10_000));
        }
    }
    pulses
}

/// Load every action in the config. Fails on the first broken one, naming it.
pub fn load_actions(config: &Config) -> anyhow::Result<BTreeMap<String, Action>> {
    let mut actions = BTreeMap::new();
    for (name, action_config) in &config.actions {
        let action = Action::load(name, action_config, config.radio.repeats)
            .with_context(|| format!("action `{name}` ({})", action_config.file.display()))?;
        actions.insert(name.clone(), action);
    }
    Ok(actions)
}

/// Why `Radio::send` failed. Kept as an enum so the BLE protocol can answer
/// with a precise error code.
#[derive(Debug)]
pub enum SendError {
    UnknownAction(String),
    Transmit(anyhow::Error),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::UnknownAction(name) => write!(f, "unknown action `{name}`"),
            // `{:#}` prints an anyhow error with all its context on one line.
            SendError::Transmit(error) => write!(f, "transmission failed: {error:#}"),
        }
    }
}

impl std::error::Error for SendError {}

/// The action table plus the transmitter.
///
/// The transmitter sits in a `Mutex` so that `send` can take `&self`. That
/// matters because the daemon shares one `Radio` between the scheduler and
/// the BLE server, and shared things can only be reached through `&self`.
/// The mutex also guarantees two sends never overlap.
pub struct Radio {
    actions: BTreeMap<String, Action>,
    transmitter: Mutex<Box<dyn Transmitter>>,
    /// A lock file shared with other pi-alarm processes (see `send`).
    lock_path: Option<PathBuf>,
}

impl Radio {
    pub fn new(actions: BTreeMap<String, Action>, transmitter: Box<dyn Transmitter>) -> Radio {
        Radio {
            actions,
            transmitter: Mutex::new(transmitter),
            lock_path: None,
        }
    }

    /// Use a lock file so that the daemon and a `pi-alarm send` typed at the
    /// same moment do not both drive the radio.
    pub fn with_lock_file(mut self, path: PathBuf) -> Radio {
        self.lock_path = Some(path);
        self
    }

    pub fn actions(&self) -> &BTreeMap<String, Action> {
        &self.actions
    }

    pub fn action_names(&self) -> Vec<String> {
        self.actions.keys().cloned().collect()
    }

    pub fn has_action(&self, name: &str) -> bool {
        self.actions.contains_key(name)
    }

    /// Transmit an action. Blocks until it has been sent (about 0.3 s).
    pub fn send(&self, name: &str) -> Result<(), SendError> {
        let Some(action) = self.actions.get(name) else {
            return Err(SendError::UnknownAction(name.to_string()));
        };

        // Held until the end of this function. Best effort: if the lock file
        // cannot be opened we transmit anyway.
        let _process_lock = self.lock_path.as_deref().and_then(lock_radio);

        let mut transmitter = self.transmitter.lock().unwrap();
        log::info!(
            "sending `{name}` via {} ({:.0} ms on air)",
            transmitter.name(),
            action.transmission.duration_micros() as f64 / 1000.0
        );
        transmitter
            .transmit(&action.transmission)
            .map_err(SendError::Transmit)
    }
}

/// Take the cross-process radio lock, waiting if another pi-alarm process
/// holds it. The lock is released when the returned `File` is dropped.
///
/// Whoever touches the CC1101 in a way that could disturb a transmission
/// (sending, or resetting the chip at start-up) holds this lock meanwhile.
/// Returns `None`, and logs why, if the lock file cannot be used; callers
/// then carry on without it.
pub fn lock_radio(path: &Path) -> Option<File> {
    // Opening read-only is enough for a lock and works even when the file
    // belongs to another user; create it only if it does not exist yet.
    let file = File::open(path).or_else(|_| File::create(path));
    match file {
        Ok(file) => match file.lock() {
            Ok(()) => Some(file),
            Err(error) => {
                log::debug!("cannot lock {}: {error}", path.display());
                None
            }
        },
        Err(error) => {
            log::debug!("cannot open lock file {}: {error}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::total_micros;
    use crate::subfile::Preset;
    use crate::transmitter::MockTransmitter;

    fn action_config(file: &str) -> ActionConfig {
        ActionConfig {
            file: PathBuf::from(file),
            label: None,
            mode: ActionMode::Frame,
            repeats: None,
        }
    }

    fn real_action(name: &str, file: &str) -> Action {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("flipper_files")
            .join(file);
        Action::load(name, &action_config(path.to_str().unwrap()), 8).unwrap()
    }

    #[test]
    fn real_captures_become_eight_clean_frames() {
        for (name, file, code) in [
            ("fan_off", "0_fan.sub", "A2E8DF"),
            ("fan_3", "3_fan.sub", "A2E83F"),
            ("light_toggle", "Lite.sub", "A2E85F"),
        ] {
            let action = real_action(name, file);
            let tx = &action.transmission;
            assert_eq!(tx.frequency_hz, 304_250_000);
            assert_eq!(tx.preset, Preset::Ook270Async);
            // 8 x (49 frame pulses + 1 gap).
            assert_eq!(tx.pulses.len(), 8 * 50, "{name}");
            // Starts with the carrier on, ends with it off.
            assert!(tx.pulses[0].high);
            assert!(!tx.pulses.last().unwrap().high);
            // About 8 x (28 ms frame + 10 ms gap).
            let millis = tx.duration_micros() / 1000;
            assert!((280..=340).contains(&millis), "{name}: {millis} ms");

            let frame = action.frame.as_ref().unwrap();
            assert_eq!(frame.code_hex.as_deref(), Some(code), "{name}");
            assert_eq!(frame.repeats, 8);
            assert_eq!(action.label, name);
        }
    }

    #[test]
    fn every_repeat_is_identical() {
        let action = real_action("fan_3", "3_fan.sub");
        let pulses = &action.transmission.pulses;
        let first = &pulses[..50];
        for copy in pulses.chunks(50) {
            assert_eq!(copy, first);
        }
    }

    #[test]
    fn per_action_repeats_and_label_override_the_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("flipper_files/Lite.sub");
        let mut config = action_config(path.to_str().unwrap());
        config.repeats = Some(3);
        config.label = Some("Light".to_string());
        let action = Action::load("light_toggle", &config, 8).unwrap();
        assert_eq!(action.transmission.pulses.len(), 3 * 50);
        assert_eq!(action.label, "Light");
    }

    #[test]
    fn raw_mode_replays_the_capture_once() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("flipper_files/3_fan.sub");
        let mut config = action_config(path.to_str().unwrap());
        config.mode = ActionMode::Raw;
        let action = Action::load("fan_3", &config, 8).unwrap();
        assert!(action.frame.is_none());
        // The capture has 373 timings and ends with the carrier on, so one
        // closing pause is added.
        assert_eq!(action.transmission.pulses.len(), 374);
        assert!(!action.transmission.pulses.last().unwrap().high);
        // 935359 us recorded + the 10 ms closing pause.
        assert_eq!(total_micros(&action.transmission.pulses), 945_359);
    }

    #[test]
    fn decoded_key_files_are_rejected_with_advice() {
        let sub = SubFile::parse(
            "Filetype: Flipper SubGhz Key File\nVersion: 1\nFrequency: 433920000\n\
             Preset: FuriHalSubGhzPresetOok650Async\nProtocol: Princeton\nBit: 24\n\
             Key: 00 00 00 00 00 95 D5 D4\nTE: 400\n",
        )
        .unwrap();
        let error = Action::from_sub_file("x", &action_config("x.sub"), 8, &sub).unwrap_err();
        assert!(error.to_string().contains("Read RAW"), "{error}");
    }

    #[test]
    fn unsupported_presets_and_bands_are_rejected() {
        let fsk = SubFile::parse(
            "Filetype: Flipper SubGhz RAW File\nVersion: 1\nFrequency: 433920000\n\
             Preset: FuriHalSubGhzPreset2FSKDev238Async\nProtocol: RAW\nRAW_Data: 400 -400\n",
        )
        .unwrap();
        let error = Action::from_sub_file("x", &action_config("x.sub"), 8, &fsk).unwrap_err();
        assert!(error.to_string().contains("not supported"), "{error}");

        let off_band = SubFile::parse(
            "Filetype: Flipper SubGhz RAW File\nVersion: 1\nFrequency: 350000000\n\
             Preset: FuriHalSubGhzPresetOok270Async\nProtocol: RAW\nRAW_Data: 400 -400\n",
        )
        .unwrap();
        assert!(Action::from_sub_file("x", &action_config("x.sub"), 8, &off_band).is_err());
    }

    #[test]
    fn a_capture_without_a_repeating_frame_suggests_raw_mode() {
        let noise = SubFile::parse(
            "Filetype: Flipper SubGhz RAW File\nVersion: 1\nFrequency: 304250000\n\
             Preset: FuriHalSubGhzPresetOok270Async\nProtocol: RAW\n\
             RAW_Data: 65 -130 99 -66 131 -66\n",
        )
        .unwrap();
        let error = Action::from_sub_file("x", &action_config("x.sub"), 8, &noise).unwrap_err();
        // `{:#}` includes the context messages, where the advice is.
        let message = format!("{error:#}");
        assert!(message.contains("mode = \"raw\""), "{message}");
    }

    #[test]
    fn overlong_raw_replays_are_refused() {
        // 0_fan.sub is 2.8 s of capture; twice that is over the 5 s limit.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("flipper_files/0_fan.sub");
        let mut config = action_config(path.to_str().unwrap());
        config.mode = ActionMode::Raw;
        config.repeats = Some(2);
        let error = Action::load("fan_off", &config, 8).unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
    }

    #[test]
    fn send_transmits_the_named_action() {
        let mut actions = BTreeMap::new();
        actions.insert("fan_3".to_string(), real_action("fan_3", "3_fan.sub"));
        actions.insert(
            "light_toggle".to_string(),
            real_action("light_toggle", "Lite.sub"),
        );
        let mock = MockTransmitter::new();
        let log = mock.sent_log();
        let radio = Radio::new(actions, Box::new(mock));

        assert_eq!(radio.action_names(), ["fan_3", "light_toggle"]);
        assert!(radio.has_action("fan_3"));
        assert!(!radio.has_action("fan_1"));

        radio.send("light_toggle").unwrap();
        assert_eq!(log.count(), 1);
        assert_eq!(log.all()[0], radio.actions()["light_toggle"].transmission);
    }

    #[test]
    fn send_reports_unknown_actions_and_transmit_failures() {
        let mut actions = BTreeMap::new();
        actions.insert("fan_3".to_string(), real_action("fan_3", "3_fan.sub"));
        let mock = MockTransmitter::new();
        let log = mock.sent_log();
        let radio = Radio::new(actions, Box::new(mock));

        assert!(matches!(
            radio.send("fan_1"),
            Err(SendError::UnknownAction(name)) if name == "fan_1"
        ));
        assert_eq!(log.count(), 0);

        log.set_failing(true);
        assert!(matches!(radio.send("fan_3"), Err(SendError::Transmit(_))));
    }

    #[test]
    fn the_lock_file_does_not_get_in_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let mut actions = BTreeMap::new();
        actions.insert("fan_3".to_string(), real_action("fan_3", "3_fan.sub"));
        let mock = MockTransmitter::new();
        let log = mock.sent_log();
        let radio =
            Radio::new(actions, Box::new(mock)).with_lock_file(dir.path().join("radio.lock"));
        radio.send("fan_3").unwrap();
        radio.send("fan_3").unwrap();
        assert_eq!(log.count(), 2);
    }

    #[test]
    fn load_actions_names_the_broken_action() {
        let text = "data_dir = \"d\"\n[actions.fan_9]\nfile = \"does-not-exist.sub\"\n";
        let config = Config::parse(text, Path::new("/nonexistent")).unwrap();
        let error = load_actions(&config).unwrap_err();
        assert!(error.to_string().contains("fan_9"), "{error}");
    }
}
