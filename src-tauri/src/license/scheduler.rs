// SPDX-License-Identifier: MIT
//! The update tick's timing (SW-PB-9 AC-5, kept by SV-PBF-278): a check at launch, an hourly
//! tick, a check on a tick when the last read is at least `max_age` old, and a probe every minute
//! for a wall-clock jump well past the monotonic time (the Mac woke up). `static_channel::run_ticks`
//! is the loop; this holds its constants and the rules it asks.
//!
//! Whether a check is due is decided on the device's wall clock. The tick itself runs on tokio's
//! monotonic clock, which on macOS does not count sleep, hence the wake probe.

use std::time::Duration;

/// A device clock this far behind the last read is a rollback.
pub const CLOCK_ROLLBACK_SECS: i64 = 10 * 60;
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub const TICK: Duration = Duration::from_secs(60 * 60);
/// How often the loop looks for a sleep gap. It reads only the clocks.
pub const WAKE_PROBE: Duration = Duration::from_secs(60);
/// Wall time this much ahead of monotonic time between two probes means the Mac slept.
pub const SLEEP_GAP_SECS: i64 = 5 * 60;
/// SW-PB-23 AC-4: while a newer build is offered, a check is due once this much wall time has
/// passed, so the hourly tick checks every time. A pulled build then reaches the app within
/// about an hour.
pub const PENDING_MAX_AGE: Duration = Duration::from_secs(55 * 60);

/// The check age in force: `max_age`, or at most `PENDING_MAX_AGE` while an update is pending.
/// A shorter staging interval (its 60-second floor included) stays as it is.
pub fn effective_max_age(max_age: Duration, update_pending: bool) -> Duration {
    if update_pending {
        max_age.min(PENDING_MAX_AGE)
    } else {
        max_age
    }
}

/// The loop's tick: hourly, or sooner when the check age is shorter (staging).
pub fn tick_for(max_age: Duration) -> Duration {
    TICK.min(max_age)
}

/// A check is due when none has succeeded, when `max_age` of wall time has passed since the last
/// one, when the device clock is more than the rollback tolerance behind it, or when the state
/// needs an online check. The loop asks only on a tick or a wake, so a check that keeps failing
/// is retried at the tick's cadence, never faster.
pub fn check_due(
    last_success: Option<i64>,
    now: i64,
    max_age: Duration,
    needs_online: bool,
) -> bool {
    let max_age = i64::try_from(max_age.as_secs()).unwrap_or(i64::MAX);
    needs_online
        || last_success.map_or(true, |at| {
            now.saturating_sub(at) >= max_age || now < at.saturating_sub(CLOCK_ROLLBACK_SECS)
        })
}

#[cfg(all(test, not(cs_channel = "internal")))]
mod tests {
    use super::*;

    const WALL: i64 = 1_760_500_000;
    const HOUR: i64 = 60 * 60;

    #[test]
    fn the_pending_age_is_about_an_hour_and_keeps_the_staging_floor() {
        assert_eq!(effective_max_age(DEFAULT_MAX_AGE, false), DEFAULT_MAX_AGE);
        assert_eq!(effective_max_age(DEFAULT_MAX_AGE, true), PENDING_MAX_AGE);
        assert!(
            PENDING_MAX_AGE < TICK,
            "every hourly tick finds a check due"
        );
        assert!(
            PENDING_MAX_AGE >= Duration::from_secs(50 * 60),
            "not faster than about hourly"
        );
        // A staging interval shorter than an hour (60 s at least in a release build) stays.
        let staging = Duration::from_secs(60);
        assert_eq!(effective_max_age(staging, true), staging);
        assert_eq!(effective_max_age(staging, false), staging);
    }

    #[test]
    fn a_check_is_due_by_wall_time() {
        let day = DEFAULT_MAX_AGE;
        assert!(check_due(None, WALL, day, false), "never checked");
        assert!(
            !check_due(Some(WALL), WALL + HOUR - 1, day, false),
            "just checked"
        );
        assert!(!check_due(Some(WALL), WALL + 24 * HOUR - 1, day, false));
        assert!(
            check_due(Some(WALL), WALL + 24 * HOUR, day, false),
            "a day of wall time"
        );
        assert!(
            !check_due(Some(WALL), WALL - 30, day, false),
            "a small step back is not a reason to check"
        );
        assert!(
            check_due(Some(WALL), WALL - 11 * 60, day, false),
            "a clock set back past the tolerance checks"
        );
        assert!(
            check_due(Some(WALL), WALL + 60, day, true),
            "needs_online makes a check due at once"
        );
    }
}
