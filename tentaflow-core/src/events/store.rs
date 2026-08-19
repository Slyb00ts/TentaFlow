// ===== File: events/store.rs — the run timeline: kinds, payloads and the writer =====
//
// Everything a run's UI shows about timing is a DIFFERENCE BETWEEN EVENTS in
// this log (§2.7): TTFT is `request_started` → `first_token`, decoding is
// `first_token` → `assistant_message`, a tool's duration is `tool_call` →
// `tool_result` paired by `call_id`. `flow_executions` and `agent_runs` stay as
// projections — cheap indexes over the same facts — and when they disagree with
// the timeline, the timeline wins (§2.4.3).
//
// Three properties are enforced here rather than left to callers.
//
// **One writer allocates `seq`.** It is taken as `MAX(seq) + 1` INSIDE the
// insert transaction and `PRIMARY KEY (run_id, seq)` turns a second concurrent
// writer into a loud failure instead of a silently interleaved log
// (invariant 2).
//
// **A retry is a no-op, not a duplicate and not an error.** The caller's
// `idempotency_key` is probed first and an existing row comes back as
// `duplicate`. A crash between "the effect happened" and "the event was
// written" therefore resolves by simply writing the event again (§2.4.2).
//
// **Redaction happens before the write** (invariant 3), through
// `code_studio::redact` — the scrubber that already governs every audited
// string in this crate. There is deliberately no second redactor and no
// function here that accepts an environment map: env blocks carry tickets and
// provider keys in bulk and are never logged at all, redacted or not.
//
// **A security-relevant event and its audit copy are ONE transaction** (§2.8).
// If the outbox insert fails the event does not exist either, which is the
// correct direction: an unaudited security event is worse than a retried one.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::code_studio::redact;
use crate::db::DbPool;
use crate::flow_engine::dispatcher::{ActorKind, FlowActor, FlowOrigin, FlowRequestMeta};

/// Largest page `read_run` will return, whatever the caller asks for.
const MAX_READ_LIMIT: usize = 1000;

/// Kind of a timeline entry (§2.3). DERIVED from the payload rather than
/// supplied next to it, so the column and the body can never contradict each
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    RequestStarted,
    FirstToken,
    AssistantMessage,
    ToolCall,
    ToolResult,
    StepStart,
    StepEnd,
    TurnStart,
    TurnEnd,
    Error,
}

impl EventKind {
    /// Stable wire spelling — persisted in `run_events.kind` and rendered by
    /// the UI. Kept identical to the serde tag of the matching payload variant.
    pub fn slug(self) -> &'static str {
        match self {
            EventKind::RequestStarted => "request_started",
            EventKind::FirstToken => "first_token",
            EventKind::AssistantMessage => "assistant_message",
            EventKind::ToolCall => "tool_call",
            EventKind::ToolResult => "tool_result",
            EventKind::StepStart => "step_start",
            EventKind::StepEnd => "step_end",
            EventKind::TurnStart => "turn_start",
            EventKind::TurnEnd => "turn_end",
            EventKind::Error => "error",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        let kind = match slug {
            "request_started" => EventKind::RequestStarted,
            "first_token" => EventKind::FirstToken,
            "assistant_message" => EventKind::AssistantMessage,
            "tool_call" => EventKind::ToolCall,
            "tool_result" => EventKind::ToolResult,
            "step_start" => EventKind::StepStart,
            "step_end" => EventKind::StepEnd,
            "turn_start" => EventKind::TurnStart,
            "turn_end" => EventKind::TurnEnd,
            "error" => EventKind::Error,
            _ => return None,
        };
        Some(kind)
    }
}

