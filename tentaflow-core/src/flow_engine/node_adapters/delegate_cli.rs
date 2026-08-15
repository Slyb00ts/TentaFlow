// ===== File: flow_engine/node_adapters/delegate_cli.rs —
// DelegateCliNodeAdapter (node_type "delegate_cli", category service,
// 1-in/1-out). Delegation of one turn to a vendor CLI agent (Codex, Claude
// Code) — §16.4, §7.5.
//
// The block owns the whole life of one delegation, and the ORDER of the steps
// is the security story:
//
//   1. the session binding, resolved through `code_studio::tools` — the same
//      membership, role, autonomy-ceiling and session-status reading a
//      model-issued tool call gets, not a second one. It comes FIRST so a
//      non-member learns nothing about the node's engines;
//   2. the Phase 0B gate (`cli_adapter::ensure_engine_verified`) — an engine
//      nobody verified against a pinned CLI version never starts at all;
//   3. the egress policy (§17.3) — `local_only` has no vendor CLI, because the
//      sandbox has no route and the promise would be empty;
//   4. the provider adapter, which holds the organization's credential IN THIS
//      PROCESS and hands the CLI a ticket instead (§7.5);
//   5. the ticket, minted through `cli_adapter::issue_ticket`, which runs
//      `cli_delegate` past the PEP — holding `net_egress` is not enough;
//   6. the CLI instance, opened with the adapter's environment: a base URL
//      pointing at the adapter, the ticket as the API key, the session CA as
//      the only extra trust anchor. The credential is in none of them;
//   7. the event pump, which mirrors the vendor's stream onto the session
//      timeline and answers its approvals through `code_studio::pep` — the
//      same decision point, via `cli_bridge::resolve_approval`.
//
// Whatever happens, step 8 runs: the ticket is revoked with the run, the
// adapter is stopped (which is what releases the credential from memory), the
// CLI instance is closed and reaped, and the run row is settled with a status
// that matches what actually happened. A delegation that ran out of budget or
// out of time ends `failed`/`timed_out` and says so — it never reports a turn
// that did not finish as one that did.
// =====

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::AgentServiceSlot;
use crate::code_studio::cli_adapter::{
    self, AdapterConfig, AdapterEventSink, AdapterHandle, Budget, IssuedTicket, TicketDecision,
    TicketRegistry, TicketRequest,
};
use crate::code_studio::cli_bridge::{
    self, ApprovalContext, BridgeEvent, CliBridge, CliInstance, OpenCliInstance, TurnState,
    APPROVAL_TIMEOUT,
};
use crate::code_studio::events::{self, EventPayload, SessionEvent};
use crate::code_studio::patch::PatchScope;
use crate::code_studio::pep::{self, AskKind, Capability};
use crate::code_studio::tools::{self, Bound, ToolCallCtx};
use crate::code_studio::{paths as cs_paths, redact};
use crate::db::DbPool;
use crate::flow_engine::envelope::{ChatRole, FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

use super::patch_review::InteractionGate;

const NODE_TYPE: &str = "delegate_cli";

/// The engines §7.5 defines a ticket protocol for. A configuration naming
/// anything else is refused at parse time rather than at ticket time, because
/// that is a typo, not a missing component.
const KNOWN_ENGINES: &[&str] = &["claude-code", "codex"];

/// Default output variable of the block.
const DEFAULT_OUTPUT_VARIABLE: &str = "delegate_cli";

/// How often the vendor's event stream is drained. The bridge buffers, so this
/// is a latency knob, not a correctness one.
const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Transcript budget handed back to the flow, mirroring the tool-result budget
/// so a chatty CLI cannot blow the turn that reads its summary.
const MAX_TRANSCRIPT_CHARS: usize = tools::MAX_RESULT_CHARS;

/// One validated `delegate_cli` configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationConfig {
    pub engine: String,
    pub service_id: i64,
    /// The single model the ticket authorizes. Not optional: a ticket without a
    /// model authorizes every model the credential can reach (§7.5), so there
    /// is no sensible default to fall back on.
    pub model: String,
    /// Token budget the ticket carries. A delegation with no ceiling is
    /// refused: an opaque vendor loop must not be able to spend without bound.
    pub budget: i64,
    pub timeout_secs: u64,
    pub output_variable: String,
}

impl DelegationConfig {
    pub fn parse(node: &FlowNode) -> Result<Self> {
        let engine = node
            .config
            .get("engine")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "delegate_cli node '{}': 'engine' is required ({})",
                    node.id,
                    KNOWN_ENGINES.join(" | ")
                )
            })?;
        if !KNOWN_ENGINES.contains(&engine) {
            return Err(anyhow!(
                "delegate_cli node '{}': unknown engine '{engine}'; expected one of {}",
                node.id,
                KNOWN_ENGINES.join(", ")
            ));
        }
        let service_id = node
            .config
            .get("service_id")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                anyhow!(
                    "delegate_cli node '{}': 'service_id' must name the service running the CLI",
                    node.id
                )
            })?;
        let budget = node
            .config
            .get("budget")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                anyhow!(
                    "delegate_cli node '{}': 'budget' (tokens) is required; an unbounded \
                     delegation cannot be authorized",
                    node.id
                )
            })?;
        let model = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "delegate_cli node '{}': 'model' is required; the ticket is bound to one \
                     model and cannot be minted without it",
                    node.id
                )
            })?;
        Ok(Self {
            engine: engine.to_string(),
            service_id,
            model: model.to_string(),
            budget,
            timeout_secs: node
                .config
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .filter(|n| *n > 0)
                .unwrap_or(1800),
            output_variable: node
                .config
                .get("output_variable")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_OUTPUT_VARIABLE)
                .to_string(),
        })
    }

    /// The ticket's spending ceiling. The token count is the operator's; the
    /// request and byte floors come from the adapter's own default, because
    /// they still bound a provider that reports no usage at all.
    fn budget(&self) -> Budget {
        Budget {
            max_total_tokens: self.budget as u64,
            ..Budget::default_for_run()
        }
    }

    /// Spellings of the configured model a ticket accepts. Both come from OUR
    /// catalog convention (`<engine>/<id>`), so this is not a guess about how a
    /// vendor aliases its own names: a CLI that sends anything else is refused
    /// by the adapter with `model_not_allowed`, loudly.
    fn model_aliases(&self) -> BTreeSet<String> {
        let mut aliases = BTreeSet::new();
        aliases.insert(self.model.clone());
        if let Some(bare) = self.model.rsplit('/').next() {
            aliases.insert(bare.to_string());
        }
        aliases.insert(format!("{}/{}", self.engine, self.model));
        aliases
    }
}

pub struct DelegateCliNodeAdapter {
    service: AgentServiceSlot,
}

impl DelegateCliNodeAdapter {
    pub fn new(service: AgentServiceSlot) -> Self {
        Self { service }
    }
}

// =============================================================================
// Timeline sink
// =============================================================================

/// Where the adapter's egress and ticket decisions land. `cli_adapter` writes
/// to no database by design, so this is the caller's half of that contract.
struct TimelineSink {
    pool: DbPool,
    session_id: String,
    run_id: String,
    counter: AtomicU64,
}

