# Learning notes

The code was written to be read by someone who knows ownership and borrowing
and not much more yet. No macros of our own, one small generic function
(`parse_params` in `protocol.rs`), and comments wherever the Rust is not
obvious.

## Reading order

Start at the top; each file only uses ideas from the ones above it.

1. `subfile.rs` → `signal.rs` → `transmitter.rs` → `cc1101.rs` → `config.rs`
   → `radio.rs`
2. `alarm.rs` → `store.rs` → `schedule.rs` → `scheduler.rs` → `clock.rs`
3. `app.rs` → `protocol.rs` → `ble.rs` → `daemon.rs` → `main.rs`

Every module ends with a `mod tests`. The tests are short and are the
quickest way to see how a function is meant to be used. Run one module's
tests with, for example, `cargo test schedule::`.

## Rust concepts by module

### `subfile.rs` (the .sub parser)
- **Enums that carry data**: `Payload::Raw(Vec<i32>)` vs `Payload::Key(..)`,
  and `Preset::Other(String)`. `match` forces you to handle each case.
- **`Option` for "not seen yet"**: every header field starts as `None` and
  `ok_or(...)` turns a missing one into an error at the end.
- **A hand-written error type**: `SubFileError` with `impl Display` and
  `impl std::error::Error`. Worth reading once before reaching for `anyhow`
  or `thiserror`, which generate the same thing.
- **`let ... else`**: `let Some((name, value)) = line.split_once(':') else { return Err(..) };`
- **Borrowing from `self`**: `Preset::name(&self) -> &str` returns a
  reference that lives as long as the preset.
- **`include_str!`** to compile the real capture files into the tests.

### `signal.rs` (finding the frame)
- **Slices** (`&[i32]`, `&[Pulse]`) as function parameters instead of
  `&Vec<..>`.
- **Iterator chains**: `iter().zip(..).all(..)`, `filter_map`, `chunks(2)`,
  `windows(2)`. Compare `similar` (iterator style) with the plain `for`
  loops in `extract_frame`; both are fine Rust.
- **`#[derive(Clone, Copy)]`** on the tiny `Pulse` struct, so it is copied
  like a number instead of moved.
- **`std::mem::take`** to move a `Vec` out of a variable you keep using.
- **`impl Default`** for an options struct.

### `transmitter.rs` (the trait)
- **Traits and trait objects**: `trait Transmitter`, and
  `Box<dyn Transmitter>` to hold "some transmitter, I don't care which".
- **`Send`** as a supertrait: what it promises and why the daemon needs it.
- **`Arc<Mutex<T>>`** for state shared between a test and the mock it gave
  away.

### `cc1101.rs` (the radio driver)
- **A second trait as a seam for testing**: `Cc1101Io` is implemented by the
  real hardware (`PiIo`) and by a simulated chip in the tests.
- **`const` tables** of register values, `&'static` slices.
- **Bit manipulation**: shifts, masks, `as u8` truncation.
- **`unsafe` with a `// SAFETY:` comment**: two tiny calls into libc.
- **`Drop`** (RAII): `RealtimePriority` restores the thread priority when it
  goes out of scope, even on an early return.
- **Shut-down that always runs**: in `transmit`, the clean-up steps collect
  their `Result`s and report afterwards, instead of using `?` and skipping
  the rest.

### `config.rs`
- **`serde` derive**: `#[derive(Deserialize)]` and the attributes
  `default`, `rename_all`, `deny_unknown_fields`.
- **`PathBuf` vs `&Path`** (owned vs borrowed, like `String` vs `&str`).
- **`BTreeMap`** when you want keys kept in order.

### `radio.rs`
- **`anyhow`**: `Result<T>`, `bail!`, and `.context(...)` /
  `.with_context(|| ...)` to add "while doing X" to an error.
- **Interior mutability**: `Radio::send(&self)` can transmit, although it
  only has a shared reference, because the transmitter sits in a `Mutex`.
- **A builder-style method**: `with_lock_file(mut self, ..) -> Radio`.
- **`matches!`** in tests for enum variants that have no `PartialEq`.

### `alarm.rs`
- **Deriving ordering** (`PartialOrd`, `Ord`) on an enum to make it sortable.
- **Validation returning a typed error** (`Result<(), AlarmError>`).
- **`pub(crate)`** on the test helper so other modules' tests can reuse it.

### `store.rs`
- **File I/O and `std::io::ErrorKind`** (telling "not found" from real
  errors).
- **A lock held by a value**: `let _lock = self.lock()?;` keeps the lock
  until the end of the function; the underscore prefix means "kept for its
  lifetime, not otherwise used".
- **Threads in a test**: `std::thread::spawn` and `join`.

### `schedule.rs` (time arithmetic)
- **Pure functions**: nothing here reads the clock or a file, which is why
  it has the most thorough tests.
- **`chrono` and `chrono-tz`**: `NaiveDate`, `DateTime<Utc>`, converting
  between zones, and the three-way `LocalResult` that makes daylight-saving
  edge cases explicit.
- **`sort_by_key` is stable**, and why that matters here.

### `scheduler.rs`
- **A struct with behaviour and state**, `&mut self` methods.
- **`retain`** to filter a `Vec` in place.
- **Serialising state** with `Serialize`/`Deserialize` on a private struct.
- **Tests as a specification**: each scenario from the module comment
  (restart, clock forward, clock back) is one test with a descriptive name.

