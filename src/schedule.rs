//! Time arithmetic: when exactly does each step of each alarm happen?
//!
//! Everything here is a pure function: alarms and a time range go in, a list
//! of steps with exact UTC instants comes out. Nothing reads the clock, so
//! all of it is easy to test, including the awkward daylight-saving days.
//!
//! # Rules
//!
//! * An alarm's time is wall-clock time in the daemon's time zone.
//! * `days` refers to the local date of the **alarm time**, not of a step. A
//!   Monday 00:05 alarm with a step at -10 minutes fires that step at 23:55
//!   on Sunday night, and it is still "Monday's alarm".
//! * A step fires at `alarm instant + offset`, counted in real elapsed
//!   seconds.
//! * **Clocks go forward** (spring): a time such as 02:30 that does not exist
//!   that night is treated as if the clocks had not changed yet, which is
//!   03:30 on the new clock. The alarm still fires once that day.
//! * **Clocks go back** (autumn): a time such as 01:30 that happens twice
//!   fires at the first one only.

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;

use crate::alarm::{Alarm, Day, MAX_OFFSET_SECS};

/// One step of one alarm on one particular day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueStep {
    pub alarm_id: String,
    /// Position of the step in the alarm's `steps` list.
    pub step_index: usize,
    pub action: String,
    /// The instant this step should run.
    pub fire_at: DateTime<Utc>,
    /// The instant of the alarm itself (the step's offset is relative to it).
    pub alarm_at: DateTime<Utc>,
}

/// The UTC instant of wall-clock time `hour:minute` on `date` in zone `tz`.
///
/// Returns `None` only for an invalid hour/minute.
pub fn alarm_instant(tz: Tz, date: NaiveDate, hour: u32, minute: u32) -> Option<DateTime<Utc>> {
    // A "naive" date-time has no time zone attached yet.
    let naive = date.and_hms_opt(hour, minute, 0)?;

    // Asking "which instant is this local time?" has three possible answers.
    let instant = match tz.from_local_datetime(&naive) {
        // The normal case: exactly one.
        LocalResult::Single(time) => time.with_timezone(&Utc),
        // Clocks went back and this time happens twice: take the first.
        LocalResult::Ambiguous(first, _second) => first.with_timezone(&Utc),
        // Clocks went forward and skipped this time. Use the UTC offset that
        // was in force about a day earlier, i.e. pretend the change has not
        // happened yet. 02:30 in the old offset is 03:30 on the new clock.
        LocalResult::None => {
            let a_day_earlier = naive - Duration::days(1);
            let old_offset = tz
                .offset_from_utc_datetime(&a_day_earlier)
                .fix()
                .local_minus_utc();
            Utc.from_utc_datetime(&(naive - Duration::seconds(i64::from(old_offset))))
        }
    };
    Some(instant)
}