impl AdapterEventSink for TimelineSink {
    fn record(&self, event: EventPayload) {
        let ordinal = self.counter.fetch_add(1, Ordering::Relaxed);
        // A failed append must not take the relay down: the answer the CLI is
        // waiting for is worth more than one timeline row, and the row is
        // reported through the log instead.
        if let Err(error) = events::append(
            &self.pool,
            &self.session_id,
            SessionEvent::new(format!("cli-adapter:{}:{ordinal}", self.run_id), event)
                .with_run(self.run_id.clone()),
        ) {
            tracing::warn!("delegate_cli: adapter event was not journalled: {error:#}");
        }
    }
}

// =============================================================================
// Run bookkeeping
// =============================================================================

fn open_run(
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
    parent_run_id: Option<&str>,
) -> Result<()> {
    let mut conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    let tx = conn.transaction()?;
    let ordinal: i64 = tx.query_row(
        "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM session_runs WHERE session_id = ?1",
        rusqlite::params![session_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO session_runs \
            (run_id, session_id, ordinal, kind, trigger, parent_run_id, agent_id, status, \
             started_at, finished_at) \
         VALUES (?1, ?2, ?3, 'cli', 'cli_delegate', ?4, NULL, 'running', datetime('now'), NULL)",
        rusqlite::params![run_id, session_id, ordinal, parent_run_id],
    )?;
    events::append_in_tx(
        &tx,
        session_id,
        SessionEvent::new(
            format!("cli-run-started:{run_id}"),
            EventPayload::RunStarted {
                run_id: run_id.to_string(),
                kind: "cli".to_string(),
                trigger: "cli_delegate".to_string(),
            },
        )
        .with_run(run_id.to_string()),
    )?;
    tx.commit()?;
    Ok(())
}

/// Settles the run row and its timeline entry together. Called on every path,
/// including the ones that failed before the CLI ever started.
fn finish_run(pool: &DbPool, session_id: &str, run_id: &str, status: &str, error: Option<&str>) {
    let redacted = error.map(|text| redact::redact_text(text));
    let settle = || -> Result<()> {
        let mut conn = pool
            .write()
            .map_err(|e| anyhow!("workspace db write: {e}"))?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE session_runs SET status = ?2, finished_at = datetime('now') WHERE run_id = ?1",
            rusqlite::params![run_id, status],
        )?;
        events::append_in_tx(
            &tx,
            session_id,
            SessionEvent::new(
                format!("cli-run-finished:{run_id}"),
                EventPayload::RunFinished {
                    run_id: run_id.to_string(),
                    status: status.to_string(),
                    error: redacted.clone(),
                },
            )
            .with_run(run_id.to_string()),
        )?;
        tx.commit()?;
        Ok(())
    };
    if let Err(error) = settle() {
        tracing::warn!("delegate_cli: run '{run_id}' could not be settled: {error:#}");
    }
}

// =============================================================================
// The pump
// =============================================================================

/// What one delegated turn produced.
#[derive(Debug)]
struct Pumped {
    transcript: String,
    state: Option<TurnState>,
    approvals: u32,
    denied_approvals: u32,
}