/// Body of one timeline entry, stored as `payload_json`.
///
/// Internally tagged with the same `kind` spelling as the column: the row stays
/// self-describing for a reader holding only the JSON, and the tag cannot drift
/// from the column because both come from the variant.
///
/// Fields that the schema already carries as columns — `call_id`, `node_id`,
/// `session_id` — are NOT repeated here. `tool` is named `name` because §2.7's
/// tool-duration query reads it as `payload_json ->> '$.name'`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventPayload {
    /// Opens a run. This is the accountability record of "who asked what, from
    /// where" and the only kind mirrored into `audit_log`.
    RequestStarted {
        model: Option<String>,
        flow_id: Option<String>,
        service_type: Option<String>,
        modality: Option<String>,
    },
    /// First NON-EMPTY delta of a streaming node (§2.6). Carries no body: its
    /// whole content is the instant it happened, and `at_ms` already holds it.
    FirstToken {},
    AssistantMessage {
        text: String,
        /// Absent when the engine reported none — a missing count is a gap in
        /// the log, never a fabricated zero (invariant 6).
        tokens: Option<u64>,
    },
    ToolCall {
        name: String,
        arguments: BTreeMap<String, String>,
    },
    ToolResult {
        ok: bool,
        summary: String,
    },
    StepStart {
        step: String,
    },
    StepEnd {
        step: String,
        status: String,
    },
    TurnStart {
        turn: u32,
    },
    TurnEnd {
        turn: u32,
        status: String,
    },
    Error {
        stage: String,
        message: String,
    },
}

impl EventPayload {
    pub fn kind(&self) -> EventKind {
        match self {
            EventPayload::RequestStarted { .. } => EventKind::RequestStarted,
            EventPayload::FirstToken {} => EventKind::FirstToken,
            EventPayload::AssistantMessage { .. } => EventKind::AssistantMessage,
            EventPayload::ToolCall { .. } => EventKind::ToolCall,
            EventPayload::ToolResult { .. } => EventKind::ToolResult,
            EventPayload::StepStart { .. } => EventKind::StepStart,
            EventPayload::StepEnd { .. } => EventKind::StepEnd,
            EventPayload::TurnStart { .. } => EventKind::TurnStart,
            EventPayload::TurnEnd { .. } => EventKind::TurnEnd,
            EventPayload::Error { .. } => EventKind::Error,
        }
    }

    /// Which entries are mirrored into the main database's `audit_log` (§2.8).
    ///
    /// Only the opening of a run. That row answers the accountability question
    /// — which actor, from which origin, against which model — and carries the
    /// `correlation_id` that turns an audit entry into a link back to this
    /// point on the timeline (the column migration v129 added for exactly that).
    /// The rest of the log is DIAGNOSTIC: mirroring every token and tool result
    /// would multiply the hash-chained audit log by the traffic of the node
    /// while adding nothing an investigator cannot reach through the link.
    pub fn security_relevant(&self) -> bool {
        matches!(self, EventPayload::RequestStarted { .. })
    }

    /// Runs every free-text field through the scrubber. Called by the writer,
    /// so redaction is a property of the write path and not of caller
    /// discipline (invariant 3).
    ///
    /// Enum-typed and bounded fields (`ok`, `turn`, `status`, `kind`) are left
    /// alone: they cannot carry a credential, and scrubbing them would only add
    /// ways for a rule to eat a value the log exists to show.
    pub fn redacted(self) -> Self {
        match self {
            EventPayload::AssistantMessage { text, tokens } => EventPayload::AssistantMessage {
                text: redact::redact_text(&text),
                tokens,
            },
            EventPayload::ToolCall { name, arguments } => EventPayload::ToolCall {
                name,
                arguments: arguments
                    .into_iter()
                    .map(|(key, value)| (key, redact::redact_text(&value)))
                    .collect(),
            },
            EventPayload::ToolResult { ok, summary } => EventPayload::ToolResult {
                ok,
                summary: redact::redact_text(&summary),
            },
            EventPayload::Error { stage, message } => EventPayload::Error {
                stage,
                message: redact::redact_text(&message),
            },
            EventPayload::StepEnd { step, status } => EventPayload::StepEnd {
                step,
                status: redact::redact_text(&status),
            },
            EventPayload::TurnEnd { turn, status } => EventPayload::TurnEnd {
                turn,
                status: redact::redact_text(&status),
            },
            other => other,
        }
    }
}