/// Every step that fires in the range `after < fire_at <= until`.
///
/// The result is sorted by time. Steps at the same instant keep the order of
/// the alarm list and then of the alarm's own step list.
pub fn steps_between(
    alarms: &[Alarm],
    tz: Tz,
    after: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Vec<DueStep> {
    let mut due = Vec::new();
    if until <= after {
        return due;
    }

    // Which calendar days could contain an alarm whose steps land in the
    // range? A step is at most MAX_OFFSET_SECS away from its alarm, and the
    // extra day on each side covers time zone and DST differences.
    let margin = Duration::seconds(i64::from(MAX_OFFSET_SECS)) + Duration::days(1);
    let first_date = (after - margin).with_timezone(&tz).date_naive();
    let last_date = (until + margin).with_timezone(&tz).date_naive();

    let mut date = first_date;
    while date <= last_date {
        let day = Day::from_weekday(date.weekday());
        for alarm in alarms {
            if !alarm.enabled || !alarm.days.contains(&day) {
                continue;
            }
            let Some(alarm_at) = alarm_instant(tz, date, alarm.hour, alarm.minute) else {
                continue;
            };
            // `enumerate` gives (index, item) pairs.
            for (step_index, step) in alarm.steps.iter().enumerate() {
                let fire_at = alarm_at + Duration::seconds(i64::from(step.offset_s));
                if fire_at > after && fire_at <= until {
                    due.push(DueStep {
                        alarm_id: alarm.id.clone(),
                        step_index,
                        action: step.action.clone(),
                        fire_at,
                        alarm_at,
                    });
                }
            }
        }
        // `succ_opt` is "the next day"; it only fails at the end of time.
        match date.succ_opt() {
            Some(next) => date = next,
            None => break,
        }
    }

    // Stable sort: equal times keep the order they were pushed in.
    due.sort_by_key(|step| step.fire_at);
    due
}

/// The next step after `after`, looking up to eight days ahead (every
/// enabled alarm fires at least once a week).
pub fn next_step_after(alarms: &[Alarm], tz: Tz, after: DateTime<Utc>) -> Option<DueStep> {
    let horizon = after + Duration::days(8);
    steps_between(alarms, tz, after, horizon).into_iter().next()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::alarm::Step;
    use chrono::Timelike;
    use chrono_tz::America::{Chicago, New_York};
    use chrono_tz::Australia::Lord_Howe;
    use chrono_tz::Europe::London;
    use chrono_tz::Pacific::Auckland;

    /// Shorthand for a UTC instant.
    pub(crate) fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    /// An alarm with the given time, days and `(offset, action)` steps.
    pub(crate) fn alarm(
        id: &str,
        hour: u32,
        minute: u32,
        days: &[Day],
        steps: &[(i32, &str)],
    ) -> Alarm {
        Alarm {
            id: id.to_string(),
            label: String::new(),
            hour,
            minute,
            days: days.to_vec(),
            enabled: true,
            steps: steps
                .iter()
                .map(|(offset_s, action)| Step {
                    offset_s: *offset_s,
                    action: action.to_string(),
                })
                .collect(),
        }
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    // ---- alarm_instant ---------------------------------------------------

    #[test]
    fn ordinary_days_use_the_zone_offset() {
        // Chicago is UTC-6 in winter and UTC-5 in summer.
        assert_eq!(
            alarm_instant(Chicago, date(2026, 1, 15), 6, 30),
            Some(utc(2026, 1, 15, 12, 30))
        );
        assert_eq!(
            alarm_instant(Chicago, date(2026, 7, 15), 6, 30),
            Some(utc(2026, 7, 15, 11, 30))
        );
    }

    #[test]
    fn invalid_times_give_none() {
        assert_eq!(alarm_instant(Chicago, date(2026, 1, 15), 24, 0), None);
        assert_eq!(alarm_instant(Chicago, date(2026, 1, 15), 6, 60), None);
    }

    #[test]
    fn spring_forward_skipped_time_fires_an_hour_later_on_the_new_clock() {
        // US clocks jump from 02:00 to 03:00 on 2026-03-08.
        // 02:30 does not exist. In the old offset (UTC-5) it is 07:30 UTC,
        // which the new clock (UTC-4) shows as 03:30.
        let instant = alarm_instant(New_York, date(2026, 3, 8), 2, 30).unwrap();
        assert_eq!(instant, utc(2026, 3, 8, 7, 30));
        let local = instant.with_timezone(&New_York);
        assert_eq!((local.hour(), local.minute()), (3, 30));
    }

    #[test]
    fn spring_forward_does_not_disturb_times_outside_the_gap() {
        // 01:59 is still on the old offset, 03:00 already on the new one.
        assert_eq!(
            alarm_instant(New_York, date(2026, 3, 8), 1, 59),
            Some(utc(2026, 3, 8, 6, 59))
        );
        assert_eq!(
            alarm_instant(New_York, date(2026, 3, 8), 3, 0),
            Some(utc(2026, 3, 8, 7, 0))
        );
        // A 07:00 alarm is an hour earlier in UTC than the day before.
        assert_eq!(
            alarm_instant(New_York, date(2026, 3, 7), 7, 0),
            Some(utc(2026, 3, 7, 12, 0))
        );
        assert_eq!(
            alarm_instant(New_York, date(2026, 3, 8), 7, 0),
            Some(utc(2026, 3, 8, 11, 0))
        );
    }

    #[test]
    fn fall_back_repeated_time_fires_at_the_first_occurrence() {
        // US clocks go from 02:00 back to 01:00 on 2026-11-01, so 01:30
        // happens at 05:30 UTC (daylight time) and again at 06:30 UTC.
        assert_eq!(
            alarm_instant(New_York, date(2026, 11, 1), 1, 30),
            Some(utc(2026, 11, 1, 5, 30))
        );
        // A 07:00 alarm is an hour later in UTC than the day before.
        assert_eq!(
            alarm_instant(New_York, date(2026, 10, 31), 7, 0),
            Some(utc(2026, 10, 31, 11, 0))
        );
        assert_eq!(
            alarm_instant(New_York, date(2026, 11, 1), 7, 0),
            Some(utc(2026, 11, 1, 12, 0))
        );
    }

    #[test]
    fn european_transitions_work_too() {
        // UK: 01:00 -> 02:00 on 2026-03-29, 02:00 -> 01:00 on 2026-10-25.
        assert_eq!(
            alarm_instant(London, date(2026, 3, 29), 1, 30),
            Some(utc(2026, 3, 29, 1, 30)),
            "skipped time, old offset is UTC+0"
        );
        assert_eq!(
            alarm_instant(London, date(2026, 10, 25), 1, 30),
            Some(utc(2026, 10, 25, 0, 30)),
            "repeated time, first occurrence is still summer time"
        );
    }

    #[test]
    fn a_half_hour_daylight_shift_is_handled() {
        // Lord Howe Island moves its clocks by only 30 minutes:
        // 02:00 -> 02:30 on 2026-10-04 (UTC+10:30 -> UTC+11).
        // 02:15 does not exist; in the old offset it is 15:45 UTC the day
        // before, which the new clock shows as 02:45.
        let instant = alarm_instant(Lord_Howe, date(2026, 10, 4), 2, 15).unwrap();
        assert_eq!(instant, utc(2026, 10, 3, 15, 45));
        let local = instant.with_timezone(&Lord_Howe);
        assert_eq!((local.hour(), local.minute()), (2, 45));
    }

    // ---- steps_between ---------------------------------------------------

    #[test]
    fn fires_on_selected_days_only() {
        // 2026-11-09 is a Monday.
        let alarms = [alarm("a", 6, 30, &[Day::Mon, Day::Wed], &[(0, "fan_3")])];
        let week = steps_between(
            &alarms,
            Chicago,
            utc(2026, 11, 9, 0, 0),
            utc(2026, 11, 16, 0, 0),
        );
        let times: Vec<DateTime<Utc>> = week.iter().map(|step| step.fire_at).collect();
        assert_eq!(
            times,
            vec![utc(2026, 11, 9, 12, 30), utc(2026, 11, 11, 12, 30)]
        );
        assert_eq!(week[0].alarm_id, "a");
        assert_eq!(week[0].action, "fan_3");
        assert_eq!(week[0].step_index, 0);
        assert_eq!(week[0].alarm_at, week[0].fire_at);
    }

    #[test]
    fn the_range_excludes_its_start_and_includes_its_end() {
        let alarms = [alarm("a", 6, 30, &Day::ALL, &[(0, "fan_3")])];
        let at = utc(2026, 11, 9, 12, 30);
        let second = Duration::seconds(1);
        assert_eq!(steps_between(&alarms, Chicago, at - second, at).len(), 1);
        assert_eq!(steps_between(&alarms, Chicago, at, at + second).len(), 0);
        assert_eq!(steps_between(&alarms, Chicago, at, at).len(), 0);
        // A backwards range is simply empty.
        assert_eq!(steps_between(&alarms, Chicago, at, at - second).len(), 0);
    }

    #[test]
    fn steps_are_offset_from_the_alarm_time() {
        let alarms = [alarm(
            "a",
            6,
            30,
            &Day::ALL,
            &[(-600, "light_toggle"), (0, "fan_3"), (90, "fan_off")],
        )];
        let steps = steps_between(
            &alarms,
            Chicago,
            utc(2026, 11, 9, 12, 0),
            utc(2026, 11, 9, 13, 0),
        );
        let summary: Vec<(&str, DateTime<Utc>)> = steps
            .iter()
            .map(|step| (step.action.as_str(), step.fire_at))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("light_toggle", utc(2026, 11, 9, 12, 20)),
                ("fan_3", utc(2026, 11, 9, 12, 30)),
                ("fan_off", utc(2026, 11, 9, 12, 31) + Duration::seconds(30)),
            ]
        );
        // All three belong to the same alarm instant.
        for step in &steps {
            assert_eq!(step.alarm_at, utc(2026, 11, 9, 12, 30));
        }
    }

    #[test]
    fn a_step_before_midnight_belongs_to_the_next_days_alarm() {
        // Monday 00:05, with a step ten minutes earlier: Sunday 23:55 local.
        let alarms = [alarm("a", 0, 5, &[Day::Mon], &[(-600, "light_toggle")])];
        let steps = steps_between(
            &alarms,
            Chicago,
            utc(2026, 11, 8, 0, 0),
            utc(2026, 11, 15, 0, 0),
        );
        assert_eq!(steps.len(), 1);
        // Sunday 2026-11-08 23:55 CST = Monday 05:55 UTC.
        assert_eq!(steps[0].fire_at, utc(2026, 11, 9, 5, 55));
        let local = steps[0].fire_at.with_timezone(&Chicago);
        assert_eq!(local.weekday(), chrono::Weekday::Sun);
    }

    #[test]
    fn a_step_after_midnight_belongs_to_the_previous_days_alarm() {
        // Friday 23:50 with a step 20 minutes later: Saturday 00:10 local.
        let alarms = [alarm("a", 23, 50, &[Day::Fri], &[(1200, "fan_off")])];
        let steps = steps_between(
            &alarms,
            Chicago,
            utc(2026, 11, 9, 0, 0),
            utc(2026, 11, 16, 0, 0),
        );
        assert_eq!(steps.len(), 1);
        let local = steps[0].fire_at.with_timezone(&Chicago);
        assert_eq!(local.weekday(), chrono::Weekday::Sat);
        assert_eq!((local.hour(), local.minute()), (0, 10));
    }

    #[test]
    fn days_are_local_days_even_far_from_utc() {
        // Auckland is UTC+13 in November. Monday 07:00 there is still
        // Sunday in UTC.
        let alarms = [alarm("a", 7, 0, &[Day::Mon], &[(0, "fan_3")])];
        let steps = steps_between(
            &alarms,
            Auckland,
            utc(2026, 11, 7, 0, 0),
            utc(2026, 11, 14, 0, 0),
        );
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].fire_at, utc(2026, 11, 8, 18, 0));
        assert_eq!(steps[0].fire_at.weekday(), chrono::Weekday::Sun);
        assert_eq!(
            steps[0].fire_at.with_timezone(&Auckland).weekday(),
            chrono::Weekday::Mon
        );
    }

    #[test]
    fn disabled_alarms_never_fire() {
        let mut off = alarm("a", 6, 30, &Day::ALL, &[(0, "fan_3")]);
        off.enabled = false;
        let steps = steps_between(
            &[off],
            Chicago,
            utc(2026, 11, 1, 0, 0),
            utc(2026, 12, 1, 0, 0),
        );
        assert!(steps.is_empty());
    }

    #[test]
    fn simultaneous_steps_keep_list_order() {
        let alarms = [
            alarm(
                "first",
                6,
                30,
                &Day::ALL,
                &[(0, "light_toggle"), (0, "fan_3")],
            ),
            alarm("second", 6, 20, &Day::ALL, &[(600, "fan_off")]),
        ];
        let steps = steps_between(
            &alarms,
            Chicago,
            utc(2026, 11, 9, 12, 0),
            utc(2026, 11, 9, 13, 0),
        );
        let order: Vec<(&str, &str)> = steps
            .iter()
            .map(|step| (step.alarm_id.as_str(), step.action.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("first", "light_toggle"),
                ("first", "fan_3"),
                ("second", "fan_off")
            ]
        );
    }

    #[test]
    fn a_daily_alarm_fires_exactly_once_per_day_all_year() {
        // 02:30 is the nastiest time: skipped in March, repeated in November.
        for (hour, minute) in [(2, 30), (1, 30), (7, 0), (0, 0), (23, 59)] {
            let alarms = [alarm("a", hour, minute, &Day::ALL, &[(0, "fan_3")])];
            // New York midnight on Jan 1 is 05:00 UTC.
            let steps = steps_between(
                &alarms,
                New_York,
                utc(2026, 1, 1, 5, 0) - Duration::seconds(1),
                utc(2027, 1, 1, 5, 0) - Duration::seconds(1),
            );
            assert_eq!(steps.len(), 365, "{hour:02}:{minute:02}");

            // Exactly one per local calendar day, in order.
            let mut expected_date = date(2026, 1, 1);
            for step in &steps {
                let local = step.fire_at.with_timezone(&New_York);
                assert_eq!(local.date_naive(), expected_date);
                // The clock shows the alarm time, except on the one day when
                // that time does not exist.
                let shown = (local.hour(), local.minute());
                if shown != (hour, minute) {
                    assert_eq!(expected_date, date(2026, 3, 8));
                    assert_eq!(shown, (hour + 1, minute));
                }
                expected_date = expected_date.succ_opt().unwrap();
            }
        }
    }

    #[test]
    fn consecutive_days_are_23_24_or_25_hours_apart() {
        let alarms = [alarm("a", 7, 0, &Day::ALL, &[(0, "fan_3")])];
        let steps = steps_between(
            &alarms,
            New_York,
            utc(2026, 1, 1, 0, 0),
            utc(2027, 1, 1, 0, 0),
        );
        let mut short_days = 0;
        let mut long_days = 0;
        // `windows(2)` gives every pair of neighbours.
        for pair in steps.windows(2) {
            let hours = (pair[1].fire_at - pair[0].fire_at).num_hours();
            match hours {
                23 => short_days += 1,
                24 => {}
                25 => long_days += 1,
                other => panic!("alarms {other} hours apart"),
            }
        }
        assert_eq!((short_days, long_days), (1, 1));
    }

    // ---- next_step_after -------------------------------------------------

    #[test]
    fn next_step_finds_the_soonest_one() {
        let alarms = [
            alarm(
                "weekday",
                6,
                30,
                &[Day::Mon],
                &[(-600, "light_toggle"), (0, "fan_3")],
            ),
            alarm("weekend", 9, 0, &[Day::Sat], &[(0, "fan_3")]),
        ];
        // Tuesday 2026-11-10: the Saturday alarm comes first.
        let next = next_step_after(&alarms, Chicago, utc(2026, 11, 10, 18, 0)).unwrap();
        assert_eq!(next.alarm_id, "weekend");
        assert_eq!(next.fire_at, utc(2026, 11, 14, 15, 0));

        // Just after that, the next is Monday's first step (06:20 local).
        let next = next_step_after(&alarms, Chicago, next.fire_at).unwrap();
        assert_eq!(next.alarm_id, "weekday");
        assert_eq!(next.action, "light_toggle");
        assert_eq!(next.fire_at, utc(2026, 11, 16, 12, 20));
    }

    #[test]
    fn next_step_is_none_without_enabled_alarms() {
        assert_eq!(next_step_after(&[], Chicago, utc(2026, 11, 10, 0, 0)), None);
        let mut off = alarm("a", 6, 30, &Day::ALL, &[(0, "fan_3")]);
        off.enabled = false;
        assert_eq!(
            next_step_after(&[off], Chicago, utc(2026, 11, 10, 0, 0)),
            None
        );
    }
}
