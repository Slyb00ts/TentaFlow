// ===== File: code_studio/cli_bridge.rs — a vendor CLI's approvals are the session's approvals =====
//
// Defect D3 of the plan (§1.2): the bridge starts Codex threads with
// `approvalPolicy: "on-request"`, the app-server asks before it touches the
// filesystem or runs a command — and nothing ever answered. The turn sat there
// until it timed out, and the operator saw a hung run with no explanation.
//
// The fix is not "auto-approve" and not a second approval UI. A CLI's request is
// routed through the SAME two components every other decision of a session goes
// through:
//
//   PEP (`code_studio::pep::authorize`)  — role, autonomy mode, boundary
//   InteractionRegistry (`agents::interaction`) — the human question, when the
//                                                 PEP says one is needed
//
// so an operator answering "may this agent run a command" answers one kind of
// question whether the agent is our harness or a vendor CLI, and the answer
// lands in the same timeline.
//
// Three rules that make the routing safe rather than merely convenient:
//
// **Unknown request kinds are denied.** A vendor can add an approval kind in any
// release. Mapping "something we do not recognize" onto the closest capability
// would be guessing with the user's filesystem; the request is refused with a
// named reason instead, and the CLI reports it as a refusal rather than hanging.
//
// **A path the request does not pin down is outside the worktree.** The target
// is resolved from the request's own parameters, and anything unresolvable is
// treated as out of bounds — the PEP's boundary check only means something if
// the caller cannot shrug and pass `inside_worktree: true`.
//
// **No database writes.** As with the egress gateway and the adapter, every
// decision produces `EventPayload`s the caller appends to the session timeline.
// The only exception is the `cli_instances` bookkeeping at the bottom of this
// file, which IS the runtime table for these instances (§5.3).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rusqlite::OptionalExtension;
use serde_json::Value;

use super::events::EventPayload;
use super::pep::{self, Capability, Decision, Target};
use crate::agents::interaction::{self, InteractionRegistry, PermissionDecision};
use crate::agents::run_manager::AgentRunManager;
use crate::db::DbPool;
use crate::flow_engine::dispatchers::progress::ProgressSink;
use crate::services::transport::Transport;
use crate::services_repo::services::ServiceRow;

/// How long an operator has to answer a CLI's approval before the turn is
/// refused. Longer than a tool permission prompt (the CLI is blocked and the
/// vendor's own timeout is minutes), short enough that a forgotten window does
/// not hold a sandbox open all day.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

// =============================================================================
// Bridge client
// =============================================================================

/// One CLI instance as the coordinator sees it.
#[derive(Debug, Clone)]
pub struct CliInstance {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub engine_id: String,
    pub service_id: i64,
    /// Identifier the BRIDGE uses; the vendor's own id is `vendor_session_id`.
    pub bridge_session_id: String,
    pub vendor_session_id: String,
    pub model: String,
    pub ticket_id: Option<String>,
    pub last_seq: u64,
}

/// What one bridge event means to the session. Everything the bridge emits maps
/// into exactly one of these; a kind we do not know becomes `Other`, which is
/// recorded and otherwise ignored — an unknown EVENT is harmless, unlike an
/// unknown approval REQUEST.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeEvent {
    Text {
        seq: u64,
        text: String,
    },
    /// A structured server→client notification from the vendor's app-server,
    /// kept as `method` + `params` rather than flattened into text: the turn's
    /// end is announced here, and reading it out of a stringified JSON blob
    /// would be parsing our own formatting.
    Notification {
        seq: u64,
        method: String,
        params: Value,
    },
    Approval {
        seq: u64,
        request: ApprovalRequest,
    },
    VendorSession {
        seq: u64,
        vendor_session_id: String,
    },
    Other {
        seq: u64,
        kind: String,
    },
}

impl BridgeEvent {
    pub fn seq(&self) -> u64 {
        match self {
            BridgeEvent::Text { seq, .. }
            | BridgeEvent::Notification { seq, .. }
            | BridgeEvent::Approval { seq, .. }
            | BridgeEvent::VendorSession { seq, .. }
            | BridgeEvent::Other { seq, .. } => *seq,
        }
    }
}

/// How a delegated turn ended, as the vendor announced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnState {
    Completed,
    Failed(String),
}

