// ===== File: code_studio/events.rs — the session timeline, and the only source of truth =====
//
// Every state a session has is a PROJECTION of this log (§13.3). `sessions.
// status`, `session_runs.status` and the operation journal are caches that
// exist because a query over them is cheap; when they disagree with the tail of
// the timeline, the timeline wins and the correction is itself an event.
//
// Three properties are enforced here rather than assumed by callers.
//
// **One writer allocates `seq`.** The number is taken as `MAX(seq) + 1` INSIDE
// the insert transaction, and `UNIQUE(session_id, seq)` in the schema turns a
// second concurrent writer into a loud failure instead of a silently interleaved
// timeline. The single writer connection of `DbPool` is what makes the read
// safe; the constraint is what makes a mistake visible.
//
// **A retry is a no-op, not a duplicate and not an error.** The caller's
// `idempotency_key` is looked up first; an existing row is returned as
// `duplicate`. A crash between "effect happened" and "event written" therefore
// resolves by simply writing the event again.
//
// **A security-relevant event and its audit copy are one transaction.** The
// outbox row is written next to the event (§13.4), already redacted, so no
// failure between the two databases can lose the audit trail. If the outbox
// insert fails, the event does not exist either — which is the correct
// direction: an unaudited security event is worse than a retried one.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::warn;
use uuid::Uuid;

use super::redact;
use crate::db::DbPool;

/// Version stamped on every payload written by this binary. It travels with the
/// row so a later reader can tell which shape it is decoding.
pub const EVENT_SCHEMA_VERSION: i64 = 1;

/// Largest page `read_after` will return, whatever the caller asks for.
const MAX_READ_LIMIT: usize = 1000;

/// Kind of a timeline entry. Derived from the payload rather than supplied
/// alongside it, so the two can never contradict each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    RunStarted,
    RunFinished,
    ToolCall,
    ToolResult,
    AgentMessage,
    ApprovalRequested,
    ApprovalDecided,
    PatchSetOpened,
    PatchDecided,
    Exec,
    GitOp,
    Egress,
    SecretAccess,
    TicketIssued,
    Sandbox,
    WorkspaceCreated,
    AutonomyChanged,
    AllowlistChanged,
    MemberAdded,
    OperationStarted,
    OperationFinished,
    OperationReconciled,
    ProjectionCorrected,
    TestRun,
}

impl EventKind {
    pub fn slug(self) -> &'static str {
        match self {
            EventKind::RunStarted => "run_started",
            EventKind::RunFinished => "run_finished",
            EventKind::ToolCall => "tool_call",
            EventKind::ToolResult => "tool_result",
            EventKind::AgentMessage => "agent_message",
            EventKind::ApprovalRequested => "approval_requested",
            EventKind::ApprovalDecided => "approval_decided",
            EventKind::PatchSetOpened => "patch_set_opened",
            EventKind::PatchDecided => "patch_decided",
            EventKind::Exec => "exec",
            EventKind::GitOp => "git_op",
            EventKind::Egress => "egress",
            EventKind::SecretAccess => "secret_access",
            EventKind::TicketIssued => "ticket_issued",
            EventKind::Sandbox => "sandbox",
            EventKind::WorkspaceCreated => "workspace_created",
            EventKind::AutonomyChanged => "autonomy_changed",
            EventKind::AllowlistChanged => "allowlist_changed",
            EventKind::MemberAdded => "member_added",
            EventKind::OperationStarted => "operation_started",
            EventKind::OperationFinished => "operation_finished",
            EventKind::OperationReconciled => "operation_reconciled",
            EventKind::ProjectionCorrected => "projection_corrected",
            EventKind::TestRun => "test_run",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        let kind = match slug {
            "run_started" => EventKind::RunStarted,
            "run_finished" => EventKind::RunFinished,
            "tool_call" => EventKind::ToolCall,
            "tool_result" => EventKind::ToolResult,
            "agent_message" => EventKind::AgentMessage,
            "approval_requested" => EventKind::ApprovalRequested,
            "approval_decided" => EventKind::ApprovalDecided,
            "patch_set_opened" => EventKind::PatchSetOpened,
            "patch_decided" => EventKind::PatchDecided,
            "exec" => EventKind::Exec,
            "git_op" => EventKind::GitOp,
            "egress" => EventKind::Egress,
            "secret_access" => EventKind::SecretAccess,
            "ticket_issued" => EventKind::TicketIssued,
            "sandbox" => EventKind::Sandbox,
            "workspace_created" => EventKind::WorkspaceCreated,
            "autonomy_changed" => EventKind::AutonomyChanged,
            "allowlist_changed" => EventKind::AllowlistChanged,
            "member_added" => EventKind::MemberAdded,
            "operation_started" => EventKind::OperationStarted,
            "operation_finished" => EventKind::OperationFinished,
            "operation_reconciled" => EventKind::OperationReconciled,
            "projection_corrected" => EventKind::ProjectionCorrected,
            "test_run" => EventKind::TestRun,
            _ => return None,
        };
        Some(kind)
    }
}

/// Git operations distinguished in the timeline. `Push`, `Merge` and
/// `MergeFinalize` are the ones §13.4 mirrors into the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitOperation {
    Fetch,
    Branch,
    Commit,
    Push,
    Merge,
    MergeFinalize,
    Worktree,
}

