// ===== File: events/audit_outbox.rs — moving the audit mirror into the main database =====
//
// A security-relevant timeline entry lives in `<data>/events.db`, but the
// organisation's audit trail lives in `tentaflow.db`. Writing both directly
// would lose the trail whenever the process died between the two writes, so the
// entry and its ALREADY REDACTED audit copy are committed together into
// `audit_outbox` (see `store::write_event`) and this module moves the copy
// across afterwards (§2.8).
//
// The delivery is AT LEAST ONCE, deliberately. The audit row is written to the
// main database FIRST and the outbox row is marked delivered SECOND; a crash
// between them replays one row, which duplicates an audit entry. The opposite
// order would LOSE one, and a duplicated audit entry is a nuisance while a
// missing one is a compliance failure. §2.8 states the same asymmetry from the
// other side: the audit trail may not lose an entry, the timeline may lose its
// tail.
//
// A row that cannot be delivered is not dropped and not retried in a tight
// loop: `attempts`, `last_error` and `next_attempt_at` give it exponential
// backoff that survives a restart, and the depth of the queue is a metric.

use std::time::Duration;

use anyhow::{anyhow, Result};
use tracing::{info, warn};

use super::store::{self, AuditEnvelope};
use crate::audit::chain::{compute_chain_for_insert, AuditRowHashInput};
use crate::db::DbPool;

/// How often the loop looks for undelivered rows.
const DELIVERY_INTERVAL: Duration = Duration::from_secs(15);

/// Rows moved per pass. Bounded so a long backlog cannot hold the main
/// database's single writer connection for an unbounded time — the very
/// contention `events.db` exists to avoid.
const DELIVERY_BATCH: usize = 128;

/// First retry delay; doubled per attempt up to `MAX_BACKOFF_SECONDS`.
const BASE_BACKOFF_SECONDS: i64 = 15;
const MAX_BACKOFF_SECONDS: i64 = 3600;

/// Backlog above which the loop reports the queue as a problem rather than a
/// transient.
const BACKLOG_ALERT: i64 = 100;

/// Risk class of a run-provenance audit row: operational metadata (origin,
/// actor, model), no prompt or response body.
const RISK_CLASS: &str = crate::audit::RiskClass::A.as_db_str();

/// Outcome of one delivery pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryReport {
    pub delivered: usize,
    pub failed: usize,
    /// Rows still undelivered after the pass — the metric worth alerting on.
    pub backlog: i64,
}

/// Moves up to `limit` undelivered audit copies into the main database.
pub fn deliver_pending(
    main_db: &DbPool,
    events_pool: &DbPool,
    limit: usize,
) -> Result<DeliveryReport> {
    let due = due_rows(events_pool, limit.min(DELIVERY_BATCH))?;

    let mut report = DeliveryReport::default();
    for (id, attempts, payload) in due {
        match decode_and_write(main_db, &payload) {
            Ok(()) => {
                mark_delivered(events_pool, id)?;
                report.delivered += 1;
            }
            Err(error) => {
                warn!(outbox_id = id, "audit delivery failed: {error:#}");
                defer(events_pool, id, attempts, &error.to_string())?;
                report.failed += 1;
            }
        }
    }
    report.backlog = backlog(events_pool)?;
    Ok(report)
}

/// Undelivered audit copies waiting on this node.
pub fn backlog(events_pool: &DbPool) -> Result<i64> {
    let conn = events_pool
        .read()
        .map_err(|e| anyhow!("events db read: {e}"))?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM audit_outbox WHERE delivered_at IS NULL",
        [],
        |row| row.get(0),
    )?)
}

/// Runs `deliver_pending` on a timer for the lifetime of the process. Started
/// from `events::init`, which is the whole point: the same mechanism in
/// `code_studio` has no caller anywhere in the tree, so its outbox is never
/// drained and its security events never reach `audit_log`.
pub fn spawn_delivery_loop(main_db: DbPool, events_pool: DbPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DELIVERY_INTERVAL);
        loop {
            tick.tick().await;
            match deliver_pending(&main_db, &events_pool, DELIVERY_BATCH) {
                Ok(report) => {
                    if report.delivered > 0 || report.failed > 0 {
                        info!(
                            delivered = report.delivered,
                            failed = report.failed,
                            backlog = report.backlog,
                            "event log audit outbox pass"
                        );
                    }
                    if report.backlog > BACKLOG_ALERT {
                        warn!(
                            backlog = report.backlog,
                            "event log audit outbox backlog is growing"
                        );
                    }
                }
                Err(e) => warn!("event log audit outbox pass failed: {e:#}"),
            }
        }
    })
}

