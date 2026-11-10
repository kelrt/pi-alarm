//! Keeps PROTOCOL.md honest: every request example in the document is fed to
//! the real request handler and must be accepted.
//!
//! An example counts as a request if its code fence starts with
//! "```json request".

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{TimeZone, Utc};
use pi_alarm::app::App;
use pi_alarm::clock::MockClock;
use pi_alarm::config::{ActionConfig, ActionMode};
use pi_alarm::protocol::handle_request;
use pi_alarm::radio::{Action, Radio};
use pi_alarm::store::AlarmStore;
use pi_alarm::transmitter::MockTransmitter;

const PROTOCOL_MD: &str = include_str!("../PROTOCOL.md");

/// Pull out the contents of every "```json request" block.
fn request_examples() -> Vec<String> {
    let mut examples = Vec::new();
    let mut current: Option<String> = None;
    for line in PROTOCOL_MD.lines() {
        match &mut current {
            // Not inside a request block: look for the start of one.
            None => {
                if line.trim() == "```json request" {
                    current = Some(String::new());
                }
            }
            // Inside one: collect lines until the closing fence.
            Some(text) => {
                if line.trim() == "```" {
                    examples.push(current.take().unwrap());
                } else {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
    }
    examples
}

/// An app with the three real captures, a mock radio and a mock clock.
fn test_app(data_dir: &Path) -> App {
    let mut actions = BTreeMap::new();
    for (name, file) in [
        ("fan_off", "0_fan.sub"),
        ("fan_3", "3_fan.sub"),
        ("light_toggle", "Lite.sub"),
    ] {
        let config = ActionConfig {
            file: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("flipper_files")
                .join(file),
            label: None,
            mode: ActionMode::Frame,
            repeats: None,
        };
        actions.insert(name.to_string(), Action::load(name, &config, 8).unwrap());
    }
    App {
        store: AlarmStore::new(data_dir),
        radio: Radio::new(actions, Box::new(MockTransmitter::new())),
        clock: Box::new(MockClock::new(
            Utc.with_ymd_and_hms(2026, 11, 10, 18, 0, 0).unwrap(),
        )),
        tz: chrono_tz::America::Chicago,
        data_dir: data_dir.to_path_buf(),
        late_grace_secs: 300,
    }
}

#[test]
fn every_request_example_in_the_document_is_accepted() {
    let examples = request_examples();
    // One per operation: status, set_time, list, put, delete, action.
    assert_eq!(examples.len(), 6, "expected one example per operation");

    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());

    let mut ops_seen = Vec::new();
    for example in &examples {
        let request: serde_json::Value =
            serde_json::from_str(example).unwrap_or_else(|e| panic!("bad JSON: {e}\n{example}"));
        ops_seen.push(request["op"].as_str().unwrap().to_string());

        let response = handle_request(&app, example.as_bytes());
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["ok"], true, "refused:\n{example}\n{response}");
        assert_eq!(response["id"], request["id"], "id must be echoed");
        assert_eq!(response["v"], 1);
    }
    assert_eq!(
        ops_seen,
        ["status", "set_time", "list", "put", "delete", "action"]
    );
}

#[test]
fn the_documented_uuids_are_the_ones_in_the_code() {
    for uuid in [
        pi_alarm::ble::SERVICE_UUID,
        pi_alarm::ble::REQUEST_UUID,
        pi_alarm::ble::RESPONSE_UUID,
        pi_alarm::ble::INFO_UUID,
    ] {
        assert!(
            PROTOCOL_MD.contains(&uuid.to_string()),
            "{uuid} is missing from PROTOCOL.md"
        );
    }
}

#[test]
fn the_documented_limits_are_the_ones_in_the_code() {
    use pi_alarm::alarm::{MAX_ALARMS, MAX_ID_CHARS, MAX_LABEL_CHARS, MAX_OFFSET_SECS, MAX_STEPS};
    use pi_alarm::protocol::MAX_MESSAGE_LEN;

    assert!(PROTOCOL_MD.contains(&format!("at most {MAX_MESSAGE_LEN} bytes")));
    assert!(PROTOCOL_MD.contains(&format!("1-{MAX_ID_CHARS} characters")));
    assert!(PROTOCOL_MD.contains(&format!("at most {MAX_LABEL_CHARS} characters")));
    assert!(PROTOCOL_MD.contains(&format!("1-{MAX_STEPS} steps")));
    assert!(PROTOCOL_MD.contains(&format!("-{MAX_OFFSET_SECS} to {MAX_OFFSET_SECS}")));
    assert!(PROTOCOL_MD.contains(&format!("({MAX_ALARMS} is the limit)")));
}

#[test]
fn the_flutter_app_uses_the_same_uuids() {
    let dart = include_str!("../app/lib/ble/uuids.dart");
    for uuid in [
        pi_alarm::ble::SERVICE_UUID,
        pi_alarm::ble::REQUEST_UUID,
        pi_alarm::ble::RESPONSE_UUID,
        pi_alarm::ble::INFO_UUID,
    ] {
        assert!(
            dart.contains(&uuid.to_string()),
            "{uuid} is missing from uuids.dart"
        );
    }
}
