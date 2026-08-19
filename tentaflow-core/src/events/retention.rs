// ===== File: events/retention.rs — the retention sweep that ships WITH the table =====
//
// §2.9 is explicit that retention arrives together with the log and not later,
// which is why this module exists in the same change as the schema. The term is
// NOT a constant here: it is resolved from `compliance_retention_policies`
// under `RetentionScopeKind::Events`, seeded per organisation at 30 days by
// migration v129, so an admin who moves the policy moves the cutoff with no
// code change.
//
// **Why the SHORTEST term across organisations wins.** §2.3 gives `run_events`
// no `org_id` column — the timeline is a per-node artefact and the browser
// queries it across origins — so a row cannot be attributed to a tenant at
// purge time. Of the two possible readings of "per organisation", only one is
// safe: keeping a row past the shortest policy on the node would hold some
// tenant's data longer than that tenant agreed to, while purging it early costs
// diagnostics on a tool that §2.8 already classifies as losable. So the sweep
// honours the minimum. A node with one organisation — the ordinary case — sees
// exactly its own policy.
//
// Deliveries are not collateral. Only DELIVERED outbox rows are removed, and
// they are removed on their own cutoff: an audit copy still waiting to reach
// `audit_log` outlives the timeline entry it came from, which is precisely what
// its self-contained shape in `db.rs` is for.

use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::compliance::models::RetentionScopeKind;
use crate::compliance::repository::resolve_retention_policy;
use crate::db::{repository, DbPool};

/// Cadence of the sweep. Daily is ample for a day-granularity term and keeps
/// the file from growing unbounded between restarts.
const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Pages returned to the freelist per `incremental_vacuum` step. Bounded so a
/// large purge does not hold the writer for the length of a full rewrite.
const VACUUM_PAGES: i64 = 4096;

/// What one sweep removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub events_deleted: usize,
    pub outbox_deleted: usize,
    pub retention_days: i64,
}

/// Shortest active `events` retention term across every organisation on the
/// node, in days. An organisation whose policy cannot be resolved is skipped
/// with a warning rather than silently defaulting — a missing policy must not
/// quietly shorten or lengthen anybody else's.
pub fn resolve_retention_days(main_db: &DbPool) -> Result<Option<i64>> {
    let org_ids = repository::list_all_org_ids(main_db)?;
    let conn = main_db.read().map_err(|e| anyhow!("main db read: {e}"))?;
    let mut shortest: Option<i64> = None;
    for org_id in &org_ids {
        match resolve_retention_policy(&conn, org_id, RetentionScopeKind::Events, None) {
            // A non-positive term would purge live history on the spot; the
            // policy CHECK only guarantees `retention_days >= minimum_days` and
            // `minimum_days` may be 0, so clamp to one day as a guard.
            Ok(policy) => {
                let days = policy.retention_days.max(1);
                shortest = Some(shortest.map_or(days, |current: i64| current.min(days)));
            }
            Err(e) => tracing::warn!("event log retention: no policy for org '{org_id}': {e}"),
        }
    }
    Ok(shortest)
}

