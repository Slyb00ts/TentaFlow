// ===== File: code_studio/audit_outbox.rs — delivering the audit mirror to the main database =====
//
// A security-relevant event lives in the workspace's runtime database, but the
// organisation's audit trail lives in the main one. Writing both directly would
// lose the trail whenever the process died between the two writes, so the event
// and its ALREADY REDACTED audit copy are committed together into `audit_outbox`
// (see `events::append_in_tx`) and this module moves the copy across afterwards
// (§13.4).
//
// The delivery is AT LEAST ONCE, deliberately. The audit row is written to the
// main database first and the outbox row is marked delivered second; a crash
// between them replays one row, which duplicates an audit entry. The opposite
// order would LOSE one, and a duplicated audit entry is a nuisance while a
// missing one is a compliance failure.
//
// A row that cannot be delivered is not dropped and not retried in a tight
// loop: `attempts`, `last_error` and `next_attempt_at` give it exponential
// backoff that survives a restart, and the depth of the queue is a metric
// (§22 — "zaległość `audit_outbox`", SLO: drained in under 60 s).

use std::time::Duration;

use anyhow::{anyhow, Result};
use tracing::{info, warn};

use super::events::{self, AuditEnvelope, EventPayload};
use super::repository;
use crate::audit::chain::{compute_chain_for_insert, AuditRowHashInput};
use crate::db::DbPool;

/// How often the loop looks for undelivered rows.
const DELIVERY_INTERVAL: Duration = Duration::from_secs(15);

/// Rows moved per pass. Bounded so one workspace with a long backlog cannot
/// hold the main database's writer for an unbounded time.
const DELIVERY_BATCH: usize = 128;

/// First retry delay; doubled per attempt up to `MAX_BACKOFF`.
const BASE_BACKOFF_SECONDS: i64 = 15;
const MAX_BACKOFF_SECONDS: i64 = 3600;

/// Backlog above which the loop reports the queue as a problem rather than a
/// transient — the alert threshold of §22.
const BACKLOG_ALERT: i64 = 100;

/// Risk class of a Code Studio audit row: operational, no personal data.
const RISK_CLASS: &str = "A";

/// Outcome of one delivery pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryReport {
    pub delivered: usize,
    pub failed: usize,
    /// Rows still undelivered after the pass — the metric that is alerted on.
    pub backlog: i64,
}

/// Moves up to `limit` undelivered audit copies into the main database.
pub fn deliver_pending(
    main_db: &DbPool,
    workspace_pool: &DbPool,
    workspace_id: &str,
    limit: usize,
) -> Result<DeliveryReport> {
    let org_id = resolve_org(main_db, workspace_id);
    let due = due_rows(workspace_pool, limit.min(DELIVERY_BATCH))?;

    let mut report = DeliveryReport::default();
    for (id, attempts, payload) in due {
        let outcome = decode_and_write(main_db, workspace_id, &org_id, &payload);
        match outcome {
            Ok(()) => {
                mark_delivered(workspace_pool, id)?;
                report.delivered += 1;
            }
            Err(error) => {
                warn!(
                    workspace_id,
                    outbox_id = id,
                    "audit delivery failed: {error:#}"
                );
                defer(workspace_pool, id, attempts, &error.to_string())?;
                report.failed += 1;
            }
        }
    }
    report.backlog = backlog(workspace_pool)?;
    Ok(report)
}

/// Undelivered audit copies of this workspace. Exposed because §22 tracks it as
/// a metric and alerts when it keeps growing.
pub fn backlog(workspace_pool: &DbPool) -> Result<i64> {
    let conn = workspace_pool
        .read()
        .map_err(|e| anyhow!("workspace db read: {e}"))?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM audit_outbox WHERE delivered_at IS NULL",
        [],
        |row| row.get(0),
    )?)
}

/// Runs `deliver_pending` on a timer for as long as the caller keeps the
/// handle. Same shape as `workspace_db::spawn_idle_sweeper`; the coordinator
/// owns the lifetime, so a closed workspace stops being polled instead of
/// pinning its pool open forever.
pub fn spawn_delivery_loop(
    main_db: DbPool,
    workspace_pool: DbPool,
    workspace_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DELIVERY_INTERVAL);
        loop {
            tick.tick().await;
            match deliver_pending(&main_db, &workspace_pool, &workspace_id, DELIVERY_BATCH) {
                Ok(report) => {
                    if report.delivered > 0 || report.failed > 0 {
                        info!(
                            workspace_id = %workspace_id,
                            delivered = report.delivered,
                            failed = report.failed,
                            backlog = report.backlog,
                            "audit outbox pass"
                        );
                    }
                    if report.backlog > BACKLOG_ALERT {
                        warn!(
                            workspace_id = %workspace_id,
                            backlog = report.backlog,
                            "audit outbox backlog is growing"
                        );
                    }
                }
                Err(e) => warn!(workspace_id = %workspace_id, "audit outbox pass failed: {e:#}"),
            }
        }
    })
}

