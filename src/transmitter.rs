//! The seam between "what to send" and "how it gets on air".
//!
//! Everything above this file (scheduler, BLE, CLI) only knows the
//! `Transmitter` trait. On the Pi the trait is implemented by the CC1101
//! driver; in tests and on a dev machine it is implemented by
//! `MockTransmitter`, which just remembers what it was asked to send.

use std::sync::{Arc, Mutex};

use crate::signal::{total_micros, Pulse};
use crate::subfile::Preset;

/// One complete transmission: tune to a frequency, then key the carrier on
/// and off following `pulses`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmission {
    pub frequency_hz: u32,
    pub preset: Preset,
    /// Already includes every repeat and the gaps between them.
    pub pulses: Vec<Pulse>,
}

impl Transmission {
    /// Time on air in microseconds.
    pub fn duration_micros(&self) -> u64 {
        total_micros(&self.pulses)
    }
}

/// Something that can put a `Transmission` on air.
///
/// `Send` is a marker trait meaning "safe to move to another thread". The
/// daemon transmits from a worker thread, so every implementation must be
/// `Send`; writing it here makes the compiler check that for us.
pub trait Transmitter: Send {
    /// Short name for log messages, e.g. `"cc1101"` or `"mock"`.
    fn name(&self) -> &str;

    /// Send the transmission. Blocks until it is finished.
    fn transmit(&mut self, transmission: &Transmission) -> anyhow::Result<()>;
}

/// A fake transmitter for tests and for running on a machine with no radio.
///
/// The list of "sent" transmissions lives behind `Arc<Mutex<...>>`:
/// * `Arc` (atomic reference count) lets several owners share one list, so a
///   test can keep a handle to it after giving the transmitter away.
/// * `Mutex` makes sure only one of them touches the list at a time.
#[derive(Debug, Default)]
pub struct MockTransmitter {
    sent: Arc<Mutex<Vec<Transmission>>>,
    fail: Arc<Mutex<bool>>,
}

impl MockTransmitter {
    pub fn new() -> MockTransmitter {
        MockTransmitter::default()
    }

    /// A second handle to the same list of sent transmissions.
    pub fn sent_log(&self) -> SentLog {
        SentLog {
            // `Arc::clone` copies the pointer, not the list.
            sent: Arc::clone(&self.sent),
            fail: Arc::clone(&self.fail),
        }
    }
}

impl Transmitter for MockTransmitter {
    fn name(&self) -> &str {
        "mock"
    }

    fn transmit(&mut self, transmission: &Transmission) -> anyhow::Result<()> {
        if *self.fail.lock().unwrap() {
            anyhow::bail!("mock transmitter was told to fail");
        }
        log::info!(
            "mock transmit: {} Hz, {} pulses, {:.1} ms",
            transmission.frequency_hz,
            transmission.pulses.len(),
            transmission.duration_micros() as f64 / 1000.0
        );
        // `lock()` waits for the mutex. It only returns an error if another
        // thread panicked while holding the lock, so `unwrap` is fine here.
        self.sent.lock().unwrap().push(transmission.clone());
        Ok(())
    }
}

/// Lets a test look at what a `MockTransmitter` sent, and make it fail.
#[derive(Debug, Clone)]
pub struct SentLog {
    sent: Arc<Mutex<Vec<Transmission>>>,
    fail: Arc<Mutex<bool>>,
}

impl SentLog {
    /// A copy of everything sent so far, oldest first.
    pub fn all(&self) -> Vec<Transmission> {
        self.sent.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    /// Make every following `transmit` call return an error (or stop doing so).
    pub fn set_failing(&self, failing: bool) {
        *self.fail.lock().unwrap() = failing;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Transmission {
        Transmission {
            frequency_hz: 304_250_000,
            preset: Preset::Ook270Async,
            pulses: vec![Pulse::high(400), Pulse::low(800)],
        }
    }

    #[test]
    fn mock_records_what_it_sends() {
        let mut mock = MockTransmitter::new();
        let log = mock.sent_log();
        assert_eq!(log.count(), 0);

        mock.transmit(&sample()).unwrap();
        mock.transmit(&sample()).unwrap();

        assert_eq!(log.count(), 2);
        assert_eq!(log.all()[0], sample());
        assert_eq!(sample().duration_micros(), 1200);
    }

    #[test]
    fn mock_can_be_told_to_fail() {
        let mut mock = MockTransmitter::new();
        let log = mock.sent_log();
        log.set_failing(true);
        assert!(mock.transmit(&sample()).is_err());
        assert_eq!(log.count(), 0);

        log.set_failing(false);
        assert!(mock.transmit(&sample()).is_ok());
        assert_eq!(log.count(), 1);
    }

    #[test]
    fn a_boxed_mock_works_through_the_trait() {
        // `Box<dyn Transmitter>` is how the rest of the program holds a
        // transmitter without knowing which kind it is.
        let mock = MockTransmitter::new();
        let log = mock.sent_log();
        let mut boxed: Box<dyn Transmitter> = Box::new(mock);
        boxed.transmit(&sample()).unwrap();
        assert_eq!(boxed.name(), "mock");
        assert_eq!(log.count(), 1);
    }
}