/// Reads a bridge event as "the turn is over", or not.
///
/// The ONE place a vendor's event vocabulary is interpreted as a terminal
/// state, for the same reason `capability_for` is the one place an approval
/// kind becomes a capability.
///
/// The method must name the TURN and end in a terminal word. Matching the tail
/// alone would read `item/completed` — emitted per message inside a turn — as
/// the end of the whole delegation, and the CLI would be closed while it was
/// still working. That is the one failure this predicate must not have: an
/// unrecognized completion merely costs the caller its timeout, whereas a
/// premature one reports a finished turn that never finished.
///
/// UNVERIFIED against a pinned CLI: §17.1 point 1 (a non-interactive mode with
/// a structured event stream) is a Phase 0B item nobody has performed in this
/// build, and `ensure_engine_verified` keeps the whole path shut until somebody
/// checks against the real binary.
pub fn turn_state(event: &BridgeEvent) -> Option<TurnState> {
    let BridgeEvent::Notification { method, params, .. } = event else {
        return None;
    };
    let lowered = method.to_ascii_lowercase();
    let mut segments = lowered.split(['/', '.', '_', '-']).filter(|s| !s.is_empty());
    if !segments.clone().any(|segment| segment == "turn") {
        return None;
    }
    let tail = segments.next_back()?;
    match tail {
        "completed" | "complete" | "finished" | "done" | "end" => Some(TurnState::Completed),
        "failed" | "error" | "aborted" | "cancelled" | "canceled" | "interrupted" => {
            let detail = params
                .get("message")
                .or_else(|| params.get("error"))
                .and_then(Value::as_str)
                .unwrap_or(method);
            Some(TurnState::Failed(detail.to_string()))
        }
        _ => None,
    }
}

/// A server→client approval request, as it arrives from the bridge.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub request_id: u64,
    pub method: String,
    pub params: Value,
}

/// Talks to one coding-agent bridge through the validated Core proxy. It holds
/// the service row rather than a URL, so the loopback and transport checks in
/// `services::coding_agent` apply to every call made here.
#[derive(Debug, Clone)]
pub struct CliBridge {
    service: ServiceRow,
}

impl CliBridge {
    pub fn new(service: ServiceRow) -> Result<Self> {
        if service.transport != Transport::AgentRpc {
            return Err(anyhow!(
                "service {} is not a coding-agent bridge",
                service.id
            ));
        }
        Ok(Self { service })
    }

    pub fn engine_id(&self) -> &str {
        &self.service.engine_id
    }

    async fn call(&self, operation: &str, payload: Value) -> Result<Value> {
        let response =
            crate::services::coding_agent::execute(&self.service, operation, &payload.to_string())
                .await
                .map_err(|error| anyhow!("coding-agent {operation}: {error}"))?;
        serde_json::from_str(&response)
            .with_context(|| format!("coding-agent {operation} returned invalid JSON"))
    }

