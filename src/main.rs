//! The `pi-alarm` command line program.
//!
//! `pi-alarm run` is the daemon. Every other subcommand is a tool for
//! testing and maintenance without the phone: sending an action, listing and
//! editing alarms, inspecting a capture file.
//!
//! The command line is described with `clap`'s derive macros: each struct or
//! enum below becomes options and subcommands, and the `///` comments become
//! the `--help` text.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};

use pi_alarm::alarm::{is_valid_id, Alarm, Day, Step};
use pi_alarm::app::{open_transmitter, App};
use pi_alarm::config::Config;
use pi_alarm::daemon;
use pi_alarm::protocol::handle_request;
use pi_alarm::radio::{load_actions, Action, Radio};
use pi_alarm::schedule::steps_between;
use pi_alarm::signal::{extract_frame, total_micros, ExtractOptions};
use pi_alarm::store::AlarmStore;
use pi_alarm::subfile::{Payload, SubFile};

#[derive(Parser)]
#[command(
    name = "pi-alarm",
    version,
    about = "Wake-up alarm that replays fan/light remote signals"
)]
struct Cli {
    /// Config file [default: ./pi-alarm.toml, then /etc/pi-alarm/pi-alarm.toml]
    #[arg(short, long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Use the mock transmitter (nothing goes on air), whatever the config says
    #[arg(long, global = true)]
    mock_radio: bool,

    /// More log output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: scheduler and BLE server
    Run,

    /// Check the config and every action's capture file
    Check {
        /// Also talk to the CC1101 and report what it answers
        #[arg(long)]
        radio: bool,
    },

    /// Show what a Flipper .sub file contains
    Inspect {
        /// Path to a .sub file
        file: PathBuf,
    },

    /// List the configured actions
    Actions,

    /// Transmit an action now, e.g. `pi-alarm send fan_3`
    Send {
        /// Action name from the config
        action: String,
    },

    /// List the alarms
    List,

    /// Add an alarm (or replace the one with the same --id)
    Add {
        /// Alarm time as HH:MM, in the daemon's time zone
        #[arg(long)]
        time: String,

        /// Days: comma-separated mon,tue,... or `daily`, `weekdays`, `weekends`
        #[arg(long, default_value = "daily")]
        days: String,

        /// A step as ACTION@OFFSET, e.g. `light_toggle@-10m` or `fan_3@0`.
        /// The offset may end in s, m or h; plain numbers are seconds.
        /// Repeat the option for more steps.
        #[arg(long = "step", value_name = "ACTION@OFFSET", required = true)]
        steps: Vec<String>,

        /// A name for the alarm
        #[arg(long, default_value = "")]
        label: String,

        /// Alarm id [default: a random one]
        #[arg(long)]
        id: Option<String>,

        /// Create it switched off
        #[arg(long)]
        disabled: bool,
    },

    /// Switch an alarm on
    Enable { id: String },

    /// Switch an alarm off
    Disable { id: String },

    /// Delete an alarm
    Delete { id: String },

    /// Show the steps that will run in the coming days
    Next {
        /// How many days to look ahead
        #[arg(long, default_value_t = 7)]
        days: i64,
    },

