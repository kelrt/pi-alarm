//! What an alarm is, and the rules a valid one must follow.
//!
//! An alarm is a wall-clock time, the days of the week it applies to, and an
//! ordered list of steps. Each step names an action and an offset in seconds
//! relative to the alarm time, for example:
//!
//! ```text
//! 06:30 on weekdays
//!   -600 s  light_toggle   (06:20)
//!      0 s  fan_3          (06:30)
//! ```
//!
//! The same struct is used in three places: the JSON file on disk, the BLE
//! messages, and the scheduler. `Serialize`/`Deserialize` are derived, so the
//! JSON layout is exactly the field names below.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::is_valid_action_name;

/// Most alarms the store will hold.
pub const MAX_ALARMS: usize = 20;
/// Most steps in one alarm.
pub const MAX_STEPS: usize = 8;
/// Longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 40;
/// A step may be at most this far from its alarm time, in seconds (6 hours).
pub const MAX_OFFSET_SECS: i32 = 6 * 3600;
/// Longest alarm id, in characters.
pub const MAX_ID_CHARS: usize = 24;

/// A day of the week. In JSON: `"mon"`, `"tue"`, ...
///
/// Deriving `PartialOrd`/`Ord` orders the variants as listed, Monday first,
/// which lets us sort a list of days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Day {
    pub const ALL: [Day; 7] = [
        Day::Mon,
        Day::Tue,
        Day::Wed,
        Day::Thu,
        Day::Fri,
        Day::Sat,
        Day::Sun,
    ];

    /// Convert from chrono's weekday type.
    pub fn from_weekday(weekday: chrono::Weekday) -> Day {
        match weekday {
            chrono::Weekday::Mon => Day::Mon,
            chrono::Weekday::Tue => Day::Tue,
            chrono::Weekday::Wed => Day::Wed,
            chrono::Weekday::Thu => Day::Thu,
            chrono::Weekday::Fri => Day::Fri,
            chrono::Weekday::Sat => Day::Sat,
            chrono::Weekday::Sun => Day::Sun,
        }
    }

    /// The lowercase three-letter name, as used in JSON and on the CLI.
    pub fn name(self) -> &'static str {
        match self {
            Day::Mon => "mon",
            Day::Tue => "tue",
            Day::Wed => "wed",
            Day::Thu => "thu",
            Day::Fri => "fri",
            Day::Sat => "sat",
            Day::Sun => "sun",
        }
    }

    /// Parse a three-letter name (any case). `None` if it is not a day.
    pub fn from_name(name: &str) -> Option<Day> {
        let lower = name.to_ascii_lowercase();
        Day::ALL.into_iter().find(|day| day.name() == lower)
    }
}

/// One thing an alarm does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Seconds relative to the alarm time. Negative = before.
    pub offset_s: i32,
    /// Name of an action from the config, e.g. `"fan_3"`.
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    /// Chosen by whoever creates the alarm (the phone app picks a random
    /// one). Sending an alarm with an existing id replaces that alarm.
    pub id: String,
    /// Free text shown in the app. May be empty.
    #[serde(default)]
    pub label: String,
    /// Hour of the alarm time, 0-23, in the daemon's time zone.
    pub hour: u32,
    /// Minute of the alarm time, 0-59.
    pub minute: u32,
    /// The days this alarm fires on. Must not be empty.
    pub days: Vec<Day>,
    pub enabled: bool,
    pub steps: Vec<Step>,
}

/// Why an alarm was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlarmError {
    BadId,
    LabelTooLong,
    BadTime,
    NoDays,
    NoSteps,
    TooManySteps,
    BadActionName(String),
    UnknownAction(String),
    OffsetTooLarge(i32),
}

impl fmt::Display for AlarmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlarmError::BadId => write!(
                f,
                "id must be 1-{MAX_ID_CHARS} characters from a-z, 0-9, - and _"
            ),
            AlarmError::LabelTooLong => {
                write!(f, "label must be at most {MAX_LABEL_CHARS} characters")
            }
            AlarmError::BadTime => write!(f, "hour must be 0-23 and minute 0-59"),
            AlarmError::NoDays => write!(f, "at least one day is required"),
            AlarmError::NoSteps => write!(f, "at least one step is required"),
            AlarmError::TooManySteps => write!(f, "at most {MAX_STEPS} steps are allowed"),
            AlarmError::BadActionName(name) => write!(f, "`{name}` is not a valid action name"),
            AlarmError::UnknownAction(name) => write!(f, "unknown action `{name}`"),
            AlarmError::OffsetTooLarge(offset) => write!(
                f,
                "step offset {offset} s is out of range (at most {MAX_OFFSET_SECS} s either way)"
            ),
        }
    }
}