    /// Opens a CLI instance on the bridge and records it in `cli_instances`.
    ///
    /// The row is written BEFORE the turn starts, so a crash between "the CLI is
    /// running" and "we know about it" leaves a row to reconcile rather than an
    /// invisible process (the same ordering rule as the session saga).
    pub async fn open(&self, pool: &DbPool, request: OpenCliInstance<'_>) -> Result<CliInstance> {
        // The environment is what points the CLI at the provider adapter and
        // hands it the ticket in place of a credential (§7.5). It is built by
        // `AdapterHandle::sandbox_env` and passed through verbatim: this module
        // must not be able to add a variable of its own, because a second
        // variable carrying real key material is exactly the failure the whole
        // adapter design exists to prevent.
        let env: serde_json::Map<String, Value> = request
            .env
            .iter()
            .map(|(name, value)| (name.clone(), Value::String(value.clone())))
            .collect();
        let created = self
            .call(
                "session.create",
                serde_json::json!({
                    "workspace": request.worktree.display().to_string(),
                    "model": request.model,
                    "resume_vendor_session_id": request.resume_vendor_session_id,
                    "fork": false,
                    "env": env,
                }),
            )
            .await?;
        let bridge_session_id = created
            .pointer("/session/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("bridge session response has no session.id"))?
            .to_string();
        let vendor_session_id = created
            .pointer("/session/vendor_session_id")
            .and_then(Value::as_str)
            .unwrap_or(&bridge_session_id)
            .to_string();
        let instance = CliInstance {
            // Minted by the CALLER, because the ticket is bound to this id and
            // the ticket has to exist before the CLI starts — a ticket issued
            // after the process is up is a window in which the CLI has a base
            // URL and no capability for it.
            id: request.instance_id.to_string(),
            session_id: request.session_id.to_string(),
            run_id: request.run_id.to_string(),
            engine_id: self.service.engine_id.clone(),
            service_id: self.service.id,
            bridge_session_id,
            vendor_session_id,
            model: request.model.to_string(),
            ticket_id: request.ticket_id.map(str::to_string),
            last_seq: 0,
        };
        insert_instance(pool, &instance)?;
        set_instance_status(pool, &instance.id, "ready")?;
        Ok(instance)
    }

    pub async fn turn(&self, instance: &CliInstance, prompt: &str) -> Result<()> {
        self.call(
            "session.turn",
            serde_json::json!({"session_id": instance.bridge_session_id, "prompt": prompt}),
        )
        .await
        .map(|_| ())
    }

    /// Drains the events the bridge has produced since the last poll and
    /// advances the instance's cursor.
    pub async fn poll(
        &self,
        pool: &DbPool,
        instance: &mut CliInstance,
    ) -> Result<Vec<BridgeEvent>> {
        let response = self
            .call(
                "session.events",
                serde_json::json!({
                    "session_id": instance.bridge_session_id,
                    "after_seq": instance.last_seq,
                }),
            )
            .await?;
        let raw = response
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut events = Vec::with_capacity(raw.len());
        for entry in raw {
            let seq = entry.get("seq").and_then(Value::as_u64).unwrap_or(0);
            instance.last_seq = instance.last_seq.max(seq);
            let kind = entry.get("kind").and_then(Value::as_str).unwrap_or("");
            let data = entry.get("data").cloned().unwrap_or(Value::Null);
            events.push(match kind {
                // A Claude Code session is a PTY, so its only channel is text.
                "terminal" => BridgeEvent::Text {
                    seq,
                    text: data
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| data.to_string()),
                },
                // The Codex app-server speaks JSON-RPC; the bridge forwards the
                // whole message. A notification keeps its shape here because
                // `turn_state` reads it, and text is only what actually carries
                // text.
                "codex" => match data.get("method").and_then(Value::as_str) {
                    Some(method) => BridgeEvent::Notification {
                        seq,
                        method: method.to_string(),
                        params: data.get("params").cloned().unwrap_or(Value::Null),
                    },
                    None => BridgeEvent::Text {
                        seq,
                        text: data
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| data.to_string()),
                    },
                },
                "approval_request" => {
                    let request_id = data
                        .get("request_id")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| anyhow!("approval event without a request id"))?;
                    BridgeEvent::Approval {
                        seq,
                        request: ApprovalRequest {
                            request_id,
                            method: data
                                .get("method")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            params: data.get("params").cloned().unwrap_or(Value::Null),
                        },
                    }
                }
                "vendor_session" => BridgeEvent::VendorSession {
                    seq,
                    vendor_session_id: data
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
                other => BridgeEvent::Other {
                    seq,
                    kind: other.to_string(),
                },
            });
        }
        if !events.is_empty() {
            set_instance_seq(pool, &instance.id, instance.last_seq)?;
        }
        Ok(events)
    }

    /// Answers one approval. The decision string is the vendor's vocabulary;
    /// `ApprovalOutcome::decision` is the only thing that produces it, so a
    /// caller cannot invent a decision the bridge would reject.
    pub async fn answer(
        &self,
        instance: &CliInstance,
        request_id: u64,
        decision: &str,
    ) -> Result<()> {
        self.call(
            "session.approval",
            serde_json::json!({
                "session_id": instance.bridge_session_id,
                "request_id": request_id,
                "decision": decision,
            }),
        )
        .await
        .map(|_| ())
    }

    /// Closes the CLI instance and records the process state the bridge
    /// reported. `reaped` means the bridge verified the process is gone (D2) —
    /// anything else is recorded as it came, not upgraded.
    pub async fn close(&self, pool: &DbPool, instance: &CliInstance) -> Result<String> {
        let response = self
            .call(
                "session.close",
                serde_json::json!({"session_id": instance.bridge_session_id}),
            )
            .await?;
        let state = response
            .get("process_state")
            .and_then(Value::as_str)
            .unwrap_or("ended")
            .to_string();
        let status = if state == "reaped" { "reaped" } else { "ended" };
        set_instance_status(pool, &instance.id, status)?;
        Ok(state)
    }
}

