//! The TOML configuration file.
//!
//! Everything that is specific to one installation lives here: which GPIO
//! the radio is wired to, where data is stored, and above all the table of
//! **actions**, each naming a `.sub` capture to replay. Nothing about the
//! remote (codes, frequency) is compiled into the program.
//!
//! `serde` does the parsing: `#[derive(Deserialize)]` on a struct generates
//! the code that fills it from the file. See `deploy/pi-alarm.example.toml`
//! for a commented example.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;

/// The whole config file.
///
/// `deny_unknown_fields` turns a misspelt key into an error instead of
/// silently ignoring it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Directory for `alarms.json` and the scheduler's state file.
    pub data_dir: PathBuf,

    /// IANA time zone name such as `"America/Chicago"`. Alarm times are in
    /// this zone. If left out, the system's time zone is used.
    #[serde(default)]
    pub timezone: Option<String>,

    #[serde(default)]
    pub radio: RadioConfig,

    #[serde(default)]
    pub ble: BleConfig,

    #[serde(default)]
    pub scheduler: SchedulerConfig,

    /// Action name -> what to transmit. A `BTreeMap` keeps the names sorted,
    /// so lists come out in a stable order.
    #[serde(default)]
    pub actions: BTreeMap<String, ActionConfig>,
}

/// Which transmitter implementation to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransmitterKind {
    /// The real radio.
    Cc1101,
    /// No radio: transmissions are only logged. For development machines.
    Mock,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioConfig {
    #[serde(default = "default_transmitter")]
    pub transmitter: TransmitterKind,

    /// How many times a frame is sent per transmission.
    #[serde(default = "default_repeats")]
    pub repeats: u32,

    #[serde(default)]
    pub cc1101: Cc1101Config,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cc1101Config {
    /// SPI bus number (0 = the pins on the 40-pin header).
    #[serde(default)]
    pub spi_bus: u8,
    /// Which chip-select line the module's CSN is wired to (0 = CE0).
    #[serde(default)]
    pub spi_chip_select: u8,
    #[serde(default = "default_spi_clock_hz")]
    pub spi_clock_hz: u32,
    /// BCM GPIO number wired to the module's GDO0 pin.
    #[serde(default = "default_gdo0_gpio")]
    pub gdo0_gpio: u8,
    /// Crystal on the module. 26 MHz unless you know otherwise.
    #[serde(default = "default_crystal_hz")]
    pub crystal_hz: u32,
    /// Output power (PATABLE value). 0xC0 is the maximum.
    #[serde(default = "default_pa_power")]
    pub pa_power: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BleConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The Bluetooth name the phone sees.
    #[serde(default = "default_device_name")]
    pub device_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    /// A step that could not run on time (daemon was down, clock jumped) is
    /// still run if it is at most this many seconds late. Older ones are
    /// skipped.
    #[serde(default = "default_late_grace_secs")]
    pub late_grace_secs: u32,
}

/// How to turn a capture into a transmission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ActionMode {
    /// Find the repeating frame in the capture and send only that (default).
    #[default]
    Frame,
    /// Replay the whole capture exactly as recorded, noise included.
    Raw,
}

/// One entry of the `[actions]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionConfig {
    /// Path to the `.sub` file. Relative paths start at the config file's
    /// directory.
    pub file: PathBuf,
    /// Text for the button in the phone app. Defaults to the action name.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub mode: ActionMode,
    /// Overrides `[radio] repeats` for this action.
    #[serde(default)]
    pub repeats: Option<u32>,
}

// serde's `default = "..."` attribute needs a function for every default
// value that is not simply `Default::default()`.
fn default_transmitter() -> TransmitterKind {
    TransmitterKind::Cc1101
}
fn default_repeats() -> u32 {
    8
}
fn default_spi_clock_hz() -> u32 {
    500_000
}
fn default_gdo0_gpio() -> u8 {
    25
}
fn default_crystal_hz() -> u32 {
    26_000_000
}
fn default_pa_power() -> u8 {
    0xC0
}
fn default_true() -> bool {
    true
}
fn default_device_name() -> String {
    "pi-alarm".to_string()
}
fn default_late_grace_secs() -> u32 {
    300
}

// These `Default` impls make a whole missing section (`[radio]`, `[ble]`,
// ...) behave as if it were present but empty.
impl Default for RadioConfig {
    fn default() -> Self {
        RadioConfig {
            transmitter: default_transmitter(),
            repeats: default_repeats(),
            cc1101: Cc1101Config::default(),
        }
    }
}

