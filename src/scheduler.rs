//! Deciding which steps to run *now*.
//!
//! `schedule.rs` answers "when does each step happen?". This file adds the
//! memory needed to run each step exactly once in a messy world:
//!
//! * **The daemon restarts** (crash, reboot, update). State is saved to a
//!   small file, so a step that already ran is not run again, and a step
//!   that came due while the daemon was down is still run if it is only a
//!   little late.
//! * **The clock jumps forward.** The Pi has no real-time clock, so its time
//!   can leap ahead by hours when the phone or NTP corrects it. Steps that
//!   are now long past are skipped, not all fired at once.
//! * **The clock jumps backward.** A step that already ran must not run a
//!   second time when the clock reaches its time again. This matters most
//!   for `light_toggle`: running it twice turns the light back off.
//!
//! The scheduler never reads the clock itself. The caller passes `now` to
//! `poll`, which keeps this file testable.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::alarm::Alarm;
use crate::schedule::{steps_between, DueStep};
use crate::store::write_atomically;

const STATE_VERSION: u32 = 1;

/// Remembers one step that has been handed out to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FiredRecord {
    alarm: String,
    step: usize,
    action: String,
    /// The instant the step was scheduled for (not when it actually ran).
    at: DateTime<Utc>,
}

impl FiredRecord {
    fn of(step: &DueStep) -> FiredRecord {
        FiredRecord {
            alarm: step.alarm_id.clone(),
            step: step.step_index,
            action: step.action.clone(),
            at: step.fire_at,
        }
    }
}

/// The contents of the state file.
#[derive(Debug, Serialize, Deserialize)]
struct SavedState {
    version: u32,
    last_checked: DateTime<Utc>,
    fired: Vec<FiredRecord>,
}

/// What `poll` found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PollResult {
    /// Steps to run now, in order.
    pub due: Vec<DueStep>,
    /// Steps that are too late to run. Reported so they can be logged.
    pub missed: Vec<DueStep>,
}

impl PollResult {
    pub fn is_empty(&self) -> bool {
        self.due.is_empty() && self.missed.is_empty()
    }
}

#[derive(Debug)]
pub struct Scheduler {
    tz: Tz,
    /// How late a step may be and still run.
    grace: Duration,
    /// Everything up to and including this instant has been dealt with.
    last_checked: DateTime<Utc>,
    /// Steps handed out recently, to prevent running one twice.
    fired: Vec<FiredRecord>,
    /// Where state is saved. `None` keeps it in memory only (tests).
    state_path: Option<PathBuf>,
}

impl Scheduler {
    /// A scheduler with no history: nothing before `now` will run.
    pub fn new(tz: Tz, grace_secs: u32, now: DateTime<Utc>) -> Scheduler {
        Scheduler {
            tz,
            grace: Duration::seconds(i64::from(grace_secs)),
            last_checked: now,
            fired: Vec::new(),
            state_path: None,
        }
    }