/// What a caller hands to `append`.
///
/// `origin` and the actor are typed and come from a [`FlowActor`] built by an
/// entry point after authorization — never from `envelope.meta`, which every
/// node including a WASM addon block can rewrite (invariant 1).
#[derive(Debug, Clone)]
pub struct RunEvent {
    /// Identity of the run, and the sequence space `seq` is allocated in.
    pub run_id: String,
    pub at_ms: i64,
    pub origin: FlowOrigin,
    pub actor_kind: ActorKind,
    pub actor_id: Option<String>,
    /// The user behind an API key; `None` marks a service key with no binding,
    /// which the UI shows explicitly rather than as an empty field.
    pub actor_user_id: Option<String>,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
    /// Flow node that produced the entry (§2.6 `FirstToken { node_id }`), not a
    /// mesh node.
    pub node_id: Option<String>,
    /// Pairs a `tool_call` with its `tool_result`. Matching by tool NAME broke
    /// once tools started running in parallel.
    pub call_id: Option<String>,
    /// Stable identity of this write; `None` opts out of deduplication, which
    /// is what an event with no natural key (a token, a message) wants.
    pub idempotency_key: Option<String>,
    /// Tenant the audit COPY belongs to. Never stored in `run_events` — §2.3
    /// has no such column and the timeline is a per-node artefact — but the
    /// `audit_log` row it produces has to land in the right organisation.
    pub org_id: Option<String>,
    pub payload: EventPayload,
}

impl RunEvent {
    pub fn new(
        run_id: impl Into<String>,
        at_ms: i64,
        origin: FlowOrigin,
        actor: &FlowActor,
        payload: EventPayload,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            at_ms,
            origin,
            actor_kind: actor.kind(),
            actor_id: actor.id().map(str::to_string),
            actor_user_id: actor.user_id().map(str::to_string),
            correlation_id: None,
            session_id: None,
            node_id: None,
            call_id: None,
            idempotency_key: None,
            org_id: None,
            payload,
        }
    }

    /// Builds an event from the provenance an entry point already stamped onto
    /// the request. This is the constructor the progress sink uses: it copies
    /// the stamp rather than re-deriving it, so there is no second place where
    /// an origin could be invented.
    pub fn from_meta(meta: &FlowRequestMeta, at_ms: i64, payload: EventPayload) -> Self {
        Self {
            run_id: meta.request_id.clone(),
            at_ms,
            origin: meta.origin,
            actor_kind: meta.actor_kind,
            actor_id: meta.actor_id.clone(),
            actor_user_id: meta.actor_user_id.clone(),
            correlation_id: meta.correlation_id.clone(),
            session_id: meta.session_id.clone(),
            node_id: None,
            call_id: None,
            idempotency_key: None,
            org_id: meta.org_id.clone(),
            payload,
        }
    }

    pub fn with_node(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }

    pub fn with_call(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    pub fn with_correlation(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_org(mut self, org_id: impl Into<String>) -> Self {
        self.org_id = Some(org_id.into());
        self
    }
}

/// Result of an append. `duplicate` tells a caller that its retry hit an
/// already recorded event — not an error, and not a second entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendedEvent {
    pub seq: i64,
    pub duplicate: bool,
}

/// One row as stored, with its payload decoded back into the typed enum.
///
/// `origin` and `actor_kind` stay as the stored slugs: `FlowOrigin` and
/// `ActorKind` expose `as_str` but no inverse, and inventing a second mapping
/// here is exactly the parallel enum that must not exist. The typing that
/// matters is on the WRITE side, where a typo would otherwise reach the disk.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub run_id: String,
    pub seq: i64,
    pub at_ms: i64,
    pub kind: EventKind,
    pub origin: String,
    pub actor_kind: String,
    pub actor_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
    pub node_id: Option<String>,
    pub call_id: Option<String>,
    pub payload: EventPayload,
}

/// Body of an audit-outbox row: everything `audit_log` needs, carrying the
/// ALREADY REDACTED payload.
///
/// Self-contained on purpose — see the schema comment in `db.rs`. The audit
/// copy must survive the retention sweep that removes the event it came from,
/// so it may not reach back into `run_events` for anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEnvelope {
    pub run_id: String,
    pub seq: i64,
    pub kind: String,
    pub at_ms: i64,
    pub created_at: String,
    pub org_id: Option<String>,
    pub origin: String,
    pub actor_kind: String,
    pub actor_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub correlation_id: Option<String>,
    pub session_id: Option<String>,
    pub payload: EventPayload,
}

