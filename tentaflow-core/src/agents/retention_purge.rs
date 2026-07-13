// ===== File: agents/retention_purge.rs — periodic retention purge for agent
// runtime state (Harness §3.3/§3.6). Redacts the PII columns of terminal
// `agent_runs` (prompt/result/run_log) and deletes the `agent_mailbox` entries
// of those runs once they pass the org's `agent_runs` retention term, leaving
// the statistical run row for audit. The term is resolved from
// `compliance_retention_policies` (default 30 days, seeded per org), so an admin
// who lengthens/shortens the policy moves the cutoff with no code change.
//
// This is the deferred phase-6/7 follow-up: agent_runs introduced a governed PII
// store (CRM/memory tool results land in run_log) that, without a purge, would
// grow unbounded outside the rest of the compliance retention machinery. The
// task runs once at startup and then daily, mirroring the oauth-cleanup pattern.

use std::time::Duration;

use crate::compliance::models::RetentionScopeKind;
use crate::compliance::repository::resolve_retention_policy;
use crate::db::{repository, DbPool};
use crate::services::org::DEFAULT_ORG_ID;

/// Cadence of the purge sweep. Daily is ample for a day-granularity retention
/// term and keeps the table from growing unbounded between restarts.
const PURGE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Runs one purge sweep across every org: resolves each org's `agent_runs`
/// retention term, redacts that org's terminal runs finished before the cutoff
/// and deletes their mailbox entries. Runs with `org_id IS NULL` are governed by
/// the default org's policy. Returns (runs_redacted, mailbox_deleted) summed
/// over all orgs. An org whose policy cannot be resolved is skipped (logged),
/// never aborting the sweep for the others.
pub fn purge_expired_agent_runtime(pool: &DbPool) -> anyhow::Result<(usize, usize)> {
    let org_ids = repository::list_all_org_ids(pool)?;
    let mut total_runs = 0usize;
    let mut total_mailbox = 0usize;

    for org_id in &org_ids {
        let is_default = org_id == DEFAULT_ORG_ID;
        let retention_days = {
            let conn = pool
                .read()
                .map_err(|_| anyhow::anyhow!("db pool poisoned"))?;
            match resolve_retention_policy(&conn, org_id, RetentionScopeKind::AgentRuns, None) {
                Ok(policy) => policy.retention_days,
                Err(e) => {
                    tracing::warn!("agent-runs purge: no retention policy for org '{org_id}': {e}");
                    continue;
                }
            }
        };
        // A non-positive term would purge everything immediately, including live
        // history — clamp to at least one day as a guard (the policy CHECK keeps
        // retention_days >= minimum_days, but minimum_days may be 0).
        let days = retention_days.max(1);
        // SQLite resolves the cutoff so the comparison matches the stored
        // `datetime('now')` format exactly (no client-side clock skew).
        let cutoff = format!("datetime('now','-{days} days')");
        let cutoff_sql = sqlite_cutoff(pool, &cutoff)?;

        let (runs, mailbox) =
            repository::purge_agent_runtime_before(pool, org_id, is_default, &cutoff_sql)?;
        total_runs += runs;
        total_mailbox += mailbox;
    }

    Ok((total_runs, total_mailbox))
}

/// Evaluates a `datetime('now','-N days')` expression in SQLite into a concrete
/// timestamp string, so the purge passes a literal cutoff (the repository binds
/// it as a parameter, not as inlined SQL). Keeps the cutoff format identical to
/// the stored `finished_at`/`created_at` values.
fn sqlite_cutoff(pool: &DbPool, expr: &str) -> anyhow::Result<String> {
    let conn = pool
        .read()
        .map_err(|_| anyhow::anyhow!("db pool poisoned"))?;
    let value: String = conn.query_row(&format!("SELECT {expr}"), [], |row| row.get(0))?;
    Ok(value)
}

