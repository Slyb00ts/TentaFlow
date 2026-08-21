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
// insert transaction, and the two halves of invariant 2 do different jobs.
// The IMMEDIATE transaction is what makes concurrent writers safe: they QUEUE
// on the busy handler and each comes out with a `seq` of its own, so nothing is
// refused and nothing interleaves. `PRIMARY KEY (run_id, seq)` is the backstop
// for a writer that allocated OUTSIDE its transaction — its snapshot is fresh,
// SQLite has nothing to complain about, and only the constraint stops it from
// filing a second row under a `seq` that is already taken.
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
use crate::db::{repository, DbPool};
use crate::flow_engine::dispatcher::{ActorKind, FlowActor, FlowOrigin, FlowRequestMeta};

/// Largest page ANY reader of `run_events` will return, whatever the caller
/// asks for — `read_run` and the browse query share it, so no entry point can
/// turn a page request into a full dump of the log.
pub const MAX_READ_LIMIT: usize = 1000;

/// Prefix of the `settings` key that opts ONE organisation in to storing
/// assistant response bodies on the timeline. The full key is
/// `events.store_assistant_body:<org_id>` — the same key-per-tenant shape the
/// main database already uses for `sync.permission_epoch:<org_id>`, in the same
/// table, read through the same [`repository::get_setting`].
///
/// Deliberately absent from `repository::SHARED_SETTING_KEYS`: whether a node
/// keeps response bodies is a property of THAT node's storage, and a fleet-wide
/// replication of the flag would turn one admin's opt-in into everybody's.
pub const ASSISTANT_BODY_SETTING_PREFIX: &str = "events.store_assistant_body:";

/// The `settings` key that governs `org_id`.
pub fn assistant_body_setting_key(org_id: &str) -> String {
    format!("{ASSISTANT_BODY_SETTING_PREFIX}{org_id}")
}

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
        /// The model's answer, or the reason it is not here. Never a bare
        /// string: the body is stored only for an organisation that asked for
        /// it, and an omission has to READ as an omission (invariant 6).
        body: ResponseBody,
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

/// The body half of an `assistant_message` row: either the answer, or a named
/// reason the answer is not stored.
///
/// One field, two variants, so the two states cannot both be half-set. Serde
/// writes them as `{"text":"..."}` and `{"omitted":"not_enabled"}`, which is
/// what makes the omission VISIBLE to a reader — an empty string would be
/// indistinguishable from a model that answered with nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseBody {
    /// The response as the model produced it, redacted like every other free
    /// text in the log.
    Text(String),
    /// The response was NOT written to disk, and why.
    Omitted(BodyOmission),
}

impl ResponseBody {
    /// The stored answer, or `None` when the body was omitted. A reader that
    /// wants the reason matches on the enum instead.
    pub fn text(&self) -> Option<&str> {
        match self {
            ResponseBody::Text(text) => Some(text),
            ResponseBody::Omitted(_) => None,
        }
    }
}

/// Why an assistant response body is absent from a row.
///
/// Both values mean "this node was not asked to keep it", and they are kept
/// apart because the fix differs: one organisation has to flip its setting, the
/// other has no organisation to flip it for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyOmission {
    /// The run's organisation has not set `events.store_assistant_body:<org>`.
    /// This is the default state of every organisation on every node.
    NotEnabled,
    /// The run named no organisation (a camera, scheduler or maintenance
    /// trigger), so there is no tenant whose opt-in could apply. Falling back
    /// to another tenant's setting would be borrowing a consent nobody gave.
    NoOrganisation,
    /// The event this row was translated from did not carry the answer. The
    /// engine's progress stream announces that a streaming node FINISHED, never
    /// what it produced, so a row built from it can say when the message
    /// completed and nothing about what it said (§2.6). Kept apart from
    /// `NotEnabled` because no setting changes it: only a writer that holds the
    /// text can, and storing an empty string instead would read as a model that
    /// answered with nothing (invariant 6).
    NotCarried,
}