impl Default for Cc1101Config {
    fn default() -> Self {
        Cc1101Config {
            spi_bus: 0,
            spi_chip_select: 0,
            spi_clock_hz: default_spi_clock_hz(),
            gdo0_gpio: default_gdo0_gpio(),
            crystal_hz: default_crystal_hz(),
            pa_power: default_pa_power(),
        }
    }
}

impl Default for BleConfig {
    fn default() -> Self {
        BleConfig {
            enabled: true,
            device_name: default_device_name(),
        }
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        SchedulerConfig {
            late_grace_secs: default_late_grace_secs(),
        }
    }
}

/// Where the config is looked for when no `--config` is given.
pub const DEFAULT_CONFIG_PATHS: [&str; 2] = ["pi-alarm.toml", "/etc/pi-alarm/pi-alarm.toml"];

impl Config {
    /// Read a config file. Relative paths inside it are made absolute,
    /// relative to the directory the config file is in.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        let base_dir = path.parent().unwrap_or(Path::new("."));
        Config::parse(&text, base_dir)
            .with_context(|| format!("invalid config file {}", path.display()))
    }

    /// Parse config text. `base_dir` is where relative paths start from.
    pub fn parse(text: &str, base_dir: &Path) -> anyhow::Result<Config> {
        let mut config: Config = toml::from_str(text)?;

        config.data_dir = base_dir.join(&config.data_dir);
        // `values_mut` lets us change each action in place.
        for action in config.actions.values_mut() {
            // `join` keeps `file` as it is when it is already absolute.
            action.file = base_dir.join(&action.file);
        }

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.radio.repeats == 0 || self.radio.repeats > 50 {
            bail!("[radio] repeats must be between 1 and 50");
        }
        for (name, action) in &self.actions {
            if !is_valid_action_name(name) {
                bail!(
                    "action name `{name}` is not valid: use 1-32 characters \
                     from a-z, 0-9 and _"
                );
            }
            if let Some(repeats) = action.repeats {
                if repeats == 0 || repeats > 50 {
                    bail!("action `{name}`: repeats must be between 1 and 50");
                }
            }
        }
        // The scheduler runs a step when it is between 0 and `late_grace_secs`
        // late, and it looks once a second. A tiny value would make it miss
        // steps; zero would mean nothing ever runs.
        if !(10..=3600).contains(&self.scheduler.late_grace_secs) {
            bail!("[scheduler] late_grace_secs must be between 10 and 3600");
        }
        if self.ble.device_name.is_empty() || self.ble.device_name.len() > 20 {
            bail!("[ble] device_name must be 1-20 characters");
        }
        if let Some(name) = &self.timezone {
            // Parsing checks that the name exists in the time zone database.
            name.parse::<chrono_tz::Tz>()
                .map_err(|_| anyhow::anyhow!("unknown timezone `{name}`"))?;
        }
        Ok(())
    }

    /// Find the config file: an explicit path, or the first default location
    /// that exists.
    pub fn find(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
        if let Some(path) = explicit {
            return Ok(path.to_path_buf());
        }
        for candidate in DEFAULT_CONFIG_PATHS {
            let path = Path::new(candidate);
            if path.exists() {
                return Ok(path.to_path_buf());
            }
        }
        bail!(
            "no config file found (looked for {}); pass --config <file>",
            DEFAULT_CONFIG_PATHS.join(", ")
        )
    }

    /// The time zone alarms are scheduled in: the configured one, or the
    /// system's.
    pub fn resolve_timezone(&self) -> anyhow::Result<chrono_tz::Tz> {
        let name = match &self.timezone {
            Some(name) => name.clone(),
            None => iana_time_zone::get_timezone()
                .context("cannot find the system time zone; set `timezone` in the config")?,
        };
        name.parse::<chrono_tz::Tz>()
            .map_err(|_| anyhow::anyhow!("unknown time zone `{name}`"))
    }
}