/// Runs a sweep once synchronously, then spawns a daily background task. The
/// task holds a clone of the pool and stops only when the process exits
/// (mirrors `oauth_cleanup::start_oauth_cleanup_task`).
pub fn start_agent_runtime_purge_task(pool: DbPool) {
    match purge_expired_agent_runtime(&pool) {
        Ok((runs, mailbox)) if runs > 0 || mailbox > 0 => tracing::info!(
            "agent-runs purge: redacted {runs} run(s), deleted {mailbox} mailbox entry(ies) at startup"
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!("agent-runs purge at startup failed: {e}"),
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PURGE_INTERVAL).await;
            match purge_expired_agent_runtime(&pool) {
                Ok((runs, mailbox)) if runs > 0 || mailbox > 0 => tracing::debug!(
                    "agent-runs purge: redacted {runs} run(s), deleted {mailbox} mailbox entry(ies)"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!("agent-runs purge failed: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::{AgentParams, AgentRunStatusUpdate, NewAgentMailboxEntry, NewAgentRun};
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn seed_agent(pool: &DbPool, id: &str) {
        repository::upsert_agent(
            pool,
            &AgentParams {
                id,
                name: "worker",
                display_name: None,
                description: "test agent",
                system_prompt: None,
                model: None,
                tools_json: "[]",
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 5,
                timeout_secs: 600,
                max_subagents: 0,
                max_spawn_depth: 1,
                flow_id: None,
                routable: true,
                is_enabled: true,
                on_child_complete: "notify",
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    /// Inserts a completed run finished `days_ago` days back (NULL org → default
    /// org policy governs it) with a non-empty prompt/result/run_log, and a
    /// mailbox entry referencing it.
    fn seed_finished_run(pool: &DbPool, run_id: &str, days_ago: i64) {
        repository::create_agent_run(
            pool,
            &NewAgentRun {
                id: run_id,
                agent_id: "a1",
                parent_run_id: None,
                flow_execution_id: None,
                user_id: Some("u1"),
                org_id: None,
                prompt: "secret prompt",
            },
        )
        .expect("create run");
        repository::update_agent_run_status(
            pool,
            run_id,
            &AgentRunStatusUpdate {
                status: "completed",
                result: Some("secret result"),
                exit_reason: Some("final_response"),
                set_finished: true,
                ..Default::default()
            },
        )
        .expect("complete run");
        repository::append_agent_run_log(pool, run_id, r#"{"kind":"tool","pii":"x"}"#)
            .expect("log");
        // Backdate finished_at to land before/after the cutoff deterministically.
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "UPDATE agent_runs SET finished_at = datetime('now', ?2), \
                 created_at = datetime('now', ?2) WHERE id = ?1",
                rusqlite::params![run_id, format!("-{days_ago} days")],
            )
            .unwrap();
        }
        let mailbox_id = format!("mb-{run_id}");
        repository::enqueue_mailbox(
            pool,
            &NewAgentMailboxEntry {
                id: &mailbox_id,
                run_id,
                target_session_id: Some("sess-1"),
                target_agent_id: Some("a1"),
                payload: "child result",
            },
        )
        .expect("enqueue mailbox");
        // Backdate the mailbox row to match the run so the purge picks it up.
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "UPDATE agent_mailbox SET created_at = datetime('now', ?2) WHERE id = ?1",
                rusqlite::params![mailbox_id, format!("-{days_ago} days")],
            )
            .unwrap();
        }
    }

    #[test]
    fn purge_redacts_expired_runs_and_deletes_their_mailbox() {
        let pool = db();
        seed_agent(&pool, "a1");
        // The seeded default policy is 30 days. One run is 40 days old (expired),
        // one is 5 days old (retained).
        seed_finished_run(&pool, "old-run", 40);
        seed_finished_run(&pool, "fresh-run", 5);

        let (runs, mailbox) = purge_expired_agent_runtime(&pool).expect("purge");
        assert_eq!(runs, 1, "exactly the expired run is redacted");
        assert_eq!(mailbox, 1, "exactly the expired run's mailbox is deleted");

        // Expired run: PII columns cleared, statistical row intact.
        let old = repository::get_agent_run(&pool, "old-run")
            .expect("get")
            .expect("row");
        assert_eq!(old.prompt, "");
        assert!(old.result.is_none());
        assert!(old.run_log.is_none());
        assert_eq!(old.status, "completed", "the statistical row survives");
        assert!(
            repository::list_undelivered_mailbox_for_session(&pool, "sess-1")
                .expect("mailbox")
                .iter()
                .all(|e| e.run_id != "old-run"),
            "expired run's mailbox entry is gone"
        );

        // Fresh run: untouched.
        let fresh = repository::get_agent_run(&pool, "fresh-run")
            .expect("get")
            .expect("row");
        assert_eq!(fresh.prompt, "secret prompt");
        assert_eq!(fresh.result.as_deref(), Some("secret result"));
        assert!(fresh.run_log.is_some());
        assert!(
            repository::list_undelivered_mailbox_for_session(&pool, "sess-1")
                .expect("mailbox")
                .iter()
                .any(|e| e.run_id == "fresh-run"),
            "fresh run's mailbox entry is retained"
        );
    }

    #[test]
    fn purge_is_idempotent_on_already_redacted_runs() {
        let pool = db();
        seed_agent(&pool, "a1");
        seed_finished_run(&pool, "old-run", 40);

        let (runs1, mailbox1) = purge_expired_agent_runtime(&pool).expect("purge 1");
        assert_eq!((runs1, mailbox1), (1, 1));
        // A second sweep finds nothing new (the row is already redacted and the
        // mailbox entry already deleted).
        let (runs2, mailbox2) = purge_expired_agent_runtime(&pool).expect("purge 2");
        assert_eq!((runs2, mailbox2), (0, 0));
    }
}
