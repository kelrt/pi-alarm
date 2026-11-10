//! The pieces every part of the program shares, gathered in one struct.
//!
//! The BLE server, the scheduler and the CLI all need the same things: the
//! alarm store, the radio, the clock and the time zone. `App` holds them.
//! The daemon wraps one `App` in an `Arc` so its tasks can share it.

use std::path::PathBuf;

use anyhow::Context;
use chrono_tz::Tz;

use crate::cc1101::{Cc1101, Cc1101Settings, PiIo};
use crate::clock::{Clock, SystemClock};
use crate::config::{Config, TransmitterKind};
use crate::radio::{load_actions, lock_radio, Radio};
use crate::store::AlarmStore;
use crate::transmitter::{MockTransmitter, Transmitter};

/// Name of the lock file, in the data directory, that keeps two pi-alarm
/// processes from using the radio at the same time.
pub const RADIO_LOCK_FILE: &str = "radio.lock";

pub struct App {
    pub store: AlarmStore,
    pub radio: Radio,
    /// `Box<dyn Clock>`: the real clock in production, a mock in tests.
    pub clock: Box<dyn Clock>,
    /// Alarm times are wall-clock times in this zone.
    pub tz: Tz,
    pub data_dir: PathBuf,
    pub late_grace_secs: u32,
}

impl App {
    /// Build the real thing from a config file: loads every action and opens
    /// the transmitter the config asks for.
    ///
    /// `force_mock` replaces the radio with the mock regardless of the
    /// config, for trying things out before the CC1101 is wired.
    pub fn from_config(config: &Config, force_mock: bool) -> anyhow::Result<App> {
        let actions = load_actions(config)?;
        let transmitter = open_transmitter(config, force_mock)?;
        let radio =
            Radio::new(actions, transmitter).with_lock_file(config.data_dir.join(RADIO_LOCK_FILE));
        Ok(App {
            store: AlarmStore::new(&config.data_dir),
            radio,
            clock: Box::new(SystemClock::new()),
            tz: config.resolve_timezone()?,
            data_dir: config.data_dir.clone(),
            late_grace_secs: config.scheduler.late_grace_secs,
        })
    }
}

/// Open the transmitter named in the config.
pub fn open_transmitter(config: &Config, force_mock: bool) -> anyhow::Result<Box<dyn Transmitter>> {
    if force_mock || config.radio.transmitter == TransmitterKind::Mock {
        log::warn!("using the MOCK transmitter: nothing will be sent over the air");
        return Ok(Box::new(MockTransmitter::new()));
    }

    // Starting the driver resets the chip. If the daemon is transmitting at
    // this very moment (we might be `pi-alarm send` run by hand), wait for it
    // to finish first. The lock is released at the end of this function.
    let _radio_lock = lock_radio(&config.data_dir.join(RADIO_LOCK_FILE));

    let wiring = &config.radio.cc1101;
    let io = PiIo::open(
        wiring.spi_bus,
        wiring.spi_chip_select,
        wiring.spi_clock_hz,
        wiring.gdo0_gpio,
    )?;
    let settings = Cc1101Settings {
        crystal_hz: wiring.crystal_hz,
        pa_power: wiring.pa_power,
    };
    let radio = Cc1101::new(Box::new(io), settings).context("CC1101 start-up failed")?;
    let info = radio.chip_info();
    log::info!(
        "CC1101 found: part 0x{:02X}, version 0x{:02X}",
        info.partnum,
        info.version
    );
    Ok(Box::new(radio))
}
