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
//   `tools::suspend_for_operator`        — the human question, when the PEP
//                                          says one is needed
//
// so an operator answering "may this agent run a command" answers one kind of
// question whether the agent is our harness or a vendor CLI, and the answer
// lands in the same timeline.
//
// The second half of that sentence is load-bearing and used not to hold. A
// vendor question raised its own interaction id, announced it on the progress
// stream and waited: the `approvals` table stayed empty while the CLI was
// blocked, so `ApprovalsListRequest` returned nothing, the console showed
// nothing, and the only way to answer was a reply carrying a UUID that had been
// published on a stream. In practice that is not an answer channel at all — a
// `claude` Edit sat for a quarter of an hour and then "lost" by starvation.
// Suspending through `tools::suspend_for_operator` puts the row down BEFORE the
// person is asked, which is what makes the question visible while it is open
// and answerable with the ordinary `ApprovalDecideRequest`.
//
// Three rules that make the routing safe rather than merely convenient:
//
// **Unknown request kinds are denied.** A vendor can add an approval kind in any
// release. Mapping "something we do not recognize" onto the closest capability
// would be guessing with the user's filesystem; the request is refused with a
// named reason instead, and the CLI reports it as a refusal rather than hanging.
// The same rule covers the ENGINE: a build that has no vocabulary for an engine
// decides nothing on its behalf.
//
// **A path the request does not pin down is outside the worktree.** The target
// is resolved from the request's own parameters, and anything unresolvable is
// treated as out of bounds — the PEP's boundary check only means something if
// the caller cannot shrug and pass `inside_worktree: true`. `ApprovalDialect`
// says what "pinned down" means for each CLI, because the two ask in genuinely
// different terms and reading one with the other's rules is how a boundary check
// becomes decorative.
//
// **What the channel does NOT carry.** A CLI raises a permission request only
// for what its OWN rules escalate to a question. `claude 2.1.233` resolves its
// read-only tools before the channel is consulted, so those calls never reach
// `authorize` — the engine is gated on everything it would have asked a human
// about, which is not the same as everything it does. Two things keep that from
// being a hole rather than a bound: the session runs with an empty, private
// config directory, so no allow rule of anyone's widens that set behind our
// back, and §9.5 gives reads the same automatic allowance our own harness has,
// so the calls we never see are the ones the PEP would have allowed anyway.
// Anything with an effect — a write, a command — is escalated by the vendor and
// decided here.
//
// **Where this module does write.** A decision the PEP makes on its own
// produces `EventPayload`s the caller appends to the session timeline, and
// nothing else. The two exceptions are both runtime tables of §5.3 that belong
// to this path: the `cli_instances` bookkeeping at the bottom of this file, and
// the `approvals` row a suspended question opens — written by
// `tools::suspend_for_operator` rather than here, so there is one writer of an
// approval row for the whole product.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use rusqlite::OptionalExtension;
use serde_json::Value;