/// Resolves whether `org_id` opted in to storing response bodies (§2.8: the
/// event log is read more widely than the compliance tables built to hold
/// prompt and response bodies under policy, and the credential-shaped redactor
/// does nothing about ordinary personal data inside an answer).
///
/// Default OFF, and fail-closed: a node that has never been configured, an
/// organisation that has not set the key, an unparsable value or a main
/// database that cannot be read all resolve to `Omitted`. The only way to a
/// stored body is an explicit `true`/`1` under that organisation's own key.
fn resolve_response_body(
    core_db: &DbPool,
    org_id: Option<&str>,
    body: ResponseBody,
) -> ResponseBody {
    // A body that never reached the writer cannot be stored by any policy, and
    // the reason it is missing is the more specific fact of the two.
    if matches!(body, ResponseBody::Omitted(_)) {
        return body;
    }
    let Some(org_id) = org_id else {
        return ResponseBody::Omitted(BodyOmission::NoOrganisation);
    };
    let enabled = match repository::get_setting(core_db, &assistant_body_setting_key(org_id)) {
        Ok(value) => value.is_some_and(|v| v == "true" || v == "1"),
        Err(error) => {
            // A settings read that failed is not an opt-in. Logged rather than
            // propagated: the event itself must still reach the timeline.
            tracing::warn!(
                org_id,
                %error,
                "event log could not read the response-body setting; body omitted"
            );
            false
        }
    };
    if enabled {
        body
    } else {
        ResponseBody::Omitted(BodyOmission::NotEnabled)
    }
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
    /// Free text is everything a caller can put a credential into: a message,
    /// a tool argument, a result summary, an error message, and the `status`
    /// strings, which the engine copies from a node's own outcome. Left alone
    /// are the fields that cannot carry one — `ok`, `turn` and the `kind` tag —
    /// and the server-minted labels `step`, `stage`, `name` and the
    /// `RequestStarted` descriptors: scrubbing those would only add ways for a
    /// rule to eat a value the log exists to show.
    pub fn redacted(self) -> Self {
        match self {
            EventPayload::AssistantMessage { body, tokens } => EventPayload::AssistantMessage {
                body: match body {
                    ResponseBody::Text(text) => ResponseBody::Text(redact::redact_text(&text)),
                    omitted @ ResponseBody::Omitted(_) => omitted,
                },
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
            // Listed one by one rather than caught by an `other => other` arm:
            // the arm that costs least to write is the one that would let a
            // variant added later carry free text past the scrubber without a
            // compiler error. Everything below has no field a credential fits
            // in — server-minted labels, a turn number, or no body at all.
            payload @ (EventPayload::RequestStarted { .. }
            | EventPayload::FirstToken {}
            | EventPayload::StepStart { .. }
            | EventPayload::TurnStart { .. }) => payload,
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
    /// Tenant this run belongs to. Stored on the timeline row AND carried into
    /// the audit copy: retention terms are resolved per organisation, so a row
    /// that cannot name its tenant cannot be purged on that tenant's term
    /// (§2.9). Server-minted like `origin` and the actor — it comes from
    /// `FlowRequestMeta.org_id`, which the entry points fill from
    /// `CallerContext.org_id` or from an already membership-checked project,
    /// never from `envelope.meta` (invariant 1).
    ///
    /// `None` means no organisation was minted for this run — a camera,
    /// scheduler or maintenance trigger. It is written as NULL and NOT
    /// substituted with the default tenant: a guessed owner is a fabricated
    /// fact (invariant 6).
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
    /// Tenant the row belongs to; `None` = no organisation was minted for the
    /// run, which the browser shows as such rather than as a tenant.
    pub org_id: Option<String>,
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
/// out with distinct `seq`; neither is refused. The `PRIMARY KEY` stays the
/// backstop for any path that allocates `seq` outside its own transaction.
///
/// `core_db` is the MAIN database, and the only thing it is read for is the
/// per-organisation response-body setting. It is a parameter rather than a
/// global so the decision belongs to the writer that has one in hand, and so a
/// test can hand it an organisation of its own.
pub fn append(pool: &DbPool, core_db: &DbPool, event: RunEvent) -> Result<AppendedEvent> {
    let mut conn = pool.write().map_err(|e| anyhow!("events db write: {e}"))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let appended = write_event(&tx, core_db, event)?;
    tx.commit()?;
    Ok(appended)
}

/// Appends one event inside a transaction the caller already owns, for the
/// callers whose state change and the event recording it must commit together
/// or not at all.
pub fn append_in_tx(
    tx: &Transaction<'_>,
    core_db: &DbPool,
    event: RunEvent,
) -> Result<AppendedEvent> {
    write_event(tx, core_db, event)
}

/// The one place a row is built, and therefore the one place the response-body
/// policy can be applied. It runs BEFORE the insert on purpose: a body filtered
/// at read time would already be on disk, which is the whole thing the opt-in
/// exists to prevent.
fn write_event(
    tx: &Transaction<'_>,
    core_db: &DbPool,
    event: RunEvent,
) -> Result<AppendedEvent> {
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

    // The event is recorded either way — the timeline still has to say that an
    // assistant message happened and when, which is what the decode-time metric
    // is a difference of. Only the BODY is subject to the opt-in.
    let payload = match event.payload {
        EventPayload::AssistantMessage { body, tokens } => EventPayload::AssistantMessage {
            body: resolve_response_body(core_db, event.org_id.as_deref(), body),
            tokens,
        },
        other => other,
    };
    let payload = payload.redacted();
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
           org_id, correlation_id, session_id, node_id, call_id, payload_json, \
           idempotency_key) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        rusqlite::params![
            event.run_id,
            seq,
            event.at_ms,
            kind.slug(),
            origin,
            actor_kind,
            event.actor_id,
            event.actor_user_id,
            event.org_id,
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

/// The columns every reader of `run_events` selects, in the order
/// [`read_stored_row`] expects them. One constant so the browse query and the
/// per-run cursor cannot drift into different column orders — the kind of bug
/// that turns a `node_id` into a `call_id` silently.
pub(super) const STORED_COLUMNS: &str = "run_id, seq, at_ms, kind, origin, actor_kind, actor_id, \
     actor_user_id, org_id, correlation_id, session_id, node_id, call_id, payload_json";

/// Raw column tuple, before `kind` and `payload_json` are parsed.
///
/// Split from the parsing because `query_map` may only fail with a
/// `rusqlite::Error`, while an unreadable kind or payload is OUR error with our
/// message — and a row that cannot be parsed must name the run and seq it came
/// from, which is only knowable after the columns are read.
pub(super) type StoredRow = (
    String,
    i64,
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

pub(super) fn read_stored_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    ))
}

/// Turns a raw row into a [`StoredEvent`], REFUSING a `kind` slug this build
/// does not know rather than guessing one. A stored value we cannot read is a
/// gap in the log, and a fallback would report it as some other kind of event.
pub(super) fn decode_stored_row(raw: StoredRow) -> Result<StoredEvent> {
    let (
        run_id,
        seq,
        at_ms,
        kind,
        origin,
        actor_kind,
        actor_id,
        actor_user_id,
        org_id,
        correlation_id,
        session_id,
        node_id,
        call_id,
        payload_json,
    ) = raw;
    let kind = EventKind::from_slug(&kind)
        .ok_or_else(|| anyhow!("run {run_id} seq {seq} has unknown kind '{kind}'"))?;
    Ok(StoredEvent {
        run_id,
        seq,
        at_ms,
        kind,
        origin,
        actor_kind,
        actor_id,
        actor_user_id,
        org_id,
        correlation_id,
        session_id,
        node_id,
        call_id,
        payload: from_json(&payload_json)?,
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
    let mut stmt = conn.prepare(&format!(
        "SELECT {STORED_COLUMNS} FROM run_events \
         WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
    ))?;
    let rows = stmt.query_map(
        rusqlite::params![run_id, after_seq, limit as i64],
        read_stored_row,
    )?;

    let mut events = Vec::new();
    for row in rows {
        events.push(decode_stored_row(row?)?);
    }
    Ok(events)
}

/// Which principal a run belongs to, for the browser's per-run ACL.
///
/// `Ok(None)` = no such run on this node. `Ok(Some(None))` = the run exists but
/// names no user (a camera, the scheduler, an unbound service key), which is
/// NOT the same as "belongs to the caller" and must not be read as one.
///
/// Answered from the FIRST event of the run: `request_started` is what an entry
/// point stamps the actor onto, and the primary key `(run_id, seq)` makes that
/// lookup a single index seek.
pub fn run_actor_user_id(pool: &DbPool, run_id: &str) -> Result<Option<Option<String>>> {
    let conn = pool.read().map_err(|e| anyhow!("events db read: {e}"))?;
    conn.query_row(
        "SELECT actor_user_id FROM run_events WHERE run_id = ?1 ORDER BY seq LIMIT 1",
        rusqlite::params![run_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map_err(|e| anyhow!("events db read: {e}"))
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
    use crate::events::test_support::{events_db, main_db, open_events_db};

    fn actor() -> FlowActor {
        FlowActor::user("u-1")
    }

    fn event(run_id: &str, payload: EventPayload) -> RunEvent {
        RunEvent::new(run_id, now_ms(), FlowOrigin::Chat, &actor(), payload)
    }

    /// Turns the response-body opt-in ON for `org_id`, exactly the way an
    /// administrator does: one row in the main database's `settings` table
    /// under that organisation's own key.
    fn enable_response_bodies(main: &DbPool, org_id: &str) {
        repository::set_setting(main, &assistant_body_setting_key(org_id), "true")
            .expect("write the response-body setting");
    }

    fn assistant(text: &str) -> EventPayload {
        EventPayload::AssistantMessage {
            body: ResponseBody::Text(text.to_string()),
            tokens: Some(12),
        }
    }

    /// The `body` object exactly as it reached the file.
    fn stored_body(pool: &DbPool, run_id: &str) -> String {
        let conn = pool.read().unwrap();
        conn.query_row(
            "SELECT payload_json ->> '$.body' FROM run_events \
             WHERE run_id = ?1 AND kind = 'assistant_message'",
            rusqlite::params![run_id],
            |row| row.get(0),
        )
        .expect("the assistant message reached the log")
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
        let main = main_db();
        append(&pool, &main, event("r-1", request_started())).unwrap();
        append(
            &pool,
            &main,
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

    /// Invariant 2, first half: the IMMEDIATE transaction the append path opens.
    ///
    /// Two THREADS on two independent connections to the same file — going
    /// through one `DbPool` would only demonstrate that a `Mutex` works. Every
    /// write must land on a `seq` of its own, and NONE may be refused: under
    /// IMMEDIATE the second writer queues on the busy handler and allocates
    /// after the first commits. A refusal here means the allocation no longer
    /// happens inside the transaction — two writers then compute the same `seq`
    /// and one of them dies on the primary key.
    ///
    /// §2.11 stage 2 words the acceptance criterion as "a loud error, not an
    /// interleave". Queuing is the stronger outcome of the two — no writer is
    /// turned away and no timeline loses a row — so the assertion below pins
    /// what the code actually guarantees.
    #[test]
    fn two_concurrent_writers_queue_and_never_share_a_seq() {
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
                    let main = main_db();
                    barrier.wait();
                    let mut accepted = Vec::new();
                    let mut refusals = Vec::new();
                    for i in 0..per_writer {
                        let payload = EventPayload::StepStart {
                            step: format!("w{writer}-{i}"),
                        };
                        match append(&pool, &main, event("r-hot", payload)) {
                            Ok(appended) => {
                                assert!(!appended.duplicate);
                                accepted.push(appended.seq);
                            }
                            Err(error) => refusals.push(error.to_string()),
                        }
                    }
                    (accepted, refusals)
                })
            })
            .collect();

        let mut accepted = Vec::new();
        let mut refusals: Vec<String> = Vec::new();
        for handle in handles {
            let (seqs, failures) = handle.join().expect("writer thread panicked");
            accepted.extend(seqs);
            refusals.extend(failures);
        }

        assert!(
            refusals.is_empty(),
            "a concurrent writer was refused instead of queued — the append path is not \
             allocating seq inside its IMMEDIATE transaction: {refusals:?}"
        );
        assert_eq!(
            accepted.len(),
            writers * per_writer,
            "not every write was accepted"
        );

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
            "{} write(s) reported success without reaching the file",
            accepted.len() as i64 - rows
        );
        // A gapless sequence is the whole point of allocating inside the
        // transaction: MAX(seq) may not run ahead of the number of rows.
        assert_eq!(max_seq, rows, "seq allocation left a gap");
    }

    /// Invariant 2, second half: the uniqueness constraint, on the path where
    /// the IMMEDIATE transaction is NOT what protects the sequence.
    ///
    /// A writer that allocates `seq` before opening its transaction sees a
    /// FRESH snapshot when it finally writes, so SQLite has no conflict to
    /// report — a stale-but-plausible `seq` would simply land, and the run
    /// would carry two rows at the same point on its timeline. The primary key
    /// is the only thing standing there, and it is exercised here through a
    /// caller-owned deferred transaction, exactly the shape `append_in_tx`
    /// accepts from callers whose isolation the store does not control.
    #[test]
    fn a_seq_allocated_outside_the_transaction_is_refused_by_the_constraint() {
        let dir = tempfile::tempdir().unwrap();
        drop(open_events_db(dir.path()));
        let main = main_db();
        let stale_writer = open_events_db(dir.path());

        // The allocation a hoisted `MAX(seq) + 1` would produce: read before
        // anything else writes, and outside any transaction of its own.
        let stale_seq: i64 = {
            let conn = stale_writer.read().unwrap();
            conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE run_id = 'r-1'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        let winner = open_events_db(dir.path());
        let taken = append(&winner, &main, event("r-1", request_started())).unwrap();
        assert_eq!(taken.seq, stale_seq, "the two writers did not race for one seq");

        let error = {
            let mut conn = stale_writer.write().unwrap();
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
                .unwrap();
            tx.execute(
                "INSERT INTO run_events \
                  (run_id, seq, at_ms, kind, origin, actor_kind, payload_json) \
                 VALUES ('r-1', ?1, 1, 'first_token', 'chat', 'user', '{\"kind\":\"first_token\"}')",
                rusqlite::params![stale_seq],
            )
            .expect_err("a seq that is already taken must not be insertable")
        };
        assert!(
            error.to_string().contains("UNIQUE constraint failed"),
            "the write was refused for the wrong reason: {error}"
        );

        let (rows, distinct): (i64, i64) = {
            let conn = stale_writer.read().unwrap();
            conn.query_row(
                "SELECT COUNT(*), COUNT(DISTINCT seq) FROM run_events WHERE run_id = 'r-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!((rows, distinct), (1, 1), "the run kept two rows at one seq");
    }

    /// §2.4.2 — a repeat under the same key is a no-op returning `duplicate`.
    #[test]
    fn a_repeat_under_the_same_key_is_a_duplicate_and_writes_nothing() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let first = append(
            &pool,
            &main,
            event("r-1", request_started()).with_idempotency_key("start:r-1"),
        )
        .unwrap();
        assert!(!first.duplicate);

        let second = append(
            &pool,
            &main,
            event("r-1", request_started()).with_idempotency_key("start:r-1"),
        )
        .unwrap();
        assert!(second.duplicate, "the retry was not recognised");
        assert_eq!(second.seq, first.seq, "the retry pointed at a different row");

        // The log has a READ POOL, so this guard is a connection of its own and
        // stays held across the append below. It used to have to be scoped:
        // with no pool `read()` handed back the writer mutex and the append
        // would have deadlocked against it.
        let conn = pool.read().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM run_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the retry inserted a second row");

        // The key scopes to the run, not to the file: the same natural key in
        // another run is a different event.
        let other = append(
            &pool,
            &main,
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
        let main = main_db();
        for _ in 0..3 {
            append(&pool, &main, event("r-1", EventPayload::FirstToken {})).unwrap();
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
        let main = main_db();
        let bearer = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        let password = "S3cretPassw0rdValue";
        let query_token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

        // The response body is stored only for an organisation that asked for
        // it, so this run belongs to one — a body that was never written is no
        // test of the scrubber.
        enable_response_bodies(&main, "org-red");
        append(
            &pool,
            &main,
            event(
                "r-1",
                assistant(&format!("calling with Authorization: Bearer {bearer}")),
            )
            .with_org("org-red"),
        )
        .unwrap();
        append(
            &pool,
            &main,
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
            &main,
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
        let main = main_db();
        let actor = FlowActor::api_key("key-7", Some("u-9".into()));
        append(
            &pool,
            &main,
            RunEvent::new("r-1", 1_000, FlowOrigin::Api, &actor, request_started())
                .with_correlation("corr-1")
                .with_session("s-1"),
        )
        .unwrap();
        append(
            &pool,
            &main,
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
        let main = main_db();
        let service = FlowActor::api_key("key-svc", None);
        append(
            &pool,
            &main,
            RunEvent::new("r-1", 1, FlowOrigin::Api, &service, request_started()),
        )
        .unwrap();
        let conn = pool.read().unwrap();
        let bound: Option<String> = conn
            .query_row("SELECT actor_user_id FROM run_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(bound, None);
    }

    /// §2.9 needs every row attributable to a tenant, so the organisation has
    /// to be ON THE ROW and not only in the audit copy. Read back with SQL:
    /// asserting the in-memory struct would prove a field exists, not that the
    /// file carries it.
    ///
    /// The value is taken from `FlowRequestMeta` — server-minted at the entry
    /// point (invariant 1) — and a run with no organisation stays NULL rather
    /// than borrowing the default tenant (invariant 6).
    #[test]
    fn the_tenant_is_stored_on_the_row_and_stays_null_when_there_is_none() {
        let (_dir, pool) = events_db();
        let main = main_db();

        let mut meta = FlowRequestMeta::new("r-tenant", FlowOrigin::Api, FlowActor::user("u-1"));
        meta.org_id = Some("org-acme".to_string());
        append(
            &pool,
            &main,
            RunEvent::from_meta(&meta, now_ms(), request_started()),
        )
        .unwrap();

        // A camera / scheduler run: no organisation was minted for it.
        let system_meta =
            FlowRequestMeta::new("r-system", FlowOrigin::Camera, FlowActor::system());
        assert!(system_meta.org_id.is_none());
        append(
            &pool,
            &main,
            RunEvent::from_meta(&system_meta, now_ms(), request_started()),
        )
        .unwrap();

        let conn = pool.read().unwrap();

        // The reader surfaces it too — the browser filters by tenant. `read_run`
        // takes a read guard of its OWN while this one is still held, which the
        // read pool makes possible; before it, both were the writer mutex and
        // the second one deadlocked.
        let stored = read_run(&pool, "r-tenant", 0, 10).unwrap();
        assert_eq!(stored[0].org_id.as_deref(), Some("org-acme"));

        let rows: Vec<(String, Option<String>)> = {
            let mut stmt = conn
                .prepare("SELECT run_id, org_id FROM run_events ORDER BY run_id")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                ("r-system".to_string(), None),
                ("r-tenant".to_string(), Some("org-acme".to_string())),
            ],
            "the timeline row did not keep its tenant"
        );

        // And the audit copy still carries the same tenant, from the same field.
        let envelope_org: Option<String> = conn
            .query_row(
                "SELECT payload_json ->> '$.org_id' FROM audit_outbox WHERE run_id = 'r-tenant'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(envelope_org.as_deref(), Some("org-acme"));
    }

    /// A browser query and an append have to be able to overlap.
    ///
    /// `events.db` is opened with a READ POOL, so `read()` hands out a
    /// connection of its own; the guard below is held for the whole append.
    /// With no pool `read()` returns the WRITER MUTEX, and this append would
    /// wait for the reader to let go — which is exactly what `read_run` does
    /// while it decodes up to 1000 payloads, and what the retention sweep does
    /// while it deletes per tenant.
    ///
    /// The append runs on another thread ONLY so a regression fails this test
    /// instead of hanging it: without the pool the two guards deadlock, and a
    /// deadlock reports nothing.
    #[test]
    fn an_append_does_not_wait_for_a_held_read() {
        let (_dir, pool) = events_db();
        let main = main_db();
        append(&pool, &main, event("r-1", request_started())).unwrap();

        let reader = pool.read().unwrap();
        let before: i64 = reader
            .query_row("SELECT COUNT(*) FROM run_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 1);

        let (tx, rx) = std::sync::mpsc::channel();
        let writer_pool = pool.clone();
        std::thread::spawn(move || {
            let main = main_db();
            let outcome = append(
                &writer_pool,
                &main,
                event("r-1", EventPayload::FirstToken {}),
            );
            let _ = tx.send(outcome.map(|appended| appended.seq).map_err(|e| e.to_string()));
        });

        let seq = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect(
                "the append never finished while a read guard was held — the event log is \
                 back to one connection, where a read guard IS the writer mutex",
            )
            .expect("append");
        assert_eq!(seq, 2);

        // The reader kept its own connection through all of it and can still
        // use it, which a writer mutex it had been handed could not survive.
        let after: i64 = reader
            .query_row("SELECT COUNT(*) FROM run_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, 2, "the reader connection did not see the committed append");
    }

    /// §2.8 — a node nobody configured stores NO response body. The event still
    /// has to be there: the timeline says an assistant message happened and
    /// when, which is what a decode time is a difference of. Only the body is
    /// left out, and the omission is written as an omission.
    #[test]
    fn a_response_body_is_not_stored_unless_the_organisation_asked_for_it() {
        let (_dir, pool) = events_db();
        let main = main_db();
        let secret = "the applicant lives at Wiejska 4 and was refused";

        append(
            &pool,
            &main,
            RunEvent::new("r-1", 1_700, FlowOrigin::Chat, &actor(), assistant(secret))
                .with_org("org-quiet"),
        )
        .unwrap();

        // Asserted against the FILE, not the struct: the whole point is that
        // the text never reached the disk.
        let raw: String = {
            let conn = pool.read().unwrap();
            conn.query_row(
                "SELECT payload_json FROM run_events WHERE kind = 'assistant_message'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert!(
            !raw.contains("Wiejska"),
            "the response body was stored for an organisation that never opted in: {raw}"
        );
        assert_eq!(
            stored_body(&pool, "r-1"),
            r#"{"omitted":"not_enabled"}"#,
            "an omitted body must READ as omitted, not as an empty answer"
        );

        // The event itself is intact — kind, instant and token count.
        let stored = read_run(&pool, "r-1", 0, 10).unwrap();
        assert_eq!(stored.len(), 1, "the event was dropped along with its body");
        assert_eq!(stored[0].kind, EventKind::AssistantMessage);
        assert_eq!(stored[0].at_ms, 1_700);
        match &stored[0].payload {
            EventPayload::AssistantMessage { body, tokens } => {
                assert_eq!(*body, ResponseBody::Omitted(BodyOmission::NotEnabled));
                assert_eq!(body.text(), None, "a reader must not get a body back");
                assert_eq!(*tokens, Some(12), "the token count is not the body");
            }
            other => panic!("expected an assistant_message payload, got {other:?}"),
        }
    }

    /// The opt-in works: with the organisation's own `settings` key set, the
    /// body is stored — the ability was kept, it just stopped being the default.
    #[test]
    fn an_organisation_that_opted_in_keeps_the_response_body() {
        let (_dir, pool) = events_db();
        let main = main_db();
        enable_response_bodies(&main, "org-loud");

        append(
            &pool,
            &main,
            event("r-1", assistant("the full answer")).with_org("org-loud"),
        )
        .unwrap();
        assert_eq!(stored_body(&pool, "r-1"), r#"{"text":"the full answer"}"#);

        // Another organisation on the same node is unaffected — the key is per
        // tenant, not per file.
        append(
            &pool,
            &main,
            event("r-2", assistant("the full answer")).with_org("org-quiet"),
        )
        .unwrap();
        assert_eq!(stored_body(&pool, "r-2"), r#"{"omitted":"not_enabled"}"#);
    }

    /// A camera, scheduler or maintenance run names no organisation, so there
    /// is no tenant whose opt-in could apply and the body is left out. The
    /// reason is its OWN value: borrowing the default tenant's setting would be
    /// acting on a consent nobody gave (invariant 6).
    #[test]
    fn a_run_with_no_organisation_omits_the_body_and_names_that_reason() {
        let (_dir, pool) = events_db();
        let main = main_db();
        // Even with every organisation on the node opted in.
        enable_response_bodies(&main, "org-loud");
        enable_response_bodies(&main, crate::services::org::DEFAULT_ORG_ID);

        let system = RunEvent::new(
            "r-system",
            1,
            FlowOrigin::Camera,
            &FlowActor::system(),
            assistant("what the camera pipeline answered"),
        );
        assert!(system.org_id.is_none());
        append(&pool, &main, system).unwrap();

        assert_eq!(
            stored_body(&pool, "r-system"),
            r#"{"omitted":"no_organisation"}"#
        );
    }

    /// Invariant 4, over EVERY table the event log actually holds — read out of
    /// `sqlite_master`, so a table added to this file later is covered without
    /// anyone remembering to extend a list here. None of them may carry a core
    /// sync descriptor, and since the ledger only ever reads the tables it holds
    /// descriptors for, a disjoint set is the whole of the guarantee. That the
    /// ledger reads them from the MAIN pool is the second, structural half:
    /// `<data>/events.db` is a different file, and no test can pin that beyond
    /// the disjointness asserted here.
    #[test]
    fn no_table_of_the_event_log_is_a_core_sync_table() {
        use crate::sync::core_registry::{descriptor_for_table, is_core_sync_table};
        let (_dir, pool) = events_db();
        let conn = pool.read().unwrap();
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert!(
            tables.iter().any(|t| t == "run_events"),
            "the event log has no run_events table: {tables:?}"
        );
        for table in &tables {
            assert!(
                !is_core_sync_table(table),
                "{table} lives in events.db and must stay out of core sync"
            );
            assert!(
                descriptor_for_table(table).is_none(),
                "{table} lives in events.db and must have no sync descriptor"
            );
        }
    }
}
