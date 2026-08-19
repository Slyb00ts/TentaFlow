// ===== File: events/retention.rs — the retention sweep that ships WITH the table =====
//
// §2.9 is explicit that retention arrives together with the log and not later,
// which is why this module exists in the same change as the schema. The term is
// NOT a constant here: it is resolved from `compliance_retention_policies`
// under `RetentionScopeKind::Events`, seeded per organisation at 30 days by
// migration v129, so an admin who moves the policy moves the cutoff with no
// code change.
//
// **Every row is purged on ITS OWN organisation's term.** A retention policy is
// a promise a tenant made about its own data, so it may only be applied to that
// tenant's rows; `run_events.org_id` is what makes that possible, and the sweep
// issues one bounded `DELETE` per organisation over the `(org_id, at_ms)` index
// rather than one cutoff for the whole file. A node with one organisation — the
// ordinary case — sees exactly its own policy, as before.
//
// **Rows no organisation claims are purged on the SHORTEST term on the node.**
// A row falls into this class when no policy can speak for it: `org_id IS NULL`
// (a camera, scheduler or maintenance run genuinely started by no tenant), an
// organisation that has since been deleted, or one whose policy does not
// resolve. Guessing an owner would fabricate a fact (invariant 6), and leaving
// the class alone would make it immortal — the file would grow for the life of
// the node and a row whose attribution is merely MISSING would outlive every
// term on it. Of the terms actually present, the shortest is the only safe one:
// keeping such a row longer would hold data past somebody's policy, while
// dropping it early costs diagnostics on a tool §2.8 already classifies as
// losable.
//
// Deliveries are not collateral. Only DELIVERED outbox rows are removed, and
// they are removed on the unattributed cutoff — the outbox carries no tenant
// and a delivered row is a spent receipt whose durable copy already lives in
// `audit_log` under compliance retention. An audit copy still waiting to reach
// `audit_log` outlives the timeline entry it came from, which is precisely what
// its self-contained shape in `db.rs` is for.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use rusqlite::Connection;

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
    /// How many organisations the sweep applied an own term for.
    pub organisations_swept: usize,
    /// Term applied to rows no organisation claims, and to the delivered
    /// outbox, in days.
    pub unattributed_retention_days: i64,
}

/// The terms one sweep runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionTerms {
    /// Active `events` term per organisation, in days. An organisation whose
    /// policy cannot be resolved is absent — its rows are not swept on somebody
    /// else's promise, they fall to `unattributed` like any other row nothing
    /// can speak for.
    pub per_org: BTreeMap<String, i64>,
    /// Term for rows no entry of `per_org` claims: the shortest term present.
    pub unattributed: i64,
}

/// Resolves the `events` term of every organisation on the node. `None` when no
/// organisation resolved a policy at all — with nothing to compare against, an
/// unattributed cutoff would be invented rather than derived, so the sweep
/// stands down instead.
pub fn resolve_retention_terms(main_db: &DbPool) -> Result<Option<RetentionTerms>> {
    let org_ids = repository::list_all_org_ids(main_db)?;
    let conn = main_db.read().map_err(|e| anyhow!("main db read: {e}"))?;
    let mut per_org = BTreeMap::new();
    for org_id in &org_ids {
        match resolve_retention_policy(&conn, org_id, RetentionScopeKind::Events, None) {
            // A non-positive term would purge live history on the spot; the
            // policy CHECK only guarantees `retention_days >= minimum_days` and
            // `minimum_days` may be 0, so clamp to one day as a guard.
            Ok(policy) => {
                per_org.insert(org_id.clone(), policy.retention_days.max(1));
            }
            Err(e) => tracing::warn!("event log retention: no policy for org '{org_id}': {e}"),
        }
    }
    let Some(unattributed) = per_org.values().copied().min() else {
        return Ok(None);
    };
    Ok(Some(RetentionTerms {
        per_org,
        unattributed,
    }))
}

/// Cutoffs for one term, evaluated IN SQLite and then bound as parameters, so
/// the literal matches the stored representation exactly — epoch milliseconds
/// for `at_ms`, `datetime('now')` text for `delivered_at` — with no client-side
/// clock or formatting skew.
fn cutoffs(conn: &Connection, retention_days: i64) -> Result<(i64, String)> {
    let offset = format!("-{retention_days} days");
    Ok(conn.query_row(
        "SELECT CAST(strftime('%s', 'now', ?1) AS INTEGER) * 1000, datetime('now', ?1)",
        rusqlite::params![offset],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    )?)
}