fn due_rows(workspace_pool: &DbPool, limit: usize) -> Result<Vec<(i64, i64, Vec<u8>)>> {
    let conn = workspace_pool
        .read()
        .map_err(|e| anyhow!("workspace db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT id, attempts, payload_cbor FROM audit_outbox \
         WHERE delivered_at IS NULL \
           AND (next_attempt_at IS NULL OR next_attempt_at <= datetime('now')) \
         ORDER BY id LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn mark_delivered(workspace_pool: &DbPool, id: i64) -> Result<()> {
    let conn = workspace_pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE audit_outbox SET delivered_at = datetime('now'), last_error = NULL WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

fn defer(workspace_pool: &DbPool, id: i64, attempts: i64, error: &str) -> Result<()> {
    let delay = backoff_seconds(attempts + 1);
    let conn = workspace_pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE audit_outbox SET attempts = attempts + 1, last_error = ?2, \
          next_attempt_at = datetime('now', ?3) WHERE id = ?1",
        rusqlite::params![id, error, format!("+{delay} seconds")],
    )?;
    Ok(())
}

/// Exponential, capped. Computed from the attempt count in the row, so the
/// schedule survives a restart instead of resetting to "immediately".
fn backoff_seconds(attempts: i64) -> i64 {
    let shift = attempts.clamp(1, 16) - 1;
    BASE_BACKOFF_SECONDS
        .saturating_mul(1i64 << shift)
        .min(MAX_BACKOFF_SECONDS)
}

fn resolve_org(main_db: &DbPool, workspace_id: &str) -> String {
    match repository::get_workspace(main_db, workspace_id) {
        Ok(Some(workspace)) => workspace.org_id,
        Ok(None) => {
            // The registry row is gone (workspace deleted) but its audit copies
            // must still land; attributing them to the default org keeps the
            // trail rather than dropping it.
            warn!(
                workspace_id,
                "audit delivery: workspace is not in the registry"
            );
            crate::services::org::DEFAULT_ORG_ID.to_string()
        }
        Err(e) => {
            warn!(
                workspace_id,
                "audit delivery: cannot read the workspace: {e:#}"
            );
            crate::services::org::DEFAULT_ORG_ID.to_string()
        }
    }
}

fn decode_and_write(
    main_db: &DbPool,
    workspace_id: &str,
    org_id: &str,
    payload: &[u8],
) -> Result<()> {
    let envelope: AuditEnvelope = events::from_cbor(payload)?;
    write_audit_row(main_db, workspace_id, org_id, &envelope)
}

/// Writes one chained row into `audit_log`, exactly the way every other writer
/// in this crate does: build the canonical input, compute the Merkle link under
/// the same lock, insert.
fn write_audit_row(
    main_db: &DbPool,
    workspace_id: &str,
    org_id: &str,
    envelope: &AuditEnvelope,
) -> Result<()> {
    let action = format!("code_studio.{}", envelope.kind);
    let details = serde_json::json!({
        "workspace_id": workspace_id,
        "event_id": envelope.event_id,
        "seq": envelope.seq,
        "payload": envelope.payload,
    })
    .to_string();
    let result = outcome(&envelope.payload);
    let severity = severity_for(result);

    let conn = main_db.write().map_err(|e| anyhow!("main db write: {e}"))?;
    let hash_input = AuditRowHashInput {
        user_id: None,
        addon_id: None,
        instance_id: None,
        action: action.as_str(),
        resource: None,
        resource_type: Some("code_studio_session"),
        resource_id: Some(envelope.session_id.as_str()),
        result: Some(result),
        error_message: None,
        details: Some(details.as_str()),
        ip_address: None,
        node_id: None,
        severity: Some(severity),
        risk_class: RISK_CLASS,
        related_claim_id: None,
        request_id: Some(envelope.event_id.as_str()),
        timestamp: envelope.created_at.as_str(),
    };
    let (prev_hash, hash) =
        compute_chain_for_insert(&conn, &hash_input).map_err(|e| anyhow!("audit chain: {e}"))?;
    conn.execute(
        "INSERT INTO audit_log \
          (timestamp, action, resource_type, resource_id, result, severity, risk_class, \
           details, org_id, request_id, prev_hash, hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            envelope.created_at,
            action,
            "code_studio_session",
            envelope.session_id,
            result,
            severity,
            RISK_CLASS,
            details,
            org_id,
            envelope.event_id,
            prev_hash,
            hash,
        ],
    )?;
    Ok(())
}

/// The `result` column of the audit row. A refusal must be distinguishable from
/// a grant at a glance — that is what these rows are read for.
fn outcome(payload: &EventPayload) -> &'static str {
    match payload {
        EventPayload::Egress { allowed, .. } => {
            if *allowed {
                "allowed"
            } else {
                "denied"
            }
        }
        EventPayload::ApprovalDecided { decision, .. } => decision_result(decision),
        EventPayload::ApprovalRequested { .. } => "requested",
        _ => "ok",
    }
}