fn due_rows(events_pool: &DbPool, limit: usize) -> Result<Vec<(i64, i64, String)>> {
    let conn = events_pool
        .read()
        .map_err(|e| anyhow!("events db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT id, attempts, payload_json FROM audit_outbox \
         WHERE delivered_at IS NULL \
           AND (next_attempt_at IS NULL OR next_attempt_at <= datetime('now')) \
         ORDER BY id LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn mark_delivered(events_pool: &DbPool, id: i64) -> Result<()> {
    let conn = events_pool
        .write()
        .map_err(|e| anyhow!("events db write: {e}"))?;
    conn.execute(
        "UPDATE audit_outbox SET delivered_at = datetime('now'), last_error = NULL WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

fn defer(events_pool: &DbPool, id: i64, attempts: i64, error: &str) -> Result<()> {
    let delay = backoff_seconds(attempts + 1);
    let conn = events_pool
        .write()
        .map_err(|e| anyhow!("events db write: {e}"))?;
    conn.execute(
        "UPDATE audit_outbox SET attempts = attempts + 1, last_error = ?2, \
          next_attempt_at = datetime('now', ?3) WHERE id = ?1",
        rusqlite::params![id, error, format!("+{delay} seconds")],
    )?;
    Ok(())
}

/// Exponential, capped. Computed from the attempt count stored in the row, so
/// the schedule survives a restart instead of resetting to "immediately".
fn backoff_seconds(attempts: i64) -> i64 {
    let shift = attempts.clamp(1, 16) - 1;
    BASE_BACKOFF_SECONDS
        .saturating_mul(1i64 << shift)
        .min(MAX_BACKOFF_SECONDS)
}

fn decode_and_write(main_db: &DbPool, payload: &str) -> Result<()> {
    let envelope: AuditEnvelope = store::from_json(payload)?;
    write_audit_row(main_db, &envelope)
}

/// Writes one chained row into `audit_log`, the way every other writer in this
/// crate does: build the canonical input, compute the Merkle link and INSERT
/// under the SAME lock, so no other writer can extend the chain in between.
fn write_audit_row(main_db: &DbPool, envelope: &AuditEnvelope) -> Result<()> {
    let action = format!("flow_run.{}", envelope.kind);
    let details = serde_json::json!({
        "run_id": envelope.run_id,
        "seq": envelope.seq,
        "at_ms": envelope.at_ms,
        "origin": envelope.origin,
        "actor_kind": envelope.actor_kind,
        "actor_id": envelope.actor_id,
        // Repeated in `details` and not only in the `user_id` column so a
        // reader can tell "an API key bound to nobody" from "a column nobody
        // filled in" (§2.5).
        "actor_user_id": envelope.actor_user_id,
        "session_id": envelope.session_id,
        "payload": envelope.payload,
    })
    .to_string();
    // Identifies THIS row rather than the request, so an at-least-once replay
    // is recognisable as a duplicate of a known entry instead of looking like a
    // second run.
    let request_id = format!("{}:{}", envelope.run_id, envelope.seq);
    let addon_id = (envelope.actor_kind == "addon")
        .then(|| envelope.actor_id.clone())
        .flatten();

    let conn = main_db.write().map_err(|e| anyhow!("main db write: {e}"))?;
    let hash_input = AuditRowHashInput {
        user_id: envelope.actor_user_id.as_deref(),
        addon_id: addon_id.as_deref(),
        instance_id: None,
        action: action.as_str(),
        resource: None,
        resource_type: Some("flow_run"),
        resource_id: Some(envelope.run_id.as_str()),
        // The mirrored event records only that a run STARTED, so there is no
        // outcome to report; `ok` would claim one that has not happened yet.
        // The run's ending lives on the timeline, reachable through
        // `correlation_id` (invariant 6).
        result: None,
        error_message: None,
        details: Some(details.as_str()),
        ip_address: None,
        node_id: None,
        severity: Some("info"),
        risk_class: RISK_CLASS,
        related_claim_id: None,
        request_id: Some(request_id.as_str()),
        timestamp: envelope.created_at.as_str(),
    };
    let (prev_hash, hash) =
        compute_chain_for_insert(&conn, &hash_input).map_err(|e| anyhow!("audit chain: {e}"))?;
    conn.execute(
        "INSERT INTO audit_log \
          (timestamp, user_id, addon_id, action, resource_type, resource_id, result, severity, \
           risk_class, details, org_id, request_id, correlation_id, prev_hash, hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        rusqlite::params![
            envelope.created_at,
            envelope.actor_user_id,
            addon_id,
            action,
            "flow_run",
            envelope.run_id,
            None::<&str>,
            // Severity classifies the ENTRY, not the run: a provenance record
            // carrying no outcome and no error is informational.
            "info",
            RISK_CLASS,
            details,
            // NULL when no organisation was minted for the run (camera,
            // scheduler, maintenance). The default tenant is not a stand-in:
            // borrowing it would show a tenant-scoped audit reader runs that
            // tenant never made (invariant 6, same rule as the timeline row).
            envelope.org_id,
            request_id,
            // The deep link of §2.10.3 — v129 added the column so an audit
            // entry can be followed to the point on the timeline that produced
            // it. Outside the hash chain because `AuditRowHashInput` predates
            // it; the chain covers the accountability facts, this is a pointer.
            envelope.correlation_id,
            prev_hash,
            hash,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::store::{append, now_ms, EventPayload, RunEvent};
    use crate::events::test_support::{events_db, main_db};
    use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};

    fn request_started(run_id: &str) -> RunEvent {
        RunEvent::new(
            run_id,
            now_ms(),
            FlowOrigin::CodeStudio,
            &FlowActor::user("u-1"),
            EventPayload::RequestStarted {
                model: Some("qwen3".into()),
                flow_id: Some("f-1".into()),
                service_type: Some("llm".into()),
                modality: Some("text".into()),
            },
        )
        .with_correlation("corr-1")
        .with_session("s-1")
    }

    fn audit_rows(main: &DbPool) -> i64 {
        let conn = main.read().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'flow_run.request_started'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// §2.8 — the event and its audit copy are ONE transaction. If the outbox
    /// insert cannot happen, the event must not exist either: an unaudited
    /// security event is worse than a retried one.
    #[test]
    fn a_failed_outbox_insert_takes_the_event_with_it() {
        let (_dir, pool) = events_db();
        let main = main_db();
        {
            let conn = pool.write().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER outbox_is_broken BEFORE INSERT ON audit_outbox \
                 BEGIN SELECT RAISE(ABORT, 'outbox unavailable'); END;",
            )
            .unwrap();
        }

        let error = append(&pool, &main, request_started("r-1")).expect_err("the append must fail loudly");
        assert!(
            error.to_string().contains("outbox unavailable"),
            "the failure was reported as something else: {error:#}"
        );

        let conn = pool.read().unwrap();
        let (events, outbox): (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM run_events), (SELECT COUNT(*) FROM audit_outbox)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(events, 0, "an event survived without its audit copy");
        assert_eq!(outbox, 0);
    }

    /// The AT-LEAST-ONCE ordering rule. The main-database audit row is written
    /// FIRST and the outbox row is marked delivered SECOND, so a crash between
    /// them replays a duplicate rather than losing an entry. The proof is to
    /// break the second step: the audit row must already be there.
    #[test]
    fn the_audit_row_is_written_before_the_outbox_row_is_marked_delivered() {
        let (_dir, pool) = events_db();
        let main = main_db();
        append(&pool, &main, request_started("r-1")).unwrap();
        {
            let conn = pool.write().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER cannot_mark_delivered BEFORE UPDATE OF delivered_at \
                   ON audit_outbox \
                 BEGIN SELECT RAISE(ABORT, 'crashed before the acknowledgement'); END;",
            )
            .unwrap();
        }

        let result = deliver_pending(&main, &pool, 10);
        assert!(result.is_err(), "the acknowledgement failure was swallowed");
        assert_eq!(
            audit_rows(&main),
            1,
            "the audit row was NOT written before the acknowledgement — this ordering \
             loses an entry on a crash"
        );

        let undelivered: i64 = {
            let conn = pool.read().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM audit_outbox WHERE delivered_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(undelivered, 1, "the row was acknowledged after all");

        // Recovery replays it: one duplicate, never a loss.
        {
            let conn = pool.write().unwrap();
            conn.execute_batch("DROP TRIGGER cannot_mark_delivered;").unwrap();
        }
        let report = deliver_pending(&main, &pool, 10).unwrap();
        assert_eq!(report.delivered, 1);
        assert_eq!(audit_rows(&main), 2, "the replay was expected to duplicate");
    }

    #[test]
    fn a_security_event_reaches_the_main_audit_log_exactly_once() {
        let (_dir, pool) = events_db();
        let main = main_db();
        append(&pool, &main, request_started("r-1")).unwrap();

        let report = deliver_pending(&main, &pool, 10).unwrap();
        assert_eq!(report.delivered, 1);
        assert_eq!(report.backlog, 0);
        assert_eq!(audit_rows(&main), 1);

        let second = deliver_pending(&main, &pool, 10).unwrap();
        assert_eq!(second.delivered, 0);
        assert_eq!(audit_rows(&main), 1, "a delivered row was delivered again");
    }

    /// §2.10.3 — the deep link. An audit entry is only followable back to the
    /// timeline if `correlation_id` is stamped on BOTH sides, and the row must
    /// be part of the hash chain like every other.
    #[test]
    fn the_delivered_row_carries_the_correlation_link_and_the_chain() {
        let (_dir, pool) = events_db();
        let main = main_db();
        append(&pool, &main, request_started("r-1")).unwrap();
        deliver_pending(&main, &pool, 10).unwrap();

        let conn = main.read().unwrap();
        let (correlation, resource, details, hash_len): (Option<String>, String, String, i64) =
            conn.query_row(
                "SELECT correlation_id, resource_id, details, LENGTH(hash) FROM audit_log \
                 WHERE action = 'flow_run.request_started'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(correlation.as_deref(), Some("corr-1"));
        assert_eq!(resource, "r-1");
        assert!(details.contains("\"origin\":\"code_studio\""), "{details}");
        assert!(details.contains("\"actor_user_id\":\"u-1\""), "{details}");
        assert_eq!(hash_len, 32, "the row is not part of the audit chain");
    }

    /// Invariant 6, on the audit side. A camera, scheduler or maintenance run
    /// carries no organisation, and the audit copy must say so: substituting
    /// the default tenant would show a tenant-scoped audit reader runs that
    /// organisation never made. Asserted on the COLUMN — the timeline row
    /// already stores NULL and the two must not disagree.
    #[test]
    fn a_run_with_no_tenant_is_audited_with_no_tenant() {
        let (_dir, pool) = events_db();
        let main = main_db();

        append(&pool, &main, request_started("r-tenant").with_org("org-acme")).unwrap();

        let system = RunEvent::new(
            "r-system",
            now_ms(),
            FlowOrigin::Camera,
            &FlowActor::system(),
            EventPayload::RequestStarted {
                model: None,
                flow_id: None,
                service_type: None,
                modality: None,
            },
        );
        assert!(system.org_id.is_none());
        append(&pool, &main, system).unwrap();

        assert_eq!(deliver_pending(&main, &pool, 10).unwrap().delivered, 2);

        let conn = main.read().unwrap();
        let rows: Vec<(String, Option<String>, Option<String>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT resource_id, org_id, result FROM audit_log \
                     WHERE action = 'flow_run.request_started' ORDER BY resource_id",
                )
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                // No outcome is known when a run opens, so `result` stays NULL
                // for both rows rather than claiming a cheerful `ok`.
                ("r-system".to_string(), None, None),
                ("r-tenant".to_string(), Some("org-acme".to_string()), None),
            ],
            "the audit copy invented a tenant or an outcome"
        );

        // A tenant-scoped reader must not pick the unattributed run up.
        let scoped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log \
                 WHERE action = 'flow_run.request_started' AND org_id = 'org-default'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(scoped, 0, "a tenant-less run was filed under the default organisation");
    }

    /// The timeline is diagnostic, the audit log is a commitment. Mirroring
    /// every token and tool result would multiply the hash-chained log by the
    /// traffic of the node.
    #[test]
    fn an_ordinary_timeline_entry_never_reaches_the_audit_log() {
        let (_dir, pool) = events_db();
        let main = main_db();
        for payload in [
            EventPayload::FirstToken {},
            EventPayload::AssistantMessage {
                body: crate::events::ResponseBody::Text("done".into()),
                tokens: Some(3),
            },
            EventPayload::ToolResult {
                ok: true,
                summary: "ok".into(),
            },
            EventPayload::Error {
                stage: "llm".into(),
                message: "timeout".into(),
            },
        ] {
            append(
                &pool,
                &main,
                RunEvent::new(
                    "r-1",
                    now_ms(),
                    FlowOrigin::Chat,
                    &FlowActor::user("u-1"),
                    payload,
                ),
            )
            .unwrap();
        }

        let report = deliver_pending(&main, &pool, 10).unwrap();
        assert_eq!(report.delivered, 0);
        let conn = main.read().unwrap();
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'flow_run.%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn a_failing_delivery_backs_off_instead_of_spinning() {
        let (_dir, pool) = events_db();
        let main = main_db();
        append(&pool, &main, request_started("r-1")).unwrap();
        {
            let conn = main.write().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER audit_is_broken BEFORE INSERT ON audit_log \
                 BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;",
            )
            .unwrap();
        }

        let report = deliver_pending(&main, &pool, 10).unwrap();
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

        let immediate = deliver_pending(&main, &pool, 10).unwrap();
        assert_eq!(immediate.failed, 0, "a deferred row was retried immediately");

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
        let recovered = deliver_pending(&main, &pool, 10).unwrap();
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
}
