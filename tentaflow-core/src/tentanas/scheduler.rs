// =============================================================================
// File: tentanas/scheduler.rs — the node's recurring work (plan-02 §5.2, tab
//       "Zadania"): scrubs, automatic snapshots with GFS retention and SMART
//       self-tests. One loop per node, one tick a minute, every decision made
//       in the node's LOCAL time — "daily at 02:00" means 02:00 where the
//       disks are, not 02:00 UTC.
//
//       The loop also closes four-eyes requests (§5.10) whose TTL passed, so
//       an operation nobody decided on expires on the clock rather than on
//       somebody opening a tab.
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

/// Minutes between two runs of `schedule` — the length of one 'frequent'
/// retention slot. The calendar cadences use their nominal length (a month is
/// 30 days): this answers "how much history do N snapshots cover", which is a
/// budget, not a timestamp. `None` for a cadence the node does not know, the
/// same cadences `next_run_after` refuses to fire.
pub fn cadence_minutes(schedule: &NasSchedule) -> Option<i64> {
    if let Some((period, _)) = period_and_offset(schedule) {
        return Some(period);
    }
    match schedule.every.as_str() {
        "daily" => Some(24 * 60),
        "weekly" => Some(7 * 24 * 60),
        "monthly" => Some(30 * 24 * 60),
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

fn stopped() -> &'static std::sync::atomic::AtomicBool {
    static FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &FLAG
}

/// Stops the loop for good — the uninstall teardown, before it touches the
/// services a schedule would otherwise use half a second later.
pub fn stop() {
    stopped().store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Starts the node's schedule loop once per process. Called next to the disk
/// sampler from the native init hook; a second call is a no-op, and a node
/// without ZFS simply finds nothing due.
pub fn start(main_db: DbPool, db: DbPool) {
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
            if stopped().load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            if super::instance_should_run(&main_db, &db) {
                tick(&main_db, &db).await;
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(main_db: &DbPool, db: &DbPool) {
    let now = Local::now();
    // A parked red-path operation must expire on the clock, not on somebody
    // opening the Tasks tab (§5.10) — an operation nobody decided on may never
    // become executable a week later.
    let node_id = crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string());
    for expired in super::approvals::expire_due(main_db, db, &node_id) {
        tracing::info!(
            request_id = %expired.request_id,
            operation = %expired.operation,
            "tentanas: approval expired unexecuted"
        );
    }
    run_due_pool_tasks(db, store::PoolTask::Scrub, now).await;
    run_due_pool_tasks(db, store::PoolTask::Trim, now).await;
    run_due_snapshots(db, now).await;
    run_due_smart_tests(db, now).await;
    // Both Elastic passes read ONE snapshot of the arrays: the second pass
    // would otherwise re-read rows the first pass has just changed, and the
    // work below is the same for both.
    let arrays = match store::elastic_arrays_all(db) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("tentanas scheduler: Elastic arrays unreadable: {e}");
            Vec::new()
        }
    };
    run_due_elastic_tasks(db, &arrays, now).await;
    run_cache_low_movers(db, &arrays).await;
    // The access audit and the outbound forwarding are per-minute work of the
    // same loop: both are cheap when there is nothing to do, and neither may
    // depend on somebody having a tab open (§5.10).
    super::access_log::collect_tick(db).await;
    super::forward::forward_tick(main_db, db).await;
}

/// The recurring scrub and the recurring TRIM (§5.10) are the same loop over
/// two tables: one row per pool, one verb each, both started as a job so the
/// Tasks tab shows them like any other run.
async fn run_due_pool_tasks(db: &DbPool, task: store::PoolTask, now: DateTime<Local>) {
    let rows = match store::list_pool_schedules(db, task) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("tentanas scheduler: {} schedules unreadable: {e}", task.kind());
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
                let _ = store::set_pool_schedule(db, task, &row.pool, true, &row.schedule, next.as_deref());
            }
            continue;
        }
        let started = match task {
            store::PoolTask::Scrub => super::pools::spawn_scheduled_scrub(db, &row.pool),
            store::PoolTask::Trim => super::pools::spawn_scheduled_trim(db, &row.pool),
        };
        let result = match &started {
            Ok(job) => format!("started job {}", job.job_id),
            Err(e) => format!("failed to start: {e}"),
        };
        if let Err(e) = store::record_pool_schedule_run(db, task, &row.pool, &result, next.as_deref()) {
            tracing::warn!(
                "tentanas scheduler: {} run not recorded: {e}",
                task.kind()
            );
        }
    }
}

