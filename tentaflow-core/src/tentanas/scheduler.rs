// =============================================================================
// File: tentanas/scheduler.rs — the node's recurring work (plan-02 §5.2, tab
//       "Zadania"): scrubs, automatic snapshots with GFS retention and SMART
//       self-tests. One loop per node, one tick a minute, every decision made
//       in the node's LOCAL time — "daily at 02:00" means 02:00 where the
//       disks are, not 02:00 UTC.
//
//       The loop owns no state: `next_run_at` lives in tentanas.db, so a
//       restart resumes exactly where it stopped and a schedule the admin
//       disables simply stops matching. Work is handed to `jobs::spawn`, so
//       every run has a log, a progress percentage and a cancel button like
//       any other job.
// =============================================================================

use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use tentaflow_protocol::tentanas::{NasSchedule, NasSnapshotSchedule};

use super::db as store;
use super::snapshots;
use crate::db::DbPool;

const TICK: Duration = Duration::from_secs(60);

/// Who the Tasks tab shows as the starter of an unattended run.
pub const STARTED_BY: &str = "scheduler";

// ----- next run -------------------------------------------------------------------

fn at_local(naive: chrono::NaiveDateTime) -> Option<DateTime<Local>> {
    // A DST spring-forward makes some local wall-clock times not exist; the
    // earliest valid instant is the one a cron-like schedule wants.
    Local.from_local_datetime(&naive).earliest()
}

/// Minutes between two runs of a sub-daily cadence, and the offset inside
/// that period the schedule's `hour`/`minute` select.
fn period_and_offset(schedule: &NasSchedule) -> Option<(i64, i64)> {
    let minute = i64::from(schedule.minute.min(59));
    let hour = i64::from(schedule.hour.min(23));
    match schedule.every.as_str() {
        "15m" => Some((15, minute % 15)),
        "30m" => Some((30, minute % 30)),
        "1h" => Some((60, minute)),
        "6h" => Some((360, (hour % 6) * 60 + minute)),
        _ => None,
    }
}

/// The first run of `schedule` strictly after `after`, in node local time.
/// `None` for a cadence string the node does not know — an unknown schedule
/// never fires rather than firing at a guessed time.
pub fn next_run_after(schedule: &NasSchedule, after: DateTime<Local>) -> Option<DateTime<Local>> {
    // Whole minutes only: the loop ticks once a minute.
    let after = after.with_second(0)?.with_nanosecond(0)?;
    if let Some((period, offset)) = period_and_offset(schedule) {
        let midnight = at_local(after.date_naive().and_hms_opt(0, 0, 0)?)?;
        let elapsed = (after - midnight).num_minutes();
        // The first slot strictly after `after`.
        let steps = (elapsed - offset).div_euclid(period) + 1;
        return Some(midnight + chrono::Duration::minutes(offset + steps * period));
    }
    let hour = u32::from(schedule.hour.min(23));
    let minute = u32::from(schedule.minute.min(59));
    match schedule.every.as_str() {
        "daily" => (0..3).find_map(|day| {
            let date = after.date_naive() + chrono::Duration::days(day);
            let candidate = at_local(date.and_hms_opt(hour, minute, 0)?)?;
            (candidate > after).then_some(candidate)
        }),
        "weekly" => {
            // 0 = Sunday, matching the protocol.
            let target = u32::from(schedule.weekday.min(6));
            (0..15).find_map(|day| {
                let date = after.date_naive() + chrono::Duration::days(day);
                if date.weekday().num_days_from_sunday() != target {
                    return None;
                }
                let candidate = at_local(date.and_hms_opt(hour, minute, 0)?)?;
                (candidate > after).then_some(candidate)
            })
        }
        "monthly" => {
            // 1..=28 only, so every month has the day.
            let day = u32::from(schedule.day.clamp(1, 28));
            (0..3).find_map(|step| {
                let (year, month) = month_after(after.year(), after.month(), step);
                let date = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
                let candidate = at_local(date.and_hms_opt(hour, minute, 0)?)?;
                (candidate > after).then_some(candidate)
            })
        }
        _ => None,
    }
}

fn month_after(year: i32, month: u32, steps: u32) -> (i32, u32) {
    let zero_based = month - 1 + steps;
    (year + (zero_based / 12) as i32, zero_based % 12 + 1)
}

