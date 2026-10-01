//! Nightly recompute of the profiles projection.
//!
//! A dated change ("from 1 March") takes effect without anyone writing at that
//! moment, so once a day, shortly after midnight in the organization's own
//! timezone, every organization's profiles are recomputed. Every node runs it:
//! the recompute is idempotent and writes only what differs, so nodes that
//! already converged through replication write nothing and there is no leader
//! to elect or lose.
//!
//! It is also the repair path for the projection itself: profile rows replicate
//! as independent operations, so a lost or reordered one leaves a node with a
//! stale row until the next recompute rewrites it from the structure, which
//! converges on every node.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};

use super::error::{OrgStructureError as E, Result};
use super::projection;
use super::repo::timezone_of;
use crate::db::DbPool;

const TICK: Duration = Duration::from_secs(60);
/// A few minutes past midnight, so the recompute never races the clock edge.
const RUN_AT: NaiveTime = NaiveTime::from_hms_opt(0, 5, 0).unwrap();

static STARTED: OnceLock<()> = OnceLock::new();

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
                run_due(&pool, Utc::now(), &mut last_run);
                last_run
            })
            .await
            {
                Ok(updated) => last_run = updated,
                Err(e) => tracing::warn!("org structure nightly recompute task failed: {e}"),
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// The organization's local day when its recompute is due, else `None`: it is
/// past `RUN_AT` there and that day has not been recomputed yet. A node that
/// was down at 00:05 catches up on its first tick after start.
pub(super) fn due_day(
    now: DateTime<Utc>,
    timezone: &str,
    last_run: Option<NaiveDate>,
) -> Result<Option<NaiveDate>> {
    let zone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| E::InvalidTimezone(timezone.to_string()))?;
    let local = now.with_timezone(&zone);
    let day = local.date_naive();
    if local.time() < RUN_AT || last_run == Some(day) {
        return Ok(None);
    }
    Ok(Some(day))
}

/// One tick: recomputes every organization that is due. A failure is logged and
/// leaves the day unmarked, so the next tick tries again.
pub(super) fn run_due(
    pool: &DbPool,
    now: DateTime<Utc>,
    last_run: &mut HashMap<String, NaiveDate>,
) {
    let orgs = match organizations_with_structure(pool) {
        Ok(orgs) => orgs,
        Err(e) => {
            tracing::warn!("org structure nightly recompute: cannot list organizations: {e}");
            return;
        }
    };
    for org_id in orgs {
        let outcome =
            due_for(pool, &org_id, now, last_run.get(&org_id).copied()).and_then(|due| match due {
                Some(day) => projection::recompute_profiles(pool, &org_id, None, day).map(|r| {
                    tracing::info!(
                        org_id = %org_id,
                        written = r.written,
                        removed = r.removed,
                        "org structure profiles recomputed"
                    );
                    Some(day)
                }),
                None => Ok(None),
            });
        match outcome {
            Ok(Some(day)) => {
                last_run.insert(org_id, day);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(org_id = %org_id, "org structure nightly recompute failed: {e}")
            }
        }
    }
}

fn due_for(
    pool: &DbPool,
    org_id: &str,
    now: DateTime<Utc>,
    last_run: Option<NaiveDate>,
) -> Result<Option<NaiveDate>> {
    let zone = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        timezone_of(&conn, org_id)?
    };
    due_day(now, &zone, last_run)
}

fn organizations_with_structure(pool: &DbPool) -> Result<Vec<String>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let mut stmt = conn.prepare("SELECT DISTINCT org_id FROM org_units ORDER BY org_id")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}