/// The Elastic Array's three cadences (§5.3): the mover, the nightly sync and
/// the scrub. One table, one row shape, one loop — `run_due_pool_tasks` over
/// two tables, with the array in place of the pool.
///
/// A REFUSED SPAWN IS NORMAL AND DOES NOT FAIL THE TICK. `insert_job` refuses
/// a second running operation on one array, so an array busy with a manual
/// sync simply records "failed to start: …" and is tried again next cadence.
/// That refusal IS the serialisation this needs; a second lock here would only
/// be able to disagree with it.
async fn run_due_elastic_tasks(
    db: &DbPool,
    arrays: &[super::elastic::ElasticArrayRow],
    now: DateTime<Local>,
) {
    for task in store::ElasticTask::ALL {
        let rows = match store::list_elastic_schedules(db, task) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "tentanas scheduler: elastic {} schedules unreadable: {e}",
                    task.kind()
                );
                continue;
            }
        };
        for row in rows {
            if !row.enabled {
                continue;
            }
            // A schedule whose array is gone from this instance fires nothing.
            let Some(array) = arrays
                .iter()
                .find(|a| a.array_id() == Some(row.array_id.as_str()))
            else {
                continue;
            };
            let next = next_run_utc(&row.schedule, now);
            if !is_due(row.next_run_at.as_deref(), now) {
                if row.next_run_at.is_none() {
                    let _ = store::set_elastic_schedule(
                        db,
                        &row.array_id,
                        task,
                        true,
                        &row.schedule,
                        next.as_deref(),
                    );
                }
                continue;
            }
            let started = match task {
                store::ElasticTask::Mover => {
                    super::elastic::spawn_mover(db, array, STARTED_BY, None)
                }
                store::ElasticTask::Sync => super::elastic::spawn_snapraid(
                    db,
                    array,
                    STARTED_BY,
                    None,
                    tentanas_helper::elastic::ElasticSnapraidKind::Sync,
                ),
                store::ElasticTask::Scrub => super::elastic::spawn_snapraid(
                    db,
                    array,
                    STARTED_BY,
                    None,
                    tentanas_helper::elastic::ElasticSnapraidKind::Scrub,
                ),
            };
            // A scheduled mover marks the retrigger clock too. Without it the
            // cache-pressure pass later in this SAME tick finds an unmarked
            // array, measures a cache the run it just started has not drained
            // yet, and asks for a second run `insert_job` can only refuse —
            // a recurring warning, and the 20-minute cooldown burned on a run
            // that did happen.
            if task == store::ElasticTask::Mover && started.is_ok() {
                super::elastic::MoverClock::global().started(&array.name);
            }
            let result = match &started {
                Ok(job) => format!("started job {}", job.job_id),
                Err(e) => format!("failed to start: {e}"),
            };
            // The advanced `next_run_at` goes in with the result whether the
            // spawn worked or not. Leaving it where it was would make a busy
            // array re-fire every single tick instead of next cadence.
            if let Err(e) =
                store::record_elastic_schedule_run(db, &row.array_id, task, &result, next.as_deref())
            {
                tracing::warn!(
                    "tentanas scheduler: elastic {} run not recorded: {e}",
                    task.kind()
                );
            }
        }
    }
}

