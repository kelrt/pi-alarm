//! The long-running service: scheduler + BLE server.
//!
//! Two tasks run side by side on one thread (tokio's "current thread"
//! runtime, which suits the single-core Pi Zero):
//!
//! * the **scheduler loop** wakes once a second, asks `Scheduler::poll` what
//!   is due and transmits it;
//! * the **BLE server** (`ble.rs`) answers the phone.
//!
//! They share one `App` through an `Arc`. Transmitting blocks for a few
//! hundred milliseconds with exact timing, so it is done on a separate
//! worker thread via `spawn_blocking`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use chrono::Timelike;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Notify;

use crate::alarm::Alarm;
use crate::app::App;
use crate::ble;
use crate::config::Config;
use crate::scheduler::Scheduler;

/// Pause between two steps that are due at the same moment, so the fan's
/// receiver sees two separate button presses.
const PAUSE_BETWEEN_STEPS: Duration = Duration::from_millis(300);

/// Run until SIGTERM or Ctrl-C.
pub async fn run(config: Config, force_mock: bool) -> anyhow::Result<()> {
    let app = Arc::new(App::from_config(&config, force_mock)?);
    log::info!(
        "pi-alarm {} starting: {} action(s), time zone {}, data in {}",
        env!("CARGO_PKG_VERSION"),
        app.radio.actions().len(),
        app.tz.name(),
        app.data_dir.display()
    );
    if !app.clock.is_synced() {
        log::warn!(
            "the clock has not been confirmed by NTP or the phone yet; \
             alarms will follow the Pi's current idea of the time"
        );
    }

    // `Notify` is a simple "wake up" signal between tasks.
    let shutdown = Arc::new(Notify::new());

    // `tokio::spawn` starts a task that runs concurrently with this one.
    let mut scheduler_task = tokio::spawn(run_scheduler(Arc::clone(&app), Arc::clone(&shutdown)));

    if config.ble.enabled {
        // We never wait for the BLE task: it loops forever and is simply
        // dropped when the program exits.
        tokio::spawn(ble::run(Arc::clone(&app), config.ble.device_name.clone()));
    } else {
        log::warn!("BLE is disabled in the config");
    }

    let mut terminate = signal(SignalKind::terminate()).context("cannot listen for SIGTERM")?;

    // `select!` waits for whichever of these happens first.
    tokio::select! {
        _ = terminate.recv() => log::info!("SIGTERM received, shutting down"),
        _ = tokio::signal::ctrl_c() => log::info!("Ctrl-C received, shutting down"),
        result = &mut scheduler_task => {
            // The scheduler loop never returns by itself, so this is a bug
            // or a panic. Exit with an error and let systemd restart us.
            result.context("the scheduler task crashed")?;
            anyhow::bail!("the scheduler stopped unexpectedly");
        }
    }

    // Ask the scheduler to save its state and stop, then wait for it.
    shutdown.notify_one();
    scheduler_task.await.context("the scheduler task crashed")?;
    Ok(())
}

/// The scheduler loop. Returns when `shutdown` is signalled.
async fn run_scheduler(app: Arc<App>, shutdown: Arc<Notify>) {
    let mut scheduler =
        Scheduler::restore(app.tz, app.late_grace_secs, app.clock.now(), &app.data_dir);
    let mut alarms: Vec<Alarm> = Vec::new();
    // The alarms file's modification time when we last read it. Starts as
    // something no real file has, so the first pass always loads.
    let mut loaded_stamp: Option<Option<SystemTime>> = None;

    loop {
        // Re-read the alarms if the file changed (the phone or the CLI
        // edited it).
        let stamp = app.store.modified();
        if loaded_stamp != Some(stamp) {
            match app.store.load() {
                Ok(list) => {
                    log::info!("loaded {} alarm(s)", list.len());
                    alarms = list;
                }
                // Keep the alarms we already have rather than losing them.
                Err(error) => log::error!("cannot load alarms: {error:#}"),
            }
            loaded_stamp = Some(stamp);
            save_state(&scheduler);
        }

        let result = scheduler.poll(&alarms, app.clock.now());
        for step in &result.missed {
            log::warn!(
                "missed `{}` of alarm `{}` (was due {})",
                step.action,
                step.alarm_id,
                step.fire_at
            );
        }
        if !result.is_empty() {
            // Save *before* transmitting: should we crash mid-way, the step
            // is not repeated on restart (a repeated light_toggle would turn
            // the light back off).
            save_state(&scheduler);
        }

        for (index, step) in result.due.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(PAUSE_BETWEEN_STEPS).await;
            }
            log::info!("alarm `{}`: running `{}`", step.alarm_id, step.action);
            let app_for_task = Arc::clone(&app);
            let action = step.action.clone();
            // The closure runs on a worker thread; `.await` waits for it
            // without blocking the BLE server.
            let sent = tokio::task::spawn_blocking(move || app_for_task.radio.send(&action)).await;
            match sent {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::error!("alarm `{}`: {error}", step.alarm_id),
                Err(error) => log::error!("transmit thread crashed: {error}"),
            }
        }

        // Sleep until just after the next whole second. Alarm times are
        // whole seconds, so this keeps us within a few milliseconds of them.
        let into_second = app.clock.now().nanosecond() / 1_000_000;
        let until_next_second =
            Duration::from_millis(u64::from(1_005u32.saturating_sub(into_second)));
        tokio::select! {
            _ = tokio::time::sleep(until_next_second) => {}
            _ = shutdown.notified() => {
                save_state(&scheduler);
                log::info!("scheduler stopped");
                return;
            }
        }
    }
}

