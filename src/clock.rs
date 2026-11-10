//! Reading and setting the time.
//!
//! The Pi Zero W has no battery-backed clock. After a power cut it wakes up
//! with a stale time and only learns the real one from the network (NTP) or
//! from the phone, which sends its time every time it connects.
//!
//! All code asks a `Clock` for the time instead of the operating system
//! directly. That lets tests use a `MockClock` they can move around freely,
//! and it means no test can ever change the real system clock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::bail;
use chrono::{DateTime, Utc};

/// A source of time that can also be corrected.
///
/// `Send + Sync` means a clock may be used from several threads at once,
/// which the daemon does.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    /// Jump the clock to `time`.
    fn set(&self, time: DateTime<Utc>) -> anyhow::Result<()>;

    /// True once we have a reason to believe the time: the kernel says NTP
    /// has synchronised it, or `set` was called since the program started.
    fn is_synced(&self) -> bool;
}

/// The real system clock.
#[derive(Debug, Default)]
pub struct SystemClock {
    /// `AtomicBool` is a bool that can be changed through a shared `&self`
    /// reference, safely from any thread.
    set_since_start: AtomicBool,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock::default()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn set(&self, time: DateTime<Utc>) -> anyhow::Result<()> {
        // The field types differ between 64-bit PCs and the 32-bit Pi, so we
        // convert with `as` to whatever this platform uses.
        let spec = libc::timespec {
            tv_sec: time.timestamp() as libc::time_t,
            tv_nsec: time.timestamp_subsec_nanos() as libc::c_long,
        };
        // SAFETY: `clock_settime` only reads the struct we pass a pointer to.
        let result = unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &spec) };
        if result != 0 {
            // Needs root or the CAP_SYS_TIME capability (the systemd unit
            // grants it).
            bail!(
                "cannot set the system clock: {}",
                std::io::Error::last_os_error()
            );
        }
        self.set_since_start.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn is_synced(&self) -> bool {
        self.set_since_start.load(Ordering::Relaxed) || kernel_says_ntp_synced()
    }
}

/// Ask the kernel whether its clock is being kept in sync by NTP.
fn kernel_says_ntp_synced() -> bool {
    // SAFETY: `timex` is a plain C struct of numbers, for which all-zero
    // bytes is a valid value. With `modes == 0`, `adjtimex` changes nothing
    // and only fills the struct in.
    let state = unsafe {
        let mut timex: libc::timex = std::mem::zeroed();
        libc::adjtimex(&mut timex)
    };
    // -1 is an error, TIME_ERROR means "not synchronised".
    state != -1 && state != libc::TIME_ERROR
}

/// A clock for tests: it shows whatever time it was last given.
#[derive(Debug)]
pub struct MockClock {
    now: Mutex<DateTime<Utc>>,
    set_calls: Mutex<Vec<DateTime<Utc>>>,
    refuse_set: AtomicBool,
    synced: AtomicBool,
}

impl MockClock {
    pub fn new(now: DateTime<Utc>) -> MockClock {
        MockClock {
            now: Mutex::new(now),
            set_calls: Mutex::new(Vec::new()),
            refuse_set: AtomicBool::new(false),
            synced: AtomicBool::new(false),
        }
    }

    /// Move the clock without recording it as a `set` call (time passing).
    pub fn advance_to(&self, time: DateTime<Utc>) {
        *self.now.lock().unwrap() = time;
    }

    /// Every time `set` was called with.
    pub fn set_calls(&self) -> Vec<DateTime<Utc>> {
        self.set_calls.lock().unwrap().clone()
    }

    /// Make `set` fail, as it does without permission to change the clock.
    pub fn refuse_set(&self, refuse: bool) {
        self.refuse_set.store(refuse, Ordering::Relaxed);
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }

    fn set(&self, time: DateTime<Utc>) -> anyhow::Result<()> {
        if self.refuse_set.load(Ordering::Relaxed) {
            bail!("cannot set the system clock: operation not permitted (mock)");
        }
        *self.now.lock().unwrap() = time;
        self.set_calls.lock().unwrap().push(time);
        self.synced.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn is_synced(&self) -> bool {
        self.synced.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn mock_clock_can_be_set_and_advanced() {
        let start = Utc.with_ymd_and_hms(2026, 11, 10, 6, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 11, 10, 7, 0, 0).unwrap();
        let clock = MockClock::new(start);
        assert_eq!(clock.now(), start);
        assert!(!clock.is_synced());

        clock.advance_to(later);
        assert_eq!(clock.now(), later);
        assert!(clock.set_calls().is_empty(), "advancing is not setting");
        assert!(!clock.is_synced());

        clock.set(start).unwrap();
        assert_eq!(clock.now(), start);
        assert_eq!(clock.set_calls(), vec![start]);
        assert!(clock.is_synced());
    }

    #[test]
    fn mock_clock_can_refuse_to_be_set() {
        let start = Utc.with_ymd_and_hms(2026, 11, 10, 6, 0, 0).unwrap();
        let clock = MockClock::new(start);
        clock.refuse_set(true);
        assert!(clock.set(start + chrono::Duration::hours(1)).is_err());
        assert_eq!(clock.now(), start);
        assert!(!clock.is_synced());
    }

    #[test]
    fn system_clock_reads_a_plausible_time() {
        // Only reads the clock. Nothing in the test suite calls
        // `SystemClock::set`.
        let clock = SystemClock::new();
        let now = clock.now();
        assert!(now > Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap());
        // Must not crash, whatever the answer is on this machine.
        let _ = clock.is_synced();
    }
}