/// Anything a reader must not skim past is a `warning`, including the outcome
/// nobody planned for.
fn severity_for(result: &str) -> &'static str {
    match result {
        "denied" | "unknown" => "warning",
        _ => "info",
    }
}

/// Maps an approval decision onto the audit vocabulary.
///
/// The decision reaches this module as a STRING written by several producers:
/// `ApprovalDecision::as_str` spells a refusal "deny", `cli_bridge` spells the
/// same refusal "denied" in all four places it records one. A rule of the shape
/// `if decision == "deny" { denied } else { allowed }` therefore filed every
/// refused vendor-CLI exec in the org-wide, hash-chained audit log as a GRANT.
///
/// An unrecognised value is never silently promoted to a grant: it is recorded
/// as `unknown` at `warning` and logged, so a producer that invents a new word
/// shows up as an anomaly to investigate instead of as consent.
fn decision_result(decision: &str) -> &'static str {
    match decision.trim().to_ascii_lowercase().as_str() {
        "allow" | "allowed" | "approve" | "approved" | "grant" | "granted" => "allowed",
        "deny" | "denied" | "refuse" | "refused" | "reject" | "rejected" => "denied",
        other => {
            warn!(
                decision = other,
                "audit delivery: unknown approval decision"
            );
            "unknown"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::events::SessionEvent;
    use crate::code_studio::workspace_db;

    #[test]
    fn adversarial_a_refused_vendor_cli_approval_is_audited_as_allowed() {
        // `outcome` matches the literal "deny", which is what
        // `ApprovalDecision::as_str` produces (tools.rs). `cli_bridge` writes
        // "denied" for every refusal it records — policy refusal, user refusal
        // and unanswered prompt alike (cli_bridge.rs:373, :384, :406, :426).
        //
        // Anything that is not exactly "deny" falls into the else arm, so a
        // refused vendor-CLI exec or patch approval reaches the org-wide,
        // hash-chained audit log with result = "allowed" and severity = "info",
        // indistinguishable from a grant. That is the one distinction §13.4
        // says the column exists to carry.
        for decision in ["deny", "denied"] {
            assert_eq!(
                outcome(&EventPayload::ApprovalDecided {
                    approval_id: "a-1".to_string(),
                    decision: decision.to_string(),
                    decided_by: "u-1".to_string(),
                }),
                "denied",
                "a refusal spelled {decision:?} was audited as a grant"
            );
        }
    }

    #[test]
    fn a_decision_nobody_defined_is_never_read_as_consent() {
        for decision in ["", "pending", "timeout", "DENY ", "Rejected"] {
            let result = outcome(&EventPayload::ApprovalDecided {
                approval_id: "a-1".to_string(),
                decision: decision.to_string(),
                decided_by: "u-1".to_string(),
            });
            assert_ne!(
                result, "allowed",
                "the decision {decision:?} was audited as a grant"
            );
            assert_eq!(
                severity_for(result),
                "warning",
                "the decision {decision:?} was filed as routine"
            );
        }
        assert_eq!(
            outcome(&EventPayload::ApprovalDecided {
                approval_id: "a-1".to_string(),
                decision: "allow".to_string(),
                decided_by: "u-1".to_string(),
            }),
            "allowed",
            "a real grant must still read as one"
        );
    }

    fn workspace_pool() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (pool, _version) = workspace_db::open_pool_at(dir.path()).expect("open workspace.db");
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('s-1', 'ws-1', 'u-1', 't', 'b', 'normal', 'f', 'v', 'idle', \
                  datetime('now'), datetime('now'))",
                [],
            )
            .unwrap();
        }
        (dir, pool)
    }

    fn main_db() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(&dir.path().join("tentaflow.db")).expect("init main db");
        (dir, db)
    }

    fn secret_access(key: &str) -> SessionEvent {
        SessionEvent::new(
            key,
            EventPayload::SecretAccess {
                secret_ref: "vault:repo-token".into(),
                purpose: "git push".into(),
            },
        )
    }

    #[test]
    fn a_security_event_reaches_the_main_audit_log_exactly_once() {
        let (_wdir, pool) = workspace_pool();
        let (_mdir, main) = main_db();
        events::append(&pool, "s-1", secret_access("k-1")).unwrap();

        let report = deliver_pending(&main, &pool, "ws-1", 10).unwrap();
        assert_eq!(report.delivered, 1);
        assert_eq!(report.backlog, 0);

        let rows: i64 = {
            let conn = main.read().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'code_studio.secret_access'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(rows, 1);

        // A delivered row is not delivered again.
        let second = deliver_pending(&main, &pool, "ws-1", 10).unwrap();
        assert_eq!(second.delivered, 0);
        let rows_after: i64 = {
            let conn = main.read().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'code_studio.secret_access'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(rows_after, 1);
    }

    #[test]
    fn the_delivered_row_carries_the_chain_and_the_redacted_payload() {
        let (_wdir, pool) = workspace_pool();
        let (_mdir, main) = main_db();
        let token = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        events::append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-egress",
                EventPayload::Egress {
                    url: format!("https://api.example.com/v1?token={token}"),
                    allowed: false,
                    reason: "host not allowlisted".into(),
                },
            ),
        )
        .unwrap();
        deliver_pending(&main, &pool, "ws-1", 10).unwrap();

        let conn = main.read().unwrap();
        let (details, result, hash_len): (String, String, i64) = conn
            .query_row(
                "SELECT details, result, LENGTH(hash) FROM audit_log \
                 WHERE action = 'code_studio.egress'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(!details.contains(token), "the audit row carried the token");
        assert!(details.contains("\"seq\":1"), "{details}");
        assert_eq!(result, "denied", "a refusal was recorded as a success");
        assert_eq!(hash_len, 32, "the row is not part of the audit chain");
    }

    #[test]
    fn a_failing_delivery_backs_off_instead_of_spinning() {
        let (_wdir, pool) = workspace_pool();
        let (_mdir, main) = main_db();
        events::append(&pool, "s-1", secret_access("k-1")).unwrap();
        // Make the main database reject the insert the way a broken schema or
        // a locked table would.
        {
            let conn = main.write().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER audit_is_broken BEFORE INSERT ON audit_log \
                 BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;",
            )
            .unwrap();
        }

        let report = deliver_pending(&main, &pool, "ws-1", 10).unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.backlog, 1, "a failed row must stay in the outbox");

        let (attempts, last_error, next): (i64, Option<String>, Option<String>) = {
            let conn = pool.read().unwrap();
            conn.query_row(
                "SELECT attempts, last_error, next_attempt_at FROM audit_outbox",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
        };
        assert_eq!(attempts, 1);
        assert!(last_error.unwrap().contains("audit unavailable"));
        assert!(next.is_some(), "no backoff was scheduled");

        // The backed-off row is not picked up again in the same minute.
        let immediate = deliver_pending(&main, &pool, "ws-1", 10).unwrap();
        assert_eq!(
            immediate.failed, 0,
            "a deferred row was retried immediately"
        );
        assert_eq!(immediate.delivered, 0);

        // Once the delay passes and the main database recovers, it lands.
        {
            let conn = main.write().unwrap();
            conn.execute_batch("DROP TRIGGER audit_is_broken;").unwrap();
        }
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "UPDATE audit_outbox SET next_attempt_at = datetime('now', '-1 second')",
                [],
            )
            .unwrap();
        }
        let recovered = deliver_pending(&main, &pool, "ws-1", 10).unwrap();
        assert_eq!(recovered.delivered, 1);
        assert_eq!(recovered.backlog, 0);
    }

    #[test]
    fn the_backoff_grows_and_is_capped() {
        assert_eq!(backoff_seconds(1), BASE_BACKOFF_SECONDS);
        assert_eq!(backoff_seconds(2), BASE_BACKOFF_SECONDS * 2);
        assert_eq!(backoff_seconds(3), BASE_BACKOFF_SECONDS * 4);
        assert_eq!(backoff_seconds(50), MAX_BACKOFF_SECONDS);
        assert!(backoff_seconds(0) >= BASE_BACKOFF_SECONDS);
    }

    #[test]
    fn an_ordinary_event_never_reaches_the_audit_log() {
        let (_wdir, pool) = workspace_pool();
        let (_mdir, main) = main_db();
        events::append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-msg",
                EventPayload::AgentMessage {
                    role: "assistant".into(),
                    text: "working on it".into(),
                },
            ),
        )
        .unwrap();

        let report = deliver_pending(&main, &pool, "ws-1", 10).unwrap();
        assert_eq!(report.delivered, 0);
        let conn = main.read().unwrap();
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'code_studio.%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }
}