/// The cache-pressure trigger of §5.3, owner decision (1).
///
/// This is the ONLY thing the minimum-free-space setting does. It starts an
/// EXTRA run outside the cadence; it is emphatically NOT a gate on the
/// scheduled ones, which move every aged file however roomy the cache is.
/// Gating them on it would mean that on an array whose cache never fills, no
/// file ever reaches parity at all — the failure would look like nothing
/// happening, which is the hardest kind to see.
///
/// The cooldown is checked BEFORE the helper is asked anything: `mover_trigger`
/// would answer `None` for a cooling array anyway, and this way a node under
/// sustained cache pressure inspects each array once per cooldown instead of
/// once a minute.
async fn run_cache_low_movers(db: &DbPool, arrays: &[super::elastic::ElasticArrayRow]) {
    let clock = super::elastic::MoverClock::global();
    for array in arrays {
        if !array.enabled || !array.mover.enabled || array.cache().next().is_none() {
            continue;
        }
        if !clock.cooled_down(&array.name) {
            continue;
        }
        let observed = match super::elastic::observe_cache(db, array).await {
            Ok(o) => o,
            Err(e) => {
                // Unknown free space is not low free space: nothing fires.
                tracing::warn!(
                    "tentanas scheduler: cache of {} not measured: {e}",
                    array.name
                );
                continue;
            }
        };
        if super::elastic::mover_trigger(array, &observed, clock)
            != super::elastic::MoverTrigger::CacheLow
        {
            continue;
        }
        // Marked before the spawn, not after. A refusal here means the array
        // is already busy with an operation — which is the outcome the trigger
        // wanted — and retrying it a minute later would re-ask the helper for
        // the same answer.
        clock.started(&array.name);
        match super::elastic::spawn_mover(db, array, STARTED_BY, None) {
            Ok(job) => tracing::info!(
                "tentanas scheduler: cache low on {}, started mover job {}",
                array.name,
                job.job_id
            ),
            Err(e) => tracing::warn!(
                "tentanas scheduler: cache low on {}, mover not started: {e}",
                array.name
            ),
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
            let started = super::jobs::spawn(db, "smart_test", &disk_id, STARTED_BY, None, None, move |h| {
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

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    /// §5.10: the recurring TRIM is due on its own clock, next to the scrub.
    /// A tick computes the first run of a schedule that has none, fires it
    /// once the deadline passes, and records the run — the same contract the
    /// scrub has always had, now for both tables.
    #[tokio::test]
    async fn a_trim_schedule_is_armed_then_fires_and_records_its_run() {
        let p = db();
        let weekly = NasSchedule {
            every: "weekly".to_string(),
            hour: 4,
            minute: 0,
            weekday: 0,
            day: 1,
        };
        store::set_pool_schedule(&p, store::PoolTask::Trim, "fast", true, &weekly, None)
            .expect("schedule");

        // First tick: nothing is due, but the schedule gets its next run.
        let now = at(2026, 9, 1, 14, 45);
        run_due_pool_tasks(&p, store::PoolTask::Trim, now).await;
        let row = store::pool_schedule(&p, store::PoolTask::Trim, "fast")
            .expect("read")
            .expect("row");
        assert_eq!(row.next_run_at, next_run_utc(&weekly, now));
        assert!(row.last_run_at.is_none(), "nothing ran yet");
        // …and the scrub table stayed empty: two tasks, two tables.
        assert!(store::list_pool_schedules(&p, store::PoolTask::Scrub)
            .expect("scrub")
            .is_empty());

        // The deadline passes: the run is recorded whether or not this host
        // has zpool, because the job's outcome is what the row carries.
        let due = chrono::DateTime::parse_from_rfc3339(row.next_run_at.as_deref().expect("next"))
            .expect("parse")
            .with_timezone(&Local)
            + chrono::Duration::minutes(1);
        run_due_pool_tasks(&p, store::PoolTask::Trim, due).await;
        let row = store::pool_schedule(&p, store::PoolTask::Trim, "fast")
            .expect("read")
            .expect("row");
        assert!(row.last_run_at.is_some(), "the trim ran");
        assert!(!row.last_result.is_empty(), "{}", row.last_result);
        assert_ne!(row.next_run_at, Some(due.to_rfc3339()), "rearmed forward");

        // A disabled schedule never fires again.
        store::set_pool_schedule(&p, store::PoolTask::Trim, "fast", false, &weekly, None)
            .expect("disable");
        run_due_pool_tasks(&p, store::PoolTask::Trim, due + chrono::Duration::days(14)).await;
        let off = store::pool_schedule(&p, store::PoolTask::Trim, "fast")
            .expect("read")
            .expect("row");
        assert_eq!(off.last_run_at, row.last_run_at, "no second run");
    }

    fn hourly() -> NasSchedule {
        NasSchedule {
            every: "1h".to_string(),
            hour: 0,
            minute: 0,
            weekday: 0,
            day: 1,
        }
    }

    /// Settles an array in this instance so the scheduler can find it: the
    /// create job, its operation closed successfully, and the job finished.
    fn settled_array(p: &DbPool, name: &str) -> tentanas_helper::elastic::ElasticCreateSpec {
        let spec = crate::tentanas::elastic::tests::create_spec(name);
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".to_string(),
            subject: spec.name.clone(),
            status: "running".to_string(),
            started_by: "test".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            p,
            &job,
            Some(&crate::tentanas::jobs::ElasticJobIntent::Create(spec.clone())),
        )
        .expect("create job");
        store::finish_elastic_operation(
            p,
            &spec.owner,
            &spec.operation_id,
            Ok(&crate::tentanas::elastic::tests::ready_result(&spec)),
        )
        .expect("settle operation");
        store::finish_job(p, &job.job_id, "succeeded", None).expect("finish job");
        spec
    }

    /// §5.3: an Elastic cadence is armed on the first tick, fires once its
    /// deadline passes, records the run and re-arms FORWARD.
    ///
    /// The mover fires here with no cache measurement available at all — a
    /// unit test has no helper to ask — and that is the point. The minimum
    /// free-space rule triggers EXTRA runs and never gates the scheduled ones;
    /// a gate would have to consult the cache, fail to read it, and skip, so
    /// this test fails the moment the threshold is turned into a condition.
    #[tokio::test]
    async fn an_elastic_cadence_fires_without_consulting_free_space_and_rearms() {
        let p = db();
        let spec = settled_array(&p, "media");
        store::set_elastic_schedule(
            &p,
            &spec.array_id,
            store::ElasticTask::Mover,
            true,
            &hourly(),
            None,
        )
        .expect("arm");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        assert_eq!(arrays.len(), 1, "the settled array is visible to the sweep");

        // First tick: nothing is due, but the cadence gets its next run.
        let now = at(2026, 9, 1, 14, 45);
        run_due_elastic_tasks(&p, &arrays, now).await;
        let armed = store::elastic_schedule(&p, &spec.array_id, store::ElasticTask::Mover)
            .expect("read")
            .expect("row");
        assert_eq!(armed.next_run_at, next_run_utc(&hourly(), now));
        assert!(armed.last_run_at.is_none(), "nothing ran yet");

        // The deadline passes: the run is recorded whether or not this host can
        // actually move a file, because the row carries the job's outcome.
        let due = DateTime::parse_from_rfc3339(armed.next_run_at.as_deref().expect("next"))
            .expect("parse")
            .with_timezone(&Local)
            + chrono::Duration::minutes(1);
        run_due_elastic_tasks(&p, &arrays, due).await;
        let fired = store::elastic_schedule(&p, &spec.array_id, store::ElasticTask::Mover)
            .expect("read")
            .expect("row");
        assert!(fired.last_run_at.is_some(), "the mover ran");
        assert!(!fired.last_result.is_empty(), "{}", fired.last_result);
        // Re-armed FORWARD. Left where it was, the cadence would re-fire on
        // every tick for the rest of the hour.
        let rearmed = DateTime::parse_from_rfc3339(fired.next_run_at.as_deref().expect("next"))
            .expect("parse")
            .with_timezone(&Local);
        assert!(rearmed > due, "re-armed forward, not left in the past");

        // A disabled cadence never fires again.
        store::set_elastic_schedule(
            &p,
            &spec.array_id,
            store::ElasticTask::Mover,
            false,
            &hourly(),
            fired.next_run_at.as_deref(),
        )
        .expect("disable");
        run_due_elastic_tasks(&p, &arrays, due + chrono::Duration::days(1)).await;
        let off = store::elastic_schedule(&p, &spec.array_id, store::ElasticTask::Mover)
            .expect("read")
            .expect("row");
        assert_eq!(off.last_run_at, fired.last_run_at, "no second run");
    }

    /// A refused spawn is NORMAL and must not stop the sweep.
    ///
    /// `insert_job` already refuses a second running operation on one array,
    /// and THAT refusal is the serialisation the plan asks for. The tick has
    /// to record it and carry on: treating it as fatal would let one array
    /// busy with a manual sync silently stop every other array's cadence.
    #[tokio::test]
    async fn a_refused_spawn_is_recorded_and_the_other_arrays_still_run() {
        let p = db();
        let busy_spec = settled_array(&p, "busy");
        let free_spec = settled_array(&p, "free");
        let armed_at = Some("2026-09-01T00:00:00Z");
        for spec in [&busy_spec, &free_spec] {
            store::set_elastic_schedule(
                &p,
                &spec.array_id,
                store::ElasticTask::Mover,
                true,
                &hourly(),
                armed_at,
            )
            .expect("arm");
        }

        // Occupy the first array with a running operation, exactly as a manual
        // sync started a moment earlier would.
        let running = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_sync".to_string(),
            subject: busy_spec.name.clone(),
            status: "running".to_string(),
            started_by: "admin".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &p,
            &running,
            Some(&crate::tentanas::jobs::ElasticJobIntent::Snapraid {
                owner: busy_spec.owner.clone(),
                array_id: busy_spec.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                kind: tentanas_helper::elastic::ElasticSnapraidKind::Sync,
            }),
        )
        .expect("occupy the array");

        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        run_due_elastic_tasks(&p, &arrays, at(2026, 9, 2, 10, 0)).await;

        let refused = store::elastic_schedule(&p, &busy_spec.array_id, store::ElasticTask::Mover)
            .expect("read")
            .expect("row");
        assert!(
            refused.last_result.starts_with("failed to start"),
            "the refusal is recorded, not swallowed: {}",
            refused.last_result
        );
        // And it re-arms anyway, so the busy array retries next cadence rather
        // than hammering the disks once a minute.
        assert_ne!(refused.next_run_at.as_deref(), armed_at, "re-armed forward");

        let started = store::elastic_schedule(&p, &free_spec.array_id, store::ElasticTask::Mover)
            .expect("read")
            .expect("row");
        assert!(
            started.last_result.starts_with("started job"),
            "one array's refusal must not stop the next one: {}",
            started.last_result
        );
    }
}