### `clock.rs`
- **`Send + Sync`** on a trait, and `AtomicBool` for a flag changed through
  `&self`.
- **Why a trait at all**: so that no test can ever set your system clock.

### `protocol.rs`
- **`serde_json::Value`** for looking at JSON before knowing its shape, then
  `from_value` into a typed struct.
- **`json!`** to build responses.
- **Byte slices**: `split_first`, `chunks`, `extend_from_slice`.
- **The one generic function**, `parse_params<T: DeserializeOwned>`.
- **Errors as values**: `Failure` is turned into an error *response*; the
  handler itself never fails.

### `ble.rs` and `daemon.rs` (async)
- **`async`/`.await`** and the tokio runtime.
- **Channels** (`mpsc`): callbacks send events, one loop receives them.
- **`tokio::select!`**: wait for whichever of several things happens first.
- **`spawn_blocking`**: running slow synchronous code without stalling the
  async tasks.
- **`move` closures and `Arc::clone`**: giving each task its own handle to
  shared data. This is the hardest part of the code base; read it last.

### `main.rs`
- **`clap` derive**: structs and enums become command-line options.
- **`fn main() -> ExitCode`** and printing errors yourself.
- **`collect` into a `Result<Vec<_>>`**: stop at the first error.

## Things to build yourself

Ordered from easiest to hardest. Each lists where to look and how you will
know it works.

### 1. Add fan speeds 1 and 2
No Rust needed for the feature itself, which is the point of the config
design.

1. On the Flipper: **Sub-GHz → Read RAW**, 304.25 MHz, AM270. Hold the
   remote's "1" button for about a second. Save as `1_fan`. Same for "2".
2. `pi-alarm inspect 1_fan.sub`. Expect "found N clean copies" and a code
   starting with `A2E8`.
3. Add to the config:
   ```toml
   [actions.fan_1]
   file = "signals/1_fan.sub"
   label = "Fan 1"
   ```
4. `pi-alarm check`, then `pi-alarm send fan_1`.

*Rust exercise:* copy the files into `flipper_files/` and extend the test
`real_captures_decode_to_the_expected_codes` in `src/signal.rs` with the two
new codes. Then look at the five command bytes side by side in binary. Can
you guess the pattern the remote uses?

### 2. `pi-alarm test-alarm <id>`
Run all the steps of an alarm right now, in order, ignoring their offsets,
so you can try an alarm without waiting for morning.

- Add a variant to `Command` in `src/main.rs` and a `match` arm.
- Load the alarm from the `AlarmStore`, build a `Radio` the way `Send`
  does, loop over `alarm.steps` calling `radio.send(&step.action)`.
- Sleep a second between steps (`std::thread::sleep`).
- Add a test to `tests/cli.rs` (the sandbox there uses the mock radio).

Practises: enums, `match`, `Option` handling, the `?` operator.

### 3. Round the frame to ideal timings
The recorded on-pulses are a little longer than the off-pulses, because the
Flipper's receiver stretches them. Add an option that snaps every pulse to
exactly 1 or 2 units.

- Write `fn idealize(frame: &[Pulse]) -> Vec<Pulse>` in `src/signal.rs`.
  Find the unit (hint: the average of all on+off pairs is 3 units), then map
  each pulse to `unit` or `2 * unit`, whichever is nearer.
- Unit-test it against the three captures: the decoded bits must not change.
- Add `idealize = true` to `ActionConfig` in `src/config.rs` and use it in
  `Action::from_sub_file` in `src/radio.rs`.

Practises: slices, iterators, borrowing a slice and returning an owned `Vec`,
adding a serde field with a default.

### 4. One-shot alarms
An alarm that fires once and then switches itself off ("tomorrow only").

- Add `once: bool` to `Alarm` with `#[serde(default)]`, so old files and old
  app versions still parse.
- After the last step of such an alarm has run, save it with
  `enabled = false`. The place is the loop in `run_scheduler`
  (`src/daemon.rs`): you have `step.alarm_id` and `app.store`.
- Decide what "the last step" means when steps have different offsets, and
  write that down as a test in `src/scheduler.rs` first.
- Add the field to PROTOCOL.md and to the Dart `Alarm` class.

Practises: changing a data type used in many places and letting the
compiler lead you to each one; serde defaults; thinking about state.

### 5. Transmit decoded Princeton keys
`subfile.rs` already parses Flipper "Key" files; `radio.rs` refuses to
transmit them. Princeton is the simplest decoded protocol:

- Each bit is 4 units of `TE` microseconds: a `1` is on for 3 and off for 1;
  a `0` is on for 1 and off for 3. Bits go out most significant first.
- After the last bit comes a sync: on for 1 unit, off for 30.
- Write `fn princeton_pulses(key: u64, bits: u32, te_us: u32) -> Vec<Pulse>`
  in a new `src/princeton.rs`, with tests, then use it in
  `Action::from_sub_file` where it currently returns the "Read RAW" error.
- Check your understanding against the Flipper firmware's
  `lib/subghz/protocols/princeton.c` before trusting the timings above; I
  wrote them from memory and did not verify them.

Practises: bit shifting, building a `Vec`, matching on `Payload`, adding a
module.