/// Action names travel over BLE and appear in alarm files, so keep them to a
/// simple, short alphabet.
pub fn is_valid_action_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../deploy/pi-alarm.example.toml");

    #[test]
    fn the_shipped_example_config_is_valid() {
        let config = Config::parse(EXAMPLE, Path::new("/etc/pi-alarm")).unwrap();
        assert_eq!(config.radio.transmitter, TransmitterKind::Cc1101);
        assert_eq!(config.radio.cc1101.gdo0_gpio, 25);
        assert_eq!(config.radio.cc1101.pa_power, 0xC0);
        let names: Vec<&String> = config.actions.keys().collect();
        assert_eq!(names, ["fan_3", "fan_off", "light_toggle"]);
        assert_eq!(
            config.actions["fan_3"].file,
            PathBuf::from("/etc/pi-alarm/signals/3_fan.sub")
        );
    }

    #[test]
    fn a_minimal_config_gets_defaults() {
        let config = Config::parse("data_dir = \"data\"\n", Path::new("/base")).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/base/data"));
        assert_eq!(config.radio.transmitter, TransmitterKind::Cc1101);
        assert_eq!(config.radio.repeats, 8);
        assert_eq!(config.radio.cc1101.spi_clock_hz, 500_000);
        assert_eq!(config.radio.cc1101.crystal_hz, 26_000_000);
        assert!(config.ble.enabled);
        assert_eq!(config.ble.device_name, "pi-alarm");
        assert_eq!(config.scheduler.late_grace_secs, 300);
        assert!(config.actions.is_empty());
        assert!(config.timezone.is_none());
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        let text = "data_dir = \"/var/lib/pi-alarm\"\n\
                    [actions.fan_1]\nfile = \"/srv/signals/1_fan.sub\"\n";
        let config = Config::parse(text, Path::new("/base")).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/pi-alarm"));
        assert_eq!(
            config.actions["fan_1"].file,
            PathBuf::from("/srv/signals/1_fan.sub")
        );
    }

    #[test]
    fn adding_an_action_is_three_lines() {
        let text = "data_dir = \"d\"\n\
                    [actions.fan_1]\n\
                    file = \"signals/1_fan.sub\"\n\
                    label = \"Fan 1\"\n\
                    repeats = 12\n\
                    mode = \"raw\"\n";
        let config = Config::parse(text, Path::new("/c")).unwrap();
        let action = &config.actions["fan_1"];
        assert_eq!(action.file, PathBuf::from("/c/signals/1_fan.sub"));
        assert_eq!(action.label.as_deref(), Some("Fan 1"));
        assert_eq!(action.repeats, Some(12));
        assert_eq!(action.mode, ActionMode::Raw);
    }

    #[test]
    fn misspelt_keys_are_errors() {
        let error = Config::parse("data_dir = \"d\"\ntimezon = \"UTC\"\n", Path::new("."))
            .unwrap_err()
            .to_string();
        assert!(error.contains("timezon"), "{error}");
    }

    #[test]
    fn bad_values_are_errors() {
        let cases = [
            "data_dir = \"d\"\ntimezone = \"Mars/Olympus\"\n",
            "data_dir = \"d\"\n[radio]\nrepeats = 0\n",
            "data_dir = \"d\"\n[radio]\ntransmitter = \"laser\"\n",
            "data_dir = \"d\"\n[actions.\"Fan 3\"]\nfile = \"x.sub\"\n",
            "data_dir = \"d\"\n[actions.fan_3]\nfile = \"x.sub\"\nrepeats = 500\n",
            "data_dir = \"d\"\n[ble]\ndevice_name = \"\"\n",
            "data_dir = \"d\"\n[scheduler]\nlate_grace_secs = 0\n",
            "data_dir = \"d\"\n[scheduler]\nlate_grace_secs = 86400\n",
            // data_dir is required
            "[radio]\nrepeats = 3\n",
        ];
        for text in cases {
            assert!(Config::parse(text, Path::new(".")).is_err(), "{text}");
        }
    }

    #[test]
    fn timezone_is_resolved_from_the_config() {
        let config = Config::parse(
            "data_dir = \"d\"\ntimezone = \"Europe/Berlin\"\n",
            Path::new("."),
        )
        .unwrap();
        assert_eq!(
            config.resolve_timezone().unwrap(),
            chrono_tz::Europe::Berlin
        );
    }

    #[test]
    fn action_name_rules() {
        assert!(is_valid_action_name("fan_3"));
        assert!(is_valid_action_name("light_toggle"));
        assert!(!is_valid_action_name(""));
        assert!(!is_valid_action_name("Fan"));
        assert!(!is_valid_action_name("fan-3"));
        assert!(!is_valid_action_name(&"x".repeat(33)));
    }
}
