//! Driver for the Texas Instruments CC1101 sub-GHz transceiver.
//!
//! We use the chip the same way the Flipper Zero does when it replays a RAW
//! file, called "asynchronous serial mode":
//!
//! 1. Over SPI, load a register preset (on/off keying, no packet handling),
//!    set the carrier frequency and the output power.
//! 2. Put the chip in transmit state.
//! 3. Now the chip's GDO0 pin is a plain input: while the Pi holds it high
//!    the carrier is on, while it is low the carrier is off. We toggle it
//!    with the recorded timings.
//!
//! The register values are the Flipper firmware's
//! `subghz_device_cc1101_preset_ook_270khz_async_regs` and `..._650khz_...`
//! tables, so the chip is configured exactly as it was during the capture.
//!
//! # Structure
//!
//! * `Cc1101Io` is the tiny hardware interface the driver needs: one SPI
//!   exchange and control of the GDO0 line.
//! * `PiIo` implements it with `rppal` on a real Raspberry Pi.
//! * `Cc1101` is the driver. It only talks to a `Cc1101Io`, so the tests at
//!   the bottom can run it against a simulated chip on any computer.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use rppal::gpio::{Gpio, OutputPin};
use rppal::spi::{Bus, Mode, SlaveSelect, Spi};

use crate::signal::Pulse;
use crate::subfile::Preset;
use crate::transmitter::{Transmission, Transmitter};

// ---------------------------------------------------------------------------
// Register map (CC1101 datasheet, tables 43-45). Only what we use.
// ---------------------------------------------------------------------------

/// SPI header bit: read instead of write.
const READ: u8 = 0x80;
/// SPI header bit: burst access. Also required to read a status register.
const BURST: u8 = 0x40;

// Configuration registers.
const IOCFG0: u8 = 0x02;
const FIFOTHR: u8 = 0x03;
const PKTCTRL0: u8 = 0x08;
const FSCTRL1: u8 = 0x0B;
const FREQ2: u8 = 0x0D;
const FREQ1: u8 = 0x0E;
const FREQ0: u8 = 0x0F;
const MDMCFG4: u8 = 0x10;
const MDMCFG3: u8 = 0x11;
const MDMCFG2: u8 = 0x12;
const MDMCFG1: u8 = 0x13;
const MDMCFG0: u8 = 0x14;
const MCSM0: u8 = 0x18;
const FOCCFG: u8 = 0x19;
const AGCCTRL2: u8 = 0x1B;
const AGCCTRL1: u8 = 0x1C;
const AGCCTRL0: u8 = 0x1D;
const WORCTRL: u8 = 0x20;
const FREND1: u8 = 0x21;
const FREND0: u8 = 0x22;
/// Output power table (8 bytes, written in one burst).
const PATABLE: u8 = 0x3E;

// Command strobes: single-byte commands.
const SRES: u8 = 0x30; // reset
const STX: u8 = 0x35; // start transmitting
const SIDLE: u8 = 0x36; // stop, go to idle
const SNOP: u8 = 0x3D; // do nothing (used to read the status byte)

// Status registers (read with the BURST bit set).
const PARTNUM: u8 = 0x30;
const VERSION: u8 = 0x31;

/// GDO0 pin function: high impedance, i.e. the chip does not drive the pin.
const IOCFG_HIGH_IMPEDANCE: u8 = 0x2E;

/// The chip's state as reported in bits 6..4 of every status byte.
const STATE_IDLE: u8 = 0b000;
const STATE_TX: u8 = 0b010;

/// Register values for `FuriHalSubGhzPresetOok270Async`.
const OOK_270_ASYNC: [(u8, u8); 16] = [
    (IOCFG0, 0x0D),   // GDO0 = asynchronous serial data
    (FIFOTHR, 0x47),  // ADC retention; FIFO thresholds unused
    (PKTCTRL0, 0x32), // asynchronous serial mode, no CRC, no whitening
    (FSCTRL1, 0x06),  // intermediate frequency 152 kHz
    (MDMCFG0, 0x00),  // channel spacing (unused)
    (MDMCFG1, 0x00),  // no preamble
    (MDMCFG2, 0x30),  // ASK/OOK modulation, no sync word
    (MDMCFG3, 0x32),  // data rate 3.79 kBaud (GDO0 is sampled at 8x this)
    (MDMCFG4, 0x67),  // receive bandwidth 270 kHz
    (MCSM0, 0x18),    // calibrate the synthesizer when leaving idle
    (FOCCFG, 0x18),   // no frequency offset compensation
    (AGCCTRL0, 0x40), // receiver gain control (irrelevant for transmit)
    (AGCCTRL1, 0x00),
    (AGCCTRL2, 0x03),
    (WORCTRL, 0xFB),
    (FREND0, 0x11), // OOK: "1" uses PATABLE[1], "0" uses PATABLE[0]
];

