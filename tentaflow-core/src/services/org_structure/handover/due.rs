//! The handover work that is due on a day and nobody asks for: putting the work
//! of an absence back on the return day, and finishing what was scheduled for
//! the departure day (a membership that ends then). A loop on every node, like
//! the profile recompute of `nightly`: it runs once a day shortly after midnight
//! of the organization's own timezone and catches up after a stop.
//!
//! `run_due` takes the day as a parameter, so a test (or an operator repairing a
//! node) can run it for any day; nothing else in the module reads the clock.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate, Utc};
use serde_json::{json, Value as Json};

use super::project_work::{provider_for, ProjectDirectory};
use super::{record, ApplyCx, Recorded, Returned, Step};
use crate::db::DbPool;
use crate::services::org_structure::error::{OrgStructureError as E, Result};
use crate::services::org_structure::nightly;
use crate::services::org_structure::repo::timezone_of;
use crate::services::org_structure::validate;

const TICK: Duration = Duration::from_secs(300);

static STARTED: OnceLock<()> = OnceLock::new();

/// What one run did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DueReport {
    /// Items put back with the person.
    pub returned: u32,
    /// Items left with the taker: closed, changed or the person is not a member any more.
    pub kept: u32,
    /// Memberships that ended on their day.
    pub ended: u32,
    /// Items that could not be finished; they stay recorded and are tried again
    /// after a growing pause, up to `MAX_ATTEMPTS` times.
    pub failed: u32,
}

/// Tries per item before it is given up: a store that stays broken for days is
/// the operator's to repair, not something to hit every night forever.
const MAX_ATTEMPTS: u32 = 5;

/// What a failed try leaves in the item's `detail`.
const ATTEMPTS: &str = "attempts";
const RETRY_AFTER: &str = "retry_after";

/// The day an item may be tried again: it waits 1, 2, 4, 8 days after its
/// first, second, third, fourth failure.
fn retry_day(attempts: u32, today: NaiveDate) -> NaiveDate {
    today
        .checked_add_days(Days::new(1u64 << attempts.saturating_sub(1).min(6)))
        .unwrap_or(today)
}

/// True while the item waits out the pause after a failed try.
fn is_waiting(detail: &Json, today: NaiveDate) -> bool {
    detail
        .get(RETRY_AFTER)
        .and_then(Json::as_str)
        .and_then(|day| validate::parse_date(day).ok())
        .is_some_and(|day| day > today)
}

/// The outcome of one more failed try.
enum Failure {
    /// Tried again from `detail.retry_after`.
    Later(Json),
    GiveUp,
}

fn after_failure(detail: &Json, today: NaiveDate) -> Failure {
    let attempts = detail.get(ATTEMPTS).and_then(Json::as_u64).unwrap_or(0) as u32 + 1;
    if attempts >= MAX_ATTEMPTS {
        return Failure::GiveUp;
    }
    let mut next = match detail {
        Json::Object(_) => detail.clone(),
        _ => json!({}),
    };
    next[ATTEMPTS] = json!(attempts);
    next[RETRY_AFTER] = json!(validate::format_date(retry_day(attempts, today)));
    Failure::Later(next)
}