use super::events::EventPayload;
use super::pep::{self, AskKind, Capability, Decision, Target};
use super::tools::{self, ApprovalDecision};
use crate::db::DbPool;
use crate::services::transport::Transport;
use crate::services_repo::services::ServiceRow;

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
    /// One object from a vendor's newline-delimited JSON stream — Claude Code
    /// run as `--print --output-format=stream-json`. It is NOT JSON-RPC: there
    /// is no method to read, the objects are self-describing through `type`,
    /// and the turn ends with a `result` object.
    StreamObject {
        seq: u64,
        object: Value,
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
            | BridgeEvent::StreamObject { seq, .. }
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
/// kind becomes a capability. Each vendor announces the end in its own shape,
/// and both were read off Phase 0B transcripts of the pinned binaries:
///
///   * `codex 0.147.0` ends EVERY turn with `turn/completed` and puts the
///     outcome in `params.turn.status` (`completed` | `failed` | `interrupted`).
///     Reading the method name alone reported a turn that wrote nothing as a
///     success — the defect this function exists to not have.
///   * `claude 2.1.233` run as `--print --output-format=stream-json` ends the
///     turn with a `{"type":"result"}` object, where `subtype`/`is_error` carry
///     the outcome.
///
/// The safe direction is asymmetric: an unrecognized ending merely costs the
/// caller its timeout, whereas inventing one reports a finished turn that never
/// finished. So anything that is terminal but not explicitly a success is
/// `Failed` with the vendor's own words.
pub fn turn_state(event: &BridgeEvent) -> Option<TurnState> {
    match event {
        BridgeEvent::Notification { method, params, .. } => codex_turn_state(method, params),
        BridgeEvent::StreamObject { object, .. } => claude_turn_state(object),
        _ => None,
    }
}

/// The Codex app-server's JSON-RPC notification.
///
/// The method must name the TURN and end in a terminal word — matching the tail
/// alone would read `item/completed`, emitted per message inside a turn, as the
/// end of the whole delegation. Once the method says "this turn is over", the
/// STATUS says how it ended.
fn codex_turn_state(method: &str, params: &Value) -> Option<TurnState> {
    let lowered = method.to_ascii_lowercase();
    let mut segments = lowered.split(['/', '.', '_', '-']).filter(|s| !s.is_empty());
    if !segments.clone().any(|segment| segment == "turn") {
        return None;
    }
    let tail = segments.next_back()?;
    if !matches!(
        tail,
        "completed"
            | "complete"
            | "finished"
            | "done"
            | "end"
            | "failed"
            | "error"
            | "aborted"
            | "cancelled"
            | "canceled"
            | "interrupted"
    ) {
        return None;
    }
    let status = params
        .pointer("/turn/status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty());
    match status {
        Some(status) if status.eq_ignore_ascii_case("completed") => Some(TurnState::Completed),
        // Every other status the vendor may ship — `failed`, `interrupted`, or
        // one added in a later release — ends the turn WITHOUT a success, and
        // is reported as such rather than being mapped onto the nearest word we
        // happen to know.
        Some(status) => Some(TurnState::Failed(format!(
            "the vendor ended the turn with status '{status}'{}",
            codex_turn_error(params)
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default()
        ))),
        // A build that announces the end without a status is taken at its word.
        None => match tail {
            "completed" | "complete" | "finished" | "done" | "end" => Some(TurnState::Completed),
            _ => Some(TurnState::Failed(
                codex_turn_error(params).unwrap_or_else(|| method.to_string()),
            )),
        },
    }
}

/// The error a failed turn carries, in either shape the app-server uses: a
/// string, or an object with a `message`.
fn codex_turn_error(params: &Value) -> Option<String> {
    let error = params
        .pointer("/turn/error")
        .or_else(|| params.get("error"))
        .or_else(|| params.get("message"))?;
    match error {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Object(_) => error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some(error.to_string())),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Claude Code's `stream-json` output. The turn ends with a `result` object;
/// everything before it (`system`, `assistant`, `user`) belongs to the turn that
/// is still running.
fn claude_turn_state(object: &Value) -> Option<TurnState> {
    if object.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    let subtype = object
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_error = object
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if subtype == "success" && !is_error {
        return Some(TurnState::Completed);
    }
    // `result` carries the vendor's message on a failure; on a limit or an
    // execution error it is the only description of what went wrong.
    let detail = object
        .get("result")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let named = if subtype.is_empty() {
        "the vendor ended the turn with an error".to_string()
    } else {
        format!("the vendor ended the turn with '{subtype}'")
    };
    Some(TurnState::Failed(match detail {
        Some(detail) => format!("{named}: {detail}"),
        None => named,
    }))
}

/// What a vendor CLI says IT spent, read out of its own event stream.
///
/// §17.3 asks for a delegated CLI's consumption to be measured **in the
/// adapter** — on a socket we own — precisely so a budget does not rest on the
/// CLI or the provider reporting honestly. This type is the other case, and it
/// is named after the gap rather than hiding it: when the engine authenticates
/// itself (`DelegationAuth::ProviderLogin`) no provider traffic crosses anything
/// of ours, so the vendor's own numbers are all there are. A budget enforced
/// from here is enforced on the word of the thing being budgeted.
///
/// Both sources the vendors offer are read, and the larger is used, because
/// neither alone is enough. The per-message reports are what exist WHILE the
/// turn runs, which is what a mid-turn ceiling needs; the terminal `result`
/// object carries the vendor's own total, which is authoritative once it
/// arrives. Taking the maximum keeps the number monotone without ever adding the
/// two together — the totals overlap, and summing them would bill a turn twice.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProviderReportedUsage {
    /// Running sum over the vendor's per-message reports.
    streamed_input: u64,
    streamed_output: u64,
    /// Totals the vendor printed when it ended the turn.
    final_input: u64,
    final_output: u64,
    /// What the vendor said the turn cost. Never computed here — there is no
    /// price feed on the node, and `cli_adapter` refuses to invent one — only
    /// repeated.
    cost_usd: f64,
    api_duration_ms: u64,
    /// How many of the vendor's own messages carried a usage report.
    reports: u32,
}

impl ProviderReportedUsage {
    pub fn input_tokens(&self) -> u64 {
        self.streamed_input.max(self.final_input)
    }

    pub fn output_tokens(&self) -> u64 {
        self.streamed_output.max(self.final_output)
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens().saturating_add(self.output_tokens())
    }

    /// `None` when the vendor reported no cost at all, so a caller can tell
    /// "free" from "not stated".
    pub fn cost_usd(&self) -> Option<f64> {
        (self.cost_usd > 0.0).then_some(self.cost_usd)
    }

    pub fn api_duration_ms(&self) -> u64 {
        self.api_duration_ms
    }

    pub fn reports(&self) -> u32 {
        self.reports
    }

    /// Folds one bridge event into the total. Everything that is not a usage
    /// report leaves it untouched.
    pub fn observe(&mut self, event: &BridgeEvent) {
        match event {
            // Claude Code `stream-json`: `assistant` objects carry the usage of
            // the API call that produced them, and the closing `result` object
            // carries the turn's total plus its cost and API time.
            BridgeEvent::StreamObject { object, .. } => {
                match object.get("type").and_then(Value::as_str) {
                    Some("assistant") => {
                        if let Some((input, output)) = usage_tokens(object.pointer("/message/usage"))
                        {
                            self.streamed_input = self.streamed_input.saturating_add(input);
                            self.streamed_output = self.streamed_output.saturating_add(output);
                            self.reports += 1;
                        }
                    }
                    Some("result") => {
                        if let Some((input, output)) = usage_tokens(object.get("usage")) {
                            self.final_input = self.final_input.max(input);
                            self.final_output = self.final_output.max(output);
                            self.reports += 1;
                        }
                        if let Some(cost) = object.get("total_cost_usd").and_then(Value::as_f64) {
                            self.cost_usd = self.cost_usd.max(cost);
                        }
                        if let Some(ms) = object.get("duration_api_ms").and_then(Value::as_u64) {
                            self.api_duration_ms = self.api_duration_ms.max(ms);
                        }
                    }
                    _ => {}
                }
            }
            // The Codex app-server puts a usage object on the notification that
            // ends the turn, under either name it has used.
            BridgeEvent::Notification { params, .. } => {
                for candidate in [params.get("usage"), params.pointer("/turn/usage")] {
                    if let Some((input, output)) = usage_tokens(candidate) {
                        self.final_input = self.final_input.max(input);
                        self.final_output = self.final_output.max(output);
                        self.reports += 1;
                    }
                }
            }
            _ => {}
        }
    }
}

/// The `(input, output)` pair of a vendor usage object, in whichever vocabulary
/// it is written. Cache tokens count as input: they were part of the prompt the
/// provider processed, and a budget that ignored them would let a long cached
/// context run free.
fn usage_tokens(usage: Option<&Value>) -> Option<(u64, u64)> {
    let usage = usage?.as_object()?;
    let read = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    let input = read("input_tokens")
        .max(read("prompt_tokens"))
        .saturating_add(read("cache_read_input_tokens"))
        .saturating_add(read("cache_creation_input_tokens"));
    let output = read("output_tokens").max(read("completion_tokens"));
    (input > 0 || output > 0).then_some((input, output))
}

/// A server→client approval request, as it arrives from the bridge.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub request_id: u64,
    pub method: String,
    pub params: Value,
}

/// Whose approval vocabulary a request is written in.
///
/// The two CLIs do not merely spell things differently, they say different
/// things. Codex asks `execCommandApproval` and names the `cwd` it wants to run
/// in. Claude Code asks `Bash` and names NO directory at all, because that CLI
/// runs every tool in the working directory its process was started in — which
/// the bridge sets to the session worktree. Reading the second with the first's
/// rules refuses every command Claude Code will ever make; reading the first
/// with the second's authorizes a Codex command that pinned down nothing.
///
/// This is a mapping, not a second policy: whichever dialect a request is in, it
/// ends up at the same `pep::authorize` with the same rule order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDialect {
    Codex,
    ClaudeCode,
}