/// `next_run_after` as the RFC 3339 UTC string the database and the protocol
/// carry.
pub fn next_run_utc(schedule: &NasSchedule, after: DateTime<Local>) -> Option<String> {
    next_run_after(schedule, after).map(|t| {
        t.with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    })
}

fn is_due(next_run_at: Option<&str>, now: DateTime<Local>) -> bool {
    let Some(next) = next_run_at else {
        // A schedule with no computed next run has one computed on this tick;
        // it does not fire retroactively.
        return false;
    };
    DateTime::parse_from_rfc3339(next).is_ok_and(|t| t <= now)
}

// ----- snapshot tiers ---------------------------------------------------------------

/// The retention tiers a run at `now` fills. `frequent` is every run; a
/// coarser tier is filled only when this run is the first one on the far side
/// of that tier's boundary, which is what makes "30 daily snapshots" mean 30
/// distinct days rather than 30 runs.
pub fn crossed_tiers(previous: Option<DateTime<Local>>, now: DateTime<Local>) -> Vec<&'static str> {
    let mut tiers = vec!["frequent"];
    let Some(prev) = previous else {
        // The first run of a schedule seeds every tier it keeps.
        tiers.extend(["hourly", "daily", "weekly", "monthly"]);
        return tiers;
    };
    if prev.date_naive() != now.date_naive() || prev.hour() != now.hour() {
        tiers.push("hourly");
    }
    if prev.date_naive() != now.date_naive() {
        tiers.push("daily");
    }
    if prev.iso_week() != now.iso_week() {
        tiers.push("weekly");
    }
    if prev.year() != now.year() || prev.month() != now.month() {
        tiers.push("monthly");
    }
    tiers
}

/// The tiers a run actually takes: the crossed ones the schedule still keeps
/// a copy of. A tier with `keep = 0` is disabled, so it is neither taken nor
/// retained.
pub fn tiers_to_take(schedule: &NasSnapshotSchedule, previous: Option<DateTime<Local>>, now: DateTime<Local>) -> Vec<&'static str> {
    let keep = snapshots::Keep::from_schedule(schedule);
    crossed_tiers(previous, now)
        .into_iter()
        .filter(|t| keep.of(t) > 0)
        .collect()
}

// ----- the loop ----------------------------------------------------------------------

/// Starts the node's schedule loop once per process. Called next to the disk
/// sampler from the native init hook; a second call is a no-op, and a node
/// without ZFS simply finds nothing due.
pub fn start(db: DbPool) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("tentanas: no tokio runtime, scheduler not started");
        return;
    };
    handle.spawn(async move {
        loop {
            tick(&db).await;
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(db: &DbPool) {
    let now = Local::now();
    run_due_scrubs(db, now).await;
    run_due_snapshots(db, now).await;
    run_due_smart_tests(db, now).await;
}

async fn run_due_scrubs(db: &DbPool, now: DateTime<Local>) {
    let rows = match store::list_scrub_schedules(db) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("tentanas scheduler: scrub schedules unreadable: {e}");
            return;
        }
    };
    for row in rows {
        if !row.enabled {
            continue;
        }
        let next = next_run_utc(&row.schedule, now);
        if !is_due(row.next_run_at.as_deref(), now) {
            if row.next_run_at.is_none() {
                let _ = store::set_scrub_schedule(db, &row.pool, true, &row.schedule, next.as_deref());
            }
            continue;
        }
        let started = super::pools::spawn_scheduled_scrub(db, &row.pool);
        let result = match &started {
            Ok(job) => format!("started job {}", job.job_id),
            Err(e) => format!("failed to start: {e}"),
        };
        if let Err(e) = store::record_scrub_run(db, &row.pool, &result, next.as_deref()) {
            tracing::warn!("tentanas scheduler: scrub run not recorded: {e}");
        }
    }
}

async fn run_due_snapshots(db: &DbPool, now: DateTime<Local>) {
    let schedules = match store::list_snapshot_schedules(db) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("tentanas scheduler: snapshot schedules unreadable: {e}");
            return;
        }
    };
    for schedule in schedules {
        if !schedule.enabled {
            continue;
        }
        let next = next_run_utc(&schedule.schedule, now);
        if !is_due(schedule.next_run_at.as_deref(), now) {
            if schedule.next_run_at.is_none() {
                let _ = store::upsert_snapshot_schedule(db, &schedule, next.as_deref());
            }
            continue;
        }
        let previous = schedule
            .last_run_at
            .as_deref()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Local));
        let tiers: Vec<String> = tiers_to_take(&schedule, previous, now)
            .into_iter()
            .map(str::to_string)
            .collect();
        let started = super::snapshots::spawn_auto(db, &schedule, tiers, now);
        let result = match &started {
            Ok(job) => format!("started job {}", job.job_id),
            Err(e) => format!("failed to start: {e}"),
        };
        if let Err(e) = store::record_snapshot_run(db, &schedule.schedule_id, &result, next.as_deref())
        {
            tracing::warn!("tentanas scheduler: snapshot run not recorded: {e}");
        }
    }
}