    /// A scheduler that continues from the state file in `data_dir`, if
    /// there is one, and saves there from now on.
    pub fn restore(tz: Tz, grace_secs: u32, now: DateTime<Utc>, data_dir: &Path) -> Scheduler {
        let state_path = data_dir.join("scheduler-state.json");
        let mut scheduler = Scheduler::new(tz, grace_secs, now);

        match load_state(&state_path) {
            Ok(Some(saved)) => {
                scheduler.fired = saved.fired;
                if saved.last_checked <= now {
                    // Normal restart: carry on where we stopped. `poll` will
                    // not look back further than the grace period anyway.
                    scheduler.last_checked = saved.last_checked;
                } else {
                    // The saved time is in the future, so the clock has gone
                    // backwards (typical after a power cut with no network).
                    // We cannot know what was missed; start from now.
                    log::warn!(
                        "clock is behind the saved scheduler state ({} < {}); \
                         it was probably reset by a power cut",
                        now,
                        saved.last_checked
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                // A damaged state file must not stop the alarm clock.
                log::warn!("ignoring unreadable scheduler state: {error:#}");
            }
        }

        scheduler.state_path = Some(state_path);
        scheduler
    }

    pub fn timezone(&self) -> Tz {
        self.tz
    }

    /// Work out what to run at time `now`.
    ///
    /// Every step is returned at most once across all calls (and across
    /// restarts, if `save` is called after each poll that returned
    /// something).
    pub fn poll(&mut self, alarms: &[Alarm], now: DateTime<Utc>) -> PollResult {
        let mut result = PollResult::default();

        if now < self.last_checked {
            // The clock moved backwards while we were running. Nothing new
            // is due; the `fired` list stops recent steps from running a
            // second time when the clock reaches them again.
            log::warn!(
                "clock moved backwards by {} s",
                (self.last_checked - now).num_seconds()
            );
            self.last_checked = now;
            // But if it moved back a long way, the clock was simply wrong
            // before and whatever ran then ran at the wrong moment. Forget
            // those records so the alarm also runs at the correct time.
            let lifetime = self.record_lifetime();
            self.fired.retain(|record| record.at <= now + lifetime);
        }

        // Never look back further than the grace period.
        let earliest = now - self.grace;
        let mut window_start = self.last_checked;
        if window_start < earliest {
            // We were away (daemon stopped, or the clock jumped forward).
            // Steps in the skipped stretch are too late. Only the last day
            // is examined; it is just for the log.
            let report_from = window_start.max(earliest - Duration::days(1));
            result.missed = steps_between(alarms, self.tz, report_from, earliest)
                .into_iter()
                .filter(|step| !self.already_fired(step))
                .collect();
            window_start = earliest;
        }

        for step in steps_between(alarms, self.tz, window_start, now) {
            if self.already_fired(&step) {
                continue;
            }
            self.fired.push(FiredRecord::of(&step));
            result.due.push(step);
        }

        self.last_checked = now;
        self.forget_old_records(now);
        result
    }

    fn already_fired(&self, step: &DueStep) -> bool {
        let record = FiredRecord::of(step);
        self.fired.contains(&record)
    }

    /// How long a record stays useful: a step can only come up again while
    /// the clock is within the grace period of it. Twice that is a margin.
    fn record_lifetime(&self) -> Duration {
        self.grace * 2 + Duration::minutes(1)
    }

    /// Drop records of steps that are now well in the past.
    ///
    /// Records *ahead* of the clock are kept here on purpose. They exist
    /// after a power cut has reset the clock to an earlier time: those steps
    /// really did run, and must not run again as the clock catches up.
    fn forget_old_records(&mut self, now: DateTime<Utc>) {
        let lifetime = self.record_lifetime();
        self.fired.retain(|record| record.at >= now - lifetime);
    }

    /// Write the state file. Does nothing for an in-memory scheduler.
    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.state_path else {
            return Ok(());
        };
        let state = SavedState {
            version: STATE_VERSION,
            last_checked: self.last_checked,
            fired: self.fired.clone(),
        };
        let json = serde_json::to_string_pretty(&state)?;
        write_atomically(path, json.as_bytes())
    }
}

/// Read the state file. `Ok(None)` if there is none yet.
fn load_state(path: &Path) -> anyhow::Result<Option<SavedState>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let state: SavedState = serde_json::from_str(&text)?;
    if state.version != STATE_VERSION {
        anyhow::bail!("unknown state version {}", state.version);
    }
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::Day;
    use crate::schedule::tests::{alarm, utc};
    use chrono_tz::America::Chicago;

    const GRACE: u32 = 300;

    /// 06:30 Chicago (12:30 UTC in November) every day:
    /// light at 06:20, fan at 06:30.
    fn morning() -> Vec<Alarm> {
        vec![alarm(
            "wake",
            6,
            30,
            &Day::ALL,
            &[(-600, "light_toggle"), (0, "fan_3")],
        )]
    }

    fn actions(steps: &[DueStep]) -> Vec<&str> {
        steps.iter().map(|step| step.action.as_str()).collect()
    }

    fn secs(seconds: i64) -> Duration {
        Duration::seconds(seconds)
    }

    // ---- normal running --------------------------------------------------