impl ApprovalDialect {
    /// An engine this build has no vocabulary for gets no mapping either — its
    /// requests are refused by name rather than guessed at.
    pub fn for_engine(engine_id: &str) -> Option<Self> {
        match engine_id {
            "codex" => Some(ApprovalDialect::Codex),
            "claude-code" => Some(ApprovalDialect::ClaudeCode),
            _ => None,
        }
    }
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
                    "args": request.args,
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

    /// Whether the ENGINE holds a provider login of its own on this node.
    ///
    /// The bridge answers it by running the vendor's own status command
    /// (`claude auth status`, `codex login status`) and reporting whether it
    /// succeeded — so the answer is the CLI's, not a setting somebody typed.
    /// That is the whole point: `DelegationAuth::ProviderLogin` hands the CLI no
    /// credential at all, and the only honest way to know a run will authenticate
    /// is to ask the thing that will do the authenticating.
    ///
    /// A probe that cannot be run is an error, never a `true`: the caller turns
    /// it into a refusal, because the permissive reading would start a CLI that
    /// then talks to a provider as nobody in particular.
    pub async fn provider_login(&self) -> Result<bool> {
        let response = self.call("auth.status", serde_json::json!({})).await?;
        Ok(response
            .get("authenticated")
            .and_then(Value::as_bool)
            .unwrap_or(false))
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
                // Claude Code runs as `--print --output-format=stream-json`, so
                // its channel is a stream of self-describing JSON objects. It
                // keeps its shape here for the same reason a Codex notification
                // does: the turn's end is in it.
                "claude" => BridgeEvent::StreamObject { seq, object: data },
                // The PTY channel, which is what the login flow uses.
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
    ///
    /// Takes the two identifiers rather than the `CliInstance` because a
    /// delegation abandoned mid-turn has to be able to close from a detached
    /// task, where the live instance is long gone.
    pub async fn close(
        &self,
        pool: &DbPool,
        instance_id: &str,
        bridge_session_id: &str,
    ) -> Result<String> {
        let response = self
            .call(
                "session.close",
                serde_json::json!({"session_id": bridge_session_id}),
            )
            .await?;
        let state = response
            .get("process_state")
            .and_then(Value::as_str)
            .unwrap_or("ended")
            .to_string();
        let status = if state == "reaped" { "reaped" } else { "ended" };
        set_instance_status(pool, instance_id, status)?;
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
    /// Arguments the CLI has to be started with for an engine whose provider is
    /// configuration rather than an environment variable (codex). Same rule as
    /// `env`: built by `AdapterHandle::cli_args` and passed through verbatim.
    pub args: &'a [String],
}

// =============================================================================
// Approval routing
// =============================================================================

/// Everything the decision depends on. Gathered by the caller, exactly like
/// `pep::SessionCtx`, so the routing itself stays testable.
pub struct ApprovalContext<'a> {
    /// The PEP context for ONE capability against ONE target, resolved per
    /// question.
    ///
    /// A closure and not a single struct: `fs_write` and `exec` are answered by
    /// different rows of `code_workspace_allowlist` and `session_grants`, so a
    /// context gathered once would let a standing permission for writing files
    /// answer a question about running a command. The TARGET travels with it
    /// for the same reason one step down — a grant earned for `cargo` must not
    /// answer for `curl` — and it is the same label the approval row stores, so
    /// a permission is read back under the name it was written with.
    pub session: &'a (dyn Fn(Capability, Option<&str>) -> pep::SessionCtx + Send + Sync),
    /// Where the question is recorded and how it reaches a person. The vendor's
    /// request becomes an ordinary Code Studio approval — one `approvals` row,
    /// visible in `ApprovalsListRequest` WHILE it is open, answerable with
    /// `ApprovalDecideRequest` — rather than a parallel channel with a card
    /// nobody can find.
    pub ask: tools::OperatorAsk<'a>,
    /// Registry database, for the standing grant an `always` answer leaves.
    pub main_db: &'a DbPool,
    pub workspace_id: &'a str,
    pub run_id: &'a str,
    pub engine_id: &'a str,
    /// The session's worktree. A request that cannot be shown to stay inside it
    /// is out of bounds.
    pub worktree: &'a Path,
}

/// The answer, plus what the timeline should say about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalOutcome {
    /// The vendor's decision vocabulary: `approved`, `approved_for_session`,
    /// `denied`.
    pub decision: &'static str,
    pub capability: Option<Capability>,
    /// Timeline entries the CALLER still has to append. Empty when the operator
    /// was asked: `suspend_for_operator` owns that half of the timeline, and it
    /// writes both entries against the `approvals` row they belong to.
    pub events: Vec<EventPayload>,
}