/// The body of an event. One variant per kind, so a field a reader needs is
/// either present by construction or genuinely not part of that event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventPayload {
    RunStarted {
        run_id: String,
        kind: String,
        trigger: String,
    },
    RunFinished {
        run_id: String,
        status: String,
        error: Option<String>,
    },
    ToolCall {
        call_id: String,
        tool: String,
        arguments: BTreeMap<String, String>,
    },
    ToolResult {
        call_id: String,
        ok: bool,
        summary: String,
    },
    AgentMessage {
        role: String,
        text: String,
    },
    ApprovalRequested {
        approval_id: String,
        capability: String,
        summary: String,
    },
    ApprovalDecided {
        approval_id: String,
        decision: String,
        decided_by: String,
    },
    PatchSetOpened {
        patch_set_id: String,
        files: u32,
    },
    PatchDecided {
        patch_set_id: String,
        decision: String,
        decided_by: String,
    },
    Exec {
        op_id: String,
        argv: Vec<String>,
        cwd: String,
        exit_code: Option<i32>,
        /// The mount the caller asked for, next to the exit code, because
        /// "exit 0" and "the worktree changed" are two different claims: a
        /// command narrowed to `cow` succeeds against a copy. Older rows
        /// predate the field and default to an empty request.
        #[serde(default)]
        requested_mount_access: String,
        /// True when the command ran against that copy, so nothing it wrote
        /// reached the worktree.
        #[serde(default)]
        writes_discarded: bool,
    },
    GitOp {
        op_id: String,
        operation: GitOperation,
        refname: Option<String>,
        old_oid: Option<String>,
        new_oid: Option<String>,
        remote: Option<String>,
    },
    Egress {
        url: String,
        allowed: bool,
        reason: String,
    },
    SecretAccess {
        secret_ref: String,
        purpose: String,
    },
    TicketIssued {
        ticket_id: String,
        engine_id: String,
        budget_tokens: u64,
    },
    Sandbox {
        sandbox_id: String,
        state: String,
        mount_access: String,
        network_access: String,
    },
    WorkspaceCreated {
        workspace_id: String,
        node_id: String,
        exec_mode: String,
    },
    AutonomyChanged {
        from: String,
        to: String,
        changed_by: String,
    },
    AllowlistChanged {
        added: Vec<String>,
        removed: Vec<String>,
        changed_by: String,
    },
    MemberAdded {
        user_id: String,
        role: String,
        added_by: String,
    },
    OperationStarted {
        op_id: String,
        op_kind: String,
        capability: String,
    },
    OperationFinished {
        op_id: String,
        op_kind: String,
        status: String,
        error: Option<String>,
    },
    OperationReconciled {
        op_id: String,
        op_kind: String,
        from: String,
        to: String,
        reason: String,
    },
    ProjectionCorrected {
        entity: String,
        id: String,
        projected: String,
        from_events: String,
    },
    /// Outcome of a linked project's test run against a commit of this session
    /// (§20). This is what closes the cycle "agent writes → review → commit →
    /// the project tests THAT commit → the result comes back": the result is a
    /// fact about the repository produced by a runner in its own sandbox, not
    /// something the agent reported, so it cannot travel as a `ToolResult`
    /// without laundering its provenance.
    ///
    /// `run_id` is the PROJECT's test run, deliberately distinct from the
    /// session run carried in `SessionEvent.run_id` — the two id spaces are
    /// different and conflating them would attribute a runner's verdict to a
    /// turn of the agent.
    TestRun {
        project_id: String,
        run_id: String,
        commit: String,
        passed: u32,
        failed: u32,
        skipped: u32,
        duration_ms: u64,
        /// CAS digest of the full report. The event carries counts; the body
        /// stays in the artifact store, well under the frame budget (§13.2).
        detail_ref: Option<String>,
    },
}

impl EventPayload {
    pub fn kind(&self) -> EventKind {
        match self {
            EventPayload::RunStarted { .. } => EventKind::RunStarted,
            EventPayload::RunFinished { .. } => EventKind::RunFinished,
            EventPayload::ToolCall { .. } => EventKind::ToolCall,
            EventPayload::ToolResult { .. } => EventKind::ToolResult,
            EventPayload::AgentMessage { .. } => EventKind::AgentMessage,
            EventPayload::ApprovalRequested { .. } => EventKind::ApprovalRequested,
            EventPayload::ApprovalDecided { .. } => EventKind::ApprovalDecided,
            EventPayload::PatchSetOpened { .. } => EventKind::PatchSetOpened,
            EventPayload::PatchDecided { .. } => EventKind::PatchDecided,
            EventPayload::Exec { .. } => EventKind::Exec,
            EventPayload::GitOp { .. } => EventKind::GitOp,
            EventPayload::Egress { .. } => EventKind::Egress,
            EventPayload::SecretAccess { .. } => EventKind::SecretAccess,
            EventPayload::TicketIssued { .. } => EventKind::TicketIssued,
            EventPayload::Sandbox { .. } => EventKind::Sandbox,
            EventPayload::WorkspaceCreated { .. } => EventKind::WorkspaceCreated,
            EventPayload::AutonomyChanged { .. } => EventKind::AutonomyChanged,
            EventPayload::AllowlistChanged { .. } => EventKind::AllowlistChanged,
            EventPayload::MemberAdded { .. } => EventKind::MemberAdded,
            EventPayload::OperationStarted { .. } => EventKind::OperationStarted,
            EventPayload::OperationFinished { .. } => EventKind::OperationFinished,
            EventPayload::OperationReconciled { .. } => EventKind::OperationReconciled,
            EventPayload::ProjectionCorrected { .. } => EventKind::ProjectionCorrected,
            EventPayload::TestRun { .. } => EventKind::TestRun,
        }
    }