/// Runs one sweep: deletes each organisation's events past that organisation's
/// term, then the rows no organisation claims, then outbox rows that were
/// already delivered before the unattributed cutoff, and returns the freed
/// pages.
pub fn sweep(main_db: &DbPool, events_pool: &DbPool) -> Result<SweepReport> {
    let Some(terms) = resolve_retention_terms(main_db)? else {
        tracing::warn!("event log retention: no organisation resolved a policy, nothing purged");
        return Ok(SweepReport::default());
    };

    // ONE guard for the whole sweep: this pool has a single connection, so a
    // read guard IS the writer mutex and taking a second one would deadlock.
    let conn = events_pool
        .write()
        .map_err(|e| anyhow!("events db write: {e}"))?;

    let mut events_deleted = 0usize;
    for (org_id, days) in &terms.per_org {
        let (cutoff_ms, _) = cutoffs(&conn, *days)?;
        events_deleted += conn.execute(
            "DELETE FROM run_events WHERE org_id = ?1 AND at_ms < ?2",
            rusqlite::params![org_id, cutoff_ms],
        )?;
    }

    let (unattributed_ms, unattributed_text) = cutoffs(&conn, terms.unattributed)?;
    // Everything the loop above could not speak for: no tenant on the row, or a
    // tenant with no resolvable policy on this node.
    // `per_org` is non-empty whenever `resolve_retention_terms` returned Some —
    // `unattributed` is the minimum over its values — so the exclusion list is
    // always built.
    let placeholders = (2..2 + terms.per_org.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "DELETE FROM run_events WHERE at_ms < ?1 \
         AND (org_id IS NULL OR org_id NOT IN ({placeholders}))"
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&unattributed_ms];
    for org_id in terms.per_org.keys() {
        params.push(org_id);
    }
    events_deleted += conn.execute(&sql, params.as_slice())?;

    let outbox_deleted = conn.execute(
        "DELETE FROM audit_outbox WHERE delivered_at IS NOT NULL AND delivered_at < ?1",
        rusqlite::params![unattributed_text],
    )?;
    if events_deleted > 0 || outbox_deleted > 0 {
        // `auto_vacuum = INCREMENTAL` only moves freed pages onto the freelist;
        // this is the step that actually returns them to the filesystem.
        conn.pragma_update(None, "incremental_vacuum", VACUUM_PAGES)?;
    }

    Ok(SweepReport {
        events_deleted,
        outbox_deleted,
        organisations_swept: terms.per_org.len(),
        unattributed_retention_days: terms.unattributed,
    })
}

