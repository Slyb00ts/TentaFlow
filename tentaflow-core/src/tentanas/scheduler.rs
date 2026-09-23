// =============================================================================
// File: tentanas/scheduler.rs — the node's recurring work (plan-02 §5.2, tab
//       "Zadania"): scrubs, automatic snapshots with GFS retention, SMART
//       self-tests and the automatic Elastic cache mover. One loop per node, one tick a minute, every decision made
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

use tentanas_helper::elastic::ElasticCacheAge;

use super::db as store;
use super::elastic::{mover_trigger, ArrayObservation, CacheStuckVerdict, ElasticArrayRow, MoverClock, MoverTrigger};
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
    run_due_smart_tests(
        db,
        now,
        super::disks::snapshot()
            .0
            .into_iter()
            .map(|d| (d.disk_id, d.path))
            .collect(),
    )
    .await;
    run_elastic_passes(db, now, super::elastic::MoverClock::global(), &HelperCacheObserver { db }).await;
    // The access audit and the outbound forwarding are per-minute work of the
    // same loop: both are cheap when there is nothing to do, and neither may
    // depend on somebody having a tab open (§5.10).
    super::access_log::collect_tick(db).await;
    super::forward::forward_tick(main_db, db).await;
}

/// The Elastic passes of one tick: scheduled SnapRAID runs and the automatic
/// mover.
///
/// NOTHING RUNS WHILE THE STARTUP RESTORES DO (A1 of the second review).
/// After a boot every private array waits for its Restore; a run started
/// before it is refused by the helper — and for the mover that refusal cost
/// the whole retrigger cooldown — while the probe it takes first holds the
/// node lock the Restore needs, which refused the Restore as busy.
async fn run_elastic_passes(
    db: &DbPool,
    now: DateTime<Local>,
    clock: &super::elastic::MoverClock,
    observer: &impl CacheObserver,
) {
    if super::elastic::startup_restores_pending() {
        return;
    }
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
    run_automatic_movers(db, &arrays, clock, observer).await;
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
            // AN UNATTENDED SYNC DOES NOT RUN OVER A SCRUB'S REPORTED ERRORS.
            //
            // A Sync is admitted on an array that needs attention — that is
            // what gives a failed or interrupted parity run a way out — but the
            // cadence must not take it while a scrub's errors are still
            // unrepaired. MEASURED on rig11 (snapraid 13.0-1, probe f1-f3): a
            // Sync leaves a marked block repairable, so the case the repair
            // exists for survives it; but the files a scrub could NOT read are
            // removed from the content file by that same Sync, and parity then
            // holds nothing of them. An admin who is told can weigh that; a
            // cadence cannot, so it skips, says why in the schedule row, and
            // raises an alert that repairing clears.
            //
            // Only the Sync. A full Scrub writes no parity and is how the array
            // is re-measured, and the mover has its own gate (an unresolved
            // parity run keeps it off the array entirely).
            let parity_fault = if task == store::ElasticTask::Sync {
                super::elastic::scheduled_sync_blocker(&array.snapraid_history)
            } else {
                None
            };
            // The alert belongs to the SYNC slot alone. Raising or clearing it
            // from the scrub's or the mover's pass over the same array would
            // undo whatever the sync's pass decided, which is how the first
            // version of this guard cleared its own alert and skipped silently.
            if task == store::ElasticTask::Sync {
            let repair_key = super::elastic::repair_alert_key(&array.name);
            let alerted = match &parity_fault {
                Some(_) => store::raise_alert(
                    db,
                    &repair_key,
                    "warning",
                    "elastic-array",
                    &array.name,
                    "Zaplanowany Sync wstrzymany: parity zgłasza błędy",
                    "Scrub tej macierzy zgłosił błędy, których nic jeszcze nie naprawiło. Node nie uruchamia                      zaplanowanego Sync, bo usunąłby z content pliki, których scrub nie mógł odczytać. Uruchom                      naprawę z parity, a potem scrub; Sync ręczny pozostaje dostępny.",
                )
                .map(|_| ()),
                None => store::resolve_alert(db, &repair_key),
            };
            if let Err(e) = alerted {
                tracing::warn!("tentanas scheduler: repair alert of {} not recorded: {e}", array.name);
            }
            }
            if let Some(reason) = parity_fault {
                // The advanced `next_run_at` goes in with the reason, so the
                // slot is skipped once and not re-tried every tick.
                if let Err(e) =
                    store::record_elastic_schedule_run(db, &row.array_id, task, &reason, next.as_deref())
                {
                    tracing::warn!("tentanas scheduler: elastic sync skip of {} not recorded: {e}", array.name);
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
            // automatic pass later in this SAME tick finds an unmarked array,
            // measures a cache the run it just started has not drained yet,
            // and asks for a second run `insert_job` can only refuse — a
            // recurring warning, and the 20-minute cooldown burned on a run
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

/// The two privileged reads the automatic mover acts on. A trait so the pass
/// below can be exercised against a real database with canned measurements:
/// a unit test has no helper to ask.
trait CacheObserver {
    /// The branch free space `mover_trigger` checks for cache pressure.
    async fn cache(&self, array: &ElasticArrayRow) -> anyhow::Result<ArrayObservation>;
    /// What a run would move right now (`ElasticCacheAge`).
    async fn age(&self, array: &ElasticArrayRow) -> anyhow::Result<ElasticCacheAge>;
}

struct HelperCacheObserver<'a> {
    db: &'a DbPool,
}

impl CacheObserver for HelperCacheObserver<'_> {
    async fn cache(&self, array: &ElasticArrayRow) -> anyhow::Result<ArrayObservation> {
        super::elastic::observe_cache(self.db, array).await
    }

    async fn age(&self, array: &ElasticArrayRow) -> anyhow::Result<ElasticCacheAge> {
        super::elastic::observe_cache_age(self.db, array).await
    }
}

/// The automatic mover (§5.3; owner decision 2026-09-17: moving files off the
/// cache is automatic, like any cache, and nobody has to configure it).
///
/// Two triggers start a run, both through `mover_trigger`:
/// * cache pressure — the cache fell below its minimum free space. Measured
///   every tick for an array that is cooled down, exactly as before, and it
///   overrides a restricting schedule;
/// * aged files — the helper's age probe found files a run would move. The
///   probe walks the cache as root, so it runs at most once per cooldown per
///   array, only when pressure did not already fire, and never while an
///   operation of the array runs (the helper would hold its lock). A schedule
///   that is switched on restricts this trigger
///   to its own slots (`run_due_elastic_tasks` fires those).
///
/// Every probe also decides the stuck-cache alert (`cache_stuck_verdict`).
///
/// A REFUSED SPAWN IS NORMAL. `insert_job` refuses a mover on an array that is
/// not active, has an unresolved operation, or already runs one; that refusal
/// is the serialisation, and the clock mark before the spawn keeps it from
/// being re-asked every minute.
async fn run_automatic_movers(
    db: &DbPool,
    arrays: &[ElasticArrayRow],
    clock: &MoverClock,
    observer: &impl CacheObserver,
) {
    for array in arrays {
        if !array.enabled || array.cache().next().is_none() || !clock.cooled_down(&array.name) {
            continue;
        }
        let Some(array_id) = array.array_id() else {
            continue;
        };
        match store::elastic_operation_running(db, array_id) {
            Ok(false) => {}
            Ok(true) => continue,
            Err(e) => {
                tracing::warn!("tentanas scheduler: operations of {} unreadable: {e}", array.name);
                continue;
            }
        }
        // The automatic settling stopped after its attempts: an admin is told,
        // once, and the alert goes when a run succeeds.
        let settle_key = super::elastic::settle_alert_key(&array.name);
        let settle_recorded = if array.mover_settles_unresolved && array.mover_failed_runs >= super::elastic::SETTLE_ATTEMPTS {
            store::raise_alert(
                db,
                &settle_key,
                "warning",
                "elastic-array",
                &array.name,
                "Automatyczne dokończenie przenoszenia wstrzymane",
                &format!(
                    "{} kolejnych przebiegów przenoszenia zakończyło się niepowodzeniem, więc node nie uruchamia \
                     następnych sam. Sprawdź przyczynę w historii przenoszenia i uruchom przenoszenie ręcznie.",
                    array.mover_failed_runs
                ),
            )
            .map(|_| ())
        } else {
            store::resolve_alert(db, &settle_key)
        };
        if let Err(e) = settle_recorded {
            tracing::warn!("tentanas scheduler: settle alert of {} not recorded: {e}", array.name);
        }
        // Settling what an earlier run left needs no measurement, and it must
        // not wait for one: the helper refuses nothing a settling run needs.
        //
        // ONCE THE SETTLING HAS STOPPED the cache is measured again. Skipping
        // it for ever would silence the very alerts that say the cache is
        // filling and that a file was left in two versions, for exactly the
        // array whose runs are failing — and the measurement is what the
        // stuck-cache and conflict alerts are made of.
        let settling = array.mover_settles_unresolved
            && array.mover_failed_runs < super::elastic::SETTLE_ATTEMPTS;
        let mut observed = if settling {
            ArrayObservation::default()
        } else {
            match observer.cache(array).await {
                Ok(o) => {
                    if let Err(e) = super::elastic::record_conflict_alert(db, &array.name, &o.conflicts) {
                        tracing::warn!("tentanas scheduler: conflict alert of {} not recorded: {e}", array.name);
                    }
                    o
                }
                Err(e) => {
                    // Unknown free space is not low free space: nothing fires.
                    tracing::warn!("tentanas scheduler: cache of {} not measured: {e}", array.name);
                    continue;
                }
            }
        };
        if mover_trigger(array, &observed, clock) == MoverTrigger::None && clock.probe_due(&array.name) {
            // Marked before the probe: a probe that fails is not retried a
            // minute later either.
            clock.probed(&array.name);
            match observer.age(array).await {
                Ok(age) => observed.cache_age = Some(age),
                Err(e) => tracing::warn!(
                    "tentanas scheduler: files waiting on the cache of {} not measured: {e}",
                    array.name
                ),
            }
        }
        let trigger = mover_trigger(array, &observed, clock);
        let mut run_started = false;
        if trigger != MoverTrigger::None {
            // Marked before the spawn, not after. A refusal here means the
            // array cannot take a run now, and asking again a minute later
            // would re-ask the helper for the same answer.
            clock.started(&array.name);
            let why = match trigger {
                MoverTrigger::CacheLow => "cache low",
                MoverTrigger::Settle => "an earlier run left something unresolved",
                _ => "aged files waiting",
            };
            match super::elastic::spawn_mover(db, array, STARTED_BY, None) {
                Ok(job) => {
                    run_started = true;
                    tracing::info!(
                        "tentanas scheduler: {why} on {}, started mover job {}",
                        array.name,
                        job.job_id
                    );
                }
                Err(e) => tracing::warn!("tentanas scheduler: {why} on {}, mover not started: {e}", array.name),
            }
        }
        if let Some(age) = observed.cache_age.as_ref() {
            let key = super::elastic::protection_alert_key(&array.name);
            let recorded = match super::elastic::cache_stuck_verdict(array, age, run_started) {
                CacheStuckVerdict::Raise { title, detail } => store::raise_alert(
                    db,
                    &key,
                    "warning",
                    "elastic-array",
                    &array.name,
                    &title,
                    &detail,
                )
                .map(|_| ()),
                CacheStuckVerdict::Clear => store::resolve_alert(db, &key),
                CacheStuckVerdict::Keep => Ok(()),
            };
            if let Err(e) = recorded {
                tracing::warn!("tentanas scheduler: cache alert of {} not recorded: {e}", array.name);
            }
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

/// §5.10: the two SMART self-test cadences, long pass then short pass.
///
/// `disks` is one `(disk_id, device path)` pair per disk, INJECTED by the
/// caller. The inventory lives in a process-global snapshot a test cannot
/// seed, and without the injection neither the order of the two passes nor
/// what a pass does when every disk refuses could be tested at all.
///
/// A REFUSED SPAWN IS NORMAL AND DOES NOT FAIL THE TICK, exactly as on the
/// Elastic path: `insert_job` refuses a self-test on a disk that is already
/// running one, because the second test would ABORT the first (ATA, SPC and
/// NVMe all behave this way) and a long test spans many ticks of a daily short
/// schedule. That refusal IS the serialisation this needs — for the scheduled
/// and the manual start alike — and a second lock here would only be able to
/// disagree with it.
async fn run_due_smart_tests(db: &DbPool, now: DateTime<Local>, disks: Vec<(String, String)>) {
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
    // Long pass FIRST. Starting a short test aborts a long one already on the
    // disk, so when both come due in the same tick the long pass — the one
    // that reads the whole surface — takes the disks, and the short pass is
    // refused on every disk the long pass has just occupied. Reversing these
    // two would make a daily short test cut every long test short.
    for long in [true, false] {
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
        // The cadence re-arms FORWARD even when every disk refuses: a pass
        // left due would retry — and log — on every tick for the hours a long
        // test takes, and the next short test is wanted at the next deadline,
        // not the minute the long one ends.
        if long {
            smart.next_long_at = next;
        } else {
            smart.next_short_at = next;
        }
        changed = true;
        let mut started_any = false;
        for (disk_id, device) in disks.iter().cloned() {
            match super::jobs::spawn(db, "smart_test", &disk_id, STARTED_BY, None, None, move |h| {
                super::jobs::smart_self_test(h, device, kind, None)
            }) {
                Ok(_) => started_any = true,
                // Logged at info, not warn: on a node whose long test is still
                // running this is the expected answer for every disk, and the
                // message carries the reason the refusal gave.
                Err(e) => tracing::info!(
                    "tentanas scheduler: SMART test for {disk_id} not started: {e}"
                ),
            }
        }
        // A pass that started NOTHING did not run, so it does not stamp
        // `last_*_at`. The Tasks tab renders that field as the schedule's
        // `last_run_at` with no result column beside it (§5.10), so a stamp
        // here would tell the operator a test ran on every disk while the
        // long pass held them all and the short pass did nothing.
        if started_any {
            if long {
                smart.last_long_at = Some(store::now());
            } else {
                smart.last_short_at = Some(store::now());
            }
        } else {
            tracing::info!(
                "tentanas scheduler: SMART {} pass started nothing, the schedule records no run",
                if long { "long" } else { "short" }
            );
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
        settle(p, crate::tentanas::elastic::tests::create_spec(name))
    }

    fn settle(
        p: &DbPool,
        spec: tentanas_helper::elastic::ElasticCreateSpec,
    ) -> tentanas_helper::elastic::ElasticCreateSpec {
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

    /// A SCHEDULED SYNC DOES NOT RUN over a scrub's reported errors, it says
    /// why in the schedule row, it raises an alert an admin can act on, and the
    /// scheduled SCRUB beside it still runs.
    ///
    /// The admission rule that unblocked the repair (`parity_admission`) admits
    /// a Sync on an array that needs attention — deliberately, because a failed
    /// or interrupted parity run must have a way out. The cadence must not take
    /// that way out by itself. MEASURED on rig11 (snapraid 13.0-1, probe f1-f3):
    /// a Sync leaves a marked block repairable, so what it costs is the other
    /// half of a scrub's report — the files the scrub could not read, which the
    /// Sync removes from the content file, after which parity holds nothing of
    /// them. An admin who is told can weigh that; a cadence cannot.
    #[tokio::test]
    async fn a_scheduled_sync_skips_an_array_whose_scrub_reported_errors() {
        use tentanas_helper::elastic::{ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome};
        let p = db();
        let spec = settled_array(&p, "media");
        // A full scrub that counted errors: the array needs attention and
        // nothing has repaired it.
        let (job, intent) = snapraid_job(&spec, Kind::Scrub);
        let crate::tentanas::jobs::ElasticJobIntent::Snapraid { operation_id, .. } = &intent else {
            panic!("a scrub intent")
        };
        let scrub_id = operation_id.clone();
        store::insert_job(&p, &job, Some(&intent)).expect("scrub job");
        store::record_snapraid_result(
            &p,
            &spec.owner,
            &scrub_id,
            &crate::tentanas::elastic::tests::snapraid_result(&spec, &scrub_id, Kind::Scrub, Outcome::Failed),
        )
        .expect("result");
        store::finish_job(&p, &job.job_id, "failed", Some("parity errors")).expect("finish");

        for task in [store::ElasticTask::Sync, store::ElasticTask::Scrub] {
            store::set_elastic_schedule(&p, &spec.array_id, task, true, &hourly(), None).expect("arm");
        }
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        // The array still admits a MANUAL parity run: this test is about who
        // starts it, not about whether it may be started.
        assert!(arrays[0].parity_run_available);
        let now = at(2026, 9, 1, 14, 45);
        run_due_elastic_tasks(&p, &arrays, now).await;
        let due = DateTime::parse_from_rfc3339(
            store::elastic_schedule(&p, &spec.array_id, store::ElasticTask::Sync)
                .expect("read")
                .expect("row")
                .next_run_at
                .as_deref()
                .expect("next"),
        )
        .expect("parse")
        .with_timezone(&Local)
            + chrono::Duration::minutes(1);
        run_due_elastic_tasks(&p, &arrays, due).await;

        // The Sync slot is SKIPPED, with the reason in the row and the cadence
        // re-armed forward so it is not retried every tick.
        let sync_row = store::elastic_schedule(&p, &spec.array_id, store::ElasticTask::Sync)
            .expect("read")
            .expect("row");
        assert!(sync_row.last_result.contains("pominięto"), "{}", sync_row.last_result);
        assert!(sync_row.last_result.contains("naprawę"), "{}", sync_row.last_result);
        let rearmed = DateTime::parse_from_rfc3339(sync_row.next_run_at.as_deref().expect("next"))
            .expect("parse")
            .with_timezone(&Local);
        assert!(rearmed > due, "the skipped slot is re-armed forward");
        assert_eq!(
            store::list_jobs(&p, 100)
                .expect("jobs")
                .into_iter()
                .filter(|job| job.kind == "elastic_sync")
                .count(),
            0,
            "no unattended sync was started"
        );
        // And an admin is told, in a way repairing clears.
        let alert = store::list_alerts(&p, true)
            .expect("alerts")
            .into_iter()
            .find(|alert| alert.subject_id == "media" && alert.title.contains("Sync wstrzymany"))
            .expect("the skip raises an alert");
        assert_eq!(alert.severity, "warning");
        assert_eq!(alert.subject_kind, "elastic-array");
        assert!(alert.detail.contains("naprawę"), "{}", alert.detail);

        // THE SCRUB SLOT STILL RUNS: a full scrub writes no parity, and it is
        // how the array gets re-measured after a repair.
        let scrub_row = store::elastic_schedule(&p, &spec.array_id, store::ElasticTask::Scrub)
            .expect("read")
            .expect("row");
        assert!(scrub_row.last_run_at.is_some(), "the scrub cadence is untouched");
        assert!(!scrub_row.last_result.contains("pominięto"), "{}", scrub_row.last_result);

        // The scheduled scrub above is still running, and one array runs one
        // operation at a time — close it the way a lost helper answer does.
        for job in store::list_jobs(&p, 100).expect("jobs").into_iter().filter(|job| job.status == "running") {
            store::finish_job(&p, &job.job_id, "failed", Some("test: helper unavailable")).expect("close");
        }
        // A SUCCEEDED REPAIR clears the fault, and the next slot runs: the skip
        // is a state an admin action leaves, not a dead end.
        let (fix_job, fix_intent) = snapraid_job(&spec, Kind::Fix { disk: "d1".into() });
        let crate::tentanas::jobs::ElasticJobIntent::Snapraid { operation_id, .. } = &fix_intent else {
            panic!("a fix intent")
        };
        let fix_id = operation_id.clone();
        store::insert_job(&p, &fix_job, Some(&fix_intent)).expect("fix job");
        let mut repaired = crate::tentanas::elastic::tests::snapraid_result(
            &spec,
            &fix_id,
            Kind::Fix { disk: "d1".into() },
            Outcome::Succeeded,
        );
        repaired.run.errors_data = Some(7);
        repaired.run.checked_blocks = None;
        repaired.state.last_run = Some(repaired.run.clone());
        store::record_snapraid_result(&p, &spec.owner, &fix_id, &repaired).expect("repair result");
        store::finish_job(&p, &fix_job.job_id, "succeeded", None).expect("finish");
        let healed = store::elastic_arrays_all(&p).expect("arrays");
        run_due_elastic_tasks(&p, &healed, rearmed + chrono::Duration::minutes(1)).await;
        assert_eq!(
            store::list_jobs(&p, 100)
                .expect("jobs")
                .into_iter()
                .filter(|job| job.kind == "elastic_sync")
                .count(),
            1,
            "with the fault repaired the cadence syncs again"
        );
        assert!(
            store::list_alerts(&p, true)
                .expect("alerts")
                .into_iter()
                .all(|alert| !alert.title.contains("Sync wstrzymany")),
            "and the alert is resolved"
        );
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
    // ----- the automatic mover ------------------------------------------------

    /// An array settled like `settled_array`, WITH a cache disk.
    fn settled_cached_array(p: &DbPool, name: &str) -> tentanas_helper::elastic::ElasticCreateSpec {
        let mut spec = crate::tentanas::elastic::tests::create_spec(name);
        let mut cache = spec.data[0].clone();
        cache.disk_id = format!("{name}-cache");
        cache.wwn = Some(format!("wwn-{name}-cache"));
        cache.serial = Some(format!("serial-{name}-cache"));
        cache.expected_uuid = uuid::Uuid::new_v4().to_string();
        spec.cache = Some(cache);
        settle(p, spec)
    }

    /// Canned helper answers, counted, so a test can say not only what the
    /// pass started but what it paid to find out.
    struct Canned {
        cache_free_pct: u64,
        age: ElasticCacheAge,
        cache_reads: std::sync::atomic::AtomicUsize,
        age_reads: std::sync::atomic::AtomicUsize,
    }

    impl Canned {
        fn new(cache_free_pct: u64, age: ElasticCacheAge) -> Self {
            Self { cache_free_pct, age, cache_reads: Default::default(), age_reads: Default::default() }
        }
        fn reads(&self) -> (usize, usize) {
            use std::sync::atomic::Ordering::Relaxed;
            (self.cache_reads.load(Relaxed), self.age_reads.load(Relaxed))
        }
    }

    impl CacheObserver for Canned {
        async fn cache(&self, array: &ElasticArrayRow) -> anyhow::Result<ArrayObservation> {
            self.cache_reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut observed = ArrayObservation::default();
            for branch in array.cache() {
                observed.probes.insert(
                    tentanas_helper::elastic::cache_branch_path(&array.name, &branch.name),
                    crate::tentanas::elastic::BranchProbe {
                        mounted: Some(true),
                        device_present: Some(true),
                        size_bytes: Some(1_000),
                        used_bytes: Some(1_000 - self.cache_free_pct * 10),
                        free_bytes: Some(self.cache_free_pct * 10),
                    },
                );
            }
            Ok(observed)
        }

        async fn age(&self, _: &ElasticArrayRow) -> anyhow::Result<ElasticCacheAge> {
            self.age_reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.age)
        }
    }

    fn waiting(due_files: u64, held_files: u64, oldest_secs: u64) -> ElasticCacheAge {
        ElasticCacheAge { due_files, due_bytes: due_files * 1024, held_files, oldest_secs: Some(oldest_secs) }
    }

    fn mover_jobs(p: &DbPool, subject: &str) -> i64 {
        p.read()
            .expect("read")
            .query_row(
                "SELECT COUNT(*) FROM nas_jobs WHERE kind = 'elastic_mover' AND subject = ?1",
                [subject],
                |r| r.get(0),
            )
            .expect("count")
    }

    fn stuck_alerts(p: &DbPool, subject: &str) -> Vec<tentaflow_protocol::tentanas::NasAlert> {
        store::list_alerts(p, true)
            .expect("alerts")
            .into_iter()
            .filter(|a| a.subject_kind == "elastic-array" && a.subject_id == subject)
            .collect()
    }

    /// The owner's decision (2026-09-17), end to end over a real database: an
    /// array with a cache, NO schedule and a roomy cache starts a run by
    /// itself once aged files wait — and then, cooling down, asks the helper
    /// nothing at all until the cooldown has passed.
    #[tokio::test]
    async fn aged_files_start_a_run_with_no_schedule_and_a_roomy_cache_then_the_array_cools_down() {
        let p = db();
        settled_cached_array(&p, "media");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        assert!(arrays[0].cache().next().is_some(), "the fixture has a cache");
        assert!(store::elastic_schedule(&p, &arrays[0].array_id().unwrap().to_string(), store::ElasticTask::Mover)
            .expect("schedule")
            .is_none());
        let clock = MoverClock::new();
        let helper = Canned::new(90, waiting(3, 0, 9_000));

        run_automatic_movers(&p, &arrays, &clock, &helper).await;
        assert_eq!(mover_jobs(&p, "media"), 1, "aged files started a run");
        assert_eq!(helper.reads(), (1, 1));
        assert!(!clock.cooled_down("media"));

        run_automatic_movers(&p, &arrays, &clock, &helper).await;
        assert_eq!(mover_jobs(&p, "media"), 1, "no second run while cooling down");
        assert_eq!(helper.reads(), (1, 1), "a cooling array costs no privileged call");
    }

    /// Fresh files are what a cache is for: probed, no run, no alert.
    #[tokio::test]
    async fn fresh_files_on_a_roomy_cache_start_nothing_and_raise_nothing() {
        let p = db();
        settled_cached_array(&p, "media");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        let helper = Canned::new(90, waiting(0, 0, 3_600));
        let clock = MoverClock::new();
        run_automatic_movers(&p, &arrays, &clock, &helper).await;
        assert_eq!(mover_jobs(&p, "media"), 0);
        assert!(stuck_alerts(&p, "media").is_empty());
        // Probed once per cooldown, not once a minute.
        run_automatic_movers(&p, &arrays, &clock, &helper).await;
        assert_eq!(helper.reads(), (2, 1));
    }

    /// A run already going is the answer: no second run, and the helper — whose
    /// lock that run holds — is not asked anything. A cacheless array has
    /// nothing to move and is never measured.
    #[tokio::test]
    async fn a_running_operation_and_a_missing_cache_start_nothing() {
        let p = db();
        let busy = settled_cached_array(&p, "busy");
        settled_array(&p, "plain");
        let running = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_mover".to_string(),
            subject: busy.name.clone(),
            status: "running".to_string(),
            started_by: "admin".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &p,
            &running,
            Some(&crate::tentanas::jobs::ElasticJobIntent::Mover {
                owner: busy.owner.clone(),
                array_id: busy.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                resume_operation_id: uuid::Uuid::now_v7().to_string(),
                rules: tentanas_helper::elastic::MoverRules::default(),
                coupled_sync: true,
            }),
        )
        .expect("a run is going");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        assert_eq!(arrays.len(), 2);
        let helper = Canned::new(5, waiting(3, 0, 90_000));
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &helper).await;
        assert_eq!(mover_jobs(&p, "busy"), 1, "only the run that was already going");
        assert_eq!(mover_jobs(&p, "plain"), 0);
        assert_eq!(helper.reads(), (0, 0));
    }

    /// Cache pressure still starts a run, and does not pay for an age probe
    /// it does not need — also on an array whose schedule restricts moving.
    #[tokio::test]
    async fn cache_pressure_still_starts_a_run_without_an_age_probe() {
        let p = db();
        let spec = settled_cached_array(&p, "media");
        store::set_elastic_schedule(
            &p,
            &spec.array_id,
            store::ElasticTask::Mover,
            true,
            &NasSchedule { every: "daily".into(), hour: 3, ..Default::default() },
            Some("2099-01-01T03:00:00Z"),
        )
        .expect("restrict");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        let helper = Canned::new(5, waiting(0, 0, 60));
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &helper).await;
        assert_eq!(mover_jobs(&p, "media"), 1);
        assert_eq!(helper.reads(), (1, 0));
    }

    /// Files past the threshold for which this pass starts a run do not open
    /// the alert: the run is the answer, and the next probe after it decides.
    #[tokio::test]
    async fn files_past_the_threshold_with_a_run_starting_raise_nothing() {
        let p = db();
        settled_cached_array(&p, "media");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        let limit = crate::tentanas::elastic::cache_stuck_after_secs(&arrays[0]);
        let moving = Canned::new(90, waiting(3, 0, limit + 60));
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &moving).await;
        assert_eq!(mover_jobs(&p, "media"), 1);
        assert!(stuck_alerts(&p, "media").is_empty());
    }

    /// A switched-on schedule restricts the age trigger to its own slots. The
    /// probe still runs, because the stuck alert depends on it.
    #[tokio::test]
    async fn a_restricting_schedule_holds_aged_files_for_its_slot() {
        let p = db();
        let spec = settled_cached_array(&p, "media");
        store::set_elastic_schedule(
            &p,
            &spec.array_id,
            store::ElasticTask::Mover,
            true,
            &NasSchedule { every: "daily".into(), hour: 3, ..Default::default() },
            Some("2099-01-01T03:00:00Z"),
        )
        .expect("restrict");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        let helper = Canned::new(90, waiting(3, 0, 9_000));
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &helper).await;
        assert_eq!(mover_jobs(&p, "media"), 0);
        assert_eq!(helper.reads(), (1, 1));
    }

    /// The stuck alert, through the node's own alert table: nothing below the
    /// threshold however much is pending, raised once files that cannot move
    /// have waited past it, and resolved by the first probe that finds nothing
    /// that old.
    #[tokio::test]
    async fn the_stuck_alert_opens_past_its_threshold_and_resolves_when_the_cache_drains() {
        let p = db();
        settled_cached_array(&p, "media");
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        let limit = crate::tentanas::elastic::cache_stuck_after_secs(&arrays[0]);

        let pending = Canned::new(90, waiting(0, 40, limit - 60));
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &pending).await;
        assert!(stuck_alerts(&p, "media").is_empty(), "files below the threshold are not stuck");

        let held = Canned::new(90, waiting(0, 2, limit + 60));
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &held).await;
        let open = stuck_alerts(&p, "media");
        assert_eq!(open.len(), 1, "{open:?}");
        assert_eq!(open[0].severity, "warning");
        assert!(open[0].detail.contains("otwarte"), "{}", open[0].detail);
        assert!(open[0].resolved_at.is_none());

        let drained = Canned::new(90, waiting(0, 0, 120));
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &drained).await;
        assert!(stuck_alerts(&p, "media").is_empty(), "the first probe below the threshold resolves it");
    }

    /// A mover job that stopped with nothing recorded — the helper killed, the
    /// job timed out, core restarted — as `finish_job` closes it.
    fn lost_mover_run(p: &DbPool, spec: &tentanas_helper::elastic::ElasticCreateSpec) -> String {
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_mover".to_string(),
            subject: spec.name.clone(),
            status: "running".to_string(),
            started_by: "scheduler".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        let operation_id = uuid::Uuid::now_v7().to_string();
        store::insert_job(
            p,
            &job,
            Some(&crate::tentanas::jobs::ElasticJobIntent::Mover {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: operation_id.clone(),
                resume_operation_id: uuid::Uuid::now_v7().to_string(),
                rules: tentanas_helper::elastic::MoverRules::default(),
                coupled_sync: true,
            }),
        )
        .expect("mover");
        store::finish_job(p, &job.job_id, "failed", Some("Brak wiarygodnego wyniku movera")).expect("finish");
        operation_id
    }

    /// H2 of the 2026-09-17 review: an unresolved mover run used to hold the
    /// array until a `fix` the helper refuses while that run is open. Now the
    /// scheduler starts the settling run on its own — with no cache
    /// measurement, since none is needed — and the array is active again as
    /// soon as a run completes. An unresolved PARITY operation still blocks.
    #[tokio::test]
    async fn an_unresolved_mover_run_is_settled_by_the_next_run_not_by_a_fix() {
        let p = db();
        let spec = settled_cached_array(&p, "media");
        lost_mover_run(&p, &spec);
        let arrays = store::elastic_arrays_all(&p).expect("arrays");
        assert_eq!(arrays[0].state, "needs_attention");
        assert!(arrays[0].unresolved_operation);
        assert!(arrays[0].mover_settles_unresolved, "only a mover is unresolved");
        // A Sync still waits: nothing knows what the unresolved run moved.
        let sync_job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_sync".to_string(),
            subject: spec.name.clone(),
            status: "running".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        assert!(store::insert_job(
            &p,
            &sync_job,
            Some(&crate::tentanas::jobs::ElasticJobIntent::Snapraid {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                kind: tentanas_helper::elastic::ElasticSnapraidKind::Sync,
            }),
        )
        .is_err());
        // The cache cannot even be measured; the settling run starts anyway.
        struct Unmeasurable;
        impl CacheObserver for Unmeasurable {
            async fn cache(&self, _: &ElasticArrayRow) -> anyhow::Result<ArrayObservation> {
                Err(anyhow::anyhow!("Elastic busy"))
            }
            async fn age(&self, _: &ElasticArrayRow) -> anyhow::Result<ElasticCacheAge> {
                Err(anyhow::anyhow!("Elastic busy"))
            }
        }
        run_automatic_movers(&p, &arrays, &MoverClock::new(), &Unmeasurable).await;
        assert_eq!(mover_jobs(&p, "media"), 2, "the settling run was started");
        // That run completes: the array serves again and nothing is unresolved.
        let (operation_id, job_id): (String, String) = p
            .read()
            .expect("read")
            .query_row(
                "SELECT operation_id, job_id FROM nas_elastic_operations WHERE kind='mover' AND state='running'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("running mover");
        let resume: String = p
            .read()
            .expect("read")
            .query_row("SELECT request_json FROM nas_elastic_operations WHERE operation_id=?1", [&operation_id], |r| r.get::<_, String>(0))
            .map(|json| match serde_json::from_str::<tentanas_helper::HelperCommand>(&json).expect("request") {
                tentanas_helper::HelperCommand::ElasticMover { resume_operation_id, .. } => resume_operation_id,
                other => panic!("{other:?}"),
            })
            .expect("request");
        let mut result = crate::tentanas::elastic::tests::mover_result(
            &spec,
            &operation_id,
            &resume,
            tentanas_helper::elastic::ElasticMoverPhase::Complete,
            true,
        );
        result.state.last_mover = Some(result.run.clone());
        store::record_mover_result(&p, &spec.owner, &operation_id, &result).expect("wynik");
        store::finish_job(&p, &job_id, "succeeded", None).expect("finish");
        let settled = store::elastic_arrays_all(&p).expect("arrays").remove(0);
        assert_eq!(settled.state, "active");
        assert!(!settled.unresolved_operation, "a later successful mover supersedes the lost one");
        assert!(!settled.mover_settles_unresolved);

        // An unresolved repair is not a mover's to settle.
        let p = db();
        let spec = settled_cached_array(&p, "media");
        let scrub = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_fix".to_string(),
            subject: spec.name.clone(),
            status: "running".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &p,
            &scrub,
            Some(&crate::tentanas::jobs::ElasticJobIntent::Snapraid {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                kind: tentanas_helper::elastic::ElasticSnapraidKind::Fix { disk: "d1".into() },
            }),
        )
        .expect("fix");
        store::finish_job(&p, &scrub.job_id, "failed", Some("Brak wyniku operacji SnapRAID")).expect("finish");
        let blocked = store::elastic_arrays_all(&p).expect("arrays").remove(0);
        assert!(blocked.unresolved_operation && !blocked.mover_settles_unresolved);
        run_automatic_movers(&p, &[blocked], &MoverClock::new(), &Unmeasurable).await;
        assert_eq!(mover_jobs(&p, "media"), 0, "no settling run over an unresolved repair");
    }

    /// One SnapRAID job of `kind`, with its intent.
    fn snapraid_job(
        spec: &tentanas_helper::elastic::ElasticCreateSpec,
        kind: tentanas_helper::elastic::ElasticSnapraidKind,
    ) -> (tentaflow_protocol::tentanas::NasJob, crate::tentanas::jobs::ElasticJobIntent) {
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: format!("elastic_{}", crate::tentanas::elastic::snapraid_kind(&kind)),
            subject: spec.name.clone(),
            status: "running".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        let intent = crate::tentanas::jobs::ElasticJobIntent::Snapraid {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: uuid::Uuid::now_v7().to_string(),
            kind,
        };
        (job, intent)
    }

    /// A SnapRAID job of `kind` that ended with no result — core restarted
    /// under it, or the helper died — as `finish_job` closes it.
    fn lost_snapraid_run(p: &DbPool, spec: &tentanas_helper::elastic::ElasticCreateSpec, kind: tentanas_helper::elastic::ElasticSnapraidKind) {
        let (job, intent) = snapraid_job(spec, kind);
        store::insert_job(p, &job, Some(&intent)).expect("snapraid job");
        store::finish_job(p, &job.job_id, "failed", Some("interrupted by core restart")).expect("finish");
    }

    /// D2 of the second review: a Sync or a Scrub whose process never reported
    /// is INTERRUPTED. It blocks nothing — the next Sync is what settles it —
    /// it reads as interrupted, and it is no evidence for a repair, which on a
    /// disk in use would revert users' files. A lost repair stays unresolved.
    #[tokio::test]
    async fn an_interrupted_sync_or_scrub_blocks_nothing_and_offers_no_repair() {
        for kind in [
            tentanas_helper::elastic::ElasticSnapraidKind::Sync,
            tentanas_helper::elastic::ElasticSnapraidKind::Scrub,
        ] {
            let p = db();
            let spec = settled_cached_array(&p, "media");
            lost_snapraid_run(&p, &spec, kind.clone());
            // W-D of the third review: the ARRAY is left as it was. A state of
            // needs_attention would refuse the very Sync the row says will
            // finish it, and only a manual Restore would clear it.
            let array = store::elastic_arrays_all(&p).expect("arrays").remove(0);
            assert_eq!(array.state, "active", "{kind:?}");
            assert!(!array.unresolved_operation, "{kind:?}");
            assert_eq!(array.snapraid_history[0].outcome, store::INTERRUPTED_OUTCOME, "{kind:?}");
            let wire = crate::tentanas::elastic::to_protocol(
                &array,
                &std::collections::BTreeMap::new(),
                &ArrayObservation::default(),
                true,
                "13.0",
                ("active", ""),
            );
            assert_eq!(crate::tentanas::elastic::repair_evidence(&wire), None, "{kind:?}");
            // The Sync the history row promises really is admitted — that is
            // the whole point of leaving the array active.
            let (sync, sync_intent) = snapraid_job(&spec, tentanas_helper::elastic::ElasticSnapraidKind::Sync);
            store::insert_job(&p, &sync, Some(&sync_intent)).expect("the next Sync settles it");
        }
        let p = db();
        let spec = settled_cached_array(&p, "media");
        lost_snapraid_run(&p, &spec, tentanas_helper::elastic::ElasticSnapraidKind::Fix { disk: "d1".into() });
        assert!(store::elastic_arrays_all(&p).expect("arrays").remove(0).unresolved_operation, "a lost repair stays unresolved");

        // A run whose result WAS recorded before core lost the job reads the
        // same way in the history as in the admission: interrupted, not a
        // fault with counters an admin would take to the repair dialog.
        let p = db();
        let spec = settled_cached_array(&p, "media");
        let (job, intent) = snapraid_job(&spec, tentanas_helper::elastic::ElasticSnapraidKind::Sync);
        let crate::tentanas::jobs::ElasticJobIntent::Snapraid { operation_id, .. } = &intent else {
            panic!("snapraid_job returns a SnapRAID intent");
        };
        let operation_id = operation_id.clone();
        store::insert_job(&p, &job, Some(&intent)).expect("sync job");
        store::record_snapraid_result(
            &p,
            &spec.owner,
            &operation_id,
            &crate::tentanas::elastic::tests::snapraid_result(
                &spec,
                &operation_id,
                tentanas_helper::elastic::ElasticSnapraidKind::Sync,
                tentanas_helper::elastic::ElasticSnapraidOutcome::Failed,
            ),
        )
        .expect("result");
        store::fail_orphaned_jobs(&p).expect("core restart");
        p.write().expect("write").execute("UPDATE nas_elastic_arrays SET state='active'", []).expect("restore");
        let lost = store::elastic_arrays_all(&p).expect("arrays").remove(0);
        assert_eq!(lost.snapraid_history[0].outcome, store::INTERRUPTED_OUTCOME);
        assert!(!lost.unresolved_operation, "the admission and the history agree");
        let wire = crate::tentanas::elastic::to_protocol(
            &lost,
            &std::collections::BTreeMap::new(),
            &ArrayObservation::default(),
            true,
            "13.0",
            ("active", ""),
        );
        assert_eq!(crate::tentanas::elastic::repair_evidence(&wire), None);
    }

    /// A2 of the second review: settling runs back off — one cooldown, then
    /// two, then four — and after `SETTLE_ATTEMPTS` failures in a row nothing
    /// starts on its own and an admin is told. W3: a run refused as busy is no
    /// failure and does not stop the settling.
    #[tokio::test]
    async fn settling_backs_off_and_stops_for_an_admin_after_repeated_failures() {
        let p = db();
        let spec = settled_cached_array(&p, "media");
        struct Unmeasurable;
        impl CacheObserver for Unmeasurable {
            async fn cache(&self, _: &ElasticArrayRow) -> anyhow::Result<ArrayObservation> {
                Err(anyhow::anyhow!("not measured"))
            }
            async fn age(&self, _: &ElasticArrayRow) -> anyhow::Result<ElasticCacheAge> {
                Err(anyhow::anyhow!("not measured"))
            }
        }
        let clock = MoverClock::new();
        for failed in 1..=crate::tentanas::elastic::SETTLE_ATTEMPTS {
            lost_mover_run(&p, &spec);
            let array = store::elastic_arrays_all(&p).expect("arrays").remove(0);
            assert_eq!(array.mover_failed_runs, failed);
            assert!(array.mover_settles_unresolved, "{failed}");
            let wait = super::super::elastic::MOVER_RETRIGGER_COOLDOWN * (1 << (failed - 1));
            clock.started("media");
            clock.rewind_for_test("media", wait - Duration::from_secs(60));
            assert_eq!(crate::tentanas::elastic::mover_trigger(&array, &ArrayObservation::default(), &clock), MoverTrigger::None, "{failed}: still waiting");
            clock.rewind_for_test("media", wait);
            let expected = if failed < crate::tentanas::elastic::SETTLE_ATTEMPTS { MoverTrigger::Settle } else { MoverTrigger::None };
            assert_eq!(crate::tentanas::elastic::mover_trigger(&array, &ArrayObservation::default(), &clock), expected, "{failed}");
        }
        let stopped = store::elastic_arrays_all(&p).expect("arrays").remove(0);
        run_automatic_movers(&p, &[stopped], &MoverClock::new(), &Unmeasurable).await;
        assert_eq!(mover_jobs(&p, "media"), i64::from(crate::tentanas::elastic::SETTLE_ATTEMPTS), "nothing more started");
        assert!(
            stuck_alerts(&p, "media").iter().any(|alert| alert.title.contains("wstrzymane")),
            "the admin is told"
        );
        // W3: a busy refusal after the failed run is not an operation that
        // settled anything, and it must not switch the settling off.
        let p = db();
        let spec = settled_cached_array(&p, "media");
        lost_mover_run(&p, &spec);
        let busy = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_mover".to_string(),
            subject: spec.name.clone(),
            status: "running".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &p,
            &busy,
            Some(&crate::tentanas::jobs::ElasticJobIntent::Mover {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                resume_operation_id: uuid::Uuid::now_v7().to_string(),
                rules: tentanas_helper::elastic::MoverRules::default(),
                coupled_sync: true,
            }),
        )
        .expect("busy mover");
        store::finish_job(
            &p,
            &busy.job_id,
            "failed",
            Some(&format!("elastic_mover exited with 69: {} Resource temporarily unavailable", tentanas_helper::elastic::ELASTIC_BUSY)),
        )
        .expect("finish");
        let array = store::elastic_arrays_all(&p).expect("arrays").remove(0);
        assert!(array.mover_settles_unresolved, "one lock collision does not switch the settling off");
        assert_eq!(array.mover_failed_runs, 1, "and is no failed run");
    }

    /// Round 4: while the settling runs the tick pays for no measurement, but
    /// once the settling has STOPPED the cache is measured again — the
    /// stuck-cache and conflict alerts are made of that measurement, and the
    /// array whose runs keep failing is the last one that should go quiet.
    #[tokio::test]
    async fn a_stopped_settling_does_not_stop_the_cache_measurement() {
        for failures in [1, crate::tentanas::elastic::SETTLE_ATTEMPTS] {
            let p = db();
            let spec = settled_cached_array(&p, "media");
            for _ in 0..failures {
                lost_mover_run(&p, &spec);
            }
            let arrays = store::elastic_arrays_all(&p).expect("arrays");
            assert_eq!(arrays[0].mover_failed_runs, failures);
            assert!(arrays[0].mover_settles_unresolved);
            let observer = Canned::new(90, ElasticCacheAge::default());
            run_automatic_movers(&p, &arrays, &MoverClock::new(), &observer).await;
            let (cache_reads, _) = observer.reads();
            if failures < crate::tentanas::elastic::SETTLE_ATTEMPTS {
                assert_eq!(cache_reads, 0, "the settling run needs no measurement");
                assert_eq!(mover_jobs(&p, "media"), failures as i64 + 1, "and it starts");
            } else {
                assert_eq!(cache_reads, 1, "the stopped array is measured again");
                assert_eq!(mover_jobs(&p, "media"), failures as i64, "nothing more is started");
            }
        }
    }

    /// A1 of the second review: the Elastic passes wait while a startup restore
    /// queue runs — a settling mover started before its Restore is refused by
    /// the helper and costs the whole cooldown — and run once it is done.
    #[tokio::test]
    async fn the_elastic_passes_wait_for_the_startup_restores() {
        let p = db();
        let spec = settled_cached_array(&p, "media");
        lost_mover_run(&p, &spec);
        struct Unmeasurable;
        impl CacheObserver for Unmeasurable {
            async fn cache(&self, _: &ElasticArrayRow) -> anyhow::Result<ArrayObservation> {
                Err(anyhow::anyhow!("not measured"))
            }
            async fn age(&self, _: &ElasticArrayRow) -> anyhow::Result<ElasticCacheAge> {
                Err(anyhow::anyhow!("not measured"))
            }
        }
        let clock = MoverClock::new();
        let pending = crate::tentanas::elastic::StartupRestores::begin();
        run_elastic_passes(&p, Local::now(), &clock, &Unmeasurable).await;
        assert_eq!(mover_jobs(&p, "media"), 1, "nothing started while the restores run");
        assert!(clock.cooled_down("media"), "and no cooldown was spent");
        drop(pending);
        // Another test may hold its own queue for a moment.
        for _ in 0..100 {
            if !crate::tentanas::elastic::startup_restores_pending() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        run_elastic_passes(&p, Local::now(), &clock, &Unmeasurable).await;
        assert_eq!(mover_jobs(&p, "media"), 2, "the settling run starts once they are done");
    }

    // ----- SMART self-tests ---------------------------------------------------

    /// Arms both SMART cadences at an explicit deadline. `run_due_smart_tests`
    /// reads only `next_*_at` to decide what is due, so seeding those two
    /// fields is what puts a pass in the past without waiting for a clock.
    fn arm_smart(p: &DbPool, next_short: DateTime<Local>, next_long: DateTime<Local>) {
        let mut smart = store::smart_schedule(p).expect("default schedule");
        smart.enabled = true;
        smart.short = schedule("daily", 3, 0);
        smart.long = schedule("weekly", 4, 0);
        smart.next_short_at = Some(next_short.to_rfc3339());
        smart.next_long_at = Some(next_long.to_rfc3339());
        store::set_smart_schedule(p, &smart).expect("arm");
    }

    /// The row a self-test holds while it runs — `insert_job` refuses a second
    /// one on the same subject, which is the whole serialisation.
    fn occupy_disk(p: &DbPool, disk_id: &str) {
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "smart_test".to_string(),
            subject: disk_id.to_string(),
            status: "running".to_string(),
            started_by: STARTED_BY.to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(p, &job, None).expect("occupy the disk");
    }

    fn smart_test_subjects(p: &DbPool) -> Vec<String> {
        let mut subjects: Vec<String> = store::list_jobs(p, 500)
            .expect("jobs")
            .into_iter()
            .filter(|j| j.kind == "smart_test")
            .map(|j| j.subject)
            .collect();
        subjects.sort();
        subjects
    }

    fn two_disks() -> Vec<(String, String)> {
        vec![
            ("disk-a".to_string(), "/dev/sda".to_string()),
            ("disk-b".to_string(), "/dev/sdb".to_string()),
        ]
    }

    /// A second self-test on one disk ABORTS the first, so a disk that is
    /// already under test is left alone — and the disks beside it still get
    /// theirs, because one refusal must not end the pass.
    #[tokio::test]
    async fn a_disk_already_running_a_self_test_gets_no_second_one() {
        let p = db();
        occupy_disk(&p, "disk-a");
        let now = at(2026, 9, 1, 3, 0);
        arm_smart(
            &p,
            now - chrono::Duration::minutes(1),
            now + chrono::Duration::days(3),
        );

        run_due_smart_tests(&p, now, two_disks()).await;

        assert_eq!(
            smart_test_subjects(&p),
            ["disk-a", "disk-b"],
            "disk-a keeps the ONE test it was already running and disk-b gets its first"
        );
        let after = store::smart_schedule(&p).expect("schedule");
        assert!(after.last_short_at.is_some(), "disk-b did start, so the pass ran");
        assert!(after.last_long_at.is_none(), "the long pass was not due");
    }

    /// Both cadences due in the same tick: the LONG pass takes the disks and
    /// the short pass is refused on every one of them. Reversing the two loop
    /// passes would make a daily short test cut every long test short — and
    /// the only thing that records WHICH pass got the disks is `last_*_at`,
    /// because the job row says "smart_test" for both.
    #[tokio::test]
    async fn the_long_pass_takes_the_disks_before_the_short_pass_sees_them() {
        let p = db();
        let now = at(2026, 9, 1, 4, 0);
        let due = now - chrono::Duration::minutes(1);
        arm_smart(&p, due, due);

        run_due_smart_tests(&p, now, two_disks()).await;

        assert_eq!(
            smart_test_subjects(&p),
            ["disk-a", "disk-b"],
            "one test per disk, not one per pass"
        );
        let after = store::smart_schedule(&p).expect("schedule");
        assert!(after.last_long_at.is_some(), "the long pass is the one that ran");
        assert!(
            after.last_short_at.is_none(),
            "the short pass started nothing, so it records no run"
        );
        // Both cadences still move forward: a pass whose disks were all busy
        // must not stay due and retry on every tick for the hours a long test
        // takes.
        assert_eq!(after.next_long_at, next_run_utc(&after.long, now));
        assert_eq!(after.next_short_at, next_run_utc(&after.short, now));
    }

    /// A pass that started NOTHING did not run. `last_short_at` is what the
    /// Tasks tab shows as the schedule's `last_run_at`, and there is no result
    /// column beside it that could say "skipped", so stamping it would tell
    /// the operator a test ran on every disk while none did.
    #[tokio::test]
    async fn a_pass_that_starts_nothing_records_no_run_but_still_rearms() {
        let p = db();
        occupy_disk(&p, "disk-a");
        occupy_disk(&p, "disk-b");
        let now = at(2026, 9, 1, 3, 0);
        arm_smart(
            &p,
            now - chrono::Duration::minutes(1),
            now + chrono::Duration::days(3),
        );

        run_due_smart_tests(&p, now, two_disks()).await;

        assert_eq!(
            smart_test_subjects(&p),
            ["disk-a", "disk-b"],
            "only the two tests that were already running"
        );
        let after = store::smart_schedule(&p).expect("schedule");
        assert!(
            after.last_short_at.is_none(),
            "every disk refused, so the schedule shows no run at {:?}",
            after.last_short_at
        );
        assert_eq!(
            after.next_short_at,
            next_run_utc(&after.short, now),
            "the cadence re-arms forward anyway"
        );
    }
}