/// Register values for `FuriHalSubGhzPresetOok650Async`.
const OOK_650_ASYNC: [(u8, u8); 16] = [
    (IOCFG0, 0x0D),
    (FIFOTHR, 0x07),
    (PKTCTRL0, 0x32),
    (FSCTRL1, 0x06),
    (MDMCFG0, 0x00),
    (MDMCFG1, 0x00),
    (MDMCFG2, 0x30),
    (MDMCFG3, 0x32),
    (MDMCFG4, 0x17), // receive bandwidth 650 kHz
    (MCSM0, 0x18),
    (FOCCFG, 0x18),
    (AGCCTRL0, 0x91),
    (AGCCTRL1, 0x00),
    (AGCCTRL2, 0x07),
    (WORCTRL, 0xFB),
    (FREND0, 0x11),
];

/// Both presets also set this front-end register.
const FREND1_VALUE: u8 = 0xB6;

/// The register table for a preset, or an error for presets we cannot send.
fn preset_registers(preset: &Preset) -> anyhow::Result<&'static [(u8, u8)]> {
    match preset {
        Preset::Ook270Async => Ok(&OOK_270_ASYNC),
        Preset::Ook650Async => Ok(&OOK_650_ASYNC),
        Preset::Other(name) => bail!(
            "preset `{name}` is not supported: only the OOK presets \
             (Ook270Async, Ook650Async) can be transmitted"
        ),
    }
}

/// The three FREQ register bytes for a carrier frequency.
///
/// The chip's frequency is `crystal / 65536 * FREQ`, so
/// `FREQ = frequency * 65536 / crystal`. The division rounds down, exactly
/// like the Flipper firmware, so both devices land on the same channel.
pub fn frequency_registers(frequency_hz: u32, crystal_hz: u32) -> [u8; 3] {
    let word = u64::from(frequency_hz) * 65_536 / u64::from(crystal_hz);
    // `as u8` keeps the lowest 8 bits, which is what we want here.
    [(word >> 16) as u8, (word >> 8) as u8, word as u8]
}

/// The CC1101 only works in three bands. These are the same limits the
/// Flipper uses.
pub fn check_frequency(frequency_hz: u32) -> anyhow::Result<()> {
    let bands = [
        (299_999_755, 348_000_335),
        (386_999_938, 464_000_000),
        (778_999_847, 928_000_000),
    ];
    for (low, high) in bands {
        if (low..=high).contains(&frequency_hz) {
            return Ok(());
        }
    }
    bail!(
        "{frequency_hz} Hz is outside the CC1101's bands \
         (300-348, 387-464 and 779-928 MHz)"
    )
}

// ---------------------------------------------------------------------------
// Hardware interface
// ---------------------------------------------------------------------------

/// The hardware access the driver needs. Implemented by `PiIo` for the real
/// thing and by a simulated chip in the tests.
pub trait Cc1101Io: Send {
    /// One SPI exchange with chip-select held low throughout. `write` is
    /// sent, and the bytes that come back are stored in `read`. Both slices
    /// must have the same length.
    fn spi_transfer(&mut self, write: &[u8], read: &mut [u8]) -> anyhow::Result<()>;

    /// Start driving the GDO0 line from our side, low.
    fn gdo0_claim(&mut self) -> anyhow::Result<()>;

    /// Set the GDO0 line. Only called between `gdo0_claim` and `gdo0_release`.
    fn gdo0_write(&mut self, high: bool);

    /// Stop driving GDO0 (our side becomes an input again).
    fn gdo0_release(&mut self);
}

/// Real hardware access on a Raspberry Pi, through `rppal`.
pub struct PiIo {
    spi: Spi,
    gpio: Gpio,
    gdo0_pin_number: u8,
    /// `Some` while we are driving the line, `None` otherwise. Dropping the
    /// `OutputPin` turns the pin back into an input.
    gdo0: Option<OutputPin>,
}

