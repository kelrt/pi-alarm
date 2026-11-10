//! The messages the phone and the Pi exchange. See PROTOCOL.md for the
//! human-readable description; this file is the implementation.
//!
//! Two layers:
//!
//! 1. **Framing.** A BLE write or notification carries only a few hundred
//!    bytes at most, sometimes just 20. A message is therefore cut into
//!    chunks, each with a one-byte header, and glued back together on the
//!    other side (`chunk_message`, `Reassembler`).
//! 2. **Messages.** Each message is a small JSON object. The phone sends a
//!    request with an `op` ("list", "put", ...) and gets exactly one
//!    response (`handle_request`).
//!
//! Nothing here knows about Bluetooth. `handle_request` takes bytes and
//! returns bytes, so the CLI (`pi-alarm request`) and the tests can use it
//! directly.

use std::fmt;

use chrono::{DateTime, Offset, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::alarm::{Alarm, MAX_ALARMS};
use crate::app::App;
use crate::radio::SendError;
use crate::schedule::next_step_after;
use crate::store::PutOutcome;

/// Version of the message format. Sent in every message as `"v"`.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Header bit: this chunk starts a message.
pub const CHUNK_FIRST: u8 = 0x01;
/// Header bit: this chunk ends a message.
pub const CHUNK_LAST: u8 = 0x02;

/// Largest message we accept or send, in bytes.
pub const MAX_MESSAGE_LEN: usize = 16 * 1024;

/// Smallest chunk size that works on every BLE connection: the minimum
/// payload of a write or notification is 20 bytes.
pub const MIN_CHUNK_LEN: usize = 20;

/// Cut a message into chunks of at most `max_chunk_len` bytes each,
/// including the one-byte header.
pub fn chunk_message(message: &[u8], max_chunk_len: usize) -> Vec<Vec<u8>> {
    // One byte of every chunk is the header; never go below 1 byte of data.
    let data_len = max_chunk_len.max(2) - 1;

    // `chunks` splits a slice into pieces of the given length (the last one
    // may be shorter). An empty message still needs one (empty) chunk.
    let pieces: Vec<&[u8]> = if message.is_empty() {
        vec![message]
    } else {
        message.chunks(data_len).collect()
    };

    let last_index = pieces.len() - 1;
    let mut chunks = Vec::with_capacity(pieces.len());
    for (index, piece) in pieces.iter().enumerate() {
        let mut header = 0;
        if index == 0 {
            header |= CHUNK_FIRST;
        }
        if index == last_index {
            header |= CHUNK_LAST;
        }
        let mut chunk = Vec::with_capacity(piece.len() + 1);
        chunk.push(header);
        chunk.extend_from_slice(piece);
        chunks.push(chunk);
    }
    chunks
}

/// Why a chunk was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// A chunk with no header byte.
    Empty,
    /// The header has bits set that we do not know.
    BadHeader(u8),
    /// A middle or last chunk arrived without a first chunk before it.
    MissingStart,
    /// The message grew beyond `MAX_MESSAGE_LEN`.
    TooLarge,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Empty => write!(f, "empty chunk"),
            FrameError::BadHeader(header) => write!(f, "unknown chunk header 0x{header:02X}"),
            FrameError::MissingStart => write!(f, "chunk received before the start of a message"),
            FrameError::TooLarge => write!(f, "message is larger than {MAX_MESSAGE_LEN} bytes"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Glues chunks back into messages.
#[derive(Debug, Default)]
pub struct Reassembler {
    buffer: Vec<u8>,
    /// True between a first chunk and its last chunk.
    in_message: bool,
}

impl Reassembler {
    pub fn new() -> Reassembler {
        Reassembler::default()
    }

    /// Forget any half-received message (call when the phone disconnects).
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.in_message = false;
    }

    /// Feed one chunk. Returns `Ok(Some(message))` when it completes a
    /// message, `Ok(None)` when more chunks are needed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Option<Vec<u8>>, FrameError> {
        // `split_first` separates the header byte from the rest.
        let Some((&header, data)) = chunk.split_first() else {
            return Err(FrameError::Empty);
        };
        if header & !(CHUNK_FIRST | CHUNK_LAST) != 0 {
            self.reset();
            return Err(FrameError::BadHeader(header));
        }

        if header & CHUNK_FIRST != 0 {
            // A first chunk always starts afresh. If the previous message was
            // cut off (the phone went out of range), its remains are dropped.
            self.reset();
            self.in_message = true;
        } else if !self.in_message {
            return Err(FrameError::MissingStart);
        }

        if self.buffer.len() + data.len() > MAX_MESSAGE_LEN {
            self.reset();
            return Err(FrameError::TooLarge);
        }
        self.buffer.extend_from_slice(data);

        if header & CHUNK_LAST != 0 {
            self.in_message = false;
            // `take` hands us the finished buffer and leaves an empty one.
            return Ok(Some(std::mem::take(&mut self.buffer)));
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// The clock is only changed if it is off by at least this much. Smaller
/// differences are within what a BLE round trip costs anyway.
const CLOCK_ADJUST_THRESHOLD_MS: i64 = 2_000;

/// Times outside this range cannot be right and are refused
/// (2024-01-01 and 2100-01-01, in milliseconds since 1970).
const MIN_PLAUSIBLE_UNIX_MS: i64 = 1_704_067_200_000;
const MAX_PLAUSIBLE_UNIX_MS: i64 = 4_102_444_800_000;

/// The fields every request has.
#[derive(Debug, Deserialize)]
struct Envelope {
    v: u32,
    /// Chosen by the sender and copied into the response, so the sender can
    /// match responses to requests.
    #[serde(default)]
    id: u64,
    op: String,
}

// One small struct per operation that has parameters. They are filled from
// the same JSON object as the envelope; unknown fields are ignored.

#[derive(Debug, Deserialize)]
struct SetTimeParams {
    /// Milliseconds since 1970-01-01 UTC.
    unix_ms: i64,
    /// The phone's current offset from UTC in minutes, if it wants the Pi to
    /// compare time zones.
    #[serde(default)]
    utc_offset_min: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct PutParams {
    alarm: Alarm,
}

#[derive(Debug, Deserialize)]
struct DeleteParams {
    alarm_id: String,
}

#[derive(Debug, Deserialize)]
struct ActionParams {
    name: String,
}

/// An error response in the making: a machine-readable code plus text.
struct Failure {
    code: &'static str,
    message: String,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
        }
    }
}

/// Handle one request and produce the response, both as JSON bytes.
///
/// This never fails: every problem becomes an error *response*.
pub fn handle_request(app: &App, request: &[u8]) -> Vec<u8> {
    let (id, outcome) = handle(app, request);
    let response = match outcome {
        Ok(mut body) => {
            // Add the standard fields to whatever the operation returned.
            body["v"] = json!(PROTOCOL_VERSION);
            body["id"] = json!(id);
            body["ok"] = json!(true);
            body
        }
        Err(failure) => {
            log::warn!("request failed: {}: {}", failure.code, failure.message);
            json!({
                "v": PROTOCOL_VERSION,
                "id": id,
                "ok": false,
                "error": { "code": failure.code, "message": failure.message },
            })
        }
    };
    response.to_string().into_bytes()
}

/// Returns the request id (0 if it could not be read) and the result.
fn handle(app: &App, request: &[u8]) -> (u64, Result<Value, Failure>) {
    let value: Value = match serde_json::from_slice(request) {
        Ok(value) => value,
        Err(error) => {
            return (
                0,
                Err(Failure::new("bad_json", format!("not valid JSON: {error}"))),
            )
        }
    };

    // Read the id on its own first, so that even a request with other
    // problems gets a response carrying the right id.
    let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);

    let envelope: Envelope = match serde_json::from_value(value.clone()) {
        Ok(envelope) => envelope,
        Err(error) => {
            return (
                id,
                Err(Failure::new(
                    "bad_request",
                    format!("a request needs \"v\" and \"op\": {error}"),
                )),
            )
        }
    };
    if envelope.v != PROTOCOL_VERSION {
        return (
            envelope.id,
            Err(Failure::new(
                "unsupported_version",
                format!(
                    "this daemon speaks protocol version {PROTOCOL_VERSION}, not {}",
                    envelope.v
                ),
            )),
        );
    }

    let result = match envelope.op.as_str() {
        "status" => op_status(app),
        "set_time" => parse_params(value).and_then(|params| op_set_time(app, params)),
        "list" => op_list(app),
        "put" => parse_params(value).and_then(|params| op_put(app, params)),
        "delete" => parse_params(value).and_then(|params| op_delete(app, params)),
        "action" => parse_params(value).and_then(|params| op_action(app, params)),
        other => Err(Failure::new("unknown_op", format!("unknown op `{other}`"))),
    };
    (envelope.id, result)
}

/// Read an operation's parameters out of the request object.
///
/// This is the one generic function in the file: `T` is whichever params
/// struct the caller expects, and `DeserializeOwned` says serde can build it.
fn parse_params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Failure> {
    serde_json::from_value(value)
        .map_err(|error| Failure::new("bad_request", format!("bad parameters: {error}")))
}

/// Shorthand for turning a storage error into a response.
fn storage_failure(error: anyhow::Error) -> Failure {
    Failure::new("storage_error", format!("{error:#}"))
}

/// The `time` object included in `status` and `set_time` responses.
fn time_info(app: &App) -> Value {
    let now = app.clock.now();
    let offset_seconds = app
        .tz
        .offset_from_utc_datetime(&now.naive_utc())
        .fix()
        .local_minus_utc();
    json!({
        "unix_ms": now.timestamp_millis(),
        "tz": app.tz.name(),
        "utc_offset_min": offset_seconds / 60,
        "synced": app.clock.is_synced(),
    })
}

fn op_status(app: &App) -> Result<Value, Failure> {
    let alarms = app.store.load().map_err(storage_failure)?;
    let next = match next_step_after(&alarms, app.tz, app.clock.now()) {
        Some(step) => json!({
            "alarm_id": step.alarm_id,
            "action": step.action,
            "unix_ms": step.fire_at.timestamp_millis(),
        }),
        None => Value::Null,
    };
    let actions: Vec<Value> = app
        .radio
        .actions()
        .values()
        .map(|action| json!({ "name": action.name, "label": action.label }))
        .collect();
    Ok(json!({
        "name": "pi-alarm",
        "version": env!("CARGO_PKG_VERSION"),
        "time": time_info(app),
        "actions": actions,
        "alarm_count": alarms.len(),
        "next": next,
    }))
}

fn op_set_time(app: &App, params: SetTimeParams) -> Result<Value, Failure> {
    if !(MIN_PLAUSIBLE_UNIX_MS..=MAX_PLAUSIBLE_UNIX_MS).contains(&params.unix_ms) {
        return Err(Failure::new(
            "bad_request",
            format!("unix_ms {} is not a plausible time", params.unix_ms),
        ));
    }
    // Cannot fail for values inside the plausible range checked above.
    let phone_time: DateTime<Utc> = Utc
        .timestamp_millis_opt(params.unix_ms)
        .single()
        .ok_or_else(|| Failure::new("bad_request", "unix_ms is out of range"))?;

    let delta_ms = (phone_time - app.clock.now()).num_milliseconds();
    let adjusted = delta_ms.abs() >= CLOCK_ADJUST_THRESHOLD_MS;
    if adjusted {
        app.clock
            .set(phone_time)
            .map_err(|error| Failure::new("clock_set_failed", format!("{error:#}")))?;
        log::info!("clock set from the phone (it was off by {delta_ms} ms)");
    }

    let mut body = json!({
        "adjusted": adjusted,
        "delta_ms": delta_ms,
        "time": time_info(app),
    });
    // If the phone told us its UTC offset, say whether ours agrees. A
    // mismatch means the Pi's time zone is set wrongly and alarms would
    // fire at the wrong hour.
    if let Some(phone_offset) = params.utc_offset_min {
        let pi_offset = body["time"]["utc_offset_min"].as_i64();
        body["tz_match"] = json!(pi_offset == Some(i64::from(phone_offset)));
    }
    Ok(body)
}

fn op_list(app: &App) -> Result<Value, Failure> {
    let alarms = app.store.load().map_err(storage_failure)?;
    Ok(json!({ "alarms": alarms }))
}

fn op_put(app: &App, params: PutParams) -> Result<Value, Failure> {
    let mut alarm = params.alarm;
    alarm.normalize();
    let known_actions = app.radio.action_names();
    alarm
        .validate(Some(&known_actions))
        .map_err(|error| Failure::new("invalid_alarm", error.to_string()))?;

    // Give a full list its own error code so the app can explain it. (The
    // store checks again under its lock; this is only for the nicer message.)
    let existing = app.store.load().map_err(storage_failure)?;
    let is_new = !existing.iter().any(|other| other.id == alarm.id);
    if is_new && existing.len() >= MAX_ALARMS {
        return Err(Failure::new(
            "too_many_alarms",
            format!("the alarm list is full ({MAX_ALARMS} alarms)"),
        ));
    }

    let outcome = app.store.put(alarm.clone()).map_err(storage_failure)?;
    log::info!(
        "alarm `{}` saved ({} {}, {})",
        alarm.id,
        alarm.time_text(),
        alarm.days_text(),
        if alarm.enabled { "on" } else { "off" }
    );
    Ok(json!({
        "created": outcome == PutOutcome::Created,
        // Sent back in its normalised form, as stored.
        "alarm": alarm,
    }))
}

fn op_delete(app: &App, params: DeleteParams) -> Result<Value, Failure> {
    let deleted = app
        .store
        .delete(&params.alarm_id)
        .map_err(storage_failure)?;
    if deleted {
        log::info!("alarm `{}` deleted", params.alarm_id);
    }
    // Deleting something that is already gone is a success: the phone may
    // be retrying a delete whose response it never received.
    Ok(json!({ "deleted": deleted }))
}

fn op_action(app: &App, params: ActionParams) -> Result<Value, Failure> {
    match app.radio.send(&params.name) {
        Ok(()) => Ok(json!({ "name": params.name })),
        Err(SendError::UnknownAction(name)) => Err(Failure::new(
            "unknown_action",
            format!("unknown action `{name}`"),
        )),
        Err(SendError::Transmit(error)) => {
            Err(Failure::new("transmit_failed", format!("{error:#}")))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::alarm::tests::sample_alarm;
    use crate::clock::{Clock, MockClock};
    use crate::config::{ActionConfig, ActionMode};
    use crate::radio::{Action, Radio};
    use crate::schedule::tests::utc;
    use crate::store::AlarmStore;
    use crate::transmitter::{MockTransmitter, SentLog};
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;

    /// Everything a test needs to poke at an `App` from the outside.
    pub(crate) struct TestApp {
        pub app: Arc<App>,
        pub sent: SentLog,
        pub clock: Arc<MockClock>,
        // Keeps the temporary directory alive for as long as the test runs.
        _dir: tempfile::TempDir,
    }

    /// A `Clock` that forwards to a shared `MockClock`, so the test keeps a
    /// handle to the clock it gave to the `App`.
    struct SharedClock(Arc<MockClock>);

    impl Clock for SharedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0.now()
        }
        fn set(&self, time: DateTime<Utc>) -> anyhow::Result<()> {
            self.0.set(time)
        }
        fn is_synced(&self) -> bool {
            self.0.is_synced()
        }
    }

    /// An `App` with the three real captures, a mock radio, a mock clock at
    /// 2026-11-10 18:00 UTC (noon in Chicago, a Tuesday) and an empty store.
    pub(crate) fn test_app() -> TestApp {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("fan_off", "0_fan.sub", "Fan off"),
            ("fan_3", "3_fan.sub", "Fan 3"),
            ("light_toggle", "Lite.sub", "Light"),
        ];
        let mut actions = BTreeMap::new();
        for (name, file, label) in files {
            let config = ActionConfig {
                file: Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("flipper_files")
                    .join(file),
                label: Some(label.to_string()),
                mode: ActionMode::Frame,
                repeats: None,
            };
            actions.insert(name.to_string(), Action::load(name, &config, 8).unwrap());
        }
        let mock = MockTransmitter::new();
        let sent = mock.sent_log();
        let clock = Arc::new(MockClock::new(utc(2026, 11, 10, 18, 0)));
        let app = App {
            store: AlarmStore::new(dir.path()),
            radio: Radio::new(actions, Box::new(mock)),
            clock: Box::new(SharedClock(Arc::clone(&clock))),
            tz: chrono_tz::America::Chicago,
            data_dir: dir.path().to_path_buf(),
            late_grace_secs: 300,
        };
        TestApp {
            app: Arc::new(app),
            sent,
            clock,
            _dir: dir,
        }
    }

    /// Send a request given as JSON text; get the response as a JSON value.
    fn request(test: &TestApp, json_text: &str) -> Value {
        let response = handle_request(&test.app, json_text.as_bytes());
        serde_json::from_slice(&response).expect("response must be valid JSON")
    }

    fn put_request(alarm: &Alarm) -> String {
        json!({ "v": 1, "id": 5, "op": "put", "alarm": alarm }).to_string()
    }

    // ---- framing -----------------------------------------------------------

    #[test]
    fn a_short_message_is_one_chunk() {
        let chunks = chunk_message(b"hello", 20);
        assert_eq!(chunks, vec![b"\x03hello".to_vec()]);
    }

    #[test]
    fn a_long_message_is_split_and_flagged() {
        let message: Vec<u8> = (0..50).collect();
        let chunks = chunk_message(&message, 20);
        // 19 data bytes per chunk: 19 + 19 + 12.
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0][0], CHUNK_FIRST);
        assert_eq!(chunks[1][0], 0);
        assert_eq!(chunks[2][0], CHUNK_LAST);
        assert_eq!(chunks[0].len(), 20);
        assert_eq!(chunks[1].len(), 20);
        assert_eq!(chunks[2].len(), 13);
    }

    #[test]
    fn a_message_that_exactly_fills_chunks_has_no_empty_tail() {
        let message = [7u8; 38];
        let chunks = chunk_message(&message, 20);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1][0], CHUNK_LAST);
        assert_eq!(chunks[1].len(), 20);
    }

    #[test]
    fn an_empty_message_is_one_header_only_chunk() {
        assert_eq!(chunk_message(b"", 20), vec![vec![CHUNK_FIRST | CHUNK_LAST]]);
    }

    #[test]
    fn chunks_reassemble_at_every_size() {
        let message: Vec<u8> = (0..1000u32).map(|n| (n % 251) as u8).collect();
        for max_chunk_len in [2, 3, 20, 23, 185, 244, 512, 2000] {
            let mut reassembler = Reassembler::new();
            let chunks = chunk_message(&message, max_chunk_len);
            let mut result = None;
            for (index, chunk) in chunks.iter().enumerate() {
                assert!(chunk.len() <= max_chunk_len);
                let output = reassembler.push(chunk).unwrap();
                if index + 1 < chunks.len() {
                    assert!(output.is_none());
                } else {
                    result = output;
                }
            }
            assert_eq!(result.as_deref(), Some(message.as_slice()));
        }
    }

    #[test]
    fn two_messages_in_a_row_stay_separate() {
        let mut reassembler = Reassembler::new();
        for text in ["first message", "second"] {
            let mut output = None;
            for chunk in chunk_message(text.as_bytes(), 6) {
                output = reassembler.push(&chunk).unwrap();
            }
            assert_eq!(output.unwrap(), text.as_bytes());
        }
    }

    #[test]
    fn a_new_first_chunk_discards_a_half_received_message() {
        let mut reassembler = Reassembler::new();
        // Start of a message that never finishes (the phone walked away).
        assert_eq!(reassembler.push(&[CHUNK_FIRST, b'a', b'b']), Ok(None));
        // A complete new message arrives.
        let output = reassembler.push(&[CHUNK_FIRST | CHUNK_LAST, b'x', b'y']);
        assert_eq!(output, Ok(Some(b"xy".to_vec())));
    }

    #[test]
    fn bad_chunks_are_rejected() {
        let mut reassembler = Reassembler::new();
        assert_eq!(reassembler.push(&[]), Err(FrameError::Empty));
        assert_eq!(
            reassembler.push(&[0x80, 1]),
            Err(FrameError::BadHeader(0x80))
        );
        // A middle chunk and a last chunk with no first chunk before them.
        assert_eq!(reassembler.push(&[0x00, 1]), Err(FrameError::MissingStart));
        assert_eq!(
            reassembler.push(&[CHUNK_LAST, 1]),
            Err(FrameError::MissingStart)
        );
        // Still usable afterwards.
        assert_eq!(
            reassembler.push(&[CHUNK_FIRST | CHUNK_LAST, 9]),
            Ok(Some(vec![9]))
        );
    }

    #[test]
    fn oversized_messages_are_rejected_and_the_buffer_is_dropped() {
        let mut reassembler = Reassembler::new();
        let big = vec![0u8; 1000];
        let mut first = vec![CHUNK_FIRST];
        first.extend_from_slice(&big);
        assert_eq!(reassembler.push(&first), Ok(None));

        let mut middle = vec![0x00];
        middle.extend_from_slice(&big);
        let mut result = Ok(None);
        for _ in 0..20 {
            result = reassembler.push(&middle);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(FrameError::TooLarge));
        // The oversized message is gone; a following chunk has no start.
        assert_eq!(reassembler.push(&middle), Err(FrameError::MissingStart));
    }

    // ---- envelope ----------------------------------------------------------

    #[test]
    fn every_response_carries_version_id_and_ok() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"id":42,"op":"list"}"#);
        assert_eq!(response["v"], 1);
        assert_eq!(response["id"], 42);
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn id_is_optional() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"op":"list"}"#);
        assert_eq!(response["ok"], true);
        assert_eq!(response["id"], 0);
    }

    #[test]
    fn broken_json_gets_an_error_response() {
        let test = test_app();
        let response = request(&test, "{ not json");
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "bad_json");
        assert_eq!(response["v"], 1);
        assert_eq!(response["id"], 0);
    }

    #[test]
    fn a_request_without_op_or_version_is_a_bad_request_with_the_right_id() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"id":7}"#);
        assert_eq!(response["error"]["code"], "bad_request");
        assert_eq!(response["id"], 7);

        let response = request(&test, r#"{"id":8,"op":"list"}"#);
        assert_eq!(response["error"]["code"], "bad_request");
        assert_eq!(response["id"], 8);
    }

    #[test]
    fn other_protocol_versions_are_refused() {
        let test = test_app();
        let response = request(&test, r#"{"v":2,"id":3,"op":"list"}"#);
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "unsupported_version");
        assert_eq!(response["id"], 3);
        // The response itself is still version 1, which is all we speak.
        assert_eq!(response["v"], 1);
    }

    #[test]
    fn unknown_ops_are_refused() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"id":3,"op":"self_destruct"}"#);
        assert_eq!(response["error"]["code"], "unknown_op");
    }

    // ---- status ------------------------------------------------------------

    #[test]
    fn status_describes_the_daemon() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"id":1,"op":"status"}"#);
        assert_eq!(response["ok"], true);
        assert_eq!(response["name"], "pi-alarm");
        assert_eq!(response["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(response["alarm_count"], 0);
        assert_eq!(response["next"], Value::Null);
        assert_eq!(
            response["actions"],
            json!([
                { "name": "fan_3", "label": "Fan 3" },
                { "name": "fan_off", "label": "Fan off" },
                { "name": "light_toggle", "label": "Light" },
            ])
        );
        let time = &response["time"];
        assert_eq!(time["unix_ms"], utc(2026, 11, 10, 18, 0).timestamp_millis());
        assert_eq!(time["tz"], "America/Chicago");
        assert_eq!(time["utc_offset_min"], -360);
        assert_eq!(time["synced"], false);
    }

    #[test]
    fn status_reports_the_next_step() {
        let test = test_app();
        // Weekdays 06:30 with the light ten minutes earlier.
        test.app.store.put(sample_alarm("work")).unwrap();
        let response = request(&test, r#"{"v":1,"id":1,"op":"status"}"#);
        assert_eq!(response["alarm_count"], 1);
        // Now is Tuesday noon; next is Wednesday 06:20 CST = 12:20 UTC.
        assert_eq!(
            response["next"],
            json!({
                "alarm_id": "work",
                "action": "light_toggle",
                "unix_ms": utc(2026, 11, 11, 12, 20).timestamp_millis(),
            })
        );
    }

    #[test]
    fn utc_offset_follows_daylight_saving() {
        let test = test_app();
        test.clock.advance_to(utc(2026, 7, 1, 12, 0));
        let response = request(&test, r#"{"v":1,"op":"status"}"#);
        assert_eq!(response["time"]["utc_offset_min"], -300);
    }

    // ---- set_time ----------------------------------------------------------

    #[test]
    fn set_time_corrects_a_wrong_clock() {
        let test = test_app();
        // The Pi thinks it is 18:00; the phone says 18:07:30.
        let phone = utc(2026, 11, 10, 18, 7) + chrono::Duration::seconds(30);
        let text = json!({
            "v": 1, "id": 9, "op": "set_time",
            "unix_ms": phone.timestamp_millis(),
            "utc_offset_min": -360,
        })
        .to_string();
        let response = request(&test, &text);

        assert_eq!(response["ok"], true);
        assert_eq!(response["adjusted"], true);
        assert_eq!(response["delta_ms"], 450_000);
        assert_eq!(response["tz_match"], true);
        assert_eq!(response["time"]["unix_ms"], phone.timestamp_millis());
        assert_eq!(response["time"]["synced"], true);
        assert_eq!(test.clock.set_calls(), vec![phone]);
        assert_eq!(test.clock.now(), phone);
    }

    #[test]
    fn set_time_can_move_the_clock_backwards() {
        let test = test_app();
        let phone = utc(2026, 11, 10, 17, 0);
        let text =
            json!({ "v": 1, "op": "set_time", "unix_ms": phone.timestamp_millis() }).to_string();
        let response = request(&test, &text);
        assert_eq!(response["adjusted"], true);
        assert_eq!(response["delta_ms"], -3_600_000);
        assert_eq!(test.clock.now(), phone);
        // No offset was sent, so no comparison is reported.
        assert!(response.get("tz_match").is_none());
    }

    #[test]
    fn set_time_leaves_a_nearly_right_clock_alone() {
        let test = test_app();
        let phone = utc(2026, 11, 10, 18, 0) + chrono::Duration::milliseconds(1_999);
        let text =
            json!({ "v": 1, "op": "set_time", "unix_ms": phone.timestamp_millis() }).to_string();
        let response = request(&test, &text);
        assert_eq!(response["ok"], true);
        assert_eq!(response["adjusted"], false);
        assert_eq!(response["delta_ms"], 1_999);
        assert!(test.clock.set_calls().is_empty());

        // Exactly at the threshold it is adjusted.
        let phone = utc(2026, 11, 10, 18, 0) + chrono::Duration::milliseconds(2_000);
        let text =
            json!({ "v": 1, "op": "set_time", "unix_ms": phone.timestamp_millis() }).to_string();
        assert_eq!(request(&test, &text)["adjusted"], true);
    }

    #[test]
    fn set_time_flags_a_time_zone_mismatch() {
        let test = test_app();
        // The phone is on New York time (UTC-5), the Pi on Chicago (UTC-6).
        let text = json!({
            "v": 1, "op": "set_time",
            "unix_ms": utc(2026, 11, 10, 18, 0).timestamp_millis(),
            "utc_offset_min": -300,
        })
        .to_string();
        let response = request(&test, &text);
        assert_eq!(response["ok"], true);
        assert_eq!(response["tz_match"], false);
        assert_eq!(response["time"]["utc_offset_min"], -360);
    }

    #[test]
    fn set_time_refuses_absurd_times() {
        let test = test_app();
        for unix_ms in [0i64, -5, 1_000_000_000, 9_999_999_999_999] {
            let text = json!({ "v": 1, "op": "set_time", "unix_ms": unix_ms }).to_string();
            let response = request(&test, &text);
            assert_eq!(response["error"]["code"], "bad_request", "{unix_ms}");
        }
        assert!(test.clock.set_calls().is_empty());
    }

    #[test]
    fn set_time_reports_when_the_clock_cannot_be_set() {
        let test = test_app();
        test.clock.refuse_set(true);
        let text = json!({
            "v": 1, "id": 4, "op": "set_time",
            "unix_ms": utc(2026, 11, 10, 19, 0).timestamp_millis(),
        })
        .to_string();
        let response = request(&test, &text);
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "clock_set_failed");
        assert_eq!(response["id"], 4);
    }

    #[test]
    fn set_time_needs_unix_ms() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"op":"set_time"}"#);
        assert_eq!(response["error"]["code"], "bad_request");
    }

    // ---- list / put / delete -----------------------------------------------

    #[test]
    fn put_creates_then_replaces() {
        let test = test_app();
        let alarm = sample_alarm("a1b2c3d4");

        let response = request(&test, &put_request(&alarm));
        assert_eq!(response["ok"], true);
        assert_eq!(response["id"], 5);
        assert_eq!(response["created"], true);
        assert_eq!(response["alarm"], json!(alarm));

        let mut changed = alarm.clone();
        changed.enabled = false;
        changed.minute = 45;
        let response = request(&test, &put_request(&changed));
        assert_eq!(response["created"], false);

        let listed = request(&test, r#"{"v":1,"op":"list"}"#);
        assert_eq!(listed["alarms"], json!([changed]));
    }

    #[test]
    fn put_stores_and_returns_the_normalised_alarm() {
        let test = test_app();
        let text = r#"{"v":1,"op":"put","alarm":{
            "id":"x","hour":7,"minute":0,
            "days":["sun","mon","sun"],"enabled":true,
            "steps":[{"offset_s":0,"action":"fan_3"},{"offset_s":-300,"action":"light_toggle"}]}}"#;
        let response = request(&test, text);
        assert_eq!(response["ok"], true);
        assert_eq!(response["alarm"]["days"], json!(["mon", "sun"]));
        assert_eq!(response["alarm"]["steps"][0]["action"], "light_toggle");
        assert_eq!(response["alarm"]["label"], "");
        assert_eq!(test.app.store.load().unwrap()[0].steps[0].offset_s, -300);
    }

    #[test]
    fn put_rejects_invalid_alarms_without_storing_them() {
        let test = test_app();

        let mut unknown_action = sample_alarm("a");
        unknown_action.steps[0].action = "fan_1".to_string();
        let response = request(&test, &put_request(&unknown_action));
        assert_eq!(response["error"]["code"], "invalid_alarm");
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fan_1"));

        let mut no_days = sample_alarm("a");
        no_days.days.clear();
        let response = request(&test, &put_request(&no_days));
        assert_eq!(response["error"]["code"], "invalid_alarm");

        // A day name that does not exist fails while parsing.
        let response = request(
            &test,
            r#"{"v":1,"op":"put","alarm":{"id":"a","hour":7,"minute":0,
                "days":["someday"],"enabled":true,"steps":[]}}"#,
        );
        assert_eq!(response["error"]["code"], "bad_request");

        assert!(test.app.store.load().unwrap().is_empty());
    }

    #[test]
    fn put_reports_a_full_list() {
        let test = test_app();
        for index in 0..crate::alarm::MAX_ALARMS {
            let response = request(&test, &put_request(&sample_alarm(&format!("a{index}"))));
            assert_eq!(response["ok"], true);
        }
        let response = request(&test, &put_request(&sample_alarm("overflow")));
        assert_eq!(response["error"]["code"], "too_many_alarms");
        // Editing an existing alarm still works when the list is full.
        let response = request(&test, &put_request(&sample_alarm("a0")));
        assert_eq!(response["created"], false);
    }

    #[test]
    fn delete_is_safe_to_repeat() {
        let test = test_app();
        request(&test, &put_request(&sample_alarm("a")));

        let response = request(&test, r#"{"v":1,"id":2,"op":"delete","alarm_id":"a"}"#);
        assert_eq!(response["ok"], true);
        assert_eq!(response["deleted"], true);

        // A retry of the same delete still succeeds.
        let response = request(&test, r#"{"v":1,"id":3,"op":"delete","alarm_id":"a"}"#);
        assert_eq!(response["ok"], true);
        assert_eq!(response["deleted"], false);

        assert_eq!(
            request(&test, r#"{"v":1,"op":"list"}"#)["alarms"],
            json!([])
        );
    }

    #[test]
    fn a_full_list_fits_in_one_message() {
        // The biggest alarms the rules allow, as many as the store holds.
        let test = test_app();
        for index in 0..crate::alarm::MAX_ALARMS {
            let mut alarm = sample_alarm(&format!("alarm-with-a-long-id-{index:03}"));
            alarm.label = "w".repeat(crate::alarm::MAX_LABEL_CHARS);
            alarm.days = crate::alarm::Day::ALL.to_vec();
            let step = crate::alarm::Step {
                offset_s: -crate::alarm::MAX_OFFSET_SECS,
                action: "light_toggle".to_string(),
            };
            alarm.steps = vec![step; crate::alarm::MAX_STEPS];
            test.app.store.put(alarm).unwrap();
        }
        let response = handle_request(&test.app, br#"{"v":1,"op":"list"}"#);
        assert!(
            response.len() <= MAX_MESSAGE_LEN,
            "a full list is {} bytes",
            response.len()
        );
    }

    // ---- action ------------------------------------------------------------

    #[test]
    fn action_transmits_now() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"id":6,"op":"action","name":"fan_3"}"#);
        assert_eq!(response["ok"], true);
        assert_eq!(response["name"], "fan_3");
        assert_eq!(test.sent.count(), 1);
        assert_eq!(
            test.sent.all()[0],
            test.app.radio.actions()["fan_3"].transmission
        );
    }

    #[test]
    fn action_errors_are_reported_by_code() {
        let test = test_app();
        let response = request(&test, r#"{"v":1,"op":"action","name":"fan_1"}"#);
        assert_eq!(response["error"]["code"], "unknown_action");
        assert_eq!(test.sent.count(), 0);

        test.sent.set_failing(true);
        let response = request(&test, r#"{"v":1,"op":"action","name":"fan_3"}"#);
        assert_eq!(response["error"]["code"], "transmit_failed");

        let response = request(&test, r#"{"v":1,"op":"action"}"#);
        assert_eq!(response["error"]["code"], "bad_request");
    }

    // ---- both layers together ----------------------------------------------

    #[test]
    fn a_request_survives_being_chunked_at_the_smallest_ble_size() {
        let test = test_app();
        let request_text = put_request(&sample_alarm("chunky"));

        // Phone -> Pi in 20-byte chunks.
        let mut reassembler = Reassembler::new();
        let mut received = None;
        for chunk in chunk_message(request_text.as_bytes(), MIN_CHUNK_LEN) {
            received = reassembler.push(&chunk).unwrap();
        }
        let response = handle_request(&test.app, &received.unwrap());

        // Pi -> phone in 20-byte chunks.
        let mut reassembler = Reassembler::new();
        let mut returned = None;
        for chunk in chunk_message(&response, MIN_CHUNK_LEN) {
            returned = reassembler.push(&chunk).unwrap();
        }
        let value: Value = serde_json::from_slice(&returned.unwrap()).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["created"], true);
        assert_eq!(test.app.store.load().unwrap()[0].id, "chunky");
    }
}