/// Routes one CLI approval request through the PEP and, when needed, the
/// operator.
pub async fn resolve_approval(
    ctx: &ApprovalContext<'_>,
    request: &ApprovalRequest,
) -> ApprovalOutcome {
    let approval_id = format!("{}:{}", ctx.run_id, request.request_id);
    let Some(dialect) = ApprovalDialect::for_engine(ctx.engine_id) else {
        return refused_by_policy(
            approval_id,
            format!(
                "engine '{}' has no approval vocabulary in this build, so its requests cannot \
                 be mapped onto a capability",
                ctx.engine_id
            ),
        );
    };
    let Some(capability) = capability_for(dialect, &request.method) else {
        // Default deny. A vendor release that adds an approval kind must not be
        // able to widen what a run may do just by naming it something new.
        return refused_by_policy(
            approval_id,
            format!(
                "approval kind '{}' is not one this build maps onto a capability",
                request.method
            ),
        );
    };

    let summary = summarize(&request.method, &request.params);
    let target = target_for(dialect, capability, &request.params, ctx.worktree);
    let label = target_label(dialect, capability, &request.params);

    let session = (ctx.session)(capability, label.as_deref());
    let decision = match pep::authorize(&session, capability, &target) {
        // A policy answer asks nobody, so it opens no `approvals` row — the row
        // IS the question — and the timeline entries travel back to the caller.
        Decision::Deny { reason } => {
            return decided_by_policy(approval_id, capability, summary, Some(reason), "denied");
        }
        Decision::Allow(_) => {
            return decided_by_policy(approval_id, capability, summary, None, "approved");
        }
        Decision::AskUser { .. } => {
            // The same suspension a model-issued tool call uses: the row is
            // written before the person is asked, and the answer may arrive on
            // the approval card or on the agent's permission channel.
            let answered = tools::suspend_for_operator(
                &ctx.ask,
                capability,
                label.as_deref(),
                &summary,
                AskKind::Permission,
            )
            .await;
            let answer = match answered {
                Ok(answer) => answer,
                // A question that could not even be recorded was never put to
                // anybody, and the CLI must not be told a person allowed it.
                Err(error) => {
                    tracing::warn!("cli approval could not be put to the operator: {error:#}");
                    return decided_by_policy(
                        approval_id,
                        capability,
                        summary,
                        Some(format!("the approval could not be recorded: {error:#}")),
                        "denied",
                    );
                }
            };
            if let Err(error) = tools::persist_grant(
                ctx.main_db,
                ctx.workspace_id,
                ctx.ask.user_id,
                capability,
                label.as_deref(),
                answer,
            ) {
                tracing::warn!("cli approval grant not stored: {error:#}");
            }
            match answer {
                // A timeout is a denial, and the CLI is told so rather than
                // being left blocked (the whole point of D3).
                ApprovalDecision::Deny => "denied",
                ApprovalDecision::AllowOnce => "approved",
                ApprovalDecision::AllowForRun => "approved_for_session",
                // The vendor has no "forever"; the standing grant lives on our
                // side, and the CLI is told "for this session".
                ApprovalDecision::Always => "approved_for_session",
            }
        }
    };
    ApprovalOutcome {
        decision,
        capability: Some(capability),
        events: Vec::new(),
    }
}

/// An answer the PEP gave on its own: nobody was asked, so no `approvals` row
/// was opened, and the two timeline entries that make it readable travel back
/// to the caller.
fn decided_by_policy(
    approval_id: String,
    capability: Capability,
    summary: String,
    reason: Option<String>,
    decision: &'static str,
) -> ApprovalOutcome {
    let mut events = vec![EventPayload::ApprovalRequested {
        approval_id: approval_id.clone(),
        capability: capability.slug().to_string(),
        summary,
    }];
    if let Some(reason) = reason {
        events.push(EventPayload::AgentMessage {
            role: "system".to_string(),
            text: reason,
        });
    }
    events.push(EventPayload::ApprovalDecided {
        approval_id,
        decision: decision.to_string(),
        decided_by: "policy".to_string(),
    });
    ApprovalOutcome {
        decision,
        capability: Some(capability),
        events,
    }
}

/// A refusal this module decided on its own, with the two timeline entries that
/// make it readable: what was asked, and what was answered.
fn refused_by_policy(approval_id: String, reason: String) -> ApprovalOutcome {
    ApprovalOutcome {
        decision: "denied",
        capability: None,
        events: vec![
            EventPayload::ApprovalRequested {
                approval_id: approval_id.clone(),
                capability: "unknown".to_string(),
                summary: reason,
            },
            EventPayload::ApprovalDecided {
                approval_id,
                decision: "denied".to_string(),
                decided_by: "policy".to_string(),
            },
        ],
    }
}

/// Which capability a vendor approval kind corresponds to. Unknown kinds return
/// `None` and are denied by the caller.
///
/// Claude Code asks by TOOL NAME, so the right-hand side is its tool set. Two
/// omissions are deliberate rather than forgotten: `WebFetch` and `WebSearch`
/// would be `net_egress`, but nothing here can tell the PEP which host they
/// reach, and a `net_egress` decision made without a host is not a decision —
/// they are refused. `Task` spawns a vendor subagent whose own tool calls come
/// back through this same channel, so refusing the spawn is the only way to keep
/// the accounting honest.
pub fn capability_for(dialect: ApprovalDialect, method: &str) -> Option<Capability> {
    let normalized = method
        .rsplit('/')
        .next()
        .unwrap_or(method)
        .to_ascii_lowercase()
        .replace(['_', '-'], "");
    match dialect {
        ApprovalDialect::Codex => match normalized.as_str() {
            "applypatchapproval" | "applypatch" | "patchapproval" => Some(Capability::FsWrite),
            "execcommandapproval" | "execcommand" | "commandapproval" => Some(Capability::Exec),
            _ => None,
        },
        ApprovalDialect::ClaudeCode => match normalized.as_str() {
            "read" | "glob" | "grep" => Some(Capability::FsRead),
            "write" | "edit" | "notebookedit" => Some(Capability::FsWrite),
            "bash" | "bashoutput" | "killshell" => Some(Capability::Exec),
            _ => None,
        },
    }
}

/// The boundary check. Every path the request names must sit inside the
/// worktree; what happens when it names NONE is the one thing the two dialects
/// answer differently, and it is a fact about the CLIs, not a preference:
///
///   * Codex passes the working directory as a request PARAMETER, so a request
///     without one pinned nothing down and is out of bounds — the safe direction
///     when the alternative is authorizing a write nobody located.
///   * Claude Code never passes one, because its tools resolve against the
///     PROCESS working directory, which the bridge set to the session worktree
///     and which no request can change. A `Bash` call that names no path acts on
///     the worktree itself, so refusing it would refuse the engine outright.
///
/// Neither reading lets a request widen its own boundary: the paths a request
/// DOES name are always checked, in both dialects.
fn target_for(
    dialect: ApprovalDialect,
    capability: Capability,
    params: &Value,
    worktree: &Path,
) -> Target {
    let (named, empty_means_inside) = match dialect {
        ApprovalDialect::Codex if capability == Capability::Exec => (
            params
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_string)
                .into_iter()
                .collect::<Vec<_>>(),
            false,
        ),
        ApprovalDialect::Codex => (patch_paths(params), false),
        ApprovalDialect::ClaudeCode => (claude_paths(params), true),
    };
    let inside_worktree = if named.is_empty() {
        empty_means_inside
    } else {
        named
            .iter()
            .all(|path| is_inside(worktree, Path::new(path)))
    };
    Target::Path { inside_worktree }
}