    /// Which events are mirrored into the main database's audit log (§13.4):
    /// approvals and refusals, secret access, ticket issuance, egress,
    /// push/merge, membership, autonomy and allowlist changes.
    pub fn security_relevant(&self) -> bool {
        match self {
            EventPayload::ApprovalRequested { .. }
            | EventPayload::ApprovalDecided { .. }
            | EventPayload::SecretAccess { .. }
            | EventPayload::TicketIssued { .. }
            | EventPayload::Egress { .. }
            | EventPayload::AutonomyChanged { .. }
            | EventPayload::AllowlistChanged { .. }
            | EventPayload::MemberAdded { .. } => true,
            EventPayload::GitOp { operation, .. } => matches!(
                operation,
                GitOperation::Push | GitOperation::Merge | GitOperation::MergeFinalize
            ),
            _ => false,
        }
    }

    /// Runs every free-text field through the scrubber. Called by `append`, so
    /// redaction is a property of the write path and not of caller discipline.
    pub fn redacted(self) -> Self {
        match self {
            EventPayload::ToolCall {
                call_id,
                tool,
                arguments,
            } => EventPayload::ToolCall {
                call_id,
                tool,
                arguments: arguments
                    .into_iter()
                    .map(|(key, value)| {
                        let value = redact::redact_text(&value);
                        (key, value)
                    })
                    .collect(),
            },
            EventPayload::ToolResult {
                call_id,
                ok,
                summary,
            } => EventPayload::ToolResult {
                call_id,
                ok,
                summary: redact::redact_text(&summary),
            },
            EventPayload::AgentMessage { role, text } => EventPayload::AgentMessage {
                role,
                text: redact::redact_text(&text),
            },
            EventPayload::ApprovalRequested {
                approval_id,
                capability,
                summary,
            } => EventPayload::ApprovalRequested {
                approval_id,
                capability,
                summary: redact::redact_text(&summary),
            },
            EventPayload::RunFinished {
                run_id,
                status,
                error,
            } => EventPayload::RunFinished {
                run_id,
                status,
                error: error.map(|e| redact::redact_text(&e)),
            },
            EventPayload::Exec {
                op_id,
                argv,
                cwd,
                exit_code,
                requested_mount_access,
                writes_discarded,
            } => EventPayload::Exec {
                op_id,
                argv: redact::redact_argv(&argv),
                cwd,
                exit_code,
                requested_mount_access,
                writes_discarded,
            },
            EventPayload::Egress {
                url,
                allowed,
                reason,
            } => EventPayload::Egress {
                url: redact::redact_url(&url),
                allowed,
                reason: redact::redact_text(&reason),
            },
            EventPayload::GitOp {
                op_id,
                operation,
                refname,
                old_oid,
                new_oid,
                remote,
            } => EventPayload::GitOp {
                op_id,
                operation,
                refname,
                old_oid,
                new_oid,
                // A remote URL is the classic place a token ends up.
                remote: remote.map(|r| redact::redact_url(&r)),
            },
            EventPayload::OperationFinished {
                op_id,
                op_kind,
                status,
                error,
            } => EventPayload::OperationFinished {
                op_id,
                op_kind,
                status,
                error: error.map(|e| redact::redact_text(&e)),
            },
            other => other,
        }
    }
}

/// What a caller hands to `append`. `session_id` is a parameter of the call
/// rather than a field, because it also selects the sequence space.
#[derive(Debug, Clone)]
pub struct SessionEvent {
    /// Stable identity of this write. The same key twice is the same event.
    pub idempotency_key: String,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
    /// Content-addressed artifact carrying the bulky, already redacted body.
    pub artifact_ref: Option<String>,
    pub payload: EventPayload,
}

impl SessionEvent {
    pub fn new(idempotency_key: impl Into<String>, payload: EventPayload) -> Self {
        Self {
            idempotency_key: idempotency_key.into(),
            run_id: None,
            agent_id: None,
            artifact_ref: None,
            payload,
        }
    }

    pub fn with_run(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    pub fn with_artifact(mut self, artifact_ref: impl Into<String>) -> Self {
        self.artifact_ref = Some(artifact_ref.into());
        self
    }
}

/// Result of an append. `duplicate` tells a caller that its retry hit an
/// already recorded event — not an error, and not a second entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendedEvent {
    pub event_id: String,
    pub seq: i64,
    pub duplicate: bool,
}

/// One event as stored, decoded back into its typed payload.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub event_id: String,
    pub seq: i64,
    pub kind: EventKind,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
    pub payload: EventPayload,
    pub artifact_ref: Option<String>,
    pub security_relevant: bool,
    pub created_at: String,
}

/// Body of an audit-outbox row: enough context for the main database's audit
/// log to stand on its own, carrying the ALREADY REDACTED payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEnvelope {
    pub event_id: String,
    pub session_id: String,
    pub seq: i64,
    pub kind: String,
    pub created_at: String,
    pub payload: EventPayload,
}

// =============================================================================
// Announcements — how a reader learns that the log grew
// =============================================================================
//
// The timeline is read in two places: the browser's subscription on this node
// and, for a workspace watched from another node, the producer that publishes
// into the mesh stream hub (§12.2). Both read THIS log — there is one producer
// of session events and it is the coordinator (§3) — and neither may poll it in
// a loop, because a poll interval is a latency floor on every keystroke of the
// agent's output and a query on an idle session that runs forever.
//
// So the writer announces. The announcement carries only a watermark: the log
// itself stays the source of truth (§13.3), and a reader that is woken re-reads
// from its own cursor. That ordering matters for `append_in_tx`, whose caller
// commits later — the announcement can arrive before the row is visible, and a
// reader that finds nothing re-reads once after a short grace instead of
// waiting for its next wake-up.