/// Mirrors the vendor's stream onto the session timeline until the turn ends,
/// the budget runs out, the deadline passes or the run is cancelled.
///
/// The loop never assumes an ending it did not observe: with no terminal
/// notification it returns `state: None`, and the caller settles the run as
/// `timed_out`.
#[allow(clippy::too_many_arguments)]
async fn pump(
    bridge: &CliBridge,
    pool: &DbPool,
    instance: &mut CliInstance,
    approvals: &ApprovalContext<'_>,
    tickets: &TicketRegistry,
    ticket_id: &str,
    deadline: Instant,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Pumped> {
    let mut pumped = Pumped {
        transcript: String::new(),
        state: None,
        approvals: 0,
        denied_approvals: 0,
    };
    let mut ordinal: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(anyhow!("delegate_cli: the run was cancelled"));
        }
        // Budget first: the adapter already stopped the traffic mid-response,
        // so continuing to poll would only add latency to a decided outcome.
        if let Some(what) = tickets.exhausted(ticket_id) {
            return Err(anyhow!(
                "delegate_cli: the delegation stopped because its {what} budget is exhausted; \
                 the CLI's traffic was cut at the adapter and nothing further was spent"
            ));
        }
        for event in bridge.poll(pool, instance).await? {
            ordinal += 1;
            match &event {
                BridgeEvent::Text { text, .. } => {
                    let text = redact::redact_text(text);
                    push_bounded(&mut pumped.transcript, &text);
                    append_message(pool, instance, &text, ordinal);
                }
                BridgeEvent::Notification { method, params, .. } => {
                    if let Some(text) = notification_text(params) {
                        let text = redact::redact_text(&text);
                        push_bounded(&mut pumped.transcript, &text);
                        append_message(pool, instance, &text, ordinal);
                    }
                    if let Some(state) = cli_bridge::turn_state(&event) {
                        tracing::debug!(
                            instance = %instance.id,
                            method = %method,
                            "delegate_cli: the vendor announced the end of the turn"
                        );
                        pumped.state = Some(state);
                    }
                }
                BridgeEvent::Approval { request, .. } => {
                    pumped.approvals += 1;
                    let outcome = cli_bridge::resolve_approval(approvals, request).await;
                    // One key per event, not per approval: an idempotency key
                    // is the identity of a WRITE, so reusing it across the ask
                    // and the answer makes the second one a duplicate and the
                    // timeline shows a question nobody ever decided.
                    for (index, payload) in outcome.events.into_iter().enumerate() {
                        let _ = events::append(
                            pool,
                            &instance.session_id,
                            SessionEvent::new(
                                format!(
                                    "cli-approval:{}:{}:{index}",
                                    instance.id, request.request_id
                                ),
                                payload,
                            )
                            .with_run(instance.run_id.clone()),
                        );
                    }
                    if outcome.decision == "denied" {
                        pumped.denied_approvals += 1;
                    }
                    // The answer goes back even when it is a refusal: an
                    // unanswered request leaves the vendor turn blocked, which
                    // is defect D3 all over again.
                    bridge
                        .answer(instance, request.request_id, outcome.decision)
                        .await?;
                }
                BridgeEvent::VendorSession {
                    vendor_session_id, ..
                } => {
                    if !vendor_session_id.is_empty() && *vendor_session_id != instance.vendor_session_id
                    {
                        instance.vendor_session_id = vendor_session_id.clone();
                        cli_bridge::set_instance_vendor_session(
                            pool,
                            &instance.id,
                            vendor_session_id,
                        )?;
                    }
                }
                BridgeEvent::Other { kind, .. } => {
                    tracing::debug!(instance = %instance.id, %kind, "delegate_cli: unmapped event");
                }
            }
        }
        if pumped.state.is_some() {
            return Ok(pumped);
        }
        if Instant::now() >= deadline {
            return Ok(pumped);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Appends to the transcript without letting one event blow the budget. The
/// timeline still carries the whole message; this bounds only what the FLOW
/// carries onward into a model's context.
fn push_bounded(transcript: &mut String, text: &str) {
    let used = transcript.chars().count();
    if used >= MAX_TRANSCRIPT_CHARS {
        return;
    }
    transcript.extend(text.chars().take(MAX_TRANSCRIPT_CHARS - used));
}

/// The human-readable half of a vendor notification, when it has one. A frame
/// that carries no text is a control frame; putting its JSON on the timeline
/// would be noise nobody reads.
fn notification_text(params: &Value) -> Option<String> {
    for field in ["text", "message", "delta", "content"] {
        if let Some(text) = params.get(field).and_then(Value::as_str) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn append_message(pool: &DbPool, instance: &CliInstance, text: &str, ordinal: u64) {
    let _ = events::append(
        pool,
        &instance.session_id,
        SessionEvent::new(
            format!("cli-msg:{}:{ordinal}", instance.id),
            EventPayload::AgentMessage {
                role: "assistant".to_string(),
                text: text.to_string(),
            },
        )
        .with_run(instance.run_id.clone()),
    );
}

// =============================================================================
// Ticket issuance
// =============================================================================

/// Mints the run's ticket, asking the operator when the PEP says to.
///
/// The PEP is consulted inside `cli_adapter::issue_ticket`; this function adds
/// only the operator round-trip, using the same suspend/persist machinery a
/// model-issued tool call uses. There is no second place where `cli_delegate`
/// is decided.
async fn mint_ticket(
    call_ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    tickets: &TicketRegistry,
    request: TicketRequest,
) -> Result<IssuedTicket> {
    // The engine is the grant's target: "always allow delegating to codex" is a
    // permission an operator can actually reason about, whereas a grant with no
    // target would cover every engine ever configured.
    let target_label = request.engine_id.clone();
    let session_ctx = tools::session_ctx_for(
        call_ctx.main_db,
        bound,
        Capability::CliDelegate,
        Some(&target_label),
    )?;
    let summary = match cli_adapter::issue_ticket(tickets, &session_ctx, request.clone())? {
        TicketDecision::Issued(ticket) => return Ok(*ticket),
        TicketDecision::Denied { reason } => {
            return Err(anyhow!("delegate_cli: {reason}"));
        }
        TicketDecision::Ask { summary } => summary,
    };

    let decision = tools::suspend_for_operator(
        call_ctx,
        bound,
        Capability::CliDelegate,
        Some(&target_label),
        &summary,
        AskKind::Permission,
    )
    .await?;
    if !decision.allows() {
        return Err(anyhow!(
            "delegate_cli: the operator refused to delegate this turn to '{target_label}'"
        ));
    }
    tools::persist_grant(
        call_ctx,
        bound,
        Capability::CliDelegate,
        Some(&target_label),
        decision,
    )?;

    // The operator answered for THIS run, so that is the grant the PEP is told
    // about. Re-running the whole decision (rather than skipping it) keeps the
    // role, the autonomy mode and the boundary in force — an approval buys none
    // of those.
    let granted = pep::SessionCtx {
        run_granted: true,
        ..session_ctx
    };
    match cli_adapter::issue_ticket(tickets, &granted, request)? {
        TicketDecision::Issued(ticket) => Ok(*ticket),
        TicketDecision::Denied { reason } => Err(anyhow!("delegate_cli: {reason}")),
        TicketDecision::Ask { summary } => Err(anyhow!(
            "delegate_cli: the delegation is still not authorized after the operator answered \
             ({summary})"
        )),
    }
}

// =============================================================================
// Adapter start
// =============================================================================

async fn start_adapter_for(
    main_db: &DbPool,
    cipher: &crate::crypto::SettingsCipher,
    bound: &Bound,
    engine_id: &str,
    sink: Arc<dyn AdapterEventSink>,
    tickets: Arc<TicketRegistry>,
) -> Result<AdapterHandle> {
    let ca_path = cs_paths::session_tmp_dir(&bound.workspace.id, &bound.session.id)?
        .join(format!("cli-{engine_id}-ca.pem"));
    cli_adapter::start_adapter(
        main_db,
        cipher,
        AdapterConfig {
            // Loopback: the bridge service is itself loopback-only
            // (`services::coding_agent`), so the CLI it spawns lives on this
            // host. On a shared loopback the TICKET is the peer check (§7.6) —
            // which is exactly why it is mandatory and scoped to one run.
            bind_addr: ([127, 0, 0, 1], 0).into(),
            engine_id: engine_id.to_string(),
            org_id: bound.workspace.org_id.clone(),
            // The vault is node-local and the runtime database of this
            // workspace only exists on its owner node, so the workspace's node
            // IS this node; reading the credential under any other key would be
            // reading a row that cannot decrypt here.
            node_id: bound.workspace.node_id.clone(),
            ca_path,
            dns_names: vec!["localhost".to_string(), "127.0.0.1".to_string()],
            tickets,
            sink,
        },
    )
    .await
}

// =============================================================================
// The block
// =============================================================================

#[async_trait]
impl NodeAdapter for DelegateCliNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Any)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("delegate_cli: missing input edge"))?;
        let envelope = &input.envelope;

        let config = DelegationConfig::parse(node)?;
        let binding = tools::binding_from_meta(&envelope.meta).ok_or_else(|| {
            anyhow!(
                "delegate_cli: this run carries no Code Studio session binding \
                 (meta.code_session)"
            )
        })?;
        let user_id = ctx.user_id.clone().ok_or_else(|| {
            anyhow!("delegate_cli: delegating a turn needs a user identity to act for")
        })?;
        let parent_run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let prompt = delegation_prompt(envelope).ok_or_else(|| {
            anyhow!("delegate_cli: there is nothing to delegate — the input carries no task text")
        })?;

        let service = self
            .service
            .read()
            .clone()
            .ok_or_else(|| anyhow!("delegate_cli: AgentService slot not wired"))?;
        let main_db = service.db().clone();
        let cipher = service.settings_cipher().clone();

        let registry = crate::agents::interaction_registry_global();
        let manager = crate::agents::agent_run_manager_global();
        let extend = |waited: Duration| ctx.extend_deadline(waited);
        let gate = InteractionGate::new(
            &registry,
            manager.as_deref(),
            ctx.progress.as_ref(),
            &ctx.progress_scope,
            &parent_run_id,
            None,
            &extend,
        );
        let call_id = format!("{}:{}", node.id, ctx.execution_id);
        let call_ctx = ToolCallCtx {
            main_db: &main_db,
            user_id: &user_id,
            run_id: (!parent_run_id.is_empty()).then_some(parent_run_id.as_str()),
            tool_call_id: &call_id,
            binding: &binding,
            gate: &gate,
        };

        let bound = tools::bind(&call_ctx).await?;

        // Step 2 — the engine has to have passed Phase 0B before anything is
        // started, opened or spent.
        cli_adapter::ensure_engine_verified(&main_db, &config.engine)
            .map_err(|refusal| anyhow!("delegate_cli: {refusal}"))?;

        // Step 3 — §17.3: under `local_only` the sandbox has no route, so a
        // vendor CLI is not "degraded", it is absent.
        if bound.workspace.egress_policy == "local_only" {
            return Err(anyhow!(
                "delegate_cli: workspace '{}' runs under the 'local_only' egress policy, which \
                 has no vendor CLI agent (§17.3) — the sandbox has no route to a provider and \
                 pretending otherwise would be a promise without a mechanism",
                bound.workspace.name
            ));
        }

        let bridge = resolve_bridge(&main_db, config.service_id, &config.engine)?;
        let worktree = tools::session_worktree(&bound.workspace.id, &bound.session.id)?;

        // The patch set is opened BEFORE the CLI writes anything, so its base
        // commit is the pre-delegation HEAD and the review that follows sees
        // exactly what the delegation changed (§16.4: "returns a patch set").
        let patch_set = tools::current_patch_set(
            &bound.pool,
            &bound.broker,
            &bound.session.id,
            &PatchScope::Work,
        )?;

        let run_id = uuid::Uuid::new_v4().to_string();
        open_run(
            &bound.pool,
            &bound.session.id,
            &run_id,
            (!parent_run_id.is_empty()).then_some(parent_run_id.as_str()),
        )?;

        let outcome = delegate(
            &call_ctx,
            &bound,
            &bridge,
            &config,
            &cipher,
            &run_id,
            &worktree,
            &prompt,
            ctx,
        )
        .await;

        match outcome {
            Ok(report) => {
                let completed = report.run_status == "completed";
                finish_run(
                    &bound.pool,
                    &bound.session.id,
                    &run_id,
                    report.run_status,
                    (!completed).then_some(report.detail.as_str()),
                );
                if !completed {
                    // The usage travels with the refusal: an operator reading a
                    // failed delegation needs to know what it already spent.
                    return Err(anyhow!(
                        "delegate_cli node '{}': the delegation to '{}' ended '{}': {} (spent {} \
                         of {} tokens over {} request(s))",
                        node.id,
                        config.engine,
                        report.run_status,
                        report.detail,
                        report.usage.total_tokens(),
                        config.budget,
                        report.usage.requests
                    ));
                }
                let mut out: FlowEnvelope = (**envelope).clone();
                out.variables.insert(
                    config.output_variable.clone(),
                    FlowValue::Json(report.to_json(&config, &run_id, &patch_set.id)),
                );
                out.payload = FlowValue::Text(report.transcript.clone());
                Ok(out)
            }
            Err(error) => {
                let message = format!("{error:#}");
                finish_run(
                    &bound.pool,
                    &bound.session.id,
                    &run_id,
                    "failed",
                    Some(&message),
                );
                Err(error)
            }
        }
    }
}

