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

use chrono::{DateTime, NaiveDate, Utc};

use super::project_work::{provider_for, ProjectDirectory};
use super::{record, ApplyCx, Recorded, Returned, Step};
use crate::db::DbPool;
use crate::services::org_structure::error::{OrgStructureError as E, Result};
use crate::services::org_structure::nightly;
use crate::services::org_structure::repo::timezone_of;

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
    /// Items that could not be finished; they stay recorded and are tried again.
    pub failed: u32,
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
    let projects = ProjectDirectory::new(org_id);

    for (header, item) in returns {
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
            Err(e) => {
                tracing::warn!(key = %item.key, "handover return failed, tried again next run: {e}");
                report.failed += 1;
            }
        }
    }

    for (header, item) in scheduled {
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
            Err(e) => {
                tracing::warn!(key = %item.key, "scheduled handover step failed, tried again next run: {e}");
                report.failed += 1;
            }
        }
    }
    Ok(report)
}