/// Grace before a reader re-reads a watermark it could not see yet. Sized for
/// the gap between `append_in_tx` and its caller's commit, not for a poll.
pub const ANNOUNCE_SETTLE: std::time::Duration = std::time::Duration::from_millis(25);

type Announcers = HashMap<String, watch::Sender<i64>>;

fn announcers() -> &'static Mutex<Announcers> {
    static ANNOUNCERS: OnceLock<Mutex<Announcers>> = OnceLock::new();
    ANNOUNCERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A reader's end of the announcement channel.
pub struct EventSignal {
    rx: watch::Receiver<i64>,
}

impl EventSignal {
    /// Highest `seq` a writer has announced for this session. A value above the
    /// reader's cursor that it cannot yet see in the log means a transaction is
    /// still in flight.
    pub fn announced(&self) -> i64 {
        *self.rx.borrow()
    }

    /// Resolves when a new event was announced.
    ///
    /// `watch` and not `Notify`: it remembers that the value moved, so an
    /// append landing between the reader's drain and its next wait cannot be
    /// missed — with a notification that only wakes parked waiters, exactly
    /// that race loses an event until the next timeout.
    pub async fn changed(&mut self) {
        // An error means every sender was dropped, which cannot happen while
        // the registry holds one; treat it as "nothing more will come" and let
        // the caller's timeout drive.
        if self.rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Subscribe to a session's timeline announcements. Subscribing BEFORE reading
/// history is what makes the handover gapless: an event written between the two
/// leaves a watermark the reader still sees.
pub fn subscribe(session_id: &str) -> EventSignal {
    let mut announcers = announcers().lock().unwrap_or_else(|e| e.into_inner());
    // Sessions come and go; an entry nobody listens to any more is dropped here
    // rather than by a sweeper, which keeps the map the size of what is being
    // watched.
    announcers.retain(|_, tx| tx.receiver_count() > 0);
    let tx = announcers
        .entry(session_id.to_string())
        .or_insert_with(|| watch::channel(0i64).0);
    EventSignal { rx: tx.subscribe() }
}

/// Announce that `seq` was written for `session_id`.
fn announce(session_id: &str, seq: i64) {
    let mut announcers = announcers().lock().unwrap_or_else(|e| e.into_inner());
    let Some(tx) = announcers.get(session_id) else {
        return;
    };
    if tx.receiver_count() == 0 {
        announcers.remove(session_id);
        return;
    }
    tx.send_replace(seq);
}

/// Appends one event in its own transaction.
///
/// The announcement is made AFTER the commit, so a reader woken by it always
/// finds the row. `append_in_tx` cannot offer that — its caller owns the
/// commit — which is why readers tolerate a watermark they cannot see yet.
pub fn append(pool: &DbPool, session_id: &str, event: SessionEvent) -> Result<AppendedEvent> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    let appended = write_event(&tx, session_id, event)?;
    tx.commit()?;
    if !appended.duplicate {
        announce(session_id, appended.seq);
    }
    Ok(appended)
}

/// Appends one event inside a transaction the caller already owns. This is the
/// primitive the operation journal uses: a state change and the event that
/// records it must commit together or not at all (§13.3).
///
/// The announcement leaves here BEFORE the caller commits — this function does
/// not know when that happens. A reader woken by it may therefore not see the
/// row yet and re-reads after `ANNOUNCE_SETTLE`; a transaction that rolls back
/// costs that one wasted read and nothing else, because the log, not the
/// announcement, is what a reader believes.
pub fn append_in_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    event: SessionEvent,
) -> Result<AppendedEvent> {
    let appended = write_event(tx, session_id, event)?;
    if !appended.duplicate {
        announce(session_id, appended.seq);
    }
    Ok(appended)
}

fn write_event(
    tx: &Transaction<'_>,
    session_id: &str,
    event: SessionEvent,
) -> Result<AppendedEvent> {
    if let Some((event_id, seq)) = tx
        .query_row(
            "SELECT event_id, seq FROM session_events \
             WHERE session_id = ?1 AND idempotency_key = ?2",
            rusqlite::params![session_id, event.idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
    {
        return Ok(AppendedEvent {
            event_id,
            seq,
            duplicate: true,
        });
    }

    let payload = event.payload.redacted();
    let kind = payload.kind();
    let security_relevant = payload.security_relevant();
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_events WHERE session_id = ?1",
        rusqlite::params![session_id],
        |row| row.get(0),
    )?;
    let event_id = Uuid::new_v4().to_string();
    let created_at = timestamp();
    let payload_cbor = to_cbor(&payload)?;

    tx.execute(
        "INSERT INTO session_events \
          (event_id, session_id, seq, idempotency_key, schema_version, kind, run_id, agent_id, \
           payload_cbor, artifact_ref, security_relevant, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            event_id,
            session_id,
            seq,
            event.idempotency_key,
            EVENT_SCHEMA_VERSION,
            kind.slug(),
            event.run_id,
            event.agent_id,
            payload_cbor,
            event.artifact_ref,
            i64::from(security_relevant),
            created_at,
        ],
    )?;

    if security_relevant {
        let envelope = AuditEnvelope {
            event_id: event_id.clone(),
            session_id: session_id.to_string(),
            seq,
            kind: kind.slug().to_string(),
            created_at: created_at.clone(),
            payload,
        };
        tx.execute(
            "INSERT INTO audit_outbox (event_id, payload_cbor, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![event_id, to_cbor(&envelope)?, created_at],
        )?;
    }

    Ok(AppendedEvent {
        event_id,
        seq,
        duplicate: false,
    })
}

