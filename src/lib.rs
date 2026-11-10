//! pi-alarm: a bedroom wake-up alarm that replays fan/light remote signals.
//!
//! The crate is split into a library (this file and its modules) and a small
//! binary (`main.rs`) so that integration tests in `tests/` can use the same
//! code as the program.
//!
//! Reading order for someone new to the code:
//!
//! 1. Radio:   `subfile` -> `signal` -> `transmitter` -> `cc1101` -> `config`
//!    -> `radio`
//! 2. Alarms:  `alarm` -> `store` -> `schedule` -> `scheduler` -> `clock`
//! 3. Service: `app` -> `protocol` -> `ble` -> `daemon` -> `main.rs`
//!
//! LEARNING.md lists the Rust ideas each module is a good example of.

pub mod alarm;
pub mod app;
pub mod ble;
pub mod cc1101;
pub mod clock;
pub mod config;
pub mod daemon;
pub mod protocol;
pub mod radio;
pub mod schedule;
pub mod scheduler;
pub mod signal;
pub mod store;
pub mod subfile;
pub mod transmitter;