/// Runs one sweep: deletes events past the resolved term and outbox rows that
/// were already delivered before it, then returns the freed pages.
pub fn sweep(main_db: &DbPool, events_pool: &DbPool) -> Result<SweepReport> {
    let Some(retention_days) = resolve_retention_days(main_db)? else {
        tracing::warn!("event log retention: no organisation resolved a policy, nothing purged");
        return Ok(SweepReport::default());
    };

    let offset = format!("-{retention_days} days");
    // Both cutoffs are evaluated IN SQLite and then bound as parameters, so the
    // literal matches the stored representation exactly — epoch milliseconds
    // for `at_ms`, `datetime('now')` text for `delivered_at` — with no
    // client-side clock or formatting skew.
    let (cutoff_ms, cutoff_text) = {
        let conn = events_pool
            .read()
            .map_err(|e| anyhow!("events db read: {e}"))?;
        conn.query_row(
            "SELECT CAST(strftime('%s', 'now', ?1) AS INTEGER) * 1000, datetime('now', ?1)",
            rusqlite::params![offset],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?
    };

    let conn = events_pool
        .write()
        .map_err(|e| anyhow!("events db write: {e}"))?;
    let events_deleted = conn.execute(
        "DELETE FROM run_events WHERE at_ms < ?1",
        rusqlite::params![cutoff_ms],
    )?;
    let outbox_deleted = conn.execute(
        "DELETE FROM audit_outbox WHERE delivered_at IS NOT NULL AND delivered_at < ?1",
        rusqlite::params![cutoff_text],
    )?;
    if events_deleted > 0 || outbox_deleted > 0 {
        // `auto_vacuum = INCREMENTAL` only moves freed pages onto the freelist;
        // this is the step that actually returns them to the filesystem.
        conn.pragma_update(None, "incremental_vacuum", VACUUM_PAGES)?;
    }

    Ok(SweepReport {
        events_deleted,
        outbox_deleted,
        retention_days,
    })
}

/// Runs one sweep synchronously at startup, then daily in the background.
/// Started from `events::init` — a retention task nobody starts is the same as
/// no retention at all.
pub fn start_retention_task(main_db: DbPool, events_pool: DbPool) {
    match sweep(&main_db, &events_pool) {
        Ok(report) if report.events_deleted > 0 || report.outbox_deleted > 0 => tracing::info!(
            "event log retention: removed {} event(s) and {} outbox row(s) (term {} days)",
            report.events_deleted,
            report.outbox_deleted,
            report.retention_days
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!("event log retention sweep at startup failed: {e:#}"),
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            match sweep(&main_db, &events_pool) {
                Ok(report) if report.events_deleted > 0 || report.outbox_deleted > 0 => {
                    tracing::debug!(
                        "event log retention: removed {} event(s) and {} outbox row(s)",
                        report.events_deleted,
                        report.outbox_deleted
                    )
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("event log retention sweep failed: {e:#}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compliance::EVENTS_RETENTION_DAYS;
    use crate::events::store::{append, EventPayload, RunEvent};
    use crate::events::test_support::{events_db, main_db};
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};
    use crate::services::org::DEFAULT_ORG_ID;

    fn at(pool: &DbPool, run_id: &str, at_ms: i64) {
        append(
            pool,
            RunEvent::new(
                run_id,
                at_ms,
                FlowOrigin::Chat,
                &FlowActor::user("u-1"),
                EventPayload::StepStart {
                    step: "n1".to_string(),
                },
            ),
        )
        .unwrap();
    }

    fn ms_ago(pool: &DbPool, days: i64) -> i64 {
        let conn = pool.read().unwrap();
        conn.query_row(
            "SELECT CAST(strftime('%s', 'now', ?1) AS INTEGER) * 1000",
            rusqlite::params![format!("-{days} days")],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn set_policy_days(main: &DbPool, days: i64) {
        let conn = main.write().unwrap();
        let changed = conn
            .execute(
                "UPDATE compliance_retention_policies SET retention_days = ?1 \
                 WHERE scope_kind = 'events'",
                rusqlite::params![days],
            )
            .unwrap();
        assert!(changed > 0, "no seeded 'events' policy to move");
    }

    fn remaining(pool: &DbPool) -> Vec<String> {
        let conn = pool.read().unwrap();
        let mut stmt = conn
            .prepare("SELECT run_id FROM run_events ORDER BY run_id")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    /// §2.9 — the term comes from the compliance policy. The assertion is on
    /// the RESOLVED value, and the sweep is then run against a policy that was
    /// MOVED: a hardcoded 30 would pass the first half and fail the second.
    #[test]
    fn the_cutoff_follows_the_compliance_policy_and_not_a_constant() {
        let (_dir, pool) = events_db();
        let main = main_db();
        assert_eq!(
            resolve_retention_days(&main).unwrap(),
            Some(EVENTS_RETENTION_DAYS),
            "the seeded default is not what the sweeper resolves"
        );

        at(&pool, "old", ms_ago(&pool, 40));
        at(&pool, "recent", ms_ago(&pool, 10));
        at(&pool, "fresh", ms_ago(&pool, 1));

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.retention_days, EVENTS_RETENTION_DAYS);
        assert_eq!(report.events_deleted, 1);
        assert_eq!(remaining(&pool), vec!["fresh", "recent"]);

        // Move the policy and the cutoff moves with it, with no code change.
        set_policy_days(&main, 5);
        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.retention_days, 5);
        assert_eq!(report.events_deleted, 1);
        assert_eq!(remaining(&pool), vec!["fresh"]);
    }

    /// §2.3 gives `run_events` no `org_id`, so a row cannot be attributed at
    /// purge time. Of the two readings of "per organisation" only the shortest
    /// term is safe — keeping a row past the strictest policy on the node would
    /// hold some tenant's data longer than that tenant agreed to.
    #[test]
    fn the_shortest_organisation_term_governs_the_whole_file() {
        let main = main_db();
        {
            let conn = main.write().unwrap();
            conn.execute(
                "INSERT INTO organizations (org_id, name, slug, status, created_at) \
                 VALUES ('org-strict', 'Strict', 'strict', 'active', datetime('now'))",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO compliance_retention_policies \
                  (retention_policy_id, org_id, slug, name_translations, scope_kind, \
                   retention_days, minimum_days, action_after_retention, is_default, is_active) \
                 VALUES ('ret-strict', 'org-strict', 'events_default', \
                  '{\"pl\":\"Zdarzenia\",\"en\":\"Events\"}', 'events', 3, 0, 'delete', 1, 1)",
                [],
            )
            .unwrap();
        }
        assert_eq!(resolve_retention_days(&main).unwrap(), Some(3));

        // And the default org's own, longer, term is what governs when it is
        // alone — the ordinary single-tenant node sees exactly its own policy.
        {
            let conn = main.write().unwrap();
            conn.execute(
                "DELETE FROM compliance_retention_policies WHERE org_id = 'org-strict'",
                [],
            )
            .unwrap();
            conn.execute(
                "DELETE FROM organizations WHERE org_id = 'org-strict'",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            resolve_retention_days(&main).unwrap(),
            Some(EVENTS_RETENTION_DAYS)
        );
        assert!(!DEFAULT_ORG_ID.is_empty());
    }

    /// A non-positive term must not wipe live history: the policy CHECK only
    /// guarantees `retention_days >= minimum_days`, and `minimum_days` may be 0.
    #[test]
    fn a_zero_day_policy_is_clamped_instead_of_purging_everything() {
        let (_dir, pool) = events_db();
        let main = main_db();
        set_policy_days(&main, 0);
        at(&pool, "fresh", ms_ago(&pool, 0));

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.retention_days, 1);
        assert_eq!(remaining(&pool), vec!["fresh"]);
    }

    /// An audit copy that has not reached `audit_log` yet must outlive the
    /// timeline entry it came from — the outbox row is self-contained exactly
    /// so a diagnostic-grade retention term cannot destroy a compliance-grade
    /// record.
    #[test]
    fn retention_never_removes_an_undelivered_audit_copy() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let old = ms_ago(&pool, 90);
        append(
            &pool,
            RunEvent::new(
                "r-old",
                old,
                FlowOrigin::Api,
                &FlowActor::api_key("key-1", None),
                EventPayload::RequestStarted {
                    model: Some("qwen3".into()),
                    flow_id: None,
                    service_type: Some("llm".into()),
                    modality: Some("text".into()),
                },
            ),
        )
        .unwrap();

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.events_deleted, 1, "the stale event should be gone");
        assert_eq!(report.outbox_deleted, 0);

        let conn = pool.read().unwrap();
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_outbox WHERE delivered_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1, "retention destroyed an audit obligation");
    }

    /// A DELIVERED copy is ordinary data and is swept on the same term, or the
    /// outbox would grow for the life of the node.
    #[test]
    fn a_delivered_audit_copy_is_swept_on_its_own_cutoff() {
        let (_dir, pool) = events_db();
        let main = main_db();
        at(&pool, "r-1", ms_ago(&pool, 1));
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO audit_outbox (run_id, seq, payload_json, created_at, delivered_at) \
                 VALUES ('r-1', 1, '{}', datetime('now','-90 days'), datetime('now','-90 days'))",
                [],
            )
            .unwrap();
        }

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.outbox_deleted, 1);
        assert_eq!(remaining(&pool), vec!["r-1"], "a fresh event was purged");
    }
}