/// What opening an instance needs.
#[derive(Debug, Clone)]
pub struct OpenCliInstance<'a> {
    /// Identity of the `cli_instances` row, minted before the ticket.
    pub instance_id: &'a str,
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub worktree: &'a Path,
    pub model: &'a str,
    pub ticket_id: Option<&'a str>,
    pub resume_vendor_session_id: Option<&'a str>,
    /// Adapter wiring for the CLI process: base URL, the ticket as its API key,
    /// the session CA. Never the organization's credential.
    pub env: &'a [(String, String)],
}

// =============================================================================
// Approval routing
// =============================================================================

/// Everything the decision depends on. Gathered by the caller, exactly like
/// `pep::SessionCtx`, so the routing itself stays testable.
pub struct ApprovalContext<'a> {
    /// The PEP context for ONE capability, resolved per question.
    ///
    /// A closure and not a single struct: `fs_write` and `exec` are answered by
    /// different rows of `code_workspace_allowlist` and `session_grants`, so a
    /// context gathered once would let a standing permission for writing files
    /// answer a question about running a command. The caller gathers per
    /// capability; this module still does no database work.
    pub session: &'a (dyn Fn(Capability) -> pep::SessionCtx + Send + Sync),
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub parent_run_id: Option<&'a str>,
    pub engine_id: &'a str,
    /// The session's worktree. A request that cannot be shown to stay inside it
    /// is out of bounds.
    pub worktree: &'a Path,
    pub registry: &'a InteractionRegistry,
    pub manager: Option<&'a AgentRunManager>,
    pub progress: &'a dyn ProgressSink,
    pub progress_scope: &'a str,
    pub timeout: Duration,
}

/// The answer, plus what the timeline should say about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalOutcome {
    /// The vendor's decision vocabulary: `approved`, `approved_for_session`,
    /// `denied`.
    pub decision: &'static str,
    /// True when the operator asked for a STANDING grant. Writing it is the
    /// caller's job — this module does not touch the grant tables.
    pub persist_grant: bool,
    pub capability: Option<Capability>,
    pub events: Vec<EventPayload>,
}

/// Routes one CLI approval request through the PEP and, when needed, the
/// operator.
pub async fn resolve_approval(
    ctx: &ApprovalContext<'_>,
    request: &ApprovalRequest,
) -> ApprovalOutcome {
    let approval_id = format!("{}:{}", ctx.run_id, request.request_id);
    let Some(capability) = capability_for(&request.method) else {
        // Default deny. A vendor release that adds an approval kind must not be
        // able to widen what a run may do just by naming it something new.
        let reason = format!(
            "approval kind '{}' is not one this build maps onto a capability",
            request.method
        );
        return ApprovalOutcome {
            decision: "denied",
            persist_grant: false,
            capability: None,
            events: vec![
                EventPayload::ApprovalRequested {
                    approval_id: approval_id.clone(),
                    capability: "unknown".to_string(),
                    summary: reason.clone(),
                },
                EventPayload::ApprovalDecided {
                    approval_id,
                    decision: "denied".to_string(),
                    decided_by: "policy".to_string(),
                },
            ],
        };
    };

    let summary = summarize(&request.method, &request.params);
    let target = target_for(capability, &request.params, ctx.worktree);
    let mut events = vec![EventPayload::ApprovalRequested {
        approval_id: approval_id.clone(),
        capability: capability.slug().to_string(),
        summary: summary.clone(),
    }];

    let session = (ctx.session)(capability);
    let (decision, persist_grant, decided_by) =
        match pep::authorize(&session, capability, &target) {
            Decision::Deny { reason } => {
                events.push(EventPayload::AgentMessage {
                    role: "system".to_string(),
                    text: reason,
                });
                ("denied", false, "policy")
            }
            Decision::Allow(_) => ("approved", false, "policy"),
            Decision::AskUser { .. } => {
                let (answer, _waited) = interaction::run_permission_request(
                    ctx.registry,
                    ctx.manager,
                    ctx.progress,
                    ctx.progress_scope,
                    ctx.run_id,
                    ctx.parent_run_id,
                    ctx.engine_id,
                    &request.method,
                    capability.slug(),
                    ctx.timeout,
                )
                .await;
                match answer {
                    // A timeout is a denial, and the CLI is told so rather than
                    // being left blocked (the whole point of D3).
                    PermissionDecision::Deny => ("denied", false, "user"),
                    PermissionDecision::AllowOnce => ("approved", false, "user"),
                    PermissionDecision::AllowForRun => ("approved_for_session", false, "user"),
                    // The vendor has no "forever"; the standing grant lives on
                    // our side, and the CLI is told "for this session".
                    PermissionDecision::Always => (
                        "approved_for_session",
                        pep::may_store_always_grant(capability),
                        "user",
                    ),
                }
            }
        };

    events.push(EventPayload::ApprovalDecided {
        approval_id,
        decision: decision.to_string(),
        decided_by: decided_by.to_string(),
    });
    ApprovalOutcome {
        decision,
        persist_grant,
        capability: Some(capability),
        events,
    }
}