impl PiIo {
    /// Open the SPI bus and remember which GPIO is wired to GDO0.
    ///
    /// `gdo0_gpio` is a BCM GPIO number (GPIO25 is physical pin 22), not a
    /// physical pin number.
    pub fn open(
        spi_bus: u8,
        chip_select: u8,
        spi_clock_hz: u32,
        gdo0_gpio: u8,
    ) -> anyhow::Result<PiIo> {
        let bus = match spi_bus {
            0 => Bus::Spi0,
            1 => Bus::Spi1,
            other => bail!("unsupported SPI bus {other} (use 0 or 1)"),
        };
        let select = match chip_select {
            0 => SlaveSelect::Ss0,
            1 => SlaveSelect::Ss1,
            2 => SlaveSelect::Ss2,
            other => bail!("unsupported SPI chip select {other} (use 0, 1 or 2)"),
        };
        // The CC1101 uses SPI mode 0 (clock idles low, sample on rising edge).
        let spi = Spi::new(bus, select, spi_clock_hz, Mode::Mode0).with_context(|| {
            format!(
                "cannot open SPI bus {spi_bus} chip select {chip_select}. \
                 Is SPI enabled (see WIRING.md) and is this a Raspberry Pi?"
            )
        })?;
        let gpio = Gpio::new().context("cannot open the GPIO device")?;
        // Check now that the pin exists and is free, so a bad pin number is
        // reported at start-up and not at the first alarm. The pin is given
        // back immediately because we must not drive it yet.
        gpio.get(gdo0_gpio)
            .with_context(|| format!("cannot use GPIO{gdo0_gpio} for GDO0"))?;
        Ok(PiIo {
            spi,
            gpio,
            gdo0_pin_number: gdo0_gpio,
            gdo0: None,
        })
    }
}

impl Cc1101Io for PiIo {
    fn spi_transfer(&mut self, write: &[u8], read: &mut [u8]) -> anyhow::Result<()> {
        self.spi
            .transfer(read, write)
            .context("SPI transfer failed")?;
        Ok(())
    }

    fn gdo0_claim(&mut self) -> anyhow::Result<()> {
        let pin = self
            .gpio
            .get(self.gdo0_pin_number)
            .with_context(|| format!("cannot use GPIO{} for GDO0", self.gdo0_pin_number))?;
        self.gdo0 = Some(pin.into_output_low());
        Ok(())
    }

    fn gdo0_write(&mut self, high: bool) {
        // `if let Some(..)` runs the block only when we hold the pin.
        if let Some(pin) = self.gdo0.as_mut() {
            if high {
                pin.set_high();
            } else {
                pin.set_low();
            }
        }
    }

    fn gdo0_release(&mut self) {
        // Assigning `None` drops the `OutputPin`; rppal then resets the pin
        // to an input.
        self.gdo0 = None;
    }
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// Settings that depend on the module, not on the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cc1101Settings {
    /// Crystal frequency. 26 MHz on nearly every module.
    pub crystal_hz: u32,
    /// PATABLE value used while the carrier is on. 0xC0 is maximum power.
    pub pa_power: u8,
}

impl Default for Cc1101Settings {
    fn default() -> Self {
        Cc1101Settings {
            crystal_hz: 26_000_000,
            pa_power: 0xC0,
        }
    }
}

/// What the chip says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipInfo {
    pub partnum: u8,
    pub version: u8,
}

/// How long we wait for the chip to change state (it needs under 1 ms).
const STATE_TIMEOUT: Duration = Duration::from_millis(50);

pub struct Cc1101 {
    /// `Box<dyn Cc1101Io>`: a heap-allocated value of *some* type that
    /// implements `Cc1101Io`. The driver does not care which.
    io: Box<dyn Cc1101Io>,
    settings: Cc1101Settings,
    info: ChipInfo,
}

impl Cc1101 {
    /// Reset the chip and check that it answers.
    pub fn new(io: Box<dyn Cc1101Io>, settings: Cc1101Settings) -> anyhow::Result<Cc1101> {
        let mut radio = Cc1101 {
            io,
            settings,
            info: ChipInfo {
                partnum: 0,
                version: 0,
            },
        };
        radio.reset()?;
        radio.info = radio.read_chip_info()?;
        // Nothing is wired to MISO, or the module has no power: the bus then
        // reads as all zeros or all ones.
        if radio.info.version == 0x00 || radio.info.version == 0xFF {
            bail!(
                "no CC1101 found on the SPI bus (VERSION register reads 0x{:02X}). \
                 Check the wiring and that the module has 3.3 V power; see WIRING.md",
                radio.info.version
            );
        }
        Ok(radio)
    }