async fn run_due_smart_tests(db: &DbPool, now: DateTime<Local>) {
    let mut smart = match store::smart_schedule(db) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("tentanas scheduler: SMART schedule unreadable: {e}");
            return;
        }
    };
    if !smart.enabled {
        return;
    }
    let mut changed = false;
    for long in [false, true] {
        let schedule = if long { smart.long.clone() } else { smart.short.clone() };
        let stored_next = if long {
            smart.next_long_at.clone()
        } else {
            smart.next_short_at.clone()
        };
        let next = next_run_utc(&schedule, now);
        if !is_due(stored_next.as_deref(), now) {
            if stored_next.is_none() {
                if long {
                    smart.next_long_at = next;
                } else {
                    smart.next_short_at = next;
                }
                changed = true;
            }
            continue;
        }
        let kind = if long {
            tentanas_helper::SelfTestKind::Long
        } else {
            tentanas_helper::SelfTestKind::Short
        };
        if long {
            smart.last_long_at = Some(store::now());
            smart.next_long_at = next;
        } else {
            smart.last_short_at = Some(store::now());
            smart.next_short_at = next;
        }
        changed = true;
        for (disk_id, device) in super::disks::snapshot()
            .0
            .into_iter()
            .map(|d| (d.disk_id, d.path))
        {
            let started = super::jobs::spawn(db, "smart_test", &disk_id, STARTED_BY, move |h| {
                super::jobs::smart_self_test(h, device, kind, None)
            });
            if let Err(e) = started {
                tracing::warn!("tentanas scheduler: SMART test for {disk_id} not started: {e}");
            }
        }
    }
    if changed {
        if let Err(e) = store::set_smart_schedule(db, &smart) {
            tracing::warn!("tentanas scheduler: SMART schedule not saved: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .expect("unambiguous local time")
    }

    fn schedule(every: &str, hour: u8, minute: u8) -> NasSchedule {
        NasSchedule {
            every: every.to_string(),
            hour,
            minute,
            weekday: 0,
            day: 1,
        }
    }

    #[test]
    fn sub_daily_cadences_land_on_their_own_slots() {
        let now = at(2026, 9, 1, 14, 7);
        let every_15 = next_run_after(&schedule("15m", 0, 0), now).expect("next");
        assert_eq!(every_15, at(2026, 9, 1, 14, 15));

        // The minute offset shifts every slot of the hour.
        let offset = next_run_after(&schedule("15m", 0, 5), now).expect("next");
        assert_eq!(offset, at(2026, 9, 1, 14, 20));

        let every_30 = next_run_after(&schedule("30m", 0, 0), now).expect("next");
        assert_eq!(every_30, at(2026, 9, 1, 14, 30));

        let hourly = next_run_after(&schedule("1h", 0, 5), now).expect("next");
        assert_eq!(hourly, at(2026, 9, 1, 15, 5));

        // Exactly on a slot returns the NEXT one, never the current instant.
        let on_slot = next_run_after(&schedule("1h", 0, 7), now).expect("next");
        assert_eq!(on_slot, at(2026, 9, 1, 15, 7));

        // 6h slots are anchored by hour % 6.
        let six = next_run_after(&schedule("6h", 2, 0), now).expect("next");
        assert_eq!(six, at(2026, 9, 1, 20, 0));
        let six_next_day = next_run_after(&schedule("6h", 2, 0), at(2026, 9, 1, 21, 0)).expect("next");
        assert_eq!(six_next_day, at(2026, 9, 2, 2, 0));
    }

    #[test]
    fn daily_weekly_and_monthly_pick_the_next_calendar_slot() {
        let daily = next_run_after(&schedule("daily", 2, 0), at(2026, 9, 1, 14, 7)).expect("next");
        assert_eq!(daily, at(2026, 9, 2, 2, 0));
        let daily_before =
            next_run_after(&schedule("daily", 23, 30), at(2026, 9, 1, 14, 7)).expect("next");
        assert_eq!(daily_before, at(2026, 9, 1, 23, 30));

        // 2026-09-01 is a Tuesday; weekday 0 is Sunday.
        let mut weekly = schedule("weekly", 2, 0);
        weekly.weekday = 0;
        assert_eq!(
            next_run_after(&weekly, at(2026, 9, 1, 14, 7)).expect("next"),
            at(2026, 9, 6, 2, 0)
        );
        weekly.weekday = 2;
        assert_eq!(
            next_run_after(&weekly, at(2026, 9, 1, 14, 7)).expect("next"),
            at(2026, 9, 8, 2, 0)
        );

        let mut monthly = schedule("monthly", 1, 30);
        monthly.day = 1;
        assert_eq!(
            next_run_after(&monthly, at(2026, 9, 1, 14, 7)).expect("next"),
            at(2026, 10, 1, 1, 30)
        );
        monthly.day = 15;
        assert_eq!(
            next_run_after(&monthly, at(2026, 12, 20, 0, 0)).expect("next"),
            at(2027, 1, 15, 1, 30)
        );
        // A day past 28 is clamped so every month really has it.
        monthly.day = 31;
        assert_eq!(
            next_run_after(&monthly, at(2026, 1, 29, 0, 0)).expect("next"),
            at(2026, 2, 28, 1, 30)
        );
    }

    #[test]
    fn an_unknown_cadence_never_fires() {
        assert!(next_run_after(&schedule("yearly", 0, 0), at(2026, 9, 1, 0, 0)).is_none());
        assert!(next_run_after(&schedule("", 0, 0), at(2026, 9, 1, 0, 0)).is_none());
    }

    #[test]
    fn due_only_when_the_stored_next_run_has_passed() {
        let now = at(2026, 9, 1, 14, 7);
        assert!(!is_due(None, now));
        assert!(is_due(Some("2026-09-01T00:00:00Z"), now));
        assert!(!is_due(Some("2099-01-01T00:00:00Z"), now));
        assert!(!is_due(Some("not a timestamp"), now));
    }

    #[test]
    fn tiers_fill_when_a_run_crosses_their_boundary() {
        let now = at(2026, 9, 1, 14, 45);
        // The very first run of a schedule seeds every tier.
        assert_eq!(
            crossed_tiers(None, now),
            ["frequent", "hourly", "daily", "weekly", "monthly"]
        );
        // Same hour: only the frequent tier.
        assert_eq!(crossed_tiers(Some(at(2026, 9, 1, 14, 30)), now), ["frequent"]);
        // New hour, same day.
        assert_eq!(
            crossed_tiers(Some(at(2026, 9, 1, 13, 45)), now),
            ["frequent", "hourly"]
        );
        // New day and new month, still the same ISO week (Mon 31 Aug and
        // Tue 1 Sep 2026 share one week), so the weekly tier does not fill.
        assert_eq!(
            crossed_tiers(Some(at(2026, 8, 31, 14, 45)), now),
            ["frequent", "hourly", "daily", "monthly"]
        );
        // Same week and same month: only the day changed.
        assert_eq!(
            crossed_tiers(Some(at(2026, 9, 2, 14, 45)), at(2026, 9, 3, 14, 45)),
            ["frequent", "hourly", "daily"]
        );
        // Previous week and previous month.
        assert_eq!(
            crossed_tiers(Some(at(2026, 8, 30, 14, 45)), now),
            ["frequent", "hourly", "daily", "weekly", "monthly"]
        );
    }

    #[test]
    fn a_disabled_tier_is_never_taken() {
        let schedule = NasSnapshotSchedule {
            keep_frequent: 96,
            keep_hourly: 0,
            keep_daily: 30,
            keep_weekly: 0,
            keep_monthly: 12,
            ..Default::default()
        };
        let taken = tiers_to_take(&schedule, Some(at(2026, 8, 30, 14, 45)), at(2026, 9, 1, 14, 45));
        assert_eq!(taken, ["frequent", "daily", "monthly"]);
        let same_hour = tiers_to_take(
            &schedule,
            Some(at(2026, 9, 1, 14, 30)),
            at(2026, 9, 1, 14, 45),
        );
        assert_eq!(same_hour, ["frequent"]);
    }
}
