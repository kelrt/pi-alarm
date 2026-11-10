//! End-to-end tests: run the real `pi-alarm` binary the way a person would.
//!
//! Files in `tests/` are "integration tests". Each is compiled as its own
//! program and can only use the crate's public interface, or, as here, the
//! built binary. Cargo puts the binary's path in `CARGO_BIN_EXE_pi-alarm`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A temporary directory holding a config that uses the mock radio and the
/// real capture files.
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let captures = Path::new(env!("CARGO_MANIFEST_DIR")).join("flipper_files");
        let config = format!(
            r#"
data_dir = "data"
timezone = "America/Chicago"

[radio]
transmitter = "mock"

[ble]
enabled = false

[actions.fan_off]
file = "{captures}/0_fan.sub"
label = "Fan off"

[actions.fan_3]
file = "{captures}/3_fan.sub"

[actions.light_toggle]
file = "{captures}/Lite.sub"
"#,
            captures = captures.display()
        );
        std::fs::write(dir.path().join("pi-alarm.toml"), config).unwrap();
        Sandbox { dir }
    }

    fn config_path(&self) -> PathBuf {
        self.dir.path().join("pi-alarm.toml")
    }

    /// Run `pi-alarm --config <sandbox config> <args...>`.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pi-alarm"))
            .arg("--config")
            .arg(self.config_path())
            .args(args)
            .output()
            .expect("failed to start pi-alarm")
    }

    /// Run and require success; returns standard output as text.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`pi-alarm {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Run and require failure; returns standard error as text.
    fn fails(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            !output.status.success(),
            "`pi-alarm {}` should have failed",
            args.join(" ")
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

#[test]
fn check_reports_the_three_real_actions() {
    let sandbox = Sandbox::new();
    let output = sandbox.ok(&["check"]);
    assert!(output.contains("America/Chicago"), "{output}");
    assert!(output.contains("A2E8DF"), "{output}");
    assert!(output.contains("A2E83F"), "{output}");
    assert!(output.contains("A2E85F"), "{output}");
    assert!(output.contains("304.250 MHz"), "{output}");
    assert!(output.trim_end().ends_with("OK"), "{output}");
}

#[test]
fn check_fails_for_a_missing_capture_file() {
    let sandbox = Sandbox::new();
    let mut config = std::fs::read_to_string(sandbox.config_path()).unwrap();
    config.push_str("\n[actions.fan_1]\nfile = \"not-captured-yet.sub\"\n");
    std::fs::write(sandbox.config_path(), config).unwrap();

    let error = sandbox.fails(&["check"]);
    assert!(error.contains("fan_1"), "{error}");
    assert!(error.contains("not-captured-yet.sub"), "{error}");
}

#[test]
fn inspect_needs_no_config() {
    let capture = Path::new(env!("CARGO_MANIFEST_DIR")).join("flipper_files/Lite.sub");
    let output = Command::new(env!("CARGO_BIN_EXE_pi-alarm"))
        .arg("inspect")
        .arg(&capture)
        // Somewhere with no pi-alarm.toml, to prove none is needed.
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("304250000 Hz"), "{text}");
    assert!(text.contains("FuriHalSubGhzPresetOok270Async"), "{text}");
    assert!(text.contains("RAW recording"), "{text}");
    assert!(text.contains("hex A2E85F"), "{text}");
}

#[test]
fn send_works_with_the_mock_radio_and_rejects_unknown_actions() {
    let sandbox = Sandbox::new();
    assert_eq!(sandbox.ok(&["send", "fan_3"]).trim(), "sent fan_3");

    let error = sandbox.fails(&["send", "fan_1"]);
    assert!(error.contains("unknown action `fan_1`"), "{error}");
    assert!(
        error.contains("light_toggle"),
        "should list the known ones: {error}"
    );
}