    /// PARTNUM and VERSION as read at start-up. A genuine CC1101 reports
    /// part 0x00 and version 0x14; some clones report version 0x04 or 0x17.
    pub fn chip_info(&self) -> ChipInfo {
        self.info
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        self.strobe(SRES)?;
        thread::sleep(Duration::from_millis(5));
        // After a reset the chip outputs a clock signal on GDO0. Switch that
        // off straight away so the pin is safe for us to drive later.
        self.write_register(IOCFG0, IOCFG_HIGH_IMPEDANCE)?;
        Ok(())
    }

    fn read_chip_info(&mut self) -> anyhow::Result<ChipInfo> {
        Ok(ChipInfo {
            partnum: self.read_status_register(PARTNUM)?,
            version: self.read_status_register(VERSION)?,
        })
    }

    /// Send a one-byte command. Returns the chip's status byte.
    fn strobe(&mut self, command: u8) -> anyhow::Result<u8> {
        let mut read = [0u8; 1];
        self.io.spi_transfer(&[command], &mut read)?;
        Ok(read[0])
    }

    fn write_register(&mut self, address: u8, value: u8) -> anyhow::Result<()> {
        let mut read = [0u8; 2];
        self.io.spi_transfer(&[address, value], &mut read)
    }

    fn read_status_register(&mut self, address: u8) -> anyhow::Result<u8> {
        let mut read = [0u8; 2];
        self.io
            .spi_transfer(&[address | READ | BURST, 0x00], &mut read)?;
        Ok(read[1])
    }

    /// Poll the status byte until the chip reaches `wanted_state`.
    fn wait_for_state(&mut self, wanted_state: u8, name: &str) -> anyhow::Result<()> {
        let started = Instant::now();
        loop {
            let status = self.strobe(SNOP)?;
            let state = (status >> 4) & 0b111;
            if state == wanted_state {
                return Ok(());
            }
            if started.elapsed() > STATE_TIMEOUT {
                bail!(
                    "CC1101 did not reach the {name} state \
                     (status byte 0x{status:02X}); check wiring and power"
                );
            }
            thread::sleep(Duration::from_micros(200));
        }
    }

    /// Load the preset, frequency and output power.
    fn configure(&mut self, frequency_hz: u32, preset: &Preset) -> anyhow::Result<()> {
        let registers = preset_registers(preset)?;
        self.strobe(SIDLE)?;
        self.wait_for_state(STATE_IDLE, "idle")?;

        for &(address, value) in registers {
            self.write_register(address, value)?;
        }
        self.write_register(FREND1, FREND1_VALUE)?;

        let [freq2, freq1, freq0] = frequency_registers(frequency_hz, self.settings.crystal_hz);
        self.write_register(FREQ2, freq2)?;
        self.write_register(FREQ1, freq1)?;
        self.write_register(FREQ0, freq0)?;

        // Power table: entry 0 is used for "carrier off", entry 1 for "on".
        let patable = [0x00, self.settings.pa_power, 0, 0, 0, 0, 0, 0];
        let mut write = [0u8; 9];
        write[0] = PATABLE | BURST;
        write[1..].copy_from_slice(&patable);
        let mut read = [0u8; 9];
        self.io.spi_transfer(&write, &mut read)?;
        Ok(())
    }

    /// The part of `transmit` between claiming GDO0 and releasing it.
    fn key_carrier(&mut self, pulses: &[Pulse]) -> anyhow::Result<()> {
        self.strobe(STX)?;
        self.wait_for_state(STATE_TX, "transmit")?;

        // Raise our thread priority for the timing-critical part. The guard
        // puts it back when it goes out of scope at the end of this block.
        {
            let _priority = RealtimePriority::try_enable();
            play_pulses(self.io.as_mut(), pulses);
        }
        Ok(())
    }
}

impl Transmitter for Cc1101 {
    fn name(&self) -> &str {
        "cc1101"
    }