/// Which capability a vendor approval kind corresponds to. Unknown kinds return
/// `None` and are denied by the caller.
pub fn capability_for(method: &str) -> Option<Capability> {
    let normalized = method
        .rsplit('/')
        .next()
        .unwrap_or(method)
        .to_ascii_lowercase()
        .replace(['_', '-'], "");
    match normalized.as_str() {
        "applypatchapproval" | "applypatch" | "patchapproval" => Some(Capability::FsWrite),
        "execcommandapproval" | "execcommand" | "commandapproval" => Some(Capability::Exec),
        _ => None,
    }
}

/// The boundary check. Every path the request names must sit inside the
/// worktree; a request that names none, or names one that cannot be resolved, is
/// outside — the safe direction when the alternative is authorizing a write the
/// PEP never actually located.
fn target_for(capability: Capability, params: &Value, worktree: &Path) -> Target {
    if capability == Capability::Exec {
        let cwd = params.get("cwd").and_then(Value::as_str);
        return Target::Path {
            inside_worktree: cwd.is_some_and(|cwd| is_inside(worktree, Path::new(cwd))),
        };
    }
    let paths = patch_paths(params);
    Target::Path {
        inside_worktree: !paths.is_empty()
            && paths
                .iter()
                .all(|path| is_inside(worktree, Path::new(path))),
    }
}

/// Paths named by an apply-patch request, in either shape the app-server uses:
/// a `changes` object keyed by path, or a list of file entries.
fn patch_paths(params: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(changes) = params.get("changes").and_then(Value::as_object) {
        paths.extend(changes.keys().cloned());
    }
    for key in ["files", "changes"] {
        if let Some(entries) = params.get(key).and_then(Value::as_array) {
            for entry in entries {
                for field in ["path", "file_path", "target"] {
                    if let Some(path) = entry.get(field).and_then(Value::as_str) {
                        paths.push(path.to_string());
                    }
                }
            }
        }
    }
    for field in ["path", "file_path", "cwd"] {
        if let Some(path) = params.get(field).and_then(Value::as_str) {
            paths.push(path.to_string());
        }
    }
    paths
}

/// Containment without touching the filesystem: the request's paths describe
/// what the CLI is ABOUT to do, so the target may not exist yet and
/// `canonicalize` would fail on exactly the interesting case. Traversal is
/// resolved lexically instead, and a relative path is resolved against the
/// worktree.
fn is_inside(worktree: &Path, candidate: &Path) -> bool {
    let absolute: PathBuf = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        worktree.join(candidate)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                if !resolved.pop() {
                    return false;
                }
            }
            std::path::Component::CurDir => {}
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved.starts_with(worktree)
}