#[test]
fn alarms_can_be_added_listed_disabled_and_deleted() {
    let sandbox = Sandbox::new();
    assert_eq!(sandbox.ok(&["list"]).trim(), "no alarms");

    sandbox.ok(&[
        "add",
        "--id",
        "work",
        "--label",
        "Work days",
        "--time",
        "06:30",
        "--days",
        "weekdays",
        "--step",
        "fan_3@0",
        "--step",
        "light_toggle@-10m",
    ]);

    let listed = sandbox.ok(&["list"]);
    assert!(
        listed.contains("work  on   06:30  mon,tue,wed,thu,fri  Work days"),
        "{listed}"
    );
    // Steps come out sorted by offset, whatever order they were given in.
    let light = listed.find("light_toggle").unwrap();
    let fan = listed.find("fan_3").unwrap();
    assert!(light < fan, "{listed}");

    // The file on disk is the documented JSON.
    let json = std::fs::read_to_string(sandbox.dir.path().join("data/alarms.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["alarms"][0]["id"], "work");
    assert_eq!(value["alarms"][0]["steps"][0]["offset_s"], -600);

    let disabled = sandbox.ok(&["disable", "work"]);
    assert!(disabled.contains("work  off"), "{disabled}");
    let enabled = sandbox.ok(&["enable", "work"]);
    assert!(enabled.contains("work  on "), "{enabled}");

    assert_eq!(sandbox.ok(&["delete", "work"]).trim(), "deleted work");
    assert_eq!(sandbox.ok(&["list"]).trim(), "no alarms");
    let error = sandbox.fails(&["delete", "work"]);
    assert!(error.contains("no alarm with id `work`"), "{error}");
}

#[test]
fn add_rejects_bad_input() {
    let sandbox = Sandbox::new();
    let cases: [&[&str]; 4] = [
        &["add", "--time", "25:00", "--step", "fan_3@0"],
        &["add", "--time", "07:00", "--step", "fan_9@0"],
        &[
            "add", "--time", "07:00", "--days", "someday", "--step", "fan_3@0",
        ],
        &["add", "--time", "07:00", "--step", "fan_3@-9h"],
    ];
    for args in cases {
        sandbox.fails(args);
    }
    assert_eq!(sandbox.ok(&["list"]).trim(), "no alarms");
}

#[test]
fn next_lists_upcoming_steps_in_local_time() {
    let sandbox = Sandbox::new();
    sandbox.ok(&[
        "add", "--id", "daily", "--time", "06:30", "--step", "fan_3@0",
    ]);
    let output = sandbox.ok(&["next", "--days", "2"]);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 2, "{output}");
    for line in lines {
        assert!(line.contains("06:30:00"), "{line}");
        assert!(line.contains("fan_3"), "{line}");
    }
}

#[test]
fn request_speaks_the_phone_protocol() {
    let sandbox = Sandbox::new();
    let output = sandbox.ok(&["request", r#"{"v":1,"id":11,"op":"status"}"#]);
    let status: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(status["ok"], true);
    assert_eq!(status["id"], 11);
    assert_eq!(status["time"]["tz"], "America/Chicago");
    assert_eq!(status["actions"][0]["name"], "fan_3");
    // No label in the config, so the name is used.
    assert_eq!(status["actions"][0]["label"], "fan_3");
    assert_eq!(status["actions"][1]["label"], "Fan off");

    let put = r#"{"v":1,"id":12,"op":"put","alarm":{"id":"phone1","label":"From phone",
        "hour":7,"minute":15,"days":["sat","sun"],"enabled":true,
        "steps":[{"offset_s":0,"action":"fan_3"}]}}"#;
    let output = sandbox.ok(&["request", put]);
    let response: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(response["created"], true);

    // The CLI and the protocol share one store.
    let listed = sandbox.ok(&["list"]);
    assert!(
        listed.contains("phone1  on   07:15  sat,sun  From phone"),
        "{listed}"
    );

    let output = sandbox.ok(&[
        "request",
        r#"{"v":1,"id":13,"op":"action","name":"light_toggle"}"#,
    ]);
    let response: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(response["ok"], true);

    // Protocol errors are still a normal response on stdout.
    let output = sandbox.ok(&["request", r#"{"v":9,"op":"status"}"#]);
    let response: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(response["error"]["code"], "unsupported_version");
}

#[test]
fn a_missing_config_is_explained() {
    let output = Command::new(env!("CARGO_BIN_EXE_pi-alarm"))
        .args(["--config", "/nonexistent/pi-alarm.toml", "list"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("cannot read config file"), "{error}");
}