    fn transmit(&mut self, transmission: &Transmission) -> anyhow::Result<()> {
        check_frequency(transmission.frequency_hz)?;
        // Checked before touching the radio, so a bad preset has no effect.
        preset_registers(&transmission.preset)?;

        self.configure(transmission.frequency_hz, &transmission.preset)?;

        // Drive GDO0 low *before* entering transmit state, so the carrier is
        // off until the first pulse.
        self.io.gdo0_claim()?;
        let keyed = self.key_carrier(&transmission.pulses);

        // Always shut down in the same order, whether keying worked or not:
        // carrier off, radio to idle, let go of the pin, chip pin to high-Z.
        self.io.gdo0_write(false);
        let idle = self.strobe(SIDLE);
        self.io.gdo0_release();
        let parked = self.write_register(IOCFG0, IOCFG_HIGH_IMPEDANCE);

        // Report the first thing that went wrong, if anything did.
        keyed?;
        idle?;
        parked?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Waits longer than this are mostly slept; shorter ones are busy-waited.
const SLEEP_THRESHOLD: Duration = Duration::from_millis(3);
/// How much of a long wait is left for busy-waiting after the sleep, to
/// absorb a late wake-up.
const SPIN_MARGIN: Duration = Duration::from_millis(2);

/// Toggle GDO0 following `pulses`, then leave it low.
///
/// Linux is not a real-time system, and `thread::sleep` on a Pi Zero can
/// easily be a millisecond late, which is longer than a whole bit. Two
/// things keep the timing usable:
///
/// * **Busy-waiting.** For short pulses we spin in a loop reading the clock
///   and never give the CPU away.
/// * **Absolute deadlines.** Each pulse ends at "start + sum of all pulses so
///   far", not "now + this pulse". A late edge therefore does not push every
///   later edge back; errors do not add up.
fn play_pulses(io: &mut dyn Cc1101Io, pulses: &[Pulse]) {
    let mut deadline = Instant::now();
    for pulse in pulses {
        io.gdo0_write(pulse.high);
        deadline += Duration::from_micros(u64::from(pulse.micros));
        wait_until(deadline);
    }
    io.gdo0_write(false);
}

fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline - now;
        if remaining > SLEEP_THRESHOLD {
            // A long gap between frames: let other programs run for most of
            // it, then busy-wait the rest.
            thread::sleep(remaining - SPIN_MARGIN);
        } else {
            // Tells the CPU we are in a spin loop; does not yield the thread.
            std::hint::spin_loop();
        }
    }
}

/// Gives the current thread real-time priority while it exists.
///
/// With the `SCHED_FIFO` policy the kernel will not pause our thread to run
/// an ordinary program, which removes most timing glitches. It needs the
/// `CAP_SYS_NICE` capability (the systemd unit grants it). Without it we
/// carry on at normal priority; the repeats usually make up for a glitch.
struct RealtimePriority {
    enabled: bool,
}

impl RealtimePriority {
    fn try_enable() -> RealtimePriority {
        let param = libc::sched_param { sched_priority: 50 };
        // SAFETY: `sched_setscheduler` only reads the struct we pass. Pid 0
        // means "the calling thread".
        let result = unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) };
        let enabled = result == 0;
        if !enabled {
            log::debug!(
                "real-time priority not available ({}); timing may jitter",
                std::io::Error::last_os_error()
            );
        }
        RealtimePriority { enabled }
    }
}

// `Drop` runs when the value goes out of scope, even on an early return.
impl Drop for RealtimePriority {
    fn drop(&mut self) {
        if self.enabled {
            let param = libc::sched_param { sched_priority: 0 };
            // SAFETY: as above.
            unsafe { libc::sched_setscheduler(0, libc::SCHED_OTHER, &param) };
        }
    }
}

