//! pi-alarm: a bedroom wake-up alarm that replays fan/light remote signals.
//!
//! The crate is split into a library (this file and its modules) and a small
//! binary (`main.rs`) so that integration tests in `tests/` can use the same
//! code as the program.
//!
//! Reading order for someone new to the code:
//! `subfile` -> `signal` -> `transmitter` -> `cc1101` -> `config` -> `radio`.

pub mod cc1101;
pub mod config;
pub mod radio;
pub mod signal;
pub mod subfile;
pub mod transmitter;