/// What the delegation is being asked to do. The block delegates a TASK, so an
/// input that carries none is a configuration error, not an empty prompt to
/// send to a vendor at the organization's expense.
fn delegation_prompt(envelope: &FlowEnvelope) -> Option<String> {
    if let Some(text) = envelope.payload.as_text() {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }
    // The last thing the PERSON said, not the last thing anyone said: an
    // assistant turn is the harness talking to itself, and sending it to a
    // vendor would delegate our own output back to us.
    envelope
        .context
        .messages
        .iter()
        .rev()
        .filter(|message| message.role == ChatRole::User)
        .find_map(|message| {
            let text = message.text_or_default();
            (!text.trim().is_empty()).then_some(text)
        })
}

fn resolve_bridge(main_db: &DbPool, service_id: i64, engine: &str) -> Result<CliBridge> {
    let row = {
        let conn = main_db
            .read()
            .map_err(|e| anyhow!("registry db read: {e}"))?;
        crate::services_repo::services::get(&conn, service_id)?
    }
    .ok_or_else(|| anyhow!("delegate_cli: service {service_id} does not exist"))?;
    if row.engine_id != engine {
        return Err(anyhow!(
            "delegate_cli: service {service_id} runs '{}', not '{engine}'",
            row.engine_id
        ));
    }
    CliBridge::new(row).map_err(|e| anyhow!("delegate_cli: {e}"))
}

/// What one finished delegation reports.
struct Report {
    run_status: &'static str,
    detail: String,
    transcript: String,
    usage: cli_adapter::Usage,
    approvals: u32,
    denied_approvals: u32,
    vendor_session_id: String,
    instance_id: String,
}

impl Report {
    fn to_json(&self, config: &DelegationConfig, run_id: &str, patch_set_id: &str) -> Value {
        json!({
            "engine": config.engine,
            "model": config.model,
            "status": self.run_status,
            "detail": self.detail,
            "run_id": run_id,
            "cli_instance_id": self.instance_id,
            "vendor_session_id": self.vendor_session_id,
            "patch_set_id": patch_set_id,
            "approvals": self.approvals,
            "approvals_denied": self.denied_approvals,
            "usage": {
                "requests": self.usage.requests,
                "input_tokens": self.usage.input_tokens,
                "output_tokens": self.usage.output_tokens,
                "total_tokens": self.usage.total_tokens(),
                "budget_tokens": config.budget,
            },
        })
    }
}