impl std::error::Error for AlarmError {}

impl Alarm {
    /// Check every rule. `known_actions` is the list of action names from the
    /// config; pass `None` to skip the "does this action exist" check.
    pub fn validate(&self, known_actions: Option<&[String]>) -> Result<(), AlarmError> {
        if !is_valid_id(&self.id) {
            return Err(AlarmError::BadId);
        }
        if self.label.chars().count() > MAX_LABEL_CHARS {
            return Err(AlarmError::LabelTooLong);
        }
        if self.hour > 23 || self.minute > 59 {
            return Err(AlarmError::BadTime);
        }
        if self.days.is_empty() {
            return Err(AlarmError::NoDays);
        }
        if self.steps.is_empty() {
            return Err(AlarmError::NoSteps);
        }
        if self.steps.len() > MAX_STEPS {
            return Err(AlarmError::TooManySteps);
        }
        for step in &self.steps {
            if !is_valid_action_name(&step.action) {
                return Err(AlarmError::BadActionName(step.action.clone()));
            }
            if step.offset_s.abs() > MAX_OFFSET_SECS {
                return Err(AlarmError::OffsetTooLarge(step.offset_s));
            }
            if let Some(known) = known_actions {
                if !known.contains(&step.action) {
                    return Err(AlarmError::UnknownAction(step.action.clone()));
                }
            }
        }
        Ok(())
    }

    /// Put the alarm in its tidy form: days sorted Monday-first without
    /// duplicates, steps sorted by offset.
    pub fn normalize(&mut self) {
        self.days.sort();
        self.days.dedup();
        // `sort_by_key` is a stable sort: steps with the same offset keep
        // the order they were given in.
        self.steps.sort_by_key(|step| step.offset_s);
    }

    /// `"06:30"`.
    pub fn time_text(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// `"mon,tue,wed"`, or `"every day"` when all seven are set.
    pub fn days_text(&self) -> String {
        if Day::ALL.iter().all(|day| self.days.contains(day)) {
            return "every day".to_string();
        }
        let names: Vec<&str> = self.days.iter().map(|day| day.name()).collect();
        names.join(",")
    }
}

pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid weekday alarm used by tests in several modules.
    pub(crate) fn sample_alarm(id: &str) -> Alarm {
        Alarm {
            id: id.to_string(),
            label: "Work".to_string(),
            hour: 6,
            minute: 30,
            days: vec![Day::Mon, Day::Tue, Day::Wed, Day::Thu, Day::Fri],
            enabled: true,
            steps: vec![
                Step {
                    offset_s: -600,
                    action: "light_toggle".to_string(),
                },
                Step {
                    offset_s: 0,
                    action: "fan_3".to_string(),
                },
            ],
        }
    }