/// Timeline cursor for the UI: everything after `after_seq`, oldest first.
pub fn read_after(
    pool: &DbPool,
    session_id: &str,
    after_seq: i64,
    limit: usize,
) -> Result<Vec<StoredEvent>> {
    let limit = limit.clamp(1, MAX_READ_LIMIT);
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT event_id, seq, kind, run_id, agent_id, payload_cbor, artifact_ref, \
          security_relevant, created_at \
         FROM session_events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![session_id, after_seq, limit as i64],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Vec<u8>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
            ))
        },
    )?;

    let mut events = Vec::new();
    for row in rows {
        let (event_id, seq, kind, run_id, agent_id, payload_cbor, artifact_ref, security, created) =
            row?;
        let kind = EventKind::from_slug(&kind)
            .ok_or_else(|| anyhow!("event {event_id} has unknown kind '{kind}'"))?;
        events.push(StoredEvent {
            event_id,
            seq,
            kind,
            run_id,
            agent_id,
            payload: from_cbor(&payload_cbor)?,
            artifact_ref,
            security_relevant: security != 0,
            created_at: created,
        });
    }
    Ok(events)
}

/// The reason each finished run recorded, keyed by run id.
///
/// `session_runs` has no column for it, and adding one would give a projection
/// its own copy of a fact the timeline already owns (§13.3). The run list reads
/// it back from here instead, because a run that ends `failed` with the reason
/// only in the database is a run nobody can diagnose without a SQL client.
///
/// The last `run_finished` of a run wins: a run reconciled after a restart
/// settles a second time, and that later verdict is the current one.
pub fn run_failure_reasons(
    pool: &DbPool,
    session_id: &str,
) -> Result<BTreeMap<String, String>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT payload_cbor FROM session_events \
         WHERE session_id = ?1 AND kind = ?2 ORDER BY seq",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![session_id, EventKind::RunFinished.slug()],
        |row| row.get::<_, Vec<u8>>(0),
    )?;
    let mut reasons = BTreeMap::new();
    for row in rows {
        let EventPayload::RunFinished { run_id, error, .. } = from_cbor(&row?)? else {
            continue;
        };
        match error.filter(|text| !text.trim().is_empty()) {
            Some(text) => {
                reasons.insert(run_id, text);
            }
            None => {
                reasons.remove(&run_id);
            }
        }
    }
    Ok(reasons)
}

/// One projection column corrected against the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCorrection {
    /// `session` or `run`.
    pub entity: String,
    pub id: String,
    /// What the cached column said.
    pub projected: String,
    /// What the events say, and what the column now holds.
    pub from_events: String,
}

/// Verifies the cached status columns against the timeline and repairs them
/// (§13.3). Run at coordinator start, before anything reads a status.
///
/// Only the live statuses are arbitrated: `idle`, `running` and `waiting_user`.
/// A session marked `interrupted`, `closing` or `closed` is deliberately left
/// alone — `interrupted` is precisely the statement "a run is open in the
/// timeline and nobody is executing it", so re-deriving `running` from those
/// same events would resurrect a run that has no process behind it.
pub fn verify_projection(pool: &DbPool, session_id: &str) -> Result<Vec<ProjectionCorrection>> {
    let tail = read_all(pool, session_id)?;
    let Some(tail_seq) = tail.last().map(|event| event.seq) else {
        return Ok(Vec::new());
    };

    let mut run_status: BTreeMap<String, String> = BTreeMap::new();
    let mut pending_approvals: BTreeSet<String> = BTreeSet::new();
    for event in &tail {
        match &event.payload {
            EventPayload::RunStarted { run_id, .. } => {
                run_status.insert(run_id.clone(), "running".to_string());
            }
            EventPayload::RunFinished { run_id, status, .. } => {
                run_status.insert(run_id.clone(), status.clone());
            }
            EventPayload::ApprovalRequested { approval_id, .. } => {
                pending_approvals.insert(approval_id.clone());
            }
            EventPayload::ApprovalDecided { approval_id, .. } => {
                pending_approvals.remove(approval_id);
            }
            _ => {}
        }
    }

    let any_run_open = run_status.values().any(|status| status == "running");
    let expected_session = if any_run_open && !pending_approvals.is_empty() {
        Some("waiting_user")
    } else if any_run_open {
        Some("running")
    } else if !run_status.is_empty() {
        Some("idle")
    } else {
        None
    };

    let mut corrections = Vec::new();
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;

    if let Some(expected) = expected_session {
        let current: Option<String> = tx
            .query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(current) = current {
            let arbitrable = matches!(current.as_str(), "idle" | "running" | "waiting_user");
            if arbitrable && current != expected {
                tx.execute(
                    "UPDATE sessions SET status = ?2, updated_at = datetime('now') WHERE id = ?1",
                    rusqlite::params![session_id, expected],
                )?;
                corrections.push(ProjectionCorrection {
                    entity: "session".to_string(),
                    id: session_id.to_string(),
                    projected: current,
                    from_events: expected.to_string(),
                });
            }
        }
    }

    for (run_id, expected) in &run_status {
        let current: Option<String> = tx
            .query_row(
                "SELECT status FROM session_runs WHERE run_id = ?1 AND session_id = ?2",
                rusqlite::params![run_id, session_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else { continue };
        if &current != expected {
            tx.execute(
                "UPDATE session_runs SET status = ?2 WHERE run_id = ?1",
                rusqlite::params![run_id, expected],
            )?;
            corrections.push(ProjectionCorrection {
                entity: "run".to_string(),
                id: run_id.clone(),
                projected: current,
                from_events: expected.clone(),
            });
        }
    }

    for correction in &corrections {
        warn!(
            session_id,
            entity = %correction.entity,
            id = %correction.id,
            projected = %correction.projected,
            from_events = %correction.from_events,
            "projection disagreed with the timeline; the timeline won"
        );
        append_in_tx(
            &tx,
            session_id,
            SessionEvent::new(
                // Keyed by the tail the decision was made on: a re-run over the
                // same timeline is one event, a later drift is a new one.
                format!(
                    "projection:{}:{}:{}:after-{tail_seq}",
                    correction.entity, correction.id, correction.from_events
                ),
                EventPayload::ProjectionCorrected {
                    entity: correction.entity.clone(),
                    id: correction.id.clone(),
                    projected: correction.projected.clone(),
                    from_events: correction.from_events.clone(),
                },
            ),
        )?;
    }

    tx.commit()?;
    Ok(corrections)
}

/// Whole timeline of a session, oldest first. Used by the projection check,
/// which has to see every run of the session, not a page of it.
fn read_all(pool: &DbPool, session_id: &str) -> Result<Vec<StoredEvent>> {
    let mut all = Vec::new();
    let mut cursor = 0i64;
    loop {
        let page = read_after(pool, session_id, cursor, MAX_READ_LIMIT)?;
        let Some(last) = page.last() else { break };
        cursor = last.seq;
        all.extend(page);
    }
    Ok(all)
}

pub(crate) fn to_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).map_err(|e| anyhow!("cbor encode: {e}"))?;
    Ok(bytes)
}