    /// Run one protocol request, exactly as the phone would send it.
    /// Example: pi-alarm request '{"v":1,"op":"status"}'
    Request {
        /// The request as JSON text
        json: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Log level: info by default, debug with --verbose. The RUST_LOG
    // environment variable still overrides this.
    let default_level = if cli.verbose { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
        .init();

    // Returning an `ExitCode` (instead of `Result`) lets us print errors in
    // our own format: `{:#}` shows an anyhow error with all its context.
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    // `inspect` is the one command that needs no config at all.
    if let Command::Inspect { file } = &cli.command {
        return inspect(file);
    }

    let config_path = Config::find(cli.config.as_deref())?;
    let config = Config::load(&config_path)?;
    let store = AlarmStore::new(&config.data_dir);

    match cli.command {
        Command::Inspect { .. } => unreachable!("handled above"),

        Command::Run => {
            // The daemon is async; everything else is plain blocking code.
            // A "current thread" runtime runs all tasks on this one thread.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("cannot start the async runtime")?;
            runtime.block_on(daemon::run(config, cli.mock_radio))
        }

        Command::Check { radio } => check(&config, &config_path, radio, cli.mock_radio),

        Command::Actions => {
            for action in load_actions(&config)?.values() {
                println!("{}", describe_action(action));
            }
            Ok(())
        }

        Command::Send { action } => {
            let actions = load_actions(&config)?;
            if !actions.contains_key(&action) {
                let names: Vec<&str> = actions.keys().map(String::as_str).collect();
                bail!(
                    "unknown action `{action}`. Known actions: {}",
                    names.join(", ")
                );
            }
            let transmitter = open_transmitter(&config, cli.mock_radio)?;
            let radio =
                Radio::new(actions, transmitter).with_lock_file(config.data_dir.join("radio.lock"));
            radio.send(&action)?;
            println!("sent {action}");
            Ok(())
        }

        Command::List => {
            let alarms = store.load()?;
            if alarms.is_empty() {
                println!("no alarms");
            }
            for alarm in &alarms {
                print_alarm(alarm);
            }
            Ok(())
        }

        Command::Add {
            time,
            days,
            steps,
            label,
            id,
            disabled,
        } => {
            let (hour, minute) = parse_time(&time)?;
            let mut alarm = Alarm {
                id: id.unwrap_or_else(random_id),
                label,
                hour,
                minute,
                days: parse_days(&days)?,
                enabled: !disabled,
                // `collect` can gather `Result`s: it stops at the first
                // error and returns it, otherwise gives the whole Vec.
                steps: steps
                    .iter()
                    .map(|text| parse_step(text))
                    .collect::<anyhow::Result<Vec<Step>>>()?,
            };
            alarm.normalize();
            let known_actions: Vec<String> = config.actions.keys().cloned().collect();
            alarm.validate(Some(&known_actions))?;
            store.put(alarm.clone())?;
            print_alarm(&alarm);
            Ok(())
        }

        Command::Enable { id } => set_enabled(&store, &id, true),
        Command::Disable { id } => set_enabled(&store, &id, false),

        Command::Delete { id } => {
            if !store.delete(&id)? {
                bail!("no alarm with id `{id}`");
            }
            println!("deleted {id}");
            Ok(())
        }

        Command::Next { days } => {
            let tz = config.resolve_timezone()?;
            let alarms = store.load()?;
            let now = Utc::now();
            let steps = steps_between(&alarms, tz, now, now + Duration::days(days));
            if steps.is_empty() {
                println!("nothing scheduled in the next {days} day(s)");
            }
            for step in steps {
                let local = step.fire_at.with_timezone(&tz);
                println!(
                    "{}  {:<16} (alarm {})",
                    local.format("%a %Y-%m-%d %H:%M:%S %Z"),
                    step.action,
                    step.alarm_id
                );
            }
            Ok(())
        }

        Command::Request { json } => {
            let app = App::from_config(&config, cli.mock_radio)?;
            let response = handle_request(&app, json.as_bytes());
            println!("{}", String::from_utf8_lossy(&response));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// check / inspect
// ---------------------------------------------------------------------------

fn check(
    config: &Config,
    config_path: &Path,
    probe_radio: bool,
    force_mock: bool,
) -> anyhow::Result<()> {
    println!("config      {}", config_path.display());
    println!("data dir    {}", config.data_dir.display());
    println!("time zone   {}", config.resolve_timezone()?.name());
    println!(
        "ble         {}",
        if config.ble.enabled {
            format!("enabled, advertising as `{}`", config.ble.device_name)
        } else {
            "disabled".to_string()
        }
    );

    let actions = load_actions(config)?;
    println!("actions     {}", actions.len());
    for action in actions.values() {
        println!("  {}", describe_action(action));
    }
    if actions.is_empty() {
        println!("  (none: add [actions.NAME] sections to the config)");
    }

    // Warn about alarms that refer to actions that no longer exist.
    let store = AlarmStore::new(&config.data_dir);
    let alarms = store.load()?;
    println!("alarms      {}", alarms.len());
    for alarm in &alarms {
        for step in &alarm.steps {
            if !actions.contains_key(&step.action) {
                println!(
                    "  WARNING: alarm `{}` uses unknown action `{}`",
                    alarm.id, step.action
                );
            }
        }
    }

    if probe_radio {
        // Opening the transmitter resets the chip and reads its version.
        let transmitter = open_transmitter(config, force_mock)?;
        println!("radio       {} answered", transmitter.name());
    } else {
        println!("radio       not probed (add --radio to talk to the CC1101)");
    }
    println!("OK");
    Ok(())
}

/// One line about an action, e.g.
/// `fan_3  304.250 MHz  code A2E83F  8 x 28.2 ms  (3_fan.sub)`.
fn describe_action(action: &Action) -> String {
    let megahertz = f64::from(action.transmission.frequency_hz) / 1e6;
    let file = action
        .source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let on_air_ms = action.transmission.duration_micros() as f64 / 1000.0;
    match &action.frame {
        Some(frame) => format!(
            "{:<14} {megahertz:.3} MHz  code {:<8} {} x {:.1} ms frame, {on_air_ms:.0} ms on air  ({file})",
            action.name,
            frame.code_hex.as_deref().unwrap_or("?"),
            frame.repeats,
            total_micros(&frame.pulses) as f64 / 1000.0,
        ),
        None => format!(
            "{:<14} {megahertz:.3} MHz  raw replay, {on_air_ms:.0} ms on air  ({file})",
            action.name
        ),
    }
}

fn inspect(path: &Path) -> anyhow::Result<()> {
    let sub = SubFile::load(path)?;
    println!("file        {}", path.display());
    println!(
        "frequency   {} Hz ({:.3} MHz)",
        sub.frequency_hz,
        f64::from(sub.frequency_hz) / 1e6
    );
    println!("preset      {}", sub.preset.name());
    println!("protocol    {}", sub.protocol);

    let raw = match &sub.payload {
        Payload::Key(key) => {
            println!("type        decoded key");
            println!("bits        {}", key.bits);
            let hex: Vec<String> = key.key.iter().map(|byte| format!("{byte:02X}")).collect();
            println!("key         {}", hex.join(" "));
            if let Some(te) = key.te_us {
                println!("TE          {te} us");
            }
            println!("note        decoded keys cannot be transmitted; capture with Read RAW");
            return Ok(());
        }
        Payload::Raw(raw) => raw,
    };

    let total_us: u64 = raw
        .iter()
        .map(|value| u64::from(value.unsigned_abs()))
        .sum();
    println!("type        RAW recording");
    println!(
        "timings     {} values, {:.1} ms in total",
        raw.len(),
        total_us as f64 / 1000.0
    );

    match extract_frame(raw, &ExtractOptions::default()) {
        Ok(frame) => {
            let frame_us = total_micros(&frame.pulses);
            println!(
                "frame       {} pulses, {:.1} ms, found {} clean copies",
                frame.pulses.len(),
                frame_us as f64 / 1000.0,
                frame.copies_found
            );
            println!(
                "gap         {:.1} ms between copies",
                f64::from(frame.gap_us) / 1000.0
            );
            if let Some(bits) = pi_alarm::signal::decode_bits(&frame.pulses) {
                let hex = pi_alarm::signal::bits_to_hex(&bits).unwrap_or_else(|| "-".to_string());
                println!("code        {bits} ({} bits, hex {hex})", bits.len());
            }
            // The frame itself in Flipper notation: +on / -off microseconds.
            let timings: Vec<String> = frame
                .pulses
                .iter()
                .map(|pulse| {
                    if pulse.high {
                        format!("{}", pulse.micros)
                    } else {
                        format!("-{}", pulse.micros)
                    }
                })
                .collect();
            println!("timings     {}", timings.join(" "));
        }
        Err(error) => println!("frame       none: {error}"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// alarms
// ---------------------------------------------------------------------------

fn print_alarm(alarm: &Alarm) {
    let state = if alarm.enabled { "on " } else { "off" };
    println!(
        "{}  {}  {}  {}  {}",
        alarm.id,
        state,
        alarm.time_text(),
        alarm.days_text(),
        alarm.label
    );
    for step in &alarm.steps {
        println!("    {:>8}  {}", format_offset(step.offset_s), step.action);
    }
}

fn set_enabled(store: &AlarmStore, id: &str, enabled: bool) -> anyhow::Result<()> {
    let alarms = store.load()?;
    // `into_iter().find(..)` takes ownership of the matching alarm.
    let Some(mut alarm) = alarms.into_iter().find(|alarm| alarm.id == id) else {
        bail!("no alarm with id `{id}`");
    };
    alarm.enabled = enabled;
    store.put(alarm.clone())?;
    print_alarm(&alarm);
    Ok(())
}

/// `"06:30"` -> `(6, 30)`.
fn parse_time(text: &str) -> anyhow::Result<(u32, u32)> {
    let parsed = text.split_once(':').and_then(|(hour, minute)| {
        // `?` inside a closure that returns Option stops at the first None.
        Some((hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?))
    });
    match parsed {
        Some((hour, minute)) if hour < 24 && minute < 60 => Ok((hour, minute)),
        _ => bail!("`{text}` is not a time; use HH:MM, e.g. 06:30"),
    }
}

/// `"mon,wed"`, `"daily"`, `"weekdays"` or `"weekends"` -> list of days.
fn parse_days(text: &str) -> anyhow::Result<Vec<Day>> {
    match text.to_ascii_lowercase().as_str() {
        "daily" | "all" => return Ok(Day::ALL.to_vec()),
        "weekdays" => return Ok(vec![Day::Mon, Day::Tue, Day::Wed, Day::Thu, Day::Fri]),
        "weekends" => return Ok(vec![Day::Sat, Day::Sun]),
        _ => {}
    }
    let mut days = Vec::new();
    for name in text.split(',') {
        let Some(day) = Day::from_name(name.trim()) else {
            bail!("`{name}` is not a day; use mon,tue,wed,thu,fri,sat,sun");
        };
        days.push(day);
    }
    Ok(days)
}

/// `"light_toggle@-10m"` -> a `Step` with offset -600 seconds.
fn parse_step(text: &str) -> anyhow::Result<Step> {
    let Some((action, offset)) = text.split_once('@') else {
        bail!("`{text}` is not a step; use ACTION@OFFSET, e.g. fan_3@0 or light_toggle@-10m");
    };
    Ok(Step {
        offset_s: parse_offset(offset)?,
        action: action.to_string(),
    })
}

/// `"-10m"` -> -600, `"90"` or `"90s"` -> 90, `"1h"` -> 3600.
fn parse_offset(text: &str) -> anyhow::Result<i32> {
    let text = text.trim();
    // Split a trailing unit letter from the number in front of it.
    let (number, multiplier) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1),
        Some('m') => (&text[..text.len() - 1], 60),
        Some('h') => (&text[..text.len() - 1], 3600),
        _ => (text, 1),
    };
    let value: i32 = number
        .parse()
        .with_context(|| format!("`{text}` is not an offset; use e.g. -10m, 90s or 0"))?;
    value
        .checked_mul(multiplier)
        .with_context(|| format!("offset `{text}` is too large"))
}

/// -600 -> `"-10m"`, 90 -> `"+90s"`, 0 -> `"0"`.
fn format_offset(seconds: i32) -> String {
    if seconds == 0 {
        "0".to_string()
    } else if seconds % 60 == 0 {
        format!("{:+}m", seconds / 60)
    } else {
        format!("{seconds:+}s")
    }
}

/// An 8-character id. Not cryptographic, just unlikely to collide: it mixes
/// the current time with the process id.
fn random_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let mixed = (nanos as u64) ^ (u64::from(std::process::id()) << 32);
    let id = format!("{:08x}", mixed as u32);
    debug_assert!(is_valid_id(&id));
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse() {
        assert_eq!(parse_time("06:30").unwrap(), (6, 30));
        assert_eq!(parse_time("0:05").unwrap(), (0, 5));
        assert_eq!(parse_time("23:59").unwrap(), (23, 59));
        for bad in ["24:00", "12:60", "noon", "12", "12:", ":30", "-1:30"] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn days_parse() {
        assert_eq!(parse_days("daily").unwrap().len(), 7);
        assert_eq!(parse_days("weekdays").unwrap().len(), 5);
        assert_eq!(parse_days("weekends").unwrap(), vec![Day::Sat, Day::Sun]);
        assert_eq!(parse_days("mon, WED").unwrap(), vec![Day::Mon, Day::Wed]);
        assert!(parse_days("mon,funday").is_err());
        assert!(parse_days("").is_err());
    }

    #[test]
    fn steps_and_offsets_parse() {
        let step = parse_step("light_toggle@-10m").unwrap();
        assert_eq!(step.action, "light_toggle");
        assert_eq!(step.offset_s, -600);

        assert_eq!(parse_offset("0").unwrap(), 0);
        assert_eq!(parse_offset("90").unwrap(), 90);
        assert_eq!(parse_offset("+90s").unwrap(), 90);
        assert_eq!(parse_offset("-45s").unwrap(), -45);
        assert_eq!(parse_offset("1h").unwrap(), 3600);
        assert_eq!(parse_offset("-2h").unwrap(), -7200);
        for bad in ["", "m", "ten", "1.5m", "99999999h"] {
            assert!(parse_offset(bad).is_err(), "{bad}");
        }
        assert!(parse_step("fan_3").is_err());
    }

    #[test]
    fn offsets_format_compactly() {
        assert_eq!(format_offset(0), "0");
        assert_eq!(format_offset(-600), "-10m");
        assert_eq!(format_offset(300), "+5m");
        assert_eq!(format_offset(90), "+90s");
        assert_eq!(format_offset(-45), "-45s");
    }

    #[test]
    fn random_ids_are_valid() {
        let id = random_id();
        assert_eq!(id.len(), 8);
        assert!(is_valid_id(&id));
    }

    #[test]
    fn the_command_line_definition_is_consistent() {
        // clap checks its own configuration (duplicate flags and so on).
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