fn save_state(scheduler: &Scheduler) {
    if let Err(error) = scheduler.save() {
        // Not fatal: the worst case is a step repeated after a restart.
        log::error!("cannot save scheduler state: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::tests::sample_alarm;
    use crate::protocol::tests::{test_app, TestApp};
    use crate::schedule::tests::utc;

    /// Let the scheduler loop run for a moment of real time.
    async fn let_it_run() {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    /// Wait (up to five seconds) until the mock radio has sent `count`
    /// transmissions. Polling like this keeps the tests independent of how
    /// fast the machine is.
    async fn wait_for_sent(test: &TestApp, count: usize) {
        for _ in 0..100 {
            if test.sent.count() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "expected {count} transmission(s), saw {}",
            test.sent.count()
        );
    }

    // `#[tokio::test]` gives each test its own small async runtime.
    #[tokio::test]
    async fn the_loop_transmits_due_steps_and_stops_on_request() {
        let test = test_app();
        // Weekdays 06:30 Chicago = 12:30 UTC, light ten minutes before.
        test.app.store.put(sample_alarm("work")).unwrap();
        // Start one second before the light step (Wednesday 12:20 UTC).
        test.clock
            .advance_to(utc(2026, 11, 11, 12, 20) - chrono::Duration::seconds(1));

        let shutdown = Arc::new(Notify::new());
        let task = tokio::spawn(run_scheduler(Arc::clone(&test.app), Arc::clone(&shutdown)));
        let_it_run().await;
        assert_eq!(test.sent.count(), 0, "nothing is due yet");

        // The clock reaches the light step, then (later) the fan step.
        test.clock.advance_to(utc(2026, 11, 11, 12, 20));
        wait_for_sent(&test, 1).await;
        assert_eq!(
            test.sent.all()[0],
            test.app.radio.actions()["light_toggle"].transmission
        );

        test.clock.advance_to(utc(2026, 11, 11, 12, 30));
        wait_for_sent(&test, 2).await;
        assert_eq!(
            test.sent.all()[1],
            test.app.radio.actions()["fan_3"].transmission
        );

        // Nothing more happens while the clock stands still.
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        assert_eq!(test.sent.count(), 2);

        // Shut down: the loop returns and has saved its state.
        shutdown.notify_one();
        task.await.unwrap();
        assert!(test.app.data_dir.join("scheduler-state.json").exists());
    }

    #[tokio::test]
    async fn the_loop_notices_alarms_added_while_running() {
        let test = test_app();
        test.clock.advance_to(utc(2026, 11, 11, 12, 29));

        let shutdown = Arc::new(Notify::new());
        let task = tokio::spawn(run_scheduler(Arc::clone(&test.app), Arc::clone(&shutdown)));
        let_it_run().await;

        // The phone saves an alarm while the daemon is running.
        test.app.store.put(sample_alarm("work")).unwrap();
        test.clock.advance_to(utc(2026, 11, 11, 12, 30));
        wait_for_sent(&test, 1).await;

        shutdown.notify_one();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_failing_radio_does_not_stop_the_loop() {
        let test = test_app();
        test.app.store.put(sample_alarm("work")).unwrap();
        test.sent.set_failing(true);
        test.clock.advance_to(utc(2026, 11, 11, 12, 29));

        let shutdown = Arc::new(Notify::new());
        let task = tokio::spawn(run_scheduler(Arc::clone(&test.app), Arc::clone(&shutdown)));
        let_it_run().await;

        // The fan step comes due and the transmission fails.
        test.clock.advance_to(utc(2026, 11, 11, 12, 30));
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        assert_eq!(test.sent.count(), 0);
        assert!(!task.is_finished(), "a radio error must not end the loop");

        // The radio recovers; the next day's alarm works.
        test.sent.set_failing(false);
        test.clock.advance_to(utc(2026, 11, 12, 12, 30));
        wait_for_sent(&test, 1).await;

        shutdown.notify_one();
        task.await.unwrap();
    }
}