pub(crate) fn from_cbor<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T> {
    ciborium::from_reader(bytes).map_err(|e| anyhow!("cbor decode: {e}"))
}

/// UTC in the exact shape SQLite's `datetime('now')` produces, so a timestamp
/// written from Rust sorts next to one written by a default.
pub(crate) fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::workspace_db;

    fn workspace() -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (pool, _version) = workspace_db::open_pool_at(dir.path()).expect("open workspace.db");
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('s-1', 'ws-1', 'u-1', 'Session', 'cs/u/1', 'normal', 'flow', 'v1', \
                  'idle', datetime('now'), datetime('now'))",
                [],
            )
            .unwrap();
        }
        (dir, pool)
    }

    /// A session of its own. The announcement registry is process-wide, so two
    /// tests sharing an id would wake each other's readers.
    fn session(pool: &DbPool, id: &str) {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
              flow_id, flow_version_id, status, created_at, updated_at) \
             VALUES (?1, 'ws-1', 'u-1', 'Session', 'cs/u/' || ?1, 'normal', 'flow', 'v1', \
              'idle', datetime('now'), datetime('now'))",
            rusqlite::params![id],
        )
        .unwrap();
    }

    fn message(text: &str) -> EventPayload {
        EventPayload::AgentMessage {
            role: "assistant".into(),
            text: text.into(),
        }
    }

    /// The timeline reaches its readers because the WRITER says so. A reader
    /// parked on the signal is released by the append itself — no interval, no
    /// query that runs while nothing is happening.
    #[tokio::test]
    async fn an_append_wakes_a_parked_reader_without_a_poll() {
        let (_dir, pool) = workspace();
        session(&pool, "s-wake");
        let mut signal = subscribe("s-wake");
        assert_eq!(signal.announced(), 0);

        let waiting = tokio::spawn(async move {
            signal.changed().await;
            signal.announced()
        });
        // Nothing has been written, so the reader must still be parked.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!waiting.is_finished(), "an idle session wakes nobody");

        append(&pool, "s-wake", SessionEvent::new("k-1", message("hello"))).unwrap();
        let announced = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .expect("the append must release the reader")
            .expect("task");
        assert_eq!(announced, 1);
        // And the row is already visible: `append` announces after its commit.
        assert_eq!(read_after(&pool, "s-wake", 0, 10).unwrap().len(), 1);
    }

    /// An announcement that lands between a reader's drain and its next wait is
    /// not lost — that race is exactly what a bare notification drops.
    #[tokio::test]
    async fn an_announcement_between_two_waits_is_not_lost() {
        let (_dir, pool) = workspace();
        session(&pool, "s-between");
        let mut signal = subscribe("s-between");
        append(&pool, "s-between", SessionEvent::new("k-1", message("hi"))).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), signal.changed())
            .await
            .expect("the writer announced before the reader waited");
        assert_eq!(signal.announced(), 1);
    }

    /// A retry writes nothing, so it must not wake anybody either.
    #[tokio::test]
    async fn a_duplicate_append_announces_nothing() {
        let (_dir, pool) = workspace();
        session(&pool, "s-dup");
        append(&pool, "s-dup", SessionEvent::new("k-1", message("hi"))).unwrap();
        let mut signal = subscribe("s-dup");
        let appended = append(&pool, "s-dup", SessionEvent::new("k-1", message("hi"))).unwrap();
        assert!(appended.duplicate);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), signal.changed())
                .await
                .is_err(),
            "a duplicate is not a new event"
        );
    }

    #[test]
    fn sequence_numbers_are_dense_unique_and_ordered() {
        let (_dir, pool) = workspace();
        for index in 0..5 {
            let appended = append(
                &pool,
                "s-1",
                SessionEvent::new(format!("k-{index}"), message("hello")),
            )
            .unwrap();
            assert_eq!(appended.seq, index + 1);
            assert!(!appended.duplicate);
        }

        let seqs: Vec<i64> = read_after(&pool, "s-1", 0, 100)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);

        let unique: BTreeSet<i64> = seqs.iter().copied().collect();
        assert_eq!(unique.len(), seqs.len(), "two events shared a seq");

        // The cursor is exclusive: a UI that saw seq 3 gets 4 and 5.
        let tail = read_after(&pool, "s-1", 3, 100).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].seq, 4);
    }

    #[test]
    fn the_same_idempotency_key_twice_is_one_row_and_not_an_error() {
        let (_dir, pool) = workspace();
        let first = append(&pool, "s-1", SessionEvent::new("k-1", message("once"))).unwrap();
        let second = append(&pool, "s-1", SessionEvent::new("k-1", message("once"))).unwrap();

        assert!(!first.duplicate);
        assert!(second.duplicate, "a retry was not recognised");
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.seq, second.seq);
        assert_eq!(read_after(&pool, "s-1", 0, 100).unwrap().len(), 1);
    }

    #[test]
    fn a_security_event_and_its_outbox_row_are_one_transaction() {
        let (_dir, pool) = workspace();
        let payload = EventPayload::SecretAccess {
            secret_ref: "vault:repo-token".into(),
            purpose: "git push".into(),
        };
        append(&pool, "s-1", SessionEvent::new("k-secret", payload.clone())).unwrap();
        {
            let conn = pool.read().unwrap();
            let outbox: i64 = conn
                .query_row("SELECT COUNT(*) FROM audit_outbox", [], |row| row.get(0))
                .unwrap();
            assert_eq!(outbox, 1, "a security event left no audit copy");
        }

        // Force the outbox write to fail. The event must fail with it —
        // an unaudited security event is not an acceptable outcome.
        {
            let conn = pool.write().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER outbox_is_broken BEFORE INSERT ON audit_outbox \
                 BEGIN SELECT RAISE(ABORT, 'outbox unavailable'); END;",
            )
            .unwrap();
        }
        let before = read_after(&pool, "s-1", 0, 100).unwrap().len();
        let failed = append(&pool, "s-1", SessionEvent::new("k-secret-2", payload));
        assert!(failed.is_err(), "the event survived a failed audit write");
        assert_eq!(
            read_after(&pool, "s-1", 0, 100).unwrap().len(),
            before,
            "the event was committed without its audit copy"
        );
    }

    #[test]
    fn an_ordinary_event_writes_no_audit_copy() {
        let (_dir, pool) = workspace();
        append(&pool, "s-1", SessionEvent::new("k-1", message("plain"))).unwrap();
        let conn = pool.read().unwrap();
        let outbox: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox, 0);
    }

    #[test]
    fn a_secret_in_an_event_is_redacted_before_it_reaches_the_row() {
        let (_dir, pool) = workspace();
        let token = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-exec",
                EventPayload::Exec {
                    op_id: "op-1".into(),
                    argv: vec![
                        "git".into(),
                        "push".into(),
                        format!("https://x:{token}@github.com/o/r.git"),
                    ],
                    cwd: "/w".into(),
                    exit_code: Some(0),
                    requested_mount_access: "cow".into(),
                    writes_discarded: true,
                },
            ),
        )
        .unwrap();

        // The stored bytes, not just the decoded value, must be clean.
        {
            let conn = pool.read().unwrap();
            let raw: Vec<u8> = conn
                .query_row(
                    "SELECT payload_cbor FROM session_events WHERE session_id='s-1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let text = String::from_utf8_lossy(&raw);
            assert!(!text.contains(token), "the token was persisted verbatim");
        }

        let stored = read_after(&pool, "s-1", 0, 10).unwrap();
        match &stored[0].payload {
            EventPayload::Exec { argv, .. } => {
                assert!(!argv[2].contains(token));
                assert!(argv[2].contains("github.com/o/r.git"));
            }
            other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn a_redacted_secret_reaches_the_audit_outbox_too() {
        let (_dir, pool) = workspace();
        let token = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-egress",
                EventPayload::Egress {
                    url: format!("https://api.example.com/v1?token={token}"),
                    allowed: false,
                    reason: format!("host not allowlisted, header Authorization: Bearer {token}"),
                },
            ),
        )
        .unwrap();

        let conn = pool.read().unwrap();
        let raw: Vec<u8> = conn
            .query_row("SELECT payload_cbor FROM audit_outbox", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains(token),
            "the audit copy carried the secret the event had removed"
        );
        let envelope: AuditEnvelope = from_cbor(&raw).unwrap();
        assert_eq!(envelope.kind, "egress");
        assert_eq!(envelope.seq, 1);
    }

    #[test]
    fn the_timeline_wins_over_a_stale_status_column() {
        let (_dir, pool) = workspace();
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO session_runs (run_id, session_id, ordinal, kind, trigger, status) \
                 VALUES ('r-1', 's-1', 1, 'root', 'user', 'failed')",
                [],
            )
            .unwrap();
        }
        append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-run-start",
                EventPayload::RunStarted {
                    run_id: "r-1".into(),
                    kind: "root".into(),
                    trigger: "user".into(),
                },
            ),
        )
        .unwrap();

        let corrections = verify_projection(&pool, "s-1").unwrap();
        assert_eq!(corrections.len(), 2, "{corrections:?}");
        assert!(corrections
            .iter()
            .any(|c| c.entity == "session" && c.projected == "idle" && c.from_events == "running"));
        assert!(corrections
            .iter()
            .any(|c| c.entity == "run" && c.projected == "failed" && c.from_events == "running"));

        let conn_status = |sql: &str| -> String {
            let conn = pool.read().unwrap();
            conn.query_row(sql, [], |row| row.get::<_, String>(0))
                .unwrap()
        };
        assert_eq!(
            conn_status("SELECT status FROM sessions WHERE id='s-1'"),
            "running"
        );
        assert_eq!(
            conn_status("SELECT status FROM session_runs WHERE run_id='r-1'"),
            "running"
        );

        // The correction is itself an event, and a second pass is a no-op.
        let events = read_after(&pool, "s-1", 0, 100).unwrap();
        assert!(events
            .iter()
            .any(|e| e.kind == EventKind::ProjectionCorrected));
        assert!(verify_projection(&pool, "s-1").unwrap().is_empty());
    }

    #[test]
    fn a_pending_approval_projects_as_waiting_user_and_a_decision_releases_it() {
        let (_dir, pool) = workspace();
        append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-run",
                EventPayload::RunStarted {
                    run_id: "r-1".into(),
                    kind: "root".into(),
                    trigger: "user".into(),
                },
            ),
        )
        .unwrap();
        append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-ask",
                EventPayload::ApprovalRequested {
                    approval_id: "a-1".into(),
                    capability: "git_push".into(),
                    summary: "push cs/piotr/s1".into(),
                },
            ),
        )
        .unwrap();
        verify_projection(&pool, "s-1").unwrap();
        {
            let conn = pool.read().unwrap();
            let status: String = conn
                .query_row("SELECT status FROM sessions WHERE id='s-1'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "waiting_user");
        }

        append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-decide",
                EventPayload::ApprovalDecided {
                    approval_id: "a-1".into(),
                    decision: "allow_once".into(),
                    decided_by: "u-1".into(),
                },
            ),
        )
        .unwrap();
        let corrections = verify_projection(&pool, "s-1").unwrap();
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].from_events, "running");
    }

    #[test]
    fn an_interrupted_session_is_not_dragged_back_to_running() {
        let (_dir, pool) = workspace();
        append(
            &pool,
            "s-1",
            SessionEvent::new(
                "k-run",
                EventPayload::RunStarted {
                    run_id: "r-1".into(),
                    kind: "root".into(),
                    trigger: "user".into(),
                },
            ),
        )
        .unwrap();
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "UPDATE sessions SET status='interrupted' WHERE id='s-1'",
                [],
            )
            .unwrap();
        }
        assert!(verify_projection(&pool, "s-1").unwrap().is_empty());
        let conn = pool.read().unwrap();
        let status: String = conn
            .query_row("SELECT status FROM sessions WHERE id='s-1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "interrupted");
    }

    #[test]
    fn every_kind_slug_round_trips() {
        for kind in [
            EventKind::RunStarted,
            EventKind::RunFinished,
            EventKind::ToolCall,
            EventKind::ToolResult,
            EventKind::AgentMessage,
            EventKind::ApprovalRequested,
            EventKind::ApprovalDecided,
            EventKind::PatchSetOpened,
            EventKind::PatchDecided,
            EventKind::Exec,
            EventKind::GitOp,
            EventKind::Egress,
            EventKind::SecretAccess,
            EventKind::TicketIssued,
            EventKind::Sandbox,
            EventKind::WorkspaceCreated,
            EventKind::AutonomyChanged,
            EventKind::AllowlistChanged,
            EventKind::MemberAdded,
            EventKind::OperationStarted,
            EventKind::OperationFinished,
            EventKind::OperationReconciled,
            EventKind::ProjectionCorrected,
            EventKind::TestRun,
        ] {
            assert_eq!(EventKind::from_slug(kind.slug()), Some(kind));
        }
        assert_eq!(EventKind::from_slug("not_a_kind"), None);
    }

    /// The cycle of §20 ends in the timeline, so the verdict has to be a kind
    /// of its own: derived from the payload, carried whole through CBOR, and
    /// NOT security-relevant — a runner's counts are not an audit fact, and
    /// mirroring them would dilute the log §13.4 exists for.
    #[test]
    fn a_project_test_result_is_a_timeline_event_of_its_own() {
        let payload = EventPayload::TestRun {
            project_id: "p1".into(),
            run_id: "tr-1".into(),
            commit: "c0ffee".into(),
            passed: 41,
            failed: 1,
            skipped: 2,
            duration_ms: 9_310,
            detail_ref: Some("sha256:abc".into()),
        };
        assert_eq!(payload.kind(), EventKind::TestRun);
        assert_eq!(payload.kind().slug(), "test_run");
        assert!(!payload.security_relevant());
        // Nothing here is free text, so redaction must leave it untouched
        // rather than blank a commit id that looks like a token.
        assert_eq!(payload.clone().redacted(), payload);

        let bytes = to_cbor(&payload).expect("encode");
        let decoded: EventPayload = from_cbor(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }
}