/// Appends one event in its own transaction.
///
/// IMMEDIATE, not the default deferred: `seq` is read and then written, and a
/// deferred transaction that has already read cannot upgrade behind another
/// writer — it fails with `SQLITE_BUSY` that no busy-timeout can resolve. Under
/// IMMEDIATE two writers on two connections queue on the busy handler and come
/// out with distinct `seq`. The `PRIMARY KEY` stays the backstop for any path
/// that does not go through here.
pub fn append(pool: &DbPool, event: RunEvent) -> Result<AppendedEvent> {
    let mut conn = pool.write().map_err(|e| anyhow!("events db write: {e}"))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let appended = write_event(&tx, event)?;
    tx.commit()?;
    Ok(appended)
}

/// Appends one event inside a transaction the caller already owns, for the
/// callers whose state change and the event recording it must commit together
/// or not at all.
pub fn append_in_tx(tx: &Transaction<'_>, event: RunEvent) -> Result<AppendedEvent> {
    write_event(tx, event)
}

fn write_event(tx: &Transaction<'_>, event: RunEvent) -> Result<AppendedEvent> {
    if let Some(key) = event.idempotency_key.as_deref() {
        if let Some(seq) = tx
            .query_row(
                "SELECT seq FROM run_events WHERE run_id = ?1 AND idempotency_key = ?2",
                rusqlite::params![event.run_id, key],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(AppendedEvent {
                seq,
                duplicate: true,
            });
        }
    }

    let payload = event.payload.redacted();
    let kind = payload.kind();
    let security_relevant = payload.security_relevant();
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE run_id = ?1",
        rusqlite::params![event.run_id],
        |row| row.get(0),
    )?;
    let payload_json = to_json(&payload)?;
    let origin = event.origin.as_str();
    let actor_kind = event.actor_kind.as_str();

    tx.execute(
        "INSERT INTO run_events \
          (run_id, seq, at_ms, kind, origin, actor_kind, actor_id, actor_user_id, \
           correlation_id, session_id, node_id, call_id, payload_json, idempotency_key) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        rusqlite::params![
            event.run_id,
            seq,
            event.at_ms,
            kind.slug(),
            origin,
            actor_kind,
            event.actor_id,
            event.actor_user_id,
            event.correlation_id,
            event.session_id,
            event.node_id,
            event.call_id,
            payload_json,
            event.idempotency_key,
        ],
    )?;

    if security_relevant {
        let envelope = AuditEnvelope {
            run_id: event.run_id.clone(),
            seq,
            kind: kind.slug().to_string(),
            at_ms: event.at_ms,
            created_at: timestamp(),
            org_id: event.org_id.clone(),
            origin: origin.to_string(),
            actor_kind: actor_kind.to_string(),
            actor_id: event.actor_id.clone(),
            actor_user_id: event.actor_user_id.clone(),
            correlation_id: event.correlation_id.clone(),
            session_id: event.session_id.clone(),
            payload,
        };
        tx.execute(
            "INSERT INTO audit_outbox (run_id, seq, payload_json, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                event.run_id,
                seq,
                to_json(&envelope)?,
                envelope.created_at
            ],
        )?;
    }

    Ok(AppendedEvent {
        seq,
        duplicate: false,
    })
}

/// Timeline cursor: everything of one run after `after_seq`, oldest first.
pub fn read_run(
    pool: &DbPool,
    run_id: &str,
    after_seq: i64,
    limit: usize,
) -> Result<Vec<StoredEvent>> {
    let limit = limit.clamp(1, MAX_READ_LIMIT);
    let conn = pool.read().map_err(|e| anyhow!("events db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT run_id, seq, at_ms, kind, origin, actor_kind, actor_id, actor_user_id, \
          correlation_id, session_id, node_id, call_id, payload_json \
         FROM run_events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![run_id, after_seq, limit as i64],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
            ))
        },
    )?;

    let mut events = Vec::new();
    for row in rows {
        let (
            run_id,
            seq,
            at_ms,
            kind,
            origin,
            actor_kind,
            actor_id,
            actor_user_id,
            correlation_id,
            session_id,
            node_id,
            call_id,
            payload_json,
        ) = row?;
        let kind = EventKind::from_slug(&kind)
            .ok_or_else(|| anyhow!("run {run_id} seq {seq} has unknown kind '{kind}'"))?;
        events.push(StoredEvent {
            run_id,
            seq,
            at_ms,
            kind,
            origin,
            actor_kind,
            actor_id,
            actor_user_id,
            correlation_id,
            session_id,
            node_id,
            call_id,
            payload: from_json(&payload_json)?,
        });
    }
    Ok(events)
}