// ---------------------------------------------------------------------------
// Tests, against a simulated chip
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What the simulated chip remembers. Shared with the test through an
    /// `Arc<Mutex<..>>` because the driver owns the `FakeChip` itself.
    #[derive(Debug)]
    struct ChipState {
        registers: [u8; 0x30],
        patable: Vec<u8>,
        strobes: Vec<u8>,
        state: u8,
        version: u8,
        /// If false, STX is ignored (simulates a chip that never transmits).
        tx_works: bool,
        pin_claimed: bool,
        claims: u32,
        /// Every GDO0 write: (time, level, chip was in transmit state).
        edges: Vec<(Instant, bool, bool)>,
        writes_while_unclaimed: u32,
    }

    struct FakeChip {
        shared: Arc<Mutex<ChipState>>,
    }

    fn fake_chip(version: u8) -> (FakeChip, Arc<Mutex<ChipState>>) {
        let shared = Arc::new(Mutex::new(ChipState {
            registers: [0; 0x30],
            patable: Vec::new(),
            strobes: Vec::new(),
            state: STATE_IDLE,
            version,
            tx_works: true,
            pin_claimed: false,
            claims: 0,
            edges: Vec::new(),
            writes_while_unclaimed: 0,
        }));
        let chip = FakeChip {
            shared: Arc::clone(&shared),
        };
        (chip, shared)
    }

    impl Cc1101Io for FakeChip {
        fn spi_transfer(&mut self, write: &[u8], read: &mut [u8]) -> anyhow::Result<()> {
            assert_eq!(write.len(), read.len(), "SPI buffers must match");
            let mut chip = self.shared.lock().unwrap();
            let header = write[0];
            let address = header & 0x3F;
            let is_read = header & READ != 0;
            let is_burst = header & BURST != 0;

            // Every exchange returns the status byte first.
            read[0] = chip.state << 4;

            if (0x30..=0x3D).contains(&address) && !is_burst {
                // A command strobe.
                chip.strobes.push(address);
                match address {
                    SRES => {
                        chip.registers = [0; 0x30];
                        chip.registers[IOCFG0 as usize] = 0x3F; // clock output!
                        chip.state = STATE_IDLE;
                    }
                    STX if chip.tx_works => chip.state = STATE_TX,
                    SIDLE => chip.state = STATE_IDLE,
                    _ => {}
                }
            } else if (0x30..=0x3D).contains(&address) && is_read {
                // A status register.
                read[1] = match address {
                    PARTNUM => 0x00,
                    VERSION => chip.version,
                    _ => 0,
                };
            } else if address == PATABLE {
                chip.patable = write[1..].to_vec();
            } else if is_read {
                read[1] = chip.registers[address as usize];
            } else {
                chip.registers[address as usize] = write[1];
            }
            Ok(())
        }

        fn gdo0_claim(&mut self) -> anyhow::Result<()> {
            let mut chip = self.shared.lock().unwrap();
            chip.pin_claimed = true;
            chip.claims += 1;
            Ok(())
        }

        fn gdo0_write(&mut self, high: bool) {
            let mut chip = self.shared.lock().unwrap();
            if !chip.pin_claimed {
                chip.writes_while_unclaimed += 1;
            }
            let transmitting = chip.state == STATE_TX;
            chip.edges.push((Instant::now(), high, transmitting));
        }

        fn gdo0_release(&mut self) {
            self.shared.lock().unwrap().pin_claimed = false;
        }
    }

    fn transmission(pulses: Vec<Pulse>) -> Transmission {
        Transmission {
            frequency_hz: 304_250_000,
            preset: Preset::Ook270Async,
            pulses,
        }
    }

    #[test]
    fn frequency_word_matches_known_values() {
        // 304.25 MHz, the frequency in the capture files.
        assert_eq!(
            frequency_registers(304_250_000, 26_000_000),
            [0x0B, 0xB3, 0xB1]
        );
        // Two widely published values for a 26 MHz crystal.
        assert_eq!(
            frequency_registers(433_920_000, 26_000_000),
            [0x10, 0xB0, 0x71]
        );
        assert_eq!(
            frequency_registers(315_000_000, 26_000_000),
            [0x0C, 0x1D, 0x89]
        );
    }

    #[test]
    fn frequency_word_is_within_one_step_of_the_target() {
        let [f2, f1, f0] = frequency_registers(304_250_000, 26_000_000);
        let word = (u64::from(f2) << 16) | (u64::from(f1) << 8) | u64::from(f0);
        let actual_hz = word * 26_000_000 / 65_536;
        // One step of the synthesizer is 26 MHz / 65536 = 397 Hz.
        assert!(304_250_000 - actual_hz < 397, "actual {actual_hz} Hz");
    }

    #[test]
    fn frequency_must_be_inside_a_band() {
        assert!(check_frequency(304_250_000).is_ok());
        assert!(check_frequency(433_920_000).is_ok());
        assert!(check_frequency(868_350_000).is_ok());
        assert!(check_frequency(200_000_000).is_err());
        assert!(check_frequency(350_000_000).is_err());
        assert!(check_frequency(2_400_000_000).is_err());
    }

    #[test]
    fn new_resets_the_chip_and_parks_gdo0() {
        let (chip, shared) = fake_chip(0x14);
        let radio = Cc1101::new(Box::new(chip), Cc1101Settings::default()).unwrap();
        assert_eq!(
            radio.chip_info(),
            ChipInfo {
                partnum: 0x00,
                version: 0x14
            }
        );
        let chip = shared.lock().unwrap();
        assert_eq!(chip.strobes[0], SRES);
        assert_eq!(chip.registers[IOCFG0 as usize], IOCFG_HIGH_IMPEDANCE);
        assert_eq!(chip.claims, 0, "must not drive GDO0 during start-up");
    }

    #[test]
    fn new_fails_clearly_when_no_chip_answers() {
        for version in [0x00, 0xFF] {
            let (chip, _shared) = fake_chip(version);
            // `Cc1101` has no `Debug` impl, so we cannot call `unwrap_err`;
            // matching on the result works for any type.
            let message = match Cc1101::new(Box::new(chip), Cc1101Settings::default()) {
                Ok(_) => panic!("expected an error for version 0x{version:02X}"),
                Err(error) => error.to_string(),
            };
            assert!(message.contains("no CC1101 found"), "{message}");
        }
    }

    #[test]
    fn transmit_configures_the_chip_like_the_flipper_preset() {
        let (chip, shared) = fake_chip(0x14);
        let mut radio = Cc1101::new(Box::new(chip), Cc1101Settings::default()).unwrap();
        radio
            .transmit(&transmission(vec![Pulse::high(300), Pulse::low(300)]))
            .unwrap();

        let chip = shared.lock().unwrap();
        assert_eq!(chip.registers[PKTCTRL0 as usize], 0x32, "async serial mode");
        assert_eq!(chip.registers[MDMCFG2 as usize], 0x30, "OOK modulation");
        assert_eq!(chip.registers[MDMCFG4 as usize], 0x67, "270 kHz preset");
        assert_eq!(chip.registers[MDMCFG3 as usize], 0x32);
        assert_eq!(chip.registers[FREND0 as usize], 0x11);
        assert_eq!(chip.registers[FREND1 as usize], 0xB6);
        assert_eq!(chip.registers[FREQ2 as usize], 0x0B);
        assert_eq!(chip.registers[FREQ1 as usize], 0xB3);
        assert_eq!(chip.registers[FREQ0 as usize], 0xB1);
        assert_eq!(chip.patable, vec![0x00, 0xC0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn the_650_khz_preset_differs_only_where_expected() {
        let (chip, shared) = fake_chip(0x14);
        let mut radio = Cc1101::new(Box::new(chip), Cc1101Settings::default()).unwrap();
        let mut tx = transmission(vec![Pulse::high(300), Pulse::low(300)]);
        tx.preset = Preset::Ook650Async;
        radio.transmit(&tx).unwrap();

        let chip = shared.lock().unwrap();
        assert_eq!(chip.registers[MDMCFG4 as usize], 0x17);
        assert_eq!(chip.registers[MDMCFG2 as usize], 0x30);
        assert_eq!(chip.registers[PKTCTRL0 as usize], 0x32);
    }

    #[test]
    fn power_setting_reaches_the_power_table() {
        let (chip, shared) = fake_chip(0x14);
        let settings = Cc1101Settings {
            crystal_hz: 26_000_000,
            pa_power: 0x50,
        };
        let mut radio = Cc1101::new(Box::new(chip), settings).unwrap();
        radio
            .transmit(&transmission(vec![Pulse::high(300)]))
            .unwrap();
        assert_eq!(shared.lock().unwrap().patable[1], 0x50);
    }

    #[test]
    fn transmit_keys_gdo0_only_while_the_chip_is_transmitting() {
        let (chip, shared) = fake_chip(0x14);
        let mut radio = Cc1101::new(Box::new(chip), Cc1101Settings::default()).unwrap();
        let pulses = vec![
            Pulse::high(400),
            Pulse::low(800),
            Pulse::high(800),
            Pulse::low(400),
        ];
        radio.transmit(&transmission(pulses)).unwrap();

        let chip = shared.lock().unwrap();
        assert_eq!(chip.writes_while_unclaimed, 0);
        assert_eq!(chip.claims, 1);
        assert!(!chip.pin_claimed, "GDO0 must be released afterwards");

        // The levels we asked for, then low twice: once at the end of the
        // pulse list and once more during shutdown.
        let levels: Vec<bool> = chip.edges.iter().map(|edge| edge.1).collect();
        assert_eq!(levels, vec![true, false, true, false, false, false]);
        // Every "carrier on" write happened while the chip was transmitting.
        for (_, high, transmitting) in &chip.edges {
            if *high {
                assert!(transmitting);
            }
        }

        // Afterwards: idle, and the chip no longer drives GDO0.
        assert_eq!(chip.state, STATE_IDLE);
        assert_eq!(chip.registers[IOCFG0 as usize], IOCFG_HIGH_IMPEDANCE);
        // STX was sent, and the very last strobe is SIDLE.
        assert!(chip.strobes.contains(&STX));
        assert_eq!(*chip.strobes.last().unwrap(), SIDLE);
    }

    #[test]
    fn pulses_are_never_cut_short() {
        let (chip, shared) = fake_chip(0x14);
        let mut radio = Cc1101::new(Box::new(chip), Cc1101Settings::default()).unwrap();
        // Includes one long pause to exercise the sleep-then-spin path.
        let pulses = vec![
            Pulse::high(500),
            Pulse::low(1_000),
            Pulse::high(500),
            Pulse::low(6_000),
            Pulse::high(500),
            Pulse::low(500),
        ];
        let requested: u64 = pulses.iter().map(|p| u64::from(p.micros)).sum();
        radio.transmit(&transmission(pulses)).unwrap();

        let chip = shared.lock().unwrap();
        let first = chip.edges[0].0;
        // edges[6] is the "carrier off" write after the last pulse.
        let total = chip.edges[6].0.duration_since(first);
        // The driver starts its clock just before the first edge and the fake
        // chip stamps the edge just after, so allow a hair under the total.
        let measuring_slack = Duration::from_micros(100);
        assert!(
            total + measuring_slack >= Duration::from_micros(requested),
            "burst took {total:?}, requested {requested} us"
        );
        // Deadlines are absolute, so the whole burst must not run far over
        // either. The limit is generous because test machines are noisy.
        assert!(
            total < Duration::from_micros(requested) + Duration::from_millis(50),
            "burst took {total:?}"
        );
    }

    #[test]
    fn a_chip_that_never_transmits_is_an_error_and_is_cleaned_up() {
        let (chip, shared) = fake_chip(0x14);
        shared.lock().unwrap().tx_works = false;
        let mut radio = Cc1101::new(Box::new(chip), Cc1101Settings::default()).unwrap();

        let error = radio
            .transmit(&transmission(vec![Pulse::high(400), Pulse::low(400)]))
            .unwrap_err();
        assert!(error.to_string().contains("transmit state"), "{error}");

        let chip = shared.lock().unwrap();
        assert!(!chip.pin_claimed, "GDO0 must be released after a failure");
        assert!(
            chip.edges.iter().all(|edge| !edge.1),
            "the carrier must never be keyed on"
        );
        assert_eq!(chip.registers[IOCFG0 as usize], IOCFG_HIGH_IMPEDANCE);
    }

    #[test]
    fn unsupported_presets_and_frequencies_do_not_touch_the_radio() {
        let (chip, shared) = fake_chip(0x14);
        let mut radio = Cc1101::new(Box::new(chip), Cc1101Settings::default()).unwrap();
        let strobes_before = shared.lock().unwrap().strobes.len();

        let mut fsk = transmission(vec![Pulse::high(400)]);
        fsk.preset = Preset::Other("FuriHalSubGhzPreset2FSKDev238Async".to_string());
        let error = radio.transmit(&fsk).unwrap_err();
        assert!(error.to_string().contains("not supported"), "{error}");

        let mut off_band = transmission(vec![Pulse::high(400)]);
        off_band.frequency_hz = 350_000_000;
        assert!(radio.transmit(&off_band).is_err());

        let chip = shared.lock().unwrap();
        assert_eq!(chip.strobes.len(), strobes_before);
        assert_eq!(chip.claims, 0);
    }
}