/// What a decision about this request is STORED under (§9.1: the object of a
/// permission is capability + target), and what standing permissions are read
/// back with. The boundary check is `target_for`'s job; this is the narrower
/// question of what to call the thing, and the two must not disagree.
///
/// The convention is the agent path's (`tools::target_label`): a command is
/// named by its program, a file operation by the path it names. A request that
/// pins nothing down — a multi-file patch, a `Bash` line we cannot read a
/// program out of — has no honest narrower name, and `pep::grant_pattern` spells
/// that `*`. Guessing one of several paths would store a permission that reads
/// back as covering a file the operator never saw.
fn target_label(
    dialect: ApprovalDialect,
    capability: Capability,
    params: &Value,
) -> Option<String> {
    if capability == Capability::Exec {
        let command = params.get("command")?;
        let program = match command {
            // Codex passes argv; Claude Code passes one shell line.
            Value::Array(argv) => argv.first().and_then(Value::as_str).map(str::to_string),
            Value::String(line) => line.split_whitespace().next().map(str::to_string),
            _ => None,
        }?;
        return (!program.is_empty()).then_some(program);
    }
    let named = match dialect {
        ApprovalDialect::Codex => patch_paths(params),
        ApprovalDialect::ClaudeCode => claude_paths(params),
    };
    match named.as_slice() {
        [only] if !only.is_empty() => Some(only.clone()),
        _ => None,
    }
}