    #[test]
    fn json_layout_is_stable() {
        let json = serde_json::to_string(&sample_alarm("a1")).unwrap();
        assert_eq!(
            json,
            r#"{"id":"a1","label":"Work","hour":6,"minute":30,"days":["mon","tue","wed","thu","fri"],"enabled":true,"steps":[{"offset_s":-600,"action":"light_toggle"},{"offset_s":0,"action":"fan_3"}]}"#
        );
        let back: Alarm = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sample_alarm("a1"));
    }

    #[test]
    fn label_is_optional_and_unknown_fields_are_ignored() {
        let json = r#"{"id":"x","hour":7,"minute":0,"days":["sun"],"enabled":false,
                       "steps":[{"offset_s":0,"action":"fan_3"}],"future_field":123}"#;
        let alarm: Alarm = serde_json::from_str(json).unwrap();
        assert_eq!(alarm.label, "");
        assert!(alarm.validate(None).is_ok());
    }

    #[test]
    fn the_sample_is_valid() {
        let known = vec!["fan_3".to_string(), "light_toggle".to_string()];
        assert_eq!(sample_alarm("a1").validate(Some(&known)), Ok(()));
    }

    #[test]
    fn each_rule_is_enforced() {
        let known = vec!["fan_3".to_string(), "light_toggle".to_string()];

        let mut alarm = sample_alarm("");
        assert_eq!(alarm.validate(None), Err(AlarmError::BadId));
        alarm.id = "Has Space".to_string();
        assert_eq!(alarm.validate(None), Err(AlarmError::BadId));
        alarm.id = "x".repeat(MAX_ID_CHARS + 1);
        assert_eq!(alarm.validate(None), Err(AlarmError::BadId));

        let mut alarm = sample_alarm("a");
        alarm.label = "x".repeat(MAX_LABEL_CHARS + 1);
        assert_eq!(alarm.validate(None), Err(AlarmError::LabelTooLong));

        let mut alarm = sample_alarm("a");
        alarm.hour = 24;
        assert_eq!(alarm.validate(None), Err(AlarmError::BadTime));
        alarm.hour = 23;
        alarm.minute = 60;
        assert_eq!(alarm.validate(None), Err(AlarmError::BadTime));

        let mut alarm = sample_alarm("a");
        alarm.days.clear();
        assert_eq!(alarm.validate(None), Err(AlarmError::NoDays));

        let mut alarm = sample_alarm("a");
        alarm.steps.clear();
        assert_eq!(alarm.validate(None), Err(AlarmError::NoSteps));

        let mut alarm = sample_alarm("a");
        let step = alarm.steps[0].clone();
        alarm.steps = vec![step; MAX_STEPS + 1];
        assert_eq!(alarm.validate(None), Err(AlarmError::TooManySteps));

        let mut alarm = sample_alarm("a");
        alarm.steps[0].offset_s = MAX_OFFSET_SECS + 1;
        assert_eq!(
            alarm.validate(None),
            Err(AlarmError::OffsetTooLarge(MAX_OFFSET_SECS + 1))
        );
        alarm.steps[0].offset_s = -MAX_OFFSET_SECS;
        assert_eq!(alarm.validate(None), Ok(()));

        let mut alarm = sample_alarm("a");
        alarm.steps[0].action = "Fan 3".to_string();
        assert_eq!(
            alarm.validate(None),
            Err(AlarmError::BadActionName("Fan 3".to_string()))
        );

        let mut alarm = sample_alarm("a");
        alarm.steps[1].action = "fan_1".to_string();
        assert_eq!(alarm.validate(None), Ok(()), "not checked without a list");
        assert_eq!(
            alarm.validate(Some(&known)),
            Err(AlarmError::UnknownAction("fan_1".to_string()))
        );
    }

    #[test]
    fn a_label_with_accents_counts_characters_not_bytes() {
        let mut alarm = sample_alarm("a");
        alarm.label = "é".repeat(MAX_LABEL_CHARS);
        assert_eq!(alarm.validate(None), Ok(()));
    }

    #[test]
    fn normalize_sorts_days_and_steps() {
        let mut alarm = sample_alarm("a");
        alarm.days = vec![Day::Fri, Day::Mon, Day::Fri, Day::Sun];
        alarm.steps = vec![
            Step {
                offset_s: 0,
                action: "fan_3".to_string(),
            },
            Step {
                offset_s: -600,
                action: "light_toggle".to_string(),
            },
            Step {
                offset_s: 0,
                action: "light_toggle".to_string(),
            },
        ];
        alarm.normalize();
        assert_eq!(alarm.days, vec![Day::Mon, Day::Fri, Day::Sun]);
        let order: Vec<(i32, &str)> = alarm
            .steps
            .iter()
            .map(|step| (step.offset_s, step.action.as_str()))
            .collect();
        // The two steps at offset 0 keep their original order.
        assert_eq!(
            order,
            vec![(-600, "light_toggle"), (0, "fan_3"), (0, "light_toggle")]
        );
    }

    #[test]
    fn text_helpers() {
        let mut alarm = sample_alarm("a");
        assert_eq!(alarm.time_text(), "06:30");
        assert_eq!(alarm.days_text(), "mon,tue,wed,thu,fri");
        alarm.days = Day::ALL.to_vec();
        assert_eq!(alarm.days_text(), "every day");
    }

    #[test]
    fn day_names_round_trip() {
        for day in Day::ALL {
            assert_eq!(Day::from_name(day.name()), Some(day));
        }
        assert_eq!(Day::from_name("MON"), Some(Day::Mon));
        assert_eq!(Day::from_name("monday"), None);
        assert_eq!(Day::from_weekday(chrono::Weekday::Sun), Day::Sun);
    }
}