/// The delegation proper. Every resource it acquires is released here, on every
/// path, before the caller settles the run.
#[allow(clippy::too_many_arguments)]
async fn delegate(
    call_ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    bridge: &CliBridge,
    config: &DelegationConfig,
    cipher: &crate::crypto::SettingsCipher,
    run_id: &str,
    worktree: &std::path::Path,
    prompt: &str,
    ctx: &ExecutionContext,
) -> Result<Report> {
    let tickets = Arc::new(TicketRegistry::new());
    let sink: Arc<dyn AdapterEventSink> = Arc::new(TimelineSink {
        pool: bound.pool.clone(),
        session_id: bound.session.id.clone(),
        run_id: run_id.to_string(),
        counter: AtomicU64::new(0),
    });
    let adapter = start_adapter_for(
        call_ctx.main_db,
        cipher,
        bound,
        &config.engine,
        sink,
        tickets.clone(),
    )
    .await?;

    let result = run_delegation(
        call_ctx, bound, bridge, config, &adapter, &tickets, run_id, worktree, prompt, ctx,
    )
    .await;

    // The ticket dies with the run — a stolen one is worthless the moment the
    // delegation ends (§7.5) — and stopping the adapter is what drops the
    // organization's credential out of this process.
    tickets.revoke_run(run_id);
    adapter.shutdown();
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_delegation(
    call_ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    bridge: &CliBridge,
    config: &DelegationConfig,
    adapter: &AdapterHandle,
    tickets: &Arc<TicketRegistry>,
    run_id: &str,
    worktree: &std::path::Path,
    prompt: &str,
    ctx: &ExecutionContext,
) -> Result<Report> {
    let instance_id = uuid::Uuid::new_v4().to_string();
    let wiring = adapter.wiring();
    let ticket = mint_ticket(
        call_ctx,
        bound,
        tickets,
        TicketRequest {
            session_id: bound.session.id.clone(),
            run_id: run_id.to_string(),
            cli_instance_id: instance_id.clone(),
            engine_id: config.engine.clone(),
            model: config.model.clone(),
            model_aliases: config.model_aliases(),
            methods: wiring.ticket_methods.clone(),
            path_prefixes: wiring.ticket_path_prefixes.clone(),
            budget: config.budget(),
            ttl: Duration::from_secs(config.timeout_secs),
            // The provider this engine reaches is the one an administrator
            // recorded in the vault row and signed off in the Phase 0B note;
            // `ensure_engine_verified` has already refused every engine that
            // decision was never made for. `local_only` was refused earlier,
            // and it is the one policy that forbids a provider outright.
            host_allowlisted: true,
        },
    )
    .await?;
    let _ = events::append(
        &bound.pool,
        &bound.session.id,
        SessionEvent::new(
            format!("cli-ticket:{}", ticket.claims.ticket_id),
            ticket.event(),
        )
        .with_run(run_id.to_string()),
    );

    let env = adapter.sandbox_env(&ticket);
    let mut instance = bridge
        .open(
            &bound.pool,
            OpenCliInstance {
                instance_id: &instance_id,
                session_id: &bound.session.id,
                run_id,
                worktree,
                model: &config.model,
                ticket_id: Some(&ticket.claims.ticket_id),
                resume_vendor_session_id: None,
                env: &env,
            },
        )
        .await?;

    let turn = drive_turn(
        call_ctx.main_db,
        bridge,
        bound,
        config,
        tickets,
        &ticket,
        &mut instance,
        prompt,
        ctx,
    )
    .await;

    // Closing is not conditional on success: an instance nobody closed is a
    // vendor process nobody reaps (D2).
    if let Err(error) = bridge.close(&bound.pool, &instance).await {
        tracing::warn!(
            instance = %instance.id,
            "delegate_cli: the CLI instance did not close cleanly: {error:#}"
        );
        // Recorded, never propagated: the turn's own outcome is the answer the
        // caller needs, and a failure to write this row must not replace it.
        if let Err(error) = cli_bridge::set_instance_status(&bound.pool, &instance.id, "failed") {
            tracing::warn!("delegate_cli: instance status not recorded: {error:#}");
        }
    }

    let pumped = turn?;
    let usage = tickets
        .usage(&ticket.claims.ticket_id)
        .unwrap_or_default();
    let (run_status, detail) = match &pumped.state {
        Some(TurnState::Completed) => ("completed", "the vendor reported the turn complete".into()),
        Some(TurnState::Failed(reason)) => ("failed", redact::redact_text(reason)),
        None => (
            "timed_out",
            format!(
                "the vendor announced no end of turn within {}s; the CLI was closed and its \
                 ticket revoked",
                config.timeout_secs
            ),
        ),
    };
    Ok(Report {
        run_status,
        detail,
        transcript: pumped.transcript,
        usage,
        approvals: pumped.approvals,
        denied_approvals: pumped.denied_approvals,
        vendor_session_id: instance.vendor_session_id.clone(),
        instance_id: instance.id.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn drive_turn(
    main_db: &DbPool,
    bridge: &CliBridge,
    bound: &Bound,
    config: &DelegationConfig,
    tickets: &Arc<TicketRegistry>,
    ticket: &IssuedTicket,
    instance: &mut CliInstance,
    prompt: &str,
    ctx: &ExecutionContext,
) -> Result<Pumped> {
    let registry = crate::agents::interaction_registry_global();
    let manager = crate::agents::agent_run_manager_global();
    // A CLI approval is about the filesystem or about a command, so the
    // standing permissions are read PER CAPABILITY, at the moment the question
    // arrives — an `fs_write` allowlist entry must never answer for an `exec`.
    // No target label: the request's own target is what
    // `cli_bridge::resolve_approval` bounds the decision with, and the
    // `cli_delegate` grant that started this run buys nothing here.
    let grants = |capability: Capability| -> pep::SessionCtx {
        tools::session_ctx_for(main_db, bound, capability, None).unwrap_or_else(|error| {
            // A permission table that cannot be read is not a permission. The
            // fallback holds nothing, so the question reaches the operator
            // rather than being answered from state nobody could load.
            tracing::warn!(
                "delegate_cli: standing grants unreadable, asking the operator instead: {error:#}"
            );
            pep::SessionCtx {
                role: bound.role,
                autonomy: bound.autonomy,
                is_coordinator: false,
                has_accepted_patch_set: false,
                allowlisted: false,
                session_granted: false,
                run_granted: false,
            }
        })
    };
    let worktree = tools::session_worktree(&bound.workspace.id, &bound.session.id)?;
    let approval_run_id = instance.run_id.clone();
    let approvals = ApprovalContext {
        session: &grants,
        session_id: &bound.session.id,
        run_id: &approval_run_id,
        parent_run_id: None,
        engine_id: &config.engine,
        worktree: &worktree,
        registry: &registry,
        manager: manager.as_deref(),
        progress: ctx.progress.as_ref(),
        progress_scope: &ctx.progress_scope,
        timeout: APPROVAL_TIMEOUT,
    };
    let deadline = Instant::now() + Duration::from_secs(config.timeout_secs);
    bridge.turn(instance, prompt).await?;
    pump(
        bridge,
        &bound.pool,
        instance,
        &approvals,
        tickets,
        &ticket.claims.ticket_id,
        deadline,
        &ctx.cancel_token,
    )
    .await
}

/// Kept so a caller can read the block's declared shape without executing it.
pub fn known_engines() -> &'static [&'static str] {
    KNOWN_ENGINES
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{AutonomyMode, WorkspaceRole};
    use crate::code_studio::{paths as cs_paths, workspace_db};
    use crate::services::transport::Transport;
    use crate::services_repo::services::{DeployMethod, NewService, ServiceStatus};
    use serde_json::json;
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn node(config: Value) -> FlowNode {
        FlowNode {
            id: "d1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    // =========================================================================
    // A stub for the ONE dependency a test machine cannot have
    // =========================================================================

    /// Speaks the coding-agent bridge's HTTP surface and nothing else.
    ///
    /// This stands in for the VENDOR PROCESS, which is the only part of the
    /// path a machine without `codex`/`claude` installed cannot run. Everything
    /// under test — `CliBridge`, `pump`, `resolve_approval`, the PEP, the
    /// ticket registry, the timeline — is the real implementation, reached over
    /// a real socket through the real `services::coding_agent` proxy with its
    /// loopback and transport checks in force.
    struct StubBridge {
        addr: SocketAddr,
        answered: Arc<Mutex<Vec<(u64, String)>>>,
        prompts: Arc<Mutex<Vec<String>>>,
        closed: Arc<Mutex<bool>>,
    }

    async fn stub_bridge(script: Vec<Value>) -> StubBridge {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let answered = Arc::new(Mutex::new(Vec::new()));
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(false));
        let (a, p, c) = (answered.clone(), prompts.clone(), closed.clone());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (script, a, p, c) = (script.clone(), a.clone(), p.clone(), c.clone());
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0_u8; 4096];
                    let (head, body) = loop {
                        let Ok(read) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        let Some(end) = buffer
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                            .map(|i| i + 4)
                        else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().ok())?
                            })
                            .unwrap_or(0);
                        while buffer.len() < end + length {
                            let Ok(read) = socket.read(&mut chunk).await else {
                                return;
                            };
                            if read == 0 {
                                break;
                            }
                            buffer.extend_from_slice(&chunk[..read]);
                        }
                        break (head, String::from_utf8_lossy(&buffer[end..]).into_owned());
                    };
                    let request_line = head.lines().next().unwrap_or_default().to_string();
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_string();
                    let target = parts.next().unwrap_or_default().to_string();
                    let payload: Value =
                        serde_json::from_str(&body).unwrap_or(Value::Object(Default::default()));

                    let response = if method == "POST" && target == "/sessions" {
                        json!({"session": {"id": "bridge-1", "vendor_session_id": "vendor-1"}})
                    } else if target.ends_with("/turn") {
                        p.lock()
                            .expect("prompts")
                            .push(payload["prompt"].as_str().unwrap_or_default().to_string());
                        json!({"started": true})
                    } else if target.ends_with("/approval") {
                        a.lock().expect("answered").push((
                            payload["request_id"].as_u64().unwrap_or_default(),
                            payload["decision"].as_str().unwrap_or_default().to_string(),
                        ));
                        json!({"answered": true})
                    } else if target.contains("/events") {
                        let after: u64 = target
                            .split("after_seq=")
                            .nth(1)
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let events: Vec<Value> = script
                            .iter()
                            .filter(|e| e["seq"].as_u64().unwrap_or(0) > after)
                            .cloned()
                            .collect();
                        json!({ "events": events })
                    } else if method == "DELETE" {
                        *c.lock().expect("closed") = true;
                        json!({"closed": true, "process_state": "reaped"})
                    } else {
                        json!({})
                    };
                    let body = response.to_string();
                    let out = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(out.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        StubBridge {
            addr,
            answered,
            prompts,
            closed,
        }
    }

    /// A registry database holding one coding-agent bridge service pointed at
    /// the stub. Inserted through the real repository, so the row is the row
    /// `resolve_bridge` would read in production.
    fn db_with_bridge(engine: &str, addr: SocketAddr) -> (crate::db::DbPool, i64) {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("init db");
        let mut new = NewService::minimal(engine, DeployMethod::NativeManagedCli, Transport::AgentRpc);
        new.category = "coding-agent".to_string();
        new.status = ServiceStatus::Running;
        new.endpoint_url = Some(format!("http://{addr}"));
        let id = {
            let conn = db.write().expect("write");
            crate::services_repo::services::insert(&conn, &new).expect("insert service")
        };
        (db, id)
    }

    /// A workspace runtime database with one open session and one CLI run, laid
    /// out exactly as the coordinator would leave it.
    fn workspace_fixture(workspace_id: &str, run_id: &str) -> DbPool {
        cs_paths::create_workspace_layout(workspace_id).expect("layout");
        let pool = workspace_db::open(workspace_id).expect("workspace db");
        let conn = pool.write().expect("write");
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
              flow_id, flow_version_id, status, created_at, updated_at) \
             VALUES ('sess-1', ?1, 'u-1', 'S', 'cs/u/1', 'normal', 'f', 'v', 'running', \
              datetime('now'), datetime('now'))",
            rusqlite::params![workspace_id],
        )
        .expect("session row");
        conn.execute(
            "INSERT INTO session_runs (run_id, session_id, ordinal, kind, trigger, status, \
              started_at) VALUES (?1, 'sess-1', 1, 'cli', 'cli_delegate', 'running', \
              datetime('now'))",
            rusqlite::params![run_id],
        )
        .expect("run row");
        drop(conn);
        pool
    }

    fn ticket_ctx(autonomy: AutonomyMode) -> pep::SessionCtx {
        pep::SessionCtx {
            role: WorkspaceRole::Editor,
            autonomy,
            is_coordinator: false,
            has_accepted_patch_set: false,
            allowlisted: false,
            // A standing grant so the ticket is minted without an operator; the
            // PEP is still what decides — this is the state it decides from.
            session_granted: true,
            run_granted: false,
        }
    }

    fn ticket_request(run_id: &str, instance_id: &str, budget_tokens: u64) -> TicketRequest {
        TicketRequest {
            session_id: "sess-1".into(),
            run_id: run_id.into(),
            cli_instance_id: instance_id.into(),
            engine_id: "codex".into(),
            model: "gpt-5-codex".into(),
            model_aliases: BTreeSet::new(),
            methods: ["POST".to_string()].into_iter().collect(),
            path_prefixes: vec!["/v1".to_string()],
            budget: Budget {
                max_requests: 10,
                max_total_tokens: budget_tokens,
                max_bytes: 1_000_000,
            },
            ttl: Duration::from_secs(120),
            host_allowlisted: true,
        }
    }

    /// The timeline as it is STORED, kind plus the raw CBOR bytes of the
    /// payload. Reading the bytes rather than a re-encoded copy is deliberate:
    /// the leak test has to look at what is actually on disk, and CBOR keeps
    /// text strings as literal UTF-8, so a credential that survived is
    /// findable in them.
    fn timeline(pool: &DbPool) -> Vec<(String, Vec<u8>)> {
        let conn = pool.read().expect("read");
        let mut stmt = conn
            .prepare("SELECT kind, payload_cbor FROM session_events ORDER BY seq")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("rows");
        rows
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    // =========================================================================
    // Configuration
    // =========================================================================

    #[test]
    fn configuration_is_validated_before_anything_is_attempted() {
        assert!(DelegationConfig::parse(&node(json!({}))).is_err());
        assert!(DelegationConfig::parse(&node(json!({"engine": "gpt-cli"}))).is_err());
        // A budget is not optional: an opaque vendor loop with no ceiling is
        // exactly the thing §7.5 refuses to authorize.
        assert!(
            DelegationConfig::parse(&node(json!({"engine": "codex", "service_id": 3}))).is_err()
        );
        // Neither is a model: the ticket is bound to one, and a ticket without
        // it would authorize the whole account.
        assert!(DelegationConfig::parse(&node(
            json!({"engine": "codex", "service_id": 3, "budget": 100})
        ))
        .is_err());
        let parsed = DelegationConfig::parse(&node(json!({
            "engine": "codex", "service_id": 3, "budget": 100000, "model": "gpt-5"
        })))
        .expect("valid config");
        assert_eq!(parsed.engine, "codex");
        assert_eq!(parsed.service_id, 3);
        assert_eq!(parsed.budget, 100_000);
        assert_eq!(parsed.model, "gpt-5");
        assert_eq!(parsed.timeout_secs, 1800);
        assert_eq!(parsed.output_variable, DEFAULT_OUTPUT_VARIABLE);
        assert!(known_engines().contains(&"claude-code"));
    }

    /// The operator's number is the TOKEN ceiling; the request and byte floors
    /// stay the adapter's, because they are what still bounds a provider that
    /// reports no usage at all.
    #[test]
    fn the_block_sets_the_token_ceiling_and_keeps_the_adapters_floors() {
        let config = DelegationConfig::parse(&node(json!({
            "engine": "codex", "service_id": 1, "budget": 4242, "model": "gpt-5"
        })))
        .expect("config");
        let budget = config.budget();
        assert_eq!(budget.max_total_tokens, 4242);
        assert_eq!(budget.max_requests, Budget::default_for_run().max_requests);
        assert_eq!(budget.max_bytes, Budget::default_for_run().max_bytes);
    }

    #[tokio::test]
    async fn a_bridge_running_another_engine_is_refused() {
        let stub = stub_bridge(Vec::new()).await;
        let (db, service_id) = db_with_bridge("claude-code", stub.addr);
        let error = resolve_bridge(&db, service_id, "codex").expect_err("engine mismatch");
        assert!(format!("{error:#}").contains("claude-code"));
        assert!(resolve_bridge(&db, service_id + 99, "codex").is_err());
        assert!(resolve_bridge(&db, service_id, "claude-code").is_ok());
    }

    // =========================================================================
    // Behaviour
    // =========================================================================

    /// Sets up a real workspace runtime database, a real bridge client over the
    /// stub, and a real ticket minted through the PEP.
    async fn scenario(
        workspace_id: &str,
        run_id: &str,
        instance_id: &str,
        budget_tokens: u64,
        script: Vec<Value>,
    ) -> (
        tempfile::TempDir,
        DbPool,
        CliBridge,
        TicketRegistry,
        IssuedTicket,
        CliInstance,
        StubBridge,
    ) {
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let pool = workspace_fixture(workspace_id, run_id);
        let stub = stub_bridge(script).await;
        let (db, service_id) = db_with_bridge("codex", stub.addr);
        let bridge = resolve_bridge(&db, service_id, "codex").expect("bridge");

        let tickets = TicketRegistry::new();
        let decision = cli_adapter::issue_ticket(
            &tickets,
            &ticket_ctx(AutonomyMode::Normal),
            ticket_request(run_id, instance_id, budget_tokens),
        )
        .expect("issue");
        let TicketDecision::Issued(ticket) = decision else {
            panic!("the PEP must issue a ticket to an editor holding a standing grant");
        };
        let instance = bridge
            .open(
                &pool,
                OpenCliInstance {
                    instance_id,
                    session_id: "sess-1",
                    run_id,
                    worktree: data.path(),
                    model: "gpt-5-codex",
                    ticket_id: Some(&ticket.claims.ticket_id),
                    resume_vendor_session_id: None,
                    env: &[],
                },
            )
            .await
            .expect("open instance");
        (data, pool, bridge, tickets, *ticket, instance, stub)
    }

    fn approval_context<'a>(
        grants: &'a (dyn Fn(Capability) -> pep::SessionCtx + Send + Sync),
        worktree: &'a std::path::Path,
        run_id: &'a str,
        registry: &'a crate::agents::interaction::InteractionRegistry,
        progress: &'a dyn crate::flow_engine::dispatchers::progress::ProgressSink,
    ) -> ApprovalContext<'a> {
        ApprovalContext {
            session: grants,
            session_id: "sess-1",
            run_id,
            parent_run_id: None,
            engine_id: "codex",
            worktree,
            registry,
            manager: None,
            progress,
            progress_scope: "code-studio",
            // Short on purpose: no test may depend on a human, and an
            // unanswered question is a refusal either way.
            timeout: Duration::from_millis(80),
        }
    }

    /// A budget that is crossed STOPS the delegation, and it stops it in both
    /// places that matter: the adapter refuses the CLI's next request, and the
    /// block gives up instead of polling a CLI whose traffic is already cut.
    ///
    /// The overrun is recorded by the real `TicketRegistry` off a real ticket
    /// minted through the real PEP — nothing here is a stand-in for the budget.
    #[tokio::test]
    async fn an_exhausted_budget_stops_the_delegation() {
        let _guard = cs_paths::test_data_dir_guard();
        let (data, pool, bridge, tickets, ticket, mut instance, _stub) = scenario(
            "wsbudget",
            "run-budget",
            "cli-budget",
            100,
            vec![json!({"seq": 1, "kind": "terminal", "data": {"text": "still working"}})],
        )
        .await;

        let crossed = tickets.record(
            &ticket.claims.ticket_id,
            cli_adapter::Usage {
                requests: 0,
                input_tokens: 2_000,
                output_tokens: 2_000,
                bytes_up: 0,
                bytes_down: 64,
            },
        );
        assert_eq!(crossed, Some("tokens"), "4000 tokens against a ceiling of 100");
        assert_eq!(tickets.exhausted(&ticket.claims.ticket_id), Some("tokens"));

        // The CLI's own next call no longer buys anything.
        let refusal = tickets
            .authorize(
                Some(ticket.presentation.as_str()),
                &cli_adapter::RequestFacts {
                    method: "POST",
                    path: "/v1/responses",
                    model: None,
                    body_len: 1,
                    cli_instance_id: Some("cli-budget"),
                },
            )
            .expect_err("an exhausted ticket must not authorize another request");
        assert_eq!(refusal.slug(), "budget_exhausted");

        let registry = crate::agents::interaction::InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let grants = |_: Capability| ticket_ctx(AutonomyMode::Normal);
        let approvals =
            approval_context(&grants, data.path(), "run-budget", &registry, &progress);
        let cancel = tokio_util::sync::CancellationToken::new();
        let error = pump(
            &bridge,
            &pool,
            &mut instance,
            &approvals,
            &tickets,
            &ticket.claims.ticket_id,
            // Generous: the deadline must NOT be what ends this.
            Instant::now() + Duration::from_secs(60),
            &cancel,
        )
        .await
        .expect_err("an exhausted budget must end the delegation");
        let message = format!("{error:#}");
        assert!(message.contains("tokens budget is exhausted"), "{message}");

        workspace_db::close("wsbudget");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// An approval the CLI raises is decided by the PEP, and the refusal
    /// reaches the CLI as an answer rather than as silence (defect D3).
    ///
    /// The two halves are the point: the same request is refused in `plan` mode
    /// and allowed in `autonomous` with a standing grant, so what decided is
    /// demonstrably the policy and not a constant.
    #[tokio::test]
    async fn a_bridge_approval_is_decided_by_the_pep_and_the_refusal_reaches_the_cli() {
        let _guard = cs_paths::test_data_dir_guard();
        let script = |cwd: &str| {
            vec![
                json!({"seq": 1, "kind": "approval_request", "data": {
                    "request_id": 7,
                    "method": "execCommandApproval",
                    "params": {"cwd": cwd, "command": ["cargo", "test"]}
                }}),
                json!({"seq": 2, "kind": "codex", "data": {
                    "method": "turn/completed", "params": {}
                }}),
            ]
        };

        // --- refused: `plan` mode runs no commands at all ---
        let data = tempfile::tempdir().expect("data dir");
        let cwd = data.path().to_string_lossy().to_string();
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(cwd.clone()),
        );
        let pool = workspace_fixture("wsappr", "run-appr");
        let stub = stub_bridge(script(&cwd)).await;
        let (db, service_id) = db_with_bridge("codex", stub.addr);
        let bridge = resolve_bridge(&db, service_id, "codex").expect("bridge");
        let tickets = TicketRegistry::new();
        let TicketDecision::Issued(ticket) = cli_adapter::issue_ticket(
            &tickets,
            &ticket_ctx(AutonomyMode::Normal),
            ticket_request("run-appr", "cli-appr", 10_000),
        )
        .expect("issue") else {
            panic!("ticket");
        };
        let mut instance = bridge
            .open(
                &pool,
                OpenCliInstance {
                    instance_id: "cli-appr",
                    session_id: "sess-1",
                    run_id: "run-appr",
                    worktree: data.path(),
                    model: "gpt-5-codex",
                    ticket_id: Some(&ticket.claims.ticket_id),
                    resume_vendor_session_id: None,
                    env: &[],
                },
            )
            .await
            .expect("open");

        let registry = crate::agents::interaction::InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let refusing = |_: Capability| ticket_ctx(AutonomyMode::Plan);
        let approvals =
            approval_context(&refusing, data.path(), "run-appr", &registry, &progress);
        let cancel = tokio_util::sync::CancellationToken::new();
        let pumped = pump(
            &bridge,
            &pool,
            &mut instance,
            &approvals,
            &tickets,
            &ticket.claims.ticket_id,
            Instant::now() + Duration::from_secs(30),
            &cancel,
        )
        .await
        .expect("the pump runs to the end of the turn");

        assert_eq!(pumped.approvals, 1);
        assert_eq!(pumped.denied_approvals, 1);
        assert_eq!(pumped.state, Some(TurnState::Completed));
        assert_eq!(
            *stub.answered.lock().expect("answered"),
            vec![(7_u64, "denied".to_string())],
            "the CLI must be told 'denied' — an unanswered request is the hang D3 fixed"
        );
        let events = timeline(&pool);
        assert!(events.iter().any(|(kind, _)| kind == "approval_requested"));
        assert!(events
            .iter()
            .any(|(kind, payload)| kind == "approval_decided" && contains(payload, "denied")));

        workspace_db::close("wsappr");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);

        // --- allowed: the same request, a mode and a grant that permit it ---
        let data = tempfile::tempdir().expect("data dir");
        let cwd = data.path().to_string_lossy().to_string();
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(cwd.clone()),
        );
        let pool = workspace_fixture("wsappr2", "run-appr2");
        let stub = stub_bridge(script(&cwd)).await;
        let (db, service_id) = db_with_bridge("codex", stub.addr);
        let bridge = resolve_bridge(&db, service_id, "codex").expect("bridge");
        let tickets = TicketRegistry::new();
        let TicketDecision::Issued(ticket) = cli_adapter::issue_ticket(
            &tickets,
            &ticket_ctx(AutonomyMode::Normal),
            ticket_request("run-appr2", "cli-appr2", 10_000),
        )
        .expect("issue") else {
            panic!("ticket");
        };
        let mut instance = bridge
            .open(
                &pool,
                OpenCliInstance {
                    instance_id: "cli-appr2",
                    session_id: "sess-1",
                    run_id: "run-appr2",
                    worktree: data.path(),
                    model: "gpt-5-codex",
                    ticket_id: Some(&ticket.claims.ticket_id),
                    resume_vendor_session_id: None,
                    env: &[],
                },
            )
            .await
            .expect("open");
        let allowing = |_: Capability| ticket_ctx(AutonomyMode::Autonomous);
        let approvals =
            approval_context(&allowing, data.path(), "run-appr2", &registry, &progress);
        let pumped = pump(
            &bridge,
            &pool,
            &mut instance,
            &approvals,
            &tickets,
            &ticket.claims.ticket_id,
            Instant::now() + Duration::from_secs(30),
            &cancel,
        )
        .await
        .expect("pump");
        assert_eq!(pumped.denied_approvals, 0);
        assert_eq!(
            *stub.answered.lock().expect("answered"),
            vec![(7_u64, "approved".to_string())],
            "the policy, not this block, is what decides"
        );

        workspace_db::close("wsappr2");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// Nothing on the delegation's own paths carries credential material: not
    /// the transcript handed to the flow, not the timeline, not the ticket.
    ///
    /// A vendor CLI echoes whatever it reads, so a token in its output is a
    /// realistic event, and the ticket is itself a bearer secret that must not
    /// be journalled just because it is "only" a ticket (§24 "Sekrety").
    #[tokio::test]
    async fn no_credential_material_reaches_the_transcript_or_the_timeline() {
        let _guard = cs_paths::test_data_dir_guard();
        const LEAKED: &str = "sk-ant-api03-REALLYSECRETVALUE0123456789";
        let (data, pool, bridge, tickets, ticket, mut instance, _stub) = scenario(
            "wssecret",
            "run-secret",
            "cli-secret",
            10_000,
            vec![
                json!({"seq": 1, "kind": "terminal", "data": {
                    "text": format!("authenticating with {LEAKED}\n")
                }}),
                json!({"seq": 2, "kind": "codex", "data": {
                    "method": "turn/completed", "params": {}
                }}),
            ],
        )
        .await;

        let registry = crate::agents::interaction::InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let grants = |_: Capability| ticket_ctx(AutonomyMode::Normal);
        let approvals =
            approval_context(&grants, data.path(), "run-secret", &registry, &progress);
        let cancel = tokio_util::sync::CancellationToken::new();
        let pumped = pump(
            &bridge,
            &pool,
            &mut instance,
            &approvals,
            &tickets,
            &ticket.claims.ticket_id,
            Instant::now() + Duration::from_secs(30),
            &cancel,
        )
        .await
        .expect("pump");

        assert!(
            !pumped.transcript.contains("REALLYSECRETVALUE"),
            "the transcript reaches the model and the flow: {}",
            pumped.transcript
        );
        assert!(
            pumped.transcript.contains(crate::code_studio::redact::REDACTED),
            "the line survives, the credential does not: {}",
            pumped.transcript
        );
        let stored = timeline(&pool);
        assert!(
            !stored.is_empty(),
            "the delegation has to leave a timeline at all"
        );
        for (kind, payload) in &stored {
            assert!(
                !contains(payload, "REALLYSECRETVALUE"),
                "event '{kind}' journalled the credential"
            );
            assert!(
                !contains(payload, &ticket.presentation),
                "event '{kind}' journalled the ticket, which is itself a bearer secret"
            );
        }

        workspace_db::close("wssecret");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// The wiring handed to a CLI process carries the ticket and the session
    /// CA, and the ticket is what the registry checks — so the environment that
    /// leaves this node is a capability for one run, never a credential.
    #[test]
    fn the_sandbox_wiring_of_a_delegation_is_a_ticket_and_a_trust_anchor() {
        let wiring = cli_adapter::EngineWiring::for_engine("codex").expect("wiring");
        assert!(wiring.ticket_path_prefixes.contains(&"/v1".to_string()));
        assert!(wiring.ticket_methods.contains("POST"));
        // Nothing in the wiring names a credential variable: the only key the
        // CLI ever sees is the ticket, under the vendor's own API-key name.
        assert_eq!(wiring.api_key_var, "OPENAI_API_KEY");
        assert_eq!(wiring.base_url_var, "OPENAI_BASE_URL");
    }

    /// An unfinished turn is never reported as a finished one. With no terminal
    /// notification the pump gives up at its deadline and says so, and the
    /// caller settles the run `timed_out` rather than `completed`.
    #[tokio::test]
    async fn a_turn_that_never_ends_times_out_instead_of_reporting_success() {
        let _guard = cs_paths::test_data_dir_guard();
        let (data, pool, bridge, tickets, ticket, mut instance, _stub) = scenario(
            "wstimeout",
            "run-timeout",
            "cli-timeout",
            10_000,
            vec![json!({"seq": 1, "kind": "terminal", "data": {"text": "thinking"}})],
        )
        .await;
        let registry = crate::agents::interaction::InteractionRegistry::new();
        let progress = crate::flow_engine::dispatchers::progress::NoopProgress;
        let grants = |_: Capability| ticket_ctx(AutonomyMode::Normal);
        let approvals =
            approval_context(&grants, data.path(), "run-timeout", &registry, &progress);
        let cancel = tokio_util::sync::CancellationToken::new();
        let pumped = pump(
            &bridge,
            &pool,
            &mut instance,
            &approvals,
            &tickets,
            &ticket.claims.ticket_id,
            Instant::now() + Duration::from_millis(400),
            &cancel,
        )
        .await
        .expect("a deadline is not an error");
        assert_eq!(
            pumped.state, None,
            "no vendor announcement means no reported completion"
        );
        assert!(pumped.transcript.contains("thinking"));

        workspace_db::close("wstimeout");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// The prompt reaches the CLI, and closing the instance records the state
    /// the bridge actually reported — `reaped` means the vendor process is
    /// gone, which is defect D2's promise and not a hopeful default.
    #[tokio::test]
    async fn the_prompt_reaches_the_cli_and_closing_records_the_reaped_state() {
        let _guard = cs_paths::test_data_dir_guard();
        let (_data, pool, bridge, _tickets, _ticket, instance, stub) = scenario(
            "wsturn",
            "run-turn",
            "cli-turn",
            10_000,
            Vec::new(),
        )
        .await;

        bridge
            .turn(&instance, "add a regression test for the parser")
            .await
            .expect("turn");
        assert_eq!(
            *stub.prompts.lock().expect("prompts"),
            vec!["add a regression test for the parser".to_string()]
        );

        let state = bridge.close(&pool, &instance).await.expect("close");
        assert_eq!(state, "reaped");
        assert!(*stub.closed.lock().expect("closed"));
        assert_eq!(
            cli_bridge::instance_status(&pool, "cli-turn")
                .expect("status")
                .as_deref(),
            Some("reaped")
        );

        workspace_db::close("wsturn");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }
}