pub(crate) fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| anyhow!("json encode: {e}"))
}

pub(crate) fn from_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
    serde_json::from_str(raw).map_err(|e| anyhow!("json decode: {e}"))
}

/// Wall clock for `at_ms` — epoch milliseconds, the unit §2.3 stores.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// UTC in the exact shape SQLite's `datetime('now')` produces, so a timestamp
/// written from Rust sorts next to one written by a default. This is the format
/// `audit_log.timestamp` is chained over.
pub(crate) fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::test_support::{events_db, open_events_db};

    fn actor() -> FlowActor {
        FlowActor::user("u-1")
    }

    fn event(run_id: &str, payload: EventPayload) -> RunEvent {
        RunEvent::new(run_id, now_ms(), FlowOrigin::Chat, &actor(), payload)
    }

    fn request_started() -> EventPayload {
        EventPayload::RequestStarted {
            model: Some("qwen3".into()),
            flow_id: None,
            service_type: Some("llm".into()),
            modality: Some("text".into()),
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_slug() {
        for kind in [
            EventKind::RequestStarted,
            EventKind::FirstToken,
            EventKind::AssistantMessage,
            EventKind::ToolCall,
            EventKind::ToolResult,
            EventKind::StepStart,
            EventKind::StepEnd,
            EventKind::TurnStart,
            EventKind::TurnEnd,
            EventKind::Error,
        ] {
            assert_eq!(EventKind::from_slug(kind.slug()), Some(kind));
        }
        assert_eq!(EventKind::from_slug("nonexistent"), None);
    }

    /// The serde tag and the `kind` column both come from the variant, so a
    /// payload can never claim to be something the column denies.
    #[test]
    fn the_stored_tag_agrees_with_the_kind_column() {
        let (_dir, pool) = events_db();
        append(&pool, event("r-1", request_started())).unwrap();
        append(
            &pool,
            event(
                "r-1",
                EventPayload::ToolCall {
                    name: "search".into(),
                    arguments: BTreeMap::new(),
                },
            ),
        )
        .unwrap();

        let conn = pool.read().unwrap();
        let mut stmt = conn
            .prepare("SELECT kind, payload_json ->> '$.kind' FROM run_events ORDER BY seq")
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("request_started".to_string(), "request_started".to_string()),
                ("tool_call".to_string(), "tool_call".to_string()),
            ]
        );

        // §2.7 reads the tool name straight out of the payload.
        let tool: String = conn
            .query_row(
                "SELECT payload_json ->> '$.name' FROM run_events WHERE kind = 'tool_call'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tool, "search");
    }

    /// §2.11 stage 2: two parallel writes must produce a loud error, never an
    /// interleave.
    ///
    /// Two THREADS on two independent connections to the same file — going
    /// through one `DbPool` would only demonstrate that a `Mutex` works. Every
    /// write must either land on a `seq` of its own or fail loudly; what may
    /// never happen is two rows sharing a `seq`, or a write reporting success
    /// that the file does not carry.
    #[test]
    fn two_concurrent_writers_never_share_a_seq() {
        let dir = tempfile::tempdir().unwrap();
        // Create the file and run the migration once up front: the race under
        // test is between two APPENDS, not between two schema installs.
        drop(open_events_db(dir.path()));
        let writers = 2usize;
        let per_writer = 25usize;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(writers));

        let handles: Vec<_> = (0..writers)
            .map(|writer| {
                let path = dir.path().to_path_buf();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let pool = open_events_db(&path);
                    barrier.wait();
                    let mut accepted = Vec::new();
                    let mut refused = 0usize;
                    for i in 0..per_writer {
                        let payload = EventPayload::StepStart {
                            step: format!("w{writer}-{i}"),
                        };
                        match append(&pool, event("r-hot", payload)) {
                            Ok(appended) => {
                                assert!(!appended.duplicate);
                                accepted.push(appended.seq);
                            }
                            // A loud refusal is an acceptable outcome; a silent
                            // one is not, which is what the row count below
                            // checks.
                            Err(_) => refused += 1,
                        }
                    }
                    (accepted, refused)
                })
            })
            .collect();

        let mut accepted = Vec::new();
        let mut refused = 0usize;
        for handle in handles {
            let (seqs, failures) = handle.join().expect("writer thread panicked");
            accepted.extend(seqs);
            refused += failures;
        }

        let mut sorted = accepted.clone();
        sorted.sort_unstable();
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(
            sorted, deduped,
            "two accepted writes were handed the same seq: {sorted:?}"
        );

        let pool = open_events_db(dir.path());
        let conn = pool.read().unwrap();
        let (rows, distinct, max_seq): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COUNT(DISTINCT seq), COALESCE(MAX(seq), 0) \
                 FROM run_events WHERE run_id = 'r-hot'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(rows, distinct, "the log interleaved: {rows} rows, {distinct} distinct seq");
        assert_eq!(
            rows,
            accepted.len() as i64,
            "{} write(s) reported success without reaching the file ({refused} refused loudly)",
            accepted.len() as i64 - rows
        );
        // A gapless sequence is the whole point of allocating inside the
        // transaction: MAX(seq) may not run ahead of the number of rows.
        assert_eq!(max_seq, rows, "seq allocation left a gap");
    }

    /// §2.4.2 — a repeat under the same key is a no-op returning `duplicate`.
    #[test]
    fn a_repeat_under_the_same_key_is_a_duplicate_and_writes_nothing() {
        let (_dir, pool) = events_db();
        let first = append(
            &pool,
            event("r-1", request_started()).with_idempotency_key("start:r-1"),
        )
        .unwrap();
        assert!(!first.duplicate);

        let second = append(
            &pool,
            event("r-1", request_started()).with_idempotency_key("start:r-1"),
        )
        .unwrap();
        assert!(second.duplicate, "the retry was not recognised");
        assert_eq!(second.seq, first.seq, "the retry pointed at a different row");

        // Scoped: for a single-connection `Db` a read guard IS the writer
        // mutex, so holding it across the append below would deadlock.
        {
            let conn = pool.read().unwrap();
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM run_events", [], |row| row.get(0))
                .unwrap();
            assert_eq!(rows, 1, "the retry inserted a second row");
        }

        // The key scopes to the run, not to the file: the same natural key in
        // another run is a different event.
        let other = append(
            &pool,
            event("r-2", request_started()).with_idempotency_key("start:r-1"),
        )
        .unwrap();
        assert!(!other.duplicate);
    }

    /// An event with no natural key opts out of deduplication — a token or a
    /// message is not a retryable effect and must not be collapsed.
    #[test]
    fn events_without_a_key_are_never_deduplicated() {
        let (_dir, pool) = events_db();
        for _ in 0..3 {
            append(&pool, event("r-1", EventPayload::FirstToken {})).unwrap();
        }
        let conn = pool.read().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM run_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 3);
    }

    /// Invariant 3. Asserted against the COLUMN, read back off disk — an
    /// assertion on the in-memory struct would prove only that a function was
    /// called, not that the file is clean.
    #[test]
    fn secrets_are_redacted_before_the_write_not_after_the_read() {
        let (_dir, pool) = events_db();
        let bearer = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        let password = "S3cretPassw0rdValue";
        let query_token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

        append(
            &pool,
            event(
                "r-1",
                EventPayload::AssistantMessage {
                    text: format!("calling with Authorization: Bearer {bearer}"),
                    tokens: Some(12),
                },
            ),
        )
        .unwrap();
        append(
            &pool,
            event(
                "r-1",
                EventPayload::ToolCall {
                    name: "http_get".into(),
                    arguments: BTreeMap::from([
                        (
                            "url".to_string(),
                            format!("https://svc:{password}@internal.example.com/v1"),
                        ),
                        (
                            "callback".to_string(),
                            format!("https://api.example.com/v1?token={query_token}"),
                        ),
                    ]),
                },
            ),
        )
        .unwrap();
        append(
            &pool,
            event(
                "r-1",
                EventPayload::Error {
                    stage: "llm".into(),
                    message: format!("upstream rejected Bearer {bearer}"),
                },
            ),
        )
        .unwrap();

        let stored: String = {
            let conn = pool.read().unwrap();
            let mut stmt = conn
                .prepare("SELECT payload_json FROM run_events ORDER BY seq")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<String>>>()
                .unwrap()
                .join("\n")
        };

        for secret in [bearer, password, query_token] {
            assert!(
                !stored.contains(secret),
                "the log kept a credential on disk: {stored}"
            );
        }
        assert!(
            stored.contains(redact::REDACTED),
            "nothing was redacted at all: {stored}"
        );
        // The surrounding text must survive — a scrubber that eats the whole
        // line makes the log useless for the diagnosis it exists for.
        assert!(stored.contains("http_get"), "{stored}");
        assert!(stored.contains("internal.example.com"), "{stored}");
    }

    #[test]
    fn a_run_reads_back_in_order_with_its_provenance() {
        let (_dir, pool) = events_db();
        let actor = FlowActor::api_key("key-7", Some("u-9".into()));
        append(
            &pool,
            RunEvent::new("r-1", 1_000, FlowOrigin::Api, &actor, request_started())
                .with_correlation("corr-1")
                .with_session("s-1"),
        )
        .unwrap();
        append(
            &pool,
            RunEvent::new("r-1", 1_050, FlowOrigin::Api, &actor, EventPayload::FirstToken {})
                .with_node("llm-1"),
        )
        .unwrap();

        let events = read_run(&pool, "r-1", 0, 100).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[0].kind, EventKind::RequestStarted);
        assert_eq!(events[0].origin, "api");
        assert_eq!(events[0].actor_kind, "api_key");
        assert_eq!(events[0].actor_id.as_deref(), Some("key-7"));
        assert_eq!(events[0].actor_user_id.as_deref(), Some("u-9"));
        assert_eq!(events[0].correlation_id.as_deref(), Some("corr-1"));
        assert_eq!(events[1].kind, EventKind::FirstToken);
        assert_eq!(events[1].node_id.as_deref(), Some("llm-1"));
        assert_eq!(events[1].at_ms, 1_050);

        assert_eq!(read_run(&pool, "r-1", 1, 100).unwrap().len(), 1);
    }

    /// A service key must stay distinguishable from a key bound to a user —
    /// §2.5 says the UI shows that gap explicitly rather than as an empty
    /// field, which it cannot do if the two collapse in storage.
    #[test]
    fn a_service_key_is_stored_as_a_null_binding_not_as_an_empty_one() {
        let (_dir, pool) = events_db();
        let service = FlowActor::api_key("key-svc", None);
        append(
            &pool,
            RunEvent::new("r-1", 1, FlowOrigin::Api, &service, request_started()),
        )
        .unwrap();
        let conn = pool.read().unwrap();
        let bound: Option<String> = conn
            .query_row("SELECT actor_user_id FROM run_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(bound, None);
    }

    /// Invariant 4. `run_events` cannot be swept into the Sync Ledger for two
    /// independent reasons, and both are pinned here: it has no descriptor, and
    /// the ledger only ever reads the tables it holds descriptors for — every
    /// one of them out of the MAIN pool (`repository::reseed_core_state_from_
    /// current_rows`, `ensure_default_core_sync_policies`). A table in a
    /// different file is not something the ledger can reach even by mistake.
    #[test]
    fn run_events_stays_out_of_the_sync_ledger() {
        use crate::sync::core_registry::{descriptor_for_table, is_core_sync_table};
        for table in ["run_events", "audit_outbox", "events_schema_version"] {
            assert!(
                !is_core_sync_table(table),
                "{table} must stay out of core sync"
            );
            assert!(descriptor_for_table(table).is_none());
        }
    }
}