/// Starts the loop. Idempotent, like the other core periodic tasks.
pub fn start(db: DbPool) {
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut last_run: HashMap<String, NaiveDate> = HashMap::new();
        loop {
            let pool = db.clone();
            let handed = std::mem::take(&mut last_run);
            match tokio::task::spawn_blocking(move || {
                let mut last_run = handed;
                tick(&pool, Utc::now(), &mut last_run);
                last_run
            })
            .await
            {
                Ok(updated) => last_run = updated,
                Err(e) => tracing::warn!("org handover due task failed: {e}"),
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

fn tick(pool: &DbPool, now: DateTime<Utc>, last_run: &mut HashMap<String, NaiveDate>) {
    let orgs = match pool
        .read()
        .map_err(|e| E::Db(e.to_string()))
        .and_then(|conn| record::organizations(&conn))
    {
        Ok(orgs) => orgs,
        Err(e) => {
            tracing::warn!("org handover due: cannot list organizations: {e}");
            return;
        }
    };
    for org_id in orgs {
        let due = pool
            .read()
            .map_err(|e| E::Db(e.to_string()))
            .and_then(|conn| timezone_of(&conn, &org_id))
            .and_then(|zone| nightly::due_day(now, &zone, last_run.get(&org_id).copied()));
        match due {
            Ok(Some(day)) => match run_due(pool, &org_id, day) {
                Ok(report) => {
                    if report != DueReport::default() {
                        tracing::info!(org_id = %org_id, ?report, "org handovers due");
                    }
                    // Items that failed are tried again the next day, not every tick.
                    last_run.insert(org_id, day);
                }
                Err(e) => tracing::warn!(org_id = %org_id, "org handover due run failed: {e}"),
            },
            Ok(None) => {}
            Err(e) => tracing::warn!(org_id = %org_id, "org handover due: {e}"),
        }
    }
}

/// Puts back what is due on `today` and finishes what was scheduled for it.
pub fn run_due(pool: &DbPool, org_id: &str, today: NaiveDate) -> Result<DueReport> {
    let (returns, scheduled) = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        (
            record::due_returns(&conn, org_id, today)?,
            record::due_scheduled(&conn, org_id, today)?,
        )
    };
    let mut report = DueReport::default();
    if returns.is_empty() && scheduled.is_empty() {
        return Ok(report);
    }
    if !ProjectDirectory::available() {
        tracing::warn!(
            "org handover due: Project Studio is not running, {} item(s) wait",
            returns.len() + scheduled.len()
        );
        return Ok(report);
    }
    let projects = ProjectDirectory::new(org_id, pool);

    for (header, item) in returns {
        if is_waiting(&item.detail, today) {
            continue;
        }
        let Some(provider) = provider_for(item.category, &projects) else {
            continue;
        };
        let digest = super::digest(&header.note);
        let cx = ApplyCx {
            org_id,
            actor: &header.created_by,
            actor_is_admin: false,
            handover_id: &header.id,
            from_user: &header.user_id,
            reason: header.reason,
            date: today,
            today,
            note: &header.note,
            note_digest: &digest,
        };
        let recorded = Recorded {
            key: item.key.clone(),
            from_user: item.from_user_id.clone(),
            taker: item.taker_user_id.clone(),
        };
        match provider.reverse(&cx, &recorded) {
            Ok(Returned::Back) => {
                record::mark(pool, &header.id, &item.key, "returned", None, None)?;
                report.returned += 1;
            }
            Ok(Returned::Kept(reason)) => {
                record::mark(pool, &header.id, &item.key, "kept", Some(reason), None)?;
                report.kept += 1;
            }
            Err(e) => match after_failure(&item.detail, today) {
                Failure::Later(detail) => {
                    tracing::warn!(key = %item.key, "handover return failed, tried again later: {e}");
                    record::mark(pool, &header.id, &item.key, "done", None, Some(&detail))?;
                    report.failed += 1;
                }
                Failure::GiveUp => {
                    tracing::warn!(key = %item.key, "handover return failed {MAX_ATTEMPTS} times, given up: {e}");
                    record::mark(
                        pool,
                        &header.id,
                        &item.key,
                        "kept",
                        Some("return_failed"),
                        None,
                    )?;
                    report.kept += 1;
                }
            },
        }
    }

    for (header, item) in scheduled {
        if is_waiting(&item.detail, today) {
            continue;
        }
        let Some(provider) = provider_for(item.category, &projects) else {
            continue;
        };
        let digest = super::digest(&header.note);
        let cx = ApplyCx {
            org_id,
            actor: &header.created_by,
            actor_is_admin: false,
            handover_id: &header.id,
            from_user: &header.user_id,
            reason: header.reason,
            date: today,
            today,
            note: &header.note,
            note_digest: &digest,
        };
        let recorded = Recorded {
            key: item.key.clone(),
            from_user: item.from_user_id.clone(),
            taker: item.taker_user_id.clone(),
        };
        match provider.complete(&cx, &recorded) {
            Ok(Step::Done(detail)) => {
                record::mark(pool, &header.id, &item.key, "done", None, Some(&detail))?;
                report.ended += 1;
            }
            Ok(Step::Skipped(reason)) => {
                record::mark(pool, &header.id, &item.key, "skipped", Some(reason), None)?;
            }
            Ok(Step::Refused(reason)) => {
                record::mark(pool, &header.id, &item.key, "failed", Some(reason), None)?;
                report.failed += 1;
            }
            Ok(Step::Scheduled(_)) => {}
            Err(e) => match after_failure(&item.detail, today) {
                Failure::Later(detail) => {
                    tracing::warn!(key = %item.key, "scheduled handover step failed, tried again later: {e}");
                    record::mark(
                        pool,
                        &header.id,
                        &item.key,
                        "scheduled",
                        None,
                        Some(&detail),
                    )?;
                    report.failed += 1;
                }
                Failure::GiveUp => {
                    tracing::warn!(key = %item.key, "scheduled handover step failed {MAX_ATTEMPTS} times, given up: {e}");
                    record::mark(
                        pool,
                        &header.id,
                        &item.key,
                        "failed",
                        Some("completion_failed"),
                        None,
                    )?;
                    report.failed += 1;
                }
            },
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_studio::{db as ps_db, repository};
    use crate::services::org::DEFAULT_ORG_ID;

    fn day(n: u64) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 1).unwrap() + Days::new(n)
    }

    #[test]
    fn a_failed_try_waits_one_two_four_and_eight_days_and_the_fifth_gives_up() {
        let mut detail = json!({ "status": "in_progress" });
        let mut waits = Vec::new();
        for attempt in 1..MAX_ATTEMPTS {
            match after_failure(&detail, day(0)) {
                Failure::Later(next) => {
                    assert_eq!(next[ATTEMPTS], json!(attempt));
                    assert_eq!(
                        next["status"],
                        json!("in_progress"),
                        "what the step recorded stays"
                    );
                    let retry = validate::parse_date(next[RETRY_AFTER].as_str().unwrap()).unwrap();
                    waits.push((retry - day(0)).num_days());
                    assert!(is_waiting(&next, day(0)));
                    assert!(!is_waiting(&next, retry));
                    detail = next;
                }
                Failure::GiveUp => panic!("gave up after {attempt} tries"),
            }
        }
        assert_eq!(waits, [1, 2, 4, 8]);
        assert!(matches!(after_failure(&detail, day(0)), Failure::GiveUp));
        assert!(!is_waiting(&json!({}), day(0)));
    }

    #[tokio::test]
    async fn a_return_that_keeps_failing_is_tried_a_bounded_number_of_times_and_then_left_with_the_taker(
    ) {
        let root = tempfile::tempdir().expect("project storage");
        let _ = ps_db::init(&root.path().join("projects.db"));
        let state = crate::dispatch::AppState::for_test();
        let org = DEFAULT_ORG_ID;
        let mut users = Vec::new();
        for name in ["due-giver", "due-taker"] {
            let id = crate::db::repository::create_user_account(
                &state.db,
                name,
                "hash",
                name,
                &format!("{name}@example.test"),
            )
            .expect("account");
            crate::services::org::add_membership(&state.db, org, &id, "role-org-viewer", "test")
                .expect("member");
            users.push(id);
        }
        let (giver, taker) = (&users[0], &users[1]);
        // A project whose storage can never be opened: every return fails the same way.
        let project = uuid::Uuid::new_v4().to_string();
        repository::create_project(
            &project,
            org,
            &format!("Unopenable {project}"),
            "",
            "custom",
            "[\"tests\"]",
            giver,
            "/dev/null/never",
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("registered project");

        let handover = uuid::Uuid::new_v4().to_string();
        let key = format!("test:{project}:item-1");
        {
            let mut conn = state.db.write().expect("writer");
            record::create(
                &mut conn,
                &record::Header {
                    id: handover.clone(),
                    org_id: org.to_string(),
                    user_id: giver.clone(),
                    reason: super::super::Reason::Absence,
                    project_id: None,
                    effective_on: day(0),
                    return_on: Some(day(1)),
                    note: "away".into(),
                    created_by: giver.clone(),
                    created_at_ms: record::now_ms(),
                },
                &[record::Item {
                    handover_id: handover.clone(),
                    key: key.clone(),
                    category: super::super::Category::TestItem,
                    project_id: Some(project.clone()),
                    title: "Login".into(),
                    from_user_id: giver.clone(),
                    taker_user_id: Some(taker.clone()),
                    status: "pending".into(),
                    reason: None,
                    detail: json!({}),
                }],
            )
            .expect("record");
        }
        record::mark(
            &state.db,
            &handover,
            &key,
            "done",
            None,
            Some(&json!({ "status": "pending" })),
        )
        .expect("moved");

        let mut failures = 0;
        let mut kept = 0;
        for offset in 1..60 {
            let report = run_due(&state.db, org, day(offset)).expect("run");
            failures += report.failed;
            kept += report.kept;
            // Asked again the same day, it waits instead of trying again.
            assert_eq!(
                run_due(&state.db, org, day(offset)).expect("repeat"),
                DueReport::default(),
                "day {offset}"
            );
        }
        assert_eq!(failures, MAX_ATTEMPTS - 1);
        assert_eq!(kept, 1);
        let conn = state.db.read().expect("reader");
        let item = record::items(&conn, &handover).expect("items").remove(0);
        assert_eq!(
            (item.status.as_str(), item.reason.as_deref()),
            ("kept", Some("return_failed"))
        );
        std::mem::forget(root);
    }
}