/// A one-line description for the timeline and the operator's card. Values are
/// truncated: a patch body belongs in an artifact, not in a prompt.
fn summarize(method: &str, params: &Value) -> String {
    let mut fields: BTreeMap<&str, String> = BTreeMap::new();
    for field in ["command", "cwd", "reason", "path", "file_path"] {
        if let Some(value) = params.get(field) {
            let text = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            fields.insert(field, truncate(&text, 200));
        }
    }
    if fields.is_empty() {
        return format!("the CLI requests approval for {method}");
    }
    let described = fields
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("the CLI requests approval for {method}: {described}")
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

// =============================================================================
// `cli_instances` bookkeeping (§5.3)
// =============================================================================

fn insert_instance(pool: &DbPool, instance: &CliInstance) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "INSERT INTO cli_instances \
           (id, session_id, run_id, engine_id, service_id, vendor_session_id, model, ticket_id, \
            status, last_seq, started_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'starting', 0, datetime('now'))",
        rusqlite::params![
            instance.id,
            instance.session_id,
            instance.run_id,
            instance.engine_id,
            instance.service_id,
            instance.vendor_session_id,
            instance.model,
            instance.ticket_id,
        ],
    )?;
    Ok(())
}

pub fn set_instance_status(pool: &DbPool, instance_id: &str, status: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let ended = matches!(status, "ended" | "failed" | "reaped");
    conn.execute(
        "UPDATE cli_instances SET status = ?2, ended_at = CASE WHEN ?3 THEN datetime('now') \
         ELSE ended_at END WHERE id = ?1",
        rusqlite::params![instance_id, status, ended],
    )?;
    Ok(())
}

/// Records the vendor's own session id once it announces one. Codex renames a
/// thread when it resumes, and the id is what the operator needs to find the
/// conversation on the vendor's side.
pub fn set_instance_vendor_session(
    pool: &DbPool,
    instance_id: &str,
    vendor_session_id: &str,
) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE cli_instances SET vendor_session_id = ?2 WHERE id = ?1",
        rusqlite::params![instance_id, vendor_session_id],
    )?;
    Ok(())
}

fn set_instance_seq(pool: &DbPool, instance_id: &str, last_seq: u64) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE cli_instances SET last_seq = ?2 WHERE id = ?1",
        rusqlite::params![instance_id, last_seq as i64],
    )?;
    Ok(())
}