/// Runs one sweep synchronously at startup, then daily in the background.
/// Started from `events::init` — a retention task nobody starts is the same as
/// no retention at all.
pub fn start_retention_task(main_db: DbPool, events_pool: DbPool) {
    match sweep(&main_db, &events_pool) {
        Ok(report) if report.events_deleted > 0 || report.outbox_deleted > 0 => tracing::info!(
            "event log retention: removed {} event(s) and {} outbox row(s) across {} \
             organisation(s); rows with no organisation kept {} day(s)",
            report.events_deleted,
            report.outbox_deleted,
            report.organisations_swept,
            report.unattributed_retention_days
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

    /// Writes one event of `org` (`None` = a run started by no tenant) at
    /// `at_ms`.
    fn at(pool: &DbPool, run_id: &str, at_ms: i64, org: Option<&str>) {
        let mut event = RunEvent::new(
            run_id,
            at_ms,
            FlowOrigin::Chat,
            &FlowActor::user("u-1"),
            EventPayload::StepStart {
                step: "n1".to_string(),
            },
        );
        event.org_id = org.map(str::to_string);
        append(pool, event).unwrap();
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

    fn set_policy_days(main: &DbPool, org_id: &str, days: i64) {
        let conn = main.write().unwrap();
        let changed = conn
            .execute(
                "UPDATE compliance_retention_policies SET retention_days = ?1 \
                 WHERE scope_kind = 'events' AND org_id = ?2",
                rusqlite::params![days, org_id],
            )
            .unwrap();
        assert!(changed > 0, "no seeded 'events' policy for {org_id} to move");
    }

    /// A second organisation on the node with an `events` term of its own.
    fn add_org(main: &DbPool, org_id: &str, days: i64) {
        let conn = main.write().unwrap();
        conn.execute(
            "INSERT INTO organizations (org_id, name, slug, status, created_at) \
             VALUES (?1, ?1, ?1, 'active', datetime('now'))",
            rusqlite::params![org_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO compliance_retention_policies \
              (retention_policy_id, org_id, slug, name_translations, scope_kind, \
               retention_days, minimum_days, action_after_retention, is_default, is_active) \
             VALUES (?1, ?2, 'events_default', \
              '{\"pl\":\"Zdarzenia\",\"en\":\"Events\"}', 'events', ?3, 0, 'delete', 1, 1)",
            rusqlite::params![format!("ret-{org_id}"), org_id, days],
        )
        .unwrap();
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
        let terms = resolve_retention_terms(&main).unwrap().unwrap();
        assert_eq!(
            terms.per_org.get(DEFAULT_ORG_ID).copied(),
            Some(EVENTS_RETENTION_DAYS),
            "the seeded default is not what the sweeper resolves"
        );

        at(&pool, "old", ms_ago(&pool, 40), Some(DEFAULT_ORG_ID));
        at(&pool, "recent", ms_ago(&pool, 10), Some(DEFAULT_ORG_ID));
        at(&pool, "fresh", ms_ago(&pool, 1), Some(DEFAULT_ORG_ID));

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.unattributed_retention_days, EVENTS_RETENTION_DAYS);
        assert_eq!(report.events_deleted, 1);
        assert_eq!(remaining(&pool), vec!["fresh", "recent"]);

        // Move the policy and the cutoff moves with it, with no code change.
        set_policy_days(&main, DEFAULT_ORG_ID, 5);
        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.unattributed_retention_days, 5);
        assert_eq!(report.events_deleted, 1);
        assert_eq!(remaining(&pool), vec!["fresh"]);
    }

    /// THE test for per-tenant retention: a policy is a promise one tenant made
    /// about its own data, so it may only govern that tenant's rows.
    ///
    /// Both wrong readings are caught. `lax-stale` is older than the strictest
    /// term on the node but well inside its own, so a shortest-term-wins sweep
    /// deletes it and this test fails. `strict-stale` is inside the most
    /// generous term but past its own, so a longest-term-wins sweep keeps it
    /// and this test fails too.
    #[test]
    fn each_organisation_is_purged_on_its_own_term() {
        let (_dir, pool) = events_db();
        let main = main_db();
        add_org(&main, "org-strict", 3);
        add_org(&main, "org-lax", 60);

        let terms = resolve_retention_terms(&main).unwrap().unwrap();
        assert_eq!(terms.per_org.get("org-strict").copied(), Some(3));
        assert_eq!(terms.per_org.get("org-lax").copied(), Some(60));
        assert_eq!(terms.unattributed, 3, "the shortest term on the node");

        at(&pool, "strict-stale", ms_ago(&pool, 10), Some("org-strict"));
        at(&pool, "strict-fresh", ms_ago(&pool, 1), Some("org-strict"));
        at(&pool, "lax-stale", ms_ago(&pool, 10), Some("org-lax"));
        at(&pool, "lax-ancient", ms_ago(&pool, 90), Some("org-lax"));
        at(&pool, "default-mid", ms_ago(&pool, 10), Some(DEFAULT_ORG_ID));

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.organisations_swept, 3);
        assert_eq!(report.events_deleted, 2);
        assert_eq!(
            remaining(&pool),
            vec!["default-mid", "lax-stale", "strict-fresh"],
            "a row was purged on another tenant's term"
        );
    }

    /// Rows nothing can speak for — `org_id IS NULL` (camera, scheduler,
    /// maintenance) and a tenant that is not on this node — are purged on the
    /// SHORTEST term present. Never immortal, never folded into the default
    /// organisation: that would invent an owner (invariant 6).
    #[test]
    fn rows_no_organisation_claims_are_purged_on_the_shortest_term() {
        let (_dir, pool) = events_db();
        let main = main_db();
        add_org(&main, "org-strict", 3);

        at(&pool, "system-stale", ms_ago(&pool, 10), None);
        at(&pool, "system-fresh", ms_ago(&pool, 1), None);
        // An organisation that has since been deleted from the node: the row
        // still names a tenant, but no policy can be resolved for it.
        at(&pool, "ghost-stale", ms_ago(&pool, 10), Some("org-gone"));
        // Same age, but this tenant IS on the node and keeps 30 days — proof
        // the unattributed cutoff is not simply applied to everything.
        at(&pool, "default-mid", ms_ago(&pool, 10), Some(DEFAULT_ORG_ID));

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.events_deleted, 2);
        assert_eq!(
            remaining(&pool),
            vec!["default-mid", "system-fresh"],
            "the unattributed rule hit the wrong rows"
        );
    }

    /// A non-positive term must not wipe live history: the policy CHECK only
    /// guarantees `retention_days >= minimum_days`, and `minimum_days` may be 0.
    #[test]
    fn a_zero_day_policy_is_clamped_instead_of_purging_everything() {
        let (_dir, pool) = events_db();
        let main = main_db();
        set_policy_days(&main, DEFAULT_ORG_ID, 0);
        at(&pool, "fresh", ms_ago(&pool, 0), Some(DEFAULT_ORG_ID));
        at(&pool, "fresh-system", ms_ago(&pool, 0), None);

        let report = sweep(&main, &pool).unwrap();
        assert_eq!(report.unattributed_retention_days, 1);
        assert_eq!(remaining(&pool), vec!["fresh", "fresh-system"]);
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
            )
            .with_org(DEFAULT_ORG_ID),
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

    /// A DELIVERED copy is ordinary data and is swept on the unattributed
    /// cutoff — the outbox carries no tenant of its own, and a delivered row is
    /// a spent receipt whose durable copy already sits in `audit_log`.
    /// Otherwise the outbox would grow for the life of the node.
    #[test]
    fn a_delivered_audit_copy_is_swept_on_its_own_cutoff() {
        let (_dir, pool) = events_db();
        let main = main_db();
        at(&pool, "r-1", ms_ago(&pool, 1), Some(DEFAULT_ORG_ID));
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