/// Paths one Claude Code tool input names. Its tool set puts them under exactly
/// these keys — `file_path` for Read/Write/Edit, `notebook_path` for
/// NotebookEdit, `path` for Glob/Grep — and a tool that names none acts on the
/// process working directory.
fn claude_paths(params: &Value) -> Vec<String> {
    ["file_path", "notebook_path", "path"]
        .into_iter()
        .filter_map(|field| params.get(field).and_then(Value::as_str))
        .map(str::to_string)
        .collect()
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
    for field in [
        "command",
        "cwd",
        "reason",
        "path",
        "file_path",
        "notebook_path",
        "description",
    ] {
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
    use crate::code_studio::tools::ScriptedGate;
    use crate::code_studio::{paths as cs_paths, workspace_db};

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

    /// A workspace runtime database with one open session — the table an
    /// approval row lands in, and the timeline it is recorded on. Real
    /// migrations, real writes: the point of the routing change is that a
    /// vendor question becomes a ROW, so a stub would test nothing.
    struct Fixture {
        _data: tempfile::TempDir,
        workspace_id: String,
        pool: DbPool,
        main_db: DbPool,
    }

    impl Fixture {
        fn open(workspace_id: &str) -> Self {
            let data = tempfile::tempdir().expect("data dir");
            crate::paths::set_category_override(
                crate::paths::StorageCategory::Data,
                Some(data.path().to_string_lossy().to_string()),
            );
            cs_paths::create_workspace_layout(workspace_id).expect("layout");
            let pool = workspace_db::open(workspace_id).expect("workspace db");
            {
                let conn = pool.write().expect("write");
                conn.execute(
                    "INSERT INTO sessions (id, workspace_id, user_id, title, branch, \
                      autonomy_mode, flow_id, flow_version_id, status, created_at, updated_at) \
                     VALUES ('sess-1', ?1, 'u-1', 'S', 'cs/u/1', 'normal', 'f', 'v', 'running', \
                      datetime('now'), datetime('now'))",
                    rusqlite::params![workspace_id],
                )
                .expect("session row");
            }
            Self {
                _data: data,
                workspace_id: workspace_id.to_string(),
                pool,
                main_db: crate::db::init(Path::new(":memory:")).expect("registry db"),
            }
        }

        fn ask<'a>(&'a self, gate: &'a dyn tools::ApprovalGate) -> tools::OperatorAsk<'a> {
            tools::OperatorAsk {
                pool: &self.pool,
                session_id: "sess-1",
                run_id: Some("run-1"),
                user_id: "u-1",
                gate,
            }
        }

        fn approvals(&self) -> Vec<(String, String, String, Option<String>)> {
            let conn = self.pool.read().expect("read");
            let mut stmt = conn
                .prepare(
                    "SELECT capability, target_pattern, status, decision FROM approvals \
                     ORDER BY requested_at",
                )
                .expect("prepare");
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .expect("query")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("rows");
            rows
        }

        fn event_kinds(&self) -> Vec<String> {
            let conn = self.pool.read().expect("read");
            let mut stmt = conn
                .prepare("SELECT kind FROM session_events ORDER BY seq")
                .expect("prepare");
            let rows = stmt
                .query_map([], |row| row.get(0))
                .expect("query")
                .collect::<std::result::Result<Vec<_>, _>>()
                .expect("rows");
            rows
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            workspace_db::close(&self.workspace_id);
            crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
        }
    }

    fn context<'a>(
        fixture: &'a Fixture,
        ask: tools::OperatorAsk<'a>,
        session: &'a (dyn Fn(Capability, Option<&str>) -> pep::SessionCtx + Send + Sync),
        engine_id: &'a str,
        worktree: &'a Path,
    ) -> ApprovalContext<'a> {
        ApprovalContext {
            session,
            ask,
            main_db: &fixture.main_db,
            workspace_id: &fixture.workspace_id,
            run_id: "run-1",
            engine_id,
            worktree,
        }
    }

    #[test]
    fn every_vendor_approval_kind_maps_to_a_capability_or_is_refused() {
        use ApprovalDialect::{ClaudeCode, Codex};
        assert_eq!(
            capability_for(Codex, "applyPatchApproval"),
            Some(Capability::FsWrite)
        );
        assert_eq!(
            capability_for(Codex, "codex/execCommandApproval"),
            Some(Capability::Exec)
        );
        assert_eq!(
            capability_for(Codex, "exec_command_approval"),
            Some(Capability::Exec)
        );
        assert_eq!(
            capability_for(Codex, "networkAccessApproval"),
            None,
            "a kind this build does not understand must not be mapped onto the nearest capability"
        );

        // Claude Code asks by tool name.
        assert_eq!(capability_for(ClaudeCode, "Bash"), Some(Capability::Exec));
        assert_eq!(
            capability_for(ClaudeCode, "Write"),
            Some(Capability::FsWrite)
        );
        assert_eq!(
            capability_for(ClaudeCode, "NotebookEdit"),
            Some(Capability::FsWrite)
        );
        assert_eq!(capability_for(ClaudeCode, "Read"), Some(Capability::FsRead));
        assert_eq!(capability_for(ClaudeCode, "Grep"), Some(Capability::FsRead));
        for unmapped in ["WebFetch", "WebSearch", "Task", "mcp__server__tool", ""] {
            assert_eq!(
                capability_for(ClaudeCode, unmapped),
                None,
                "'{unmapped}' has no capability this build can bound, so it must be refused"
            );
        }

        // The vocabularies are not shared: one CLI's word means nothing in the
        // other's dialect, and must not be answered as if it did.
        assert_eq!(capability_for(Codex, "Bash"), None);
        assert_eq!(capability_for(ClaudeCode, "applyPatchApproval"), None);

        // An engine nobody wrote a vocabulary for has no dialect at all.
        assert_eq!(ApprovalDialect::for_engine("codex"), Some(Codex));
        assert_eq!(ApprovalDialect::for_engine("claude-code"), Some(ClaudeCode));
        assert_eq!(ApprovalDialect::for_engine("gemini-cli"), None);
    }

    #[test]
    fn a_request_that_cannot_be_located_is_out_of_bounds() {
        use ApprovalDialect::Codex;
        let worktree = Path::new("/w/session-1");
        // Exec inside the worktree.
        let inside = serde_json::json!({"cwd": "/w/session-1/crate", "command": ["cargo","test"]});
        assert!(matches!(
            target_for(Codex, Capability::Exec, &inside, worktree),
            Target::Path {
                inside_worktree: true
            }
        ));
        // Exec with no cwd at all: unresolvable, therefore outside.
        assert!(matches!(
            target_for(
                Codex,
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
            target_for(Codex, Capability::FsWrite, &escaping, worktree),
            Target::Path {
                inside_worktree: false
            }
        ));
        // A patch listing relative paths resolves against the worktree.
        let relative = serde_json::json!({"changes": {"src/main.rs": {}}});
        assert!(matches!(
            target_for(Codex, Capability::FsWrite, &relative, worktree),
            Target::Path {
                inside_worktree: true
            }
        ));
        // A patch that names nothing at all.
        assert!(matches!(
            target_for(Codex, Capability::FsWrite, &serde_json::json!({}), worktree),
            Target::Path {
                inside_worktree: false
            }
        ));
    }

    /// The Claude Code dialect names paths in the tool input and nothing else.
    /// A tool that names none runs against the process working directory, which
    /// IS the worktree — but a tool that names one is still bounded by it, so
    /// the difference buys the engine nothing outside its own tree.
    #[test]
    fn a_claude_tool_is_bounded_by_the_paths_it_names() {
        use ApprovalDialect::ClaudeCode;
        let worktree = Path::new("/w/session-1");
        let inside = |params: Value, capability: Capability| {
            matches!(
                target_for(ClaudeCode, capability, &params, worktree),
                Target::Path {
                    inside_worktree: true
                }
            )
        };

        // Bash names no directory in this dialect; the worktree is where it runs.
        assert!(inside(
            serde_json::json!({"command": "cargo test"}),
            Capability::Exec
        ));
        // A Grep with no path searches the worktree.
        assert!(inside(
            serde_json::json!({"pattern": "todo"}),
            Capability::FsRead
        ));
        // A named path is checked, absolute or relative.
        assert!(inside(
            serde_json::json!({"file_path": "/w/session-1/src/lib.rs"}),
            Capability::FsWrite
        ));
        assert!(inside(
            serde_json::json!({"file_path": "src/lib.rs"}),
            Capability::FsWrite
        ));
        for escaping in [
            serde_json::json!({"file_path": "/etc/passwd"}),
            serde_json::json!({"file_path": "/w/session-1/../../etc/shadow"}),
            serde_json::json!({"notebook_path": "/w/other/run.ipynb"}),
            serde_json::json!({"path": "/w/session-2"}),
        ] {
            assert!(
                !inside(escaping.clone(), Capability::FsWrite),
                "{escaping} escapes the worktree and must be out of bounds"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_approval_kind_is_denied_and_the_run_is_not_left_hanging() {
        let _guard = cs_paths::test_data_dir_guard();
        let fixture = Fixture::open("wsunknown");
        let gate = ScriptedGate::answering(ApprovalDecision::AllowOnce);
        let ask = fixture.ask(&gate);
        let session = ctx();
        let grants = |_: Capability, _: Option<&str>| session.clone();
        let context = context(&fixture, ask, &grants, "codex", Path::new("/w/session-1"));
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
        assert!(gate.asked().is_empty(), "nobody is asked about a kind we cannot bound");
        assert!(
            fixture.approvals().is_empty(),
            "a refusal nobody was asked about opens no approval card"
        );
    }

    #[tokio::test]
    async fn a_command_outside_the_worktree_is_refused_without_asking_anyone() {
        let _guard = cs_paths::test_data_dir_guard();
        let fixture = Fixture::open("wsoutside");
        // A gate that would ALLOW: whatever refuses below is the boundary, and
        // the operator never being asked is the other half of the assertion.
        let gate = ScriptedGate::answering(ApprovalDecision::AllowOnce);
        let ask = fixture.ask(&gate);
        let session = ctx();
        let grants = |_: Capability, _: Option<&str>| session.clone();
        let context = context(&fixture, ask, &grants, "codex", Path::new("/w/session-1"));
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
        assert!(gate.asked().is_empty(), "a target out of bounds is not a question");
        assert!(fixture.approvals().is_empty());
    }

    /// The vendor's question is an ORDINARY Code Studio approval: the row is in
    /// `approvals`, `pending`, naming the capability and the target, WHILE the
    /// operator is being asked — not after they answered.
    ///
    /// Before this, `resolve_approval` wrote no row at all and appended its two
    /// timeline entries only once the decision was in. The console's
    /// `loadApprovals()` therefore returned nothing for the whole time the CLI
    /// was blocked, and the only answer channel was a reply carrying a UUID
    /// announced on a progress stream. A `claude` Edit sat for a quarter of an
    /// hour and was refused by starvation.
    #[tokio::test]
    async fn a_vendor_question_is_a_pending_approval_row_while_it_is_open() {
        let _guard = cs_paths::test_data_dir_guard();
        let fixture = Fixture::open("wsvendorask");

        /// A gate that reads the `approvals` table at the moment the question
        /// is put to it — the only way to assert "visible DURING", not "after".
        struct PeekingGate {
            pool: DbPool,
            seen: std::sync::Mutex<Vec<(String, String, String)>>,
        }

        #[async_trait::async_trait]
        impl tools::ApprovalGate for PeekingGate {
            async fn request(&self, _ask: &tools::Approval) -> ApprovalDecision {
                let conn = self.pool.read().expect("read");
                let mut stmt = conn
                    .prepare(
                        "SELECT capability, target_pattern, status FROM approvals \
                         WHERE session_id = 'sess-1' AND status = 'pending'",
                    )
                    .expect("prepare");
                let rows = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .expect("query")
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .expect("rows");
                *self.seen.lock().expect("seen") = rows;
                ApprovalDecision::AllowForRun
            }

            async fn present_review(&self, _prompt: &tools::ReviewPrompt) -> Option<String> {
                None
            }
        }

        let gate = PeekingGate {
            pool: fixture.pool.clone(),
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let ask = fixture.ask(&gate);
        let session = ctx();
        let grants = |_: Capability, _: Option<&str>| session.clone();
        let context = context(&fixture, ask, &grants, "codex", Path::new("/w/session-1"));
        let outcome = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 11,
                method: "execCommandApproval".into(),
                params: serde_json::json!({"cwd": "/w/session-1", "command": ["cargo", "test"]}),
            },
        )
        .await;

        assert_eq!(
            *gate.seen.lock().expect("seen"),
            vec![("exec".to_string(), "cargo".to_string(), "pending".to_string())],
            "the operator was asked before the question existed anywhere they could see it"
        );
        // And the row is closed by the answer, with the target it was asked
        // about — not with '*', which would grant every command.
        assert_eq!(
            fixture.approvals(),
            vec![(
                "exec".to_string(),
                "cargo".to_string(),
                "decided".to_string(),
                Some("allow_for_run".to_string())
            )]
        );
        assert_eq!(outcome.decision, "approved_for_session");
        // The suspension owns both timeline entries; the caller appends none.
        assert!(outcome.events.is_empty());
        assert_eq!(
            fixture.event_kinds(),
            vec!["approval_requested".to_string(), "approval_decided".to_string()],
            "one question and one decision, each written once"
        );
    }

    #[tokio::test]
    async fn an_unanswered_request_is_denied_rather_than_left_blocking() {
        let _guard = cs_paths::test_data_dir_guard();
        let fixture = Fixture::open("wsunanswered");
        // What `InteractionGate` returns when nobody answers inside the budget.
        let gate = ScriptedGate::answering(ApprovalDecision::Deny);
        let ask = fixture.ask(&gate);
        let session = ctx();
        let grants = |_: Capability, _: Option<&str>| session.clone();
        let context = context(&fixture, ask, &grants, "codex", Path::new("/w/session-1"));
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
        assert_eq!(gate.asked(), vec![Capability::FsWrite]);
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

    /// Phase 0B transcript, `codex 0.147.0`: a turn that produced nothing ends
    /// with the SAME method as one that succeeded, and the outcome is in
    /// `params.turn.status`. Reading the method alone reported a failed turn as
    /// a completed one — the harness announced "done" about a turn in which not
    /// a line was written.
    #[test]
    fn the_status_of_the_turn_decides_it_not_the_method_name() {
        let note = |params: Value| BridgeEvent::Notification {
            seq: 1,
            method: "turn/completed".to_string(),
            params,
        };
        assert_eq!(
            turn_state(&note(serde_json::json!({
                "turn": {"id": "t1", "status": "completed"}
            }))),
            Some(TurnState::Completed)
        );
        let failed = turn_state(&note(serde_json::json!({
            "turn": {
                "id": "t2",
                "status": "failed",
                "error": {"message": "stream error: unexpected status 401 Unauthorized"}
            }
        })));
        assert!(
            matches!(&failed, Some(TurnState::Failed(reason))
                if reason.contains("failed")
                    && reason.contains("stream error: unexpected status 401 Unauthorized")),
            "a failed turn must be reported as failed, with the vendor's own words: {failed:?}"
        );
        let interrupted = turn_state(&note(serde_json::json!({
            "turn": {"id": "t3", "status": "interrupted"}
        })));
        assert!(
            matches!(&interrupted, Some(TurnState::Failed(reason)) if reason.contains("interrupted")),
            "an interrupted turn is not a completed one, got {interrupted:?}"
        );
        // A status this build has never seen is not a success either.
        assert!(matches!(
            turn_state(&note(serde_json::json!({"turn": {"status": "throttled"}}))),
            Some(TurnState::Failed(_))
        ));
    }

    /// Phase 0B transcript, `claude 2.1.233` run as
    /// `--print --output-format=stream-json`: the turn ends with a `result`
    /// object, not with a JSON-RPC notification, and `subtype`/`is_error` carry
    /// the outcome.
    #[test]
    fn the_claude_stream_ends_its_turn_with_a_result_object() {
        let object = |value: Value| BridgeEvent::StreamObject {
            seq: 1,
            object: value,
        };
        assert_eq!(
            turn_state(&object(serde_json::json!({
                "type": "result",
                "subtype": "success",
                "stop_reason": "end_turn",
                "terminal_reason": "completed",
                "duration_api_ms": 4220,
                "total_cost_usd": 0.0477,
                "result": "Done."
            }))),
            Some(TurnState::Completed)
        );
        let failed = turn_state(&object(serde_json::json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "result": "the tool call could not be completed"
        })));
        assert!(
            matches!(&failed, Some(TurnState::Failed(reason))
                if reason.contains("error_during_execution")
                    && reason.contains("the tool call could not be completed")),
            "a failed turn must carry the vendor's own words, got {failed:?}"
        );
        // A success flagged as an error is still an error: the two fields
        // disagree, and the safe reading is the one that does not report a
        // finished turn.
        assert!(matches!(
            turn_state(&object(serde_json::json!({
                "type": "result", "subtype": "success", "is_error": true
            }))),
            Some(TurnState::Failed(_))
        ));
        // Everything inside the turn keeps the turn open.
        for inside in [
            serde_json::json!({"type": "system", "subtype": "init", "session_id": "s-1"}),
            serde_json::json!({"type": "assistant", "message": {"content": []}}),
            serde_json::json!({"type": "user", "message": {"content": []}}),
            serde_json::json!({"type": "stream_event"}),
        ] {
            assert_eq!(
                turn_state(&object(inside.clone())),
                None,
                "'{inside}' is not the end of the turn"
            );
        }
    }

    /// The one outcome that must never be invented. A vendor that announces
    /// nothing leaves the caller with `None`, which `delegate_cli` settles as
    /// `timed_out` — never as a completed turn.
    #[test]
    fn a_silent_vendor_never_produces_a_completed_turn() {
        let silent = [
            BridgeEvent::Text {
                seq: 1,
                text: "turn/completed".into(),
            },
            BridgeEvent::Other {
                seq: 2,
                kind: "terminal".into(),
            },
            BridgeEvent::StreamObject {
                seq: 3,
                object: serde_json::json!({"type": "assistant"}),
            },
            BridgeEvent::Notification {
                seq: 4,
                method: "item/completed".into(),
                params: serde_json::json!({"turn": {"status": "completed"}}),
            },
        ];
        for event in silent {
            assert_eq!(
                turn_state(&event),
                None,
                "no end-of-turn signal must stay no end of turn: {event:?}"
            );
        }
    }

    /// A standing permission is per capability. Before the approval context
    /// carried a resolver, one gathered `SessionCtx` answered every question —
    /// so an `fs_write` entry in the workspace allowlist silently authorized
    /// the CLI to RUN COMMANDS.
    #[tokio::test]
    async fn a_standing_write_permission_does_not_authorize_a_command() {
        let _guard = cs_paths::test_data_dir_guard();
        let fixture = Fixture::open("wsstandingcodex");
        // Nobody answers, so an ASKED question ends 'denied' — which is how the
        // test tells "allowed by the grant" from "had to ask".
        let gate = ScriptedGate::answering(ApprovalDecision::Deny);
        let ask = fixture.ask(&gate);
        let per_capability = |capability: Capability, _: Option<&str>| pep::SessionCtx {
            allowlisted: capability == Capability::FsWrite,
            ..ctx()
        };
        let context = context(
            &fixture,
            ask,
            &per_capability,
            "codex",
            Path::new("/w/session-1"),
        );

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

    /// The same rule in the Claude Code dialect, where the two questions arrive
    /// as `Write` and `Bash` rather than as approval kinds. It is the one that
    /// matters most here: this engine asks about far more tools than Codex does,
    /// so one standing grant answering for all of them would be one grant
    /// standing in for the whole policy.
    #[tokio::test]
    async fn a_standing_claude_write_grant_does_not_authorize_a_command() {
        let _guard = cs_paths::test_data_dir_guard();
        let fixture = Fixture::open("wsstandingclaude");
        // Nobody answers, so an ASKED question ends 'denied' — which is how the
        // test tells "allowed by the grant" from "had to ask".
        let gate = ScriptedGate::answering(ApprovalDecision::Deny);
        let ask = fixture.ask(&gate);
        let per_capability = |capability: Capability, _: Option<&str>| pep::SessionCtx {
            allowlisted: capability == Capability::FsWrite,
            ..ctx()
        };
        let context = context(
            &fixture,
            ask,
            &per_capability,
            "claude-code",
            Path::new("/w/session-1"),
        );

        let write = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 1,
                method: "Write".into(),
                params: serde_json::json!({"file_path": "/w/session-1/src/lib.rs"}),
            },
        )
        .await;
        assert_eq!(write.decision, "approved");
        assert_eq!(write.capability, Some(Capability::FsWrite));

        let bash = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 2,
                method: "Bash".into(),
                params: serde_json::json!({"command": "cargo test"}),
            },
        )
        .await;
        assert_eq!(
            bash.decision, "denied",
            "a write permission must not answer for a command"
        );
        assert_eq!(bash.capability, Some(Capability::Exec));
    }

    /// A request from an engine this build has no vocabulary for is refused
    /// before any capability is guessed at, and the refusal says why.
    #[tokio::test]
    async fn an_engine_without_a_dialect_decides_nothing() {
        let _guard = cs_paths::test_data_dir_guard();
        let fixture = Fixture::open("wsnodialect");
        let gate = ScriptedGate::answering(ApprovalDecision::AllowOnce);
        let ask = fixture.ask(&gate);
        let session = pep::SessionCtx {
            allowlisted: true,
            ..ctx()
        };
        let grants = |_: Capability, _: Option<&str>| session.clone();
        let context = context(&fixture, ask, &grants, "gemini-cli", Path::new("/w/session-1"));
        let outcome = resolve_approval(
            &context,
            &ApprovalRequest {
                request_id: 3,
                method: "Write".into(),
                params: serde_json::json!({"file_path": "/w/session-1/src/lib.rs"}),
            },
        )
        .await;
        assert_eq!(outcome.decision, "denied");
        assert!(outcome.capability.is_none());
        assert!(
            outcome.events.iter().any(|event| matches!(
                event,
                EventPayload::ApprovalRequested { summary, .. } if summary.contains("gemini-cli")
            )),
            "the timeline must name the engine nobody wrote a vocabulary for"
        );
    }
}