pub fn instance_status(pool: &DbPool, instance_id: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    Ok(conn
        .query_row(
            "SELECT status FROM cli_instances WHERE id = ?1",
            rusqlite::params![instance_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// Closes what a restart orphaned. A row still claiming to be live describes a
/// process this Core does not supervise; the bridge kills those at ITS startup
/// (D2), so the honest state here is `reaped` (§24 "Trwałość").
pub fn reap_orphaned_instances(pool: &DbPool) -> Result<usize> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let reaped = conn.execute(
        "UPDATE cli_instances SET status = 'reaped', ended_at = datetime('now') \
         WHERE status IN ('starting','ready','busy','idle')",
        [],
    )?;
    Ok(reaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{AutonomyMode, WorkspaceRole};

    fn ctx() -> pep::SessionCtx {
        pep::SessionCtx {
            role: WorkspaceRole::Editor,
            autonomy: AutonomyMode::Normal,
            is_coordinator: false,
            has_accepted_patch_set: false,
            allowlisted: false,
            session_granted: false,
            run_granted: false,
        }
    }

    #[test]
    fn every_vendor_approval_kind_maps_to_a_capability_or_is_refused() {
        assert_eq!(
            capability_for("applyPatchApproval"),
            Some(Capability::FsWrite)
        );
        assert_eq!(
            capability_for("codex/execCommandApproval"),
            Some(Capability::Exec)
        );
        assert_eq!(
            capability_for("exec_command_approval"),
            Some(Capability::Exec)
        );
        assert_eq!(
            capability_for("networkAccessApproval"),
            None,
            "a kind this build does not understand must not be mapped onto the nearest capability"
        );
    }

    #[test]
    fn a_request_that_cannot_be_located_is_out_of_bounds() {
        let worktree = Path::new("/w/session-1");
        // Exec inside the worktree.
        let inside = serde_json::json!({"cwd": "/w/session-1/crate", "command": ["cargo","test"]});
        assert!(matches!(
            target_for(Capability::Exec, &inside, worktree),
            Target::Path {
                inside_worktree: true
            }
        ));
        // Exec with no cwd at all: unresolvable, therefore outside.
        assert!(matches!(
            target_for(
                Capability::Exec,
                &serde_json::json!({"command": ["ls"]}),
                worktree
            ),
            Target::Path {
                inside_worktree: false
            }
        ));
        // A patch that escapes the worktree lexically.
        let escaping = serde_json::json!({"changes": {"/w/session-1/../../etc/hosts": {}}});
        assert!(matches!(
            target_for(Capability::FsWrite, &escaping, worktree),
            Target::Path {
                inside_worktree: false
            }
        ));
        // A patch listing relative paths resolves against the worktree.
        let relative = serde_json::json!({"changes": {"src/main.rs": {}}});
        assert!(matches!(
            target_for(Capability::FsWrite, &relative, worktree),
            Target::Path {
                inside_worktree: true
            }
        ));
        // A patch that names nothing at all.
        assert!(matches!(
            target_for(Capability::FsWrite, &serde_json::json!({}), worktree),
            Target::Path {
                inside_worktree: false
            }
        ));
    }

    #[tokio::test]
    async fn an_unknown_approval_kind_is_denied_and_the_run_is_not_left_hanging() {
        let registry = InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let session = ctx();
        let context = ApprovalContext {
            session: &|_| session.clone(),
            session_id: "session-1",
            run_id: "run-1",
            parent_run_id: None,
            engine_id: "codex",
            worktree: Path::new("/w/session-1"),
            registry: &registry,
            manager: None,
            progress: &progress,
            progress_scope: "code-studio",
            timeout: Duration::from_millis(50),
        };
        let outcome = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 7,
                method: "somethingBrandNew".into(),
                params: Value::Null,
            },
        )
        .await;
        assert_eq!(outcome.decision, "denied");
        assert!(outcome.capability.is_none());
        assert_eq!(
            outcome.events.len(),
            2,
            "the timeline records ask and answer"
        );
    }

    #[tokio::test]
    async fn a_command_outside_the_worktree_is_refused_without_asking_anyone() {
        let registry = InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let session = ctx();
        let context = ApprovalContext {
            session: &|_| session.clone(),
            session_id: "session-1",
            run_id: "run-1",
            parent_run_id: None,
            engine_id: "codex",
            worktree: Path::new("/w/session-1"),
            registry: &registry,
            manager: None,
            progress: &progress,
            progress_scope: "code-studio",
            // Long enough that a question WOULD block if one were asked; the
            // test finishing quickly is part of the assertion.
            timeout: Duration::from_secs(30),
        };
        let outcome = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 9,
                method: "execCommandApproval".into(),
                params: serde_json::json!({"cwd": "/etc", "command": ["rm", "-rf", "/"]}),
            },
        )
        .await;
        assert_eq!(outcome.decision, "denied");
        assert_eq!(outcome.capability, Some(Capability::Exec));
    }

    #[tokio::test]
    async fn an_operator_answer_becomes_the_vendor_decision() {
        let registry = std::sync::Arc::new(InteractionRegistry::new());
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let session = ctx();
        let answering = registry.clone();
        // Answer as soon as the question appears; the run is blocked until it
        // does, which is exactly the hang D3 fixes.
        let answerer = tokio::spawn(async move {
            for _ in 0..200 {
                let pending = answering.list_for(true, &["run-1".to_string()]);
                if let Some(question) = pending.first() {
                    assert_eq!(question.tool_name.as_deref(), Some("execCommandApproval"));
                    answering.reply(
                        &question.id,
                        interaction::InteractionReply::Permission(PermissionDecision::AllowForRun),
                    );
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            false
        });

        let context = ApprovalContext {
            session: &|_| session.clone(),
            session_id: "session-1",
            run_id: "run-1",
            parent_run_id: None,
            engine_id: "codex",
            worktree: Path::new("/w/session-1"),
            registry: &registry,
            manager: None,
            progress: &progress,
            progress_scope: "code-studio",
            timeout: Duration::from_secs(10),
        };
        let outcome = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 11,
                method: "execCommandApproval".into(),
                params: serde_json::json!({"cwd": "/w/session-1", "command": ["cargo", "test"]}),
            },
        )
        .await;
        assert!(answerer.await.expect("answerer"), "no question was raised");
        assert_eq!(outcome.decision, "approved_for_session");
        assert!(!outcome.persist_grant);
    }

    #[tokio::test]
    async fn an_unanswered_request_is_denied_rather_than_left_blocking() {
        let registry = InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let session = ctx();
        let context = ApprovalContext {
            session: &|_| session.clone(),
            session_id: "session-1",
            run_id: "run-1",
            parent_run_id: None,
            engine_id: "codex",
            worktree: Path::new("/w/session-1"),
            registry: &registry,
            manager: None,
            progress: &progress,
            progress_scope: "code-studio",
            timeout: Duration::from_millis(80),
        };
        let outcome = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 13,
                method: "applyPatchApproval".into(),
                params: serde_json::json!({"changes": {"src/lib.rs": {}}}),
            },
        )
        .await;
        assert_eq!(
            outcome.decision, "denied",
            "an unanswered approval must reach the CLI as a refusal, not as silence"
        );
    }

    #[test]
    fn a_summary_never_carries_a_patch_body() {
        let summary = summarize(
            "applyPatchApproval",
            &serde_json::json!({"path": "src/lib.rs", "reason": "x".repeat(500)}),
        );
        assert!(summary.contains("src/lib.rs"));
        assert!(summary.chars().count() < 500);
    }

    /// The turn's end is read from the vendor's own vocabulary, and reading it
    /// WRONG in the dangerous direction is what this pins down: `item/completed`
    /// arrives many times inside one turn, and treating it as the end would
    /// close a CLI that is still working and report its half-done state as
    /// finished.
    #[test]
    fn only_the_turn_ending_ends_the_turn() {
        let note = |method: &str| BridgeEvent::Notification {
            seq: 1,
            method: method.to_string(),
            params: serde_json::json!({}),
        };
        assert_eq!(turn_state(&note("turn/completed")), Some(TurnState::Completed));
        assert_eq!(
            turn_state(&note("codex/turn.completed")),
            Some(TurnState::Completed)
        );
        assert_eq!(
            turn_state(&note("turn_failed")),
            Some(TurnState::Failed("turn_failed".to_string()))
        );
        for inner in [
            "item/completed",
            "message/completed",
            "thread/started",
            "turn/started",
            "session/end",
        ] {
            assert_eq!(
                turn_state(&note(inner)),
                None,
                "'{inner}' must not be read as the end of the turn"
            );
        }
        // Text is text; only a structured notification can end a turn.
        assert_eq!(
            turn_state(&BridgeEvent::Text {
                seq: 2,
                text: "turn/completed".into()
            }),
            None
        );
    }

    /// A standing permission is per capability. Before the approval context
    /// carried a resolver, one gathered `SessionCtx` answered every question —
    /// so an `fs_write` entry in the workspace allowlist silently authorized
    /// the CLI to RUN COMMANDS.
    #[tokio::test]
    async fn a_standing_write_permission_does_not_authorize_a_command() {
        let registry = InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let per_capability = |capability: Capability| pep::SessionCtx {
            allowlisted: capability == Capability::FsWrite,
            ..ctx()
        };
        let context = ApprovalContext {
            session: &per_capability,
            session_id: "session-1",
            run_id: "run-1",
            parent_run_id: None,
            engine_id: "codex",
            worktree: Path::new("/w/session-1"),
            registry: &registry,
            manager: None,
            progress: &progress,
            progress_scope: "code-studio",
            // Nobody answers, so an ASKED question ends 'denied' — which is how
            // the test tells "allowed by the grant" from "had to ask".
            timeout: Duration::from_millis(60),
        };

        let write = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 1,
                method: "applyPatchApproval".into(),
                params: serde_json::json!({"changes": {"src/lib.rs": {}}}),
            },
        )
        .await;
        assert_eq!(
            write.decision, "approved",
            "the write grant answers the write question"
        );

        let exec = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 2,
                method: "execCommandApproval".into(),
                params: serde_json::json!({"cwd": "/w/session-1", "command": ["cargo", "test"]}),
            },
        )
        .await;
        assert_eq!(
            exec.decision, "denied",
            "a write permission must not answer for a command"
        );
        assert_eq!(exec.capability, Some(Capability::Exec));
    }
}