    #[test]
    fn each_step_fires_once_at_its_time() {
        let alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 0));

        // Tick once a second from 12:00 to 12:40 UTC and note what fires.
        let mut fired = Vec::new();
        let mut now = utc(2026, 11, 10, 12, 0);
        while now <= utc(2026, 11, 10, 12, 40) {
            let result = scheduler.poll(&alarms, now);
            assert!(result.missed.is_empty());
            for step in result.due {
                fired.push((step.action, now));
            }
            now += secs(1);
        }

        assert_eq!(
            fired,
            vec![
                ("light_toggle".to_string(), utc(2026, 11, 10, 12, 20)),
                ("fan_3".to_string(), utc(2026, 11, 10, 12, 30)),
            ]
        );
    }

    #[test]
    fn nothing_before_the_start_time_fires() {
        let alarms = morning();
        // Started at 12:25: the 12:20 light step is in the past.
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 25));
        let result = scheduler.poll(&alarms, utc(2026, 11, 10, 12, 31));
        assert_eq!(actions(&result.due), ["fan_3"]);
        assert!(result.missed.is_empty());
    }

    #[test]
    fn polling_again_at_the_same_instant_returns_nothing() {
        let alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 29));
        let at = utc(2026, 11, 10, 12, 30);
        assert_eq!(actions(&scheduler.poll(&alarms, at).due), ["fan_3"]);
        assert!(scheduler.poll(&alarms, at).is_empty());
        assert!(scheduler.poll(&alarms, at + secs(1)).is_empty());
    }

    #[test]
    fn a_step_due_exactly_now_fires_now_not_a_tick_later() {
        let alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 29));
        assert!(scheduler
            .poll(&alarms, utc(2026, 11, 10, 12, 30) - secs(1))
            .is_empty());
        let result = scheduler.poll(&alarms, utc(2026, 11, 10, 12, 30));
        assert_eq!(actions(&result.due), ["fan_3"]);
    }

    #[test]
    fn it_keeps_working_day_after_day() {
        let alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 0, 0));
        let mut count = 0;
        // One poll a minute for a week.
        let mut now = utc(2026, 11, 10, 0, 0);
        while now < utc(2026, 11, 17, 0, 0) {
            count += scheduler.poll(&alarms, now).due.len();
            now += secs(60);
        }
        assert_eq!(count, 14, "two steps on each of seven days");
        assert!(scheduler.fired.len() <= 2, "old records are forgotten");
    }

    #[test]
    fn an_alarm_added_later_is_picked_up() {
        let mut alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 13, 0));
        assert!(scheduler.poll(&alarms, utc(2026, 11, 10, 13, 1)).is_empty());

        // 07:05 local = 13:05 UTC.
        alarms.push(alarm("extra", 7, 5, &Day::ALL, &[(0, "fan_off")]));
        let result = scheduler.poll(&alarms, utc(2026, 11, 10, 13, 5));
        assert_eq!(actions(&result.due), ["fan_off"]);
    }

    #[test]
    fn an_alarm_set_for_a_moment_ago_waits_until_tomorrow() {
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 13, 0));
        assert!(scheduler.poll(&[], utc(2026, 11, 10, 13, 10)).is_empty());

        // At 13:10 the user creates an alarm for 07:08 local (13:08 UTC),
        // two minutes ago. It must not fire today.
        let alarms = vec![alarm("late", 7, 8, &Day::ALL, &[(0, "fan_3")])];
        assert!(scheduler
            .poll(&alarms, utc(2026, 11, 10, 13, 10) + secs(1))
            .is_empty());
        let tomorrow = scheduler.poll(&alarms, utc(2026, 11, 11, 13, 8));
        // A whole day passed in one poll, so only the fresh one is due.
        assert_eq!(actions(&tomorrow.due), ["fan_3"]);
    }

    // ---- being late ------------------------------------------------------

    #[test]
    fn a_slightly_late_step_still_runs() {
        let alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 0));
        // Nothing polled between 12:00 and 12:33 (the Pi was busy or asleep).
        let result = scheduler.poll(&alarms, utc(2026, 11, 10, 12, 33));
        // The fan step is 3 minutes late: inside the 5 minute grace period.
        assert_eq!(actions(&result.due), ["fan_3"]);
        // The light step is 13 minutes late: skipped and reported.
        assert_eq!(actions(&result.missed), ["light_toggle"]);
    }

    #[test]
    fn the_grace_period_boundary_is_exact() {
        let alarms = morning();
        let fan_time = utc(2026, 11, 10, 12, 30);

        // Exactly `GRACE` late is too late: the window is (now - grace, now].
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 25));
        let result = scheduler.poll(&alarms, fan_time + secs(i64::from(GRACE)));
        assert!(result.due.is_empty());
        assert_eq!(actions(&result.missed), ["fan_3"]);

        // One second less is still in time.
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 25));
        let result = scheduler.poll(&alarms, fan_time + secs(i64::from(GRACE) - 1));
        assert_eq!(actions(&result.due), ["fan_3"]);
    }

    #[test]
    fn a_big_forward_clock_jump_does_not_fire_a_backlog() {
        let alarms = morning();
        // The Pi booted believing it is Nov 3rd; a week of alarms "passed"
        // when the phone corrected the clock to Nov 10th 18:00 UTC.
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 3, 18, 0));
        let result = scheduler.poll(&alarms, utc(2026, 11, 10, 18, 0));
        assert!(result.due.is_empty(), "nothing may fire");
        // Only the last day's worth is reported as missed.
        assert_eq!(actions(&result.missed), ["light_toggle", "fan_3"]);

        // And normal service resumes the next morning.
        let next = scheduler.poll(&alarms, utc(2026, 11, 11, 12, 20));
        assert_eq!(actions(&next.due), ["light_toggle"]);
    }

    #[test]
    fn a_forward_jump_landing_just_after_an_alarm_runs_it_late() {
        let alarms = morning();
        // Clock was an hour slow; the phone fixes it at what is really 12:32.
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 11, 31));
        scheduler.poll(&alarms, utc(2026, 11, 10, 11, 32));
        let result = scheduler.poll(&alarms, utc(2026, 11, 10, 12, 32));
        assert_eq!(actions(&result.due), ["fan_3"]);
        assert_eq!(actions(&result.missed), ["light_toggle"]);
    }

    // ---- clock going backwards --------------------------------------------

    #[test]
    fn a_small_backward_step_does_not_fire_twice() {
        let alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 29));
        let fan_time = utc(2026, 11, 10, 12, 30);
        assert_eq!(actions(&scheduler.poll(&alarms, fan_time).due), ["fan_3"]);

        // NTP nudges the clock back three seconds, then time flows on
        // through 12:30:00 a second time.
        for offset in -3..=5 {
            let result = scheduler.poll(&alarms, fan_time + secs(offset));
            assert!(result.is_empty(), "fired again at offset {offset}");
        }
    }

    #[test]
    fn a_backward_step_of_a_few_minutes_does_not_toggle_the_light_twice() {
        let alarms = morning();
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 0));
        let light_time = utc(2026, 11, 10, 12, 20);
        assert_eq!(
            actions(&scheduler.poll(&alarms, light_time + secs(30)).due),
            ["light_toggle"]
        );

        // Clock goes back four minutes and runs forward again.
        let mut now = light_time - secs(240);
        let mut fired = Vec::new();
        while now <= utc(2026, 11, 10, 12, 31) {
            for step in scheduler.poll(&alarms, now).due {
                fired.push(step.action);
            }
            now += secs(1);
        }
        assert_eq!(fired, ["fan_3"], "light must not toggle again");
    }

    #[test]
    fn after_a_huge_backward_jump_alarms_run_at_the_corrected_time() {
        let alarms = morning();
        // The clock was three hours fast, so the alarm ran at the wrong
        // moment.
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 19));
        assert_eq!(
            scheduler.poll(&alarms, utc(2026, 11, 10, 12, 20)).due.len(),
            1
        );
        assert_eq!(
            scheduler.poll(&alarms, utc(2026, 11, 10, 12, 30)).due.len(),
            1
        );

        // The phone corrects it: it is really 09:31 UTC.
        assert!(scheduler.poll(&alarms, utc(2026, 11, 10, 9, 31)).is_empty());

        // When 12:20 and 12:30 really arrive, the alarm runs (again).
        let light = scheduler.poll(&alarms, utc(2026, 11, 10, 12, 20));
        assert_eq!(actions(&light.due), ["light_toggle"]);
        let fan = scheduler.poll(&alarms, utc(2026, 11, 10, 12, 30));
        assert_eq!(actions(&fan.due), ["fan_3"]);
    }

    // ---- restarts ----------------------------------------------------------

    #[test]
    fn a_restart_does_not_repeat_a_step() {
        let dir = tempfile::tempdir().unwrap();
        let alarms = morning();
        let fan_time = utc(2026, 11, 10, 12, 30);

        let mut before = Scheduler::restore(Chicago, GRACE, utc(2026, 11, 10, 12, 29), dir.path());
        assert_eq!(actions(&before.poll(&alarms, fan_time).due), ["fan_3"]);
        before.save().unwrap();
        drop(before);

        // The daemon is restarted ten seconds later.
        let restart = fan_time + secs(10);
        let mut after = Scheduler::restore(Chicago, GRACE, restart, dir.path());
        assert!(after.poll(&alarms, restart).is_empty());
        assert!(after.poll(&alarms, restart + secs(60)).is_empty());
    }

    #[test]
    fn a_restart_with_the_clock_a_few_seconds_behind_does_not_repeat_a_step() {
        let dir = tempfile::tempdir().unwrap();
        let alarms = morning();
        let fan_time = utc(2026, 11, 10, 12, 30);

        let mut before = Scheduler::restore(Chicago, GRACE, utc(2026, 11, 10, 12, 29), dir.path());
        assert_eq!(actions(&before.poll(&alarms, fan_time).due), ["fan_3"]);
        before.save().unwrap();
        drop(before);

        // Restarted with the clock five seconds *before* the step it already
        // ran. Only the saved record of that step can prevent a repeat.
        let restart = fan_time - secs(5);
        let mut after = Scheduler::restore(Chicago, GRACE, restart, dir.path());
        for offset in 0..=20 {
            let result = after.poll(&alarms, restart + secs(offset));
            assert!(result.is_empty(), "fired again {offset} s after restart");
        }
    }

    #[test]
    fn a_step_that_came_due_while_down_runs_after_the_restart() {
        let dir = tempfile::tempdir().unwrap();
        let alarms = morning();

        // Running and saved at 12:19, then the daemon crashes.
        let mut before = Scheduler::restore(Chicago, GRACE, utc(2026, 11, 10, 12, 0), dir.path());
        assert!(before.poll(&alarms, utc(2026, 11, 10, 12, 19)).is_empty());
        before.save().unwrap();
        drop(before);

        // systemd restarts it at 12:21: the 12:20 light step is one minute
        // late and must still run.
        let restart = utc(2026, 11, 10, 12, 21);
        let mut after = Scheduler::restore(Chicago, GRACE, restart, dir.path());
        let result = after.poll(&alarms, restart);
        assert_eq!(actions(&result.due), ["light_toggle"]);
        assert!(result.missed.is_empty());
    }

    #[test]
    fn a_long_outage_skips_what_is_too_late() {
        let dir = tempfile::tempdir().unwrap();
        let alarms = morning();

        let mut before = Scheduler::restore(Chicago, GRACE, utc(2026, 11, 10, 12, 0), dir.path());
        before.poll(&alarms, utc(2026, 11, 10, 12, 10));
        before.save().unwrap();
        drop(before);

        // Back up an hour later.
        let restart = utc(2026, 11, 10, 13, 10);
        let mut after = Scheduler::restore(Chicago, GRACE, restart, dir.path());
        let result = after.poll(&alarms, restart);
        assert!(result.due.is_empty());
        assert_eq!(actions(&result.missed), ["light_toggle", "fan_3"]);
    }

    #[test]
    fn the_first_ever_start_does_not_catch_up() {
        let dir = tempfile::tempdir().unwrap();
        let alarms = morning();
        // No state file yet, started one minute after the fan step.
        let start = utc(2026, 11, 10, 12, 31);
        let mut scheduler = Scheduler::restore(Chicago, GRACE, start, dir.path());
        assert!(scheduler.poll(&alarms, start).is_empty());
    }

    #[test]
    fn a_restart_with_the_clock_reset_into_the_past_starts_from_now() {
        let dir = tempfile::tempdir().unwrap();
        let alarms = morning();

        let mut before = Scheduler::restore(Chicago, GRACE, utc(2026, 11, 10, 12, 19), dir.path());
        assert_eq!(before.poll(&alarms, utc(2026, 11, 10, 12, 20)).due.len(), 1);
        assert_eq!(before.poll(&alarms, utc(2026, 11, 10, 12, 30)).due.len(), 1);
        before.save().unwrap();
        drop(before);

        // Power cut. The Pi comes back believing it is 11:45 (an old saved
        // time), i.e. *before* the state file's time.
        let restart = utc(2026, 11, 10, 11, 45);
        let mut after = Scheduler::restore(Chicago, GRACE, restart, dir.path());
        assert!(after.poll(&alarms, restart).is_empty());

        // The phone then corrects the clock to the real time, 12:40.
        // Today's steps already ran before the power cut: nothing is due.
        let result = after.poll(&alarms, utc(2026, 11, 10, 12, 40));
        assert!(result.due.is_empty());
    }

    #[test]
    fn steps_that_ran_before_a_power_cut_do_not_run_again_as_the_clock_catches_up() {
        let dir = tempfile::tempdir().unwrap();
        let alarms = morning();

        // Both steps run normally, then the power fails.
        let mut before = Scheduler::restore(Chicago, GRACE, utc(2026, 11, 10, 12, 19), dir.path());
        assert_eq!(before.poll(&alarms, utc(2026, 11, 10, 12, 20)).due.len(), 1);
        assert_eq!(before.poll(&alarms, utc(2026, 11, 10, 12, 30)).due.len(), 1);
        before.save().unwrap();
        drop(before);

        // The Pi reboots with its clock 45 minutes behind and nobody
        // corrects it. Its clock now runs through 12:20 and 12:30 a second
        // time; the light must not be toggled off again.
        let restart = utc(2026, 11, 10, 11, 45);
        let mut after = Scheduler::restore(Chicago, GRACE, restart, dir.path());
        let mut now = restart;
        while now <= utc(2026, 11, 10, 12, 45) {
            let result = after.poll(&alarms, now);
            assert!(result.due.is_empty(), "{:?} ran again", result.due);
            now += secs(1);
        }

        // Tomorrow is a normal day again.
        let tomorrow = after.poll(&alarms, utc(2026, 11, 11, 12, 20));
        assert_eq!(actions(&tomorrow.due), ["light_toggle"]);
    }

    #[test]
    fn a_damaged_state_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("scheduler-state.json"), "not json at all").unwrap();
        let alarms = morning();
        let start = utc(2026, 11, 10, 12, 0);
        let mut scheduler = Scheduler::restore(Chicago, GRACE, start, dir.path());
        let result = scheduler.poll(&alarms, utc(2026, 11, 10, 12, 20));
        assert_eq!(actions(&result.due), ["light_toggle"]);
        // Saving replaces the damaged file with a good one.
        scheduler.save().unwrap();
        assert!(load_state(&dir.path().join("scheduler-state.json"))
            .unwrap()
            .is_some());
    }

    #[test]
    fn an_in_memory_scheduler_saves_nothing() {
        let scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 10, 12, 0));
        scheduler.save().unwrap();
        assert_eq!(scheduler.timezone(), Chicago);
    }

    // ---- daylight saving through the scheduler ----------------------------

    #[test]
    fn spring_forward_night_fires_once() {
        // 02:30 does not exist on 2026-03-08 in Chicago (02:00 -> 03:00).
        // Old offset UTC-6, so the alarm is at 08:30 UTC = 03:30 CDT.
        let alarms = vec![alarm("night", 2, 30, &Day::ALL, &[(0, "fan_3")])];
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 3, 8, 6, 0));
        let mut fired = Vec::new();
        let mut now = utc(2026, 3, 8, 6, 0);
        while now <= utc(2026, 3, 8, 12, 0) {
            for step in scheduler.poll(&alarms, now).due {
                fired.push(step.fire_at);
            }
            now += secs(30);
        }
        assert_eq!(fired, vec![utc(2026, 3, 8, 8, 30)]);
    }

    #[test]
    fn fall_back_night_fires_once() {
        // 01:30 happens twice on 2026-11-01 in Chicago: 06:30 UTC (CDT) and
        // 07:30 UTC (CST). Only the first counts.
        let alarms = vec![alarm("night", 1, 30, &Day::ALL, &[(0, "fan_3")])];
        let mut scheduler = Scheduler::new(Chicago, GRACE, utc(2026, 11, 1, 5, 0));
        let mut fired = Vec::new();
        let mut now = utc(2026, 11, 1, 5, 0);
        while now <= utc(2026, 11, 1, 10, 0) {
            for step in scheduler.poll(&alarms, now).due {
                fired.push(step.fire_at);
            }
            now += secs(30);
        }
        assert_eq!(fired, vec![utc(2026, 11, 1, 6, 30)]);
    }
}
