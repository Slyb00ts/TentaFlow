// ===== File: agents/interaction.rs — pending user-interaction registry for
// in-flight runs (§3.13). Two interaction kinds share one delivery mechanic:
//   - a `core.ask_user` question / `ask_user` block (clarification, missing data)
//   - a permission grant request raised by tool_exec on a NotConfigured deny
//
// A raised interaction is parked on a `oneshot` the run awaits; the run flips to
// `waiting_user`, RELEASES its concurrency permit (anti-livelock, same rule as
// agent_wait) and PAUSES its deadline (human think-time must not consume
// `agent.timeout_secs`). The heartbeat keeps beating throughout so the watchdog
// does not reap a run that is merely waiting on a person. A reply resolves the
// oneshot; the configured timeout (default 600 s) yields a sentinel so the model
// adapts instead of hanging.
//
// The registry is process-global (mirrors the run manager / progress broker): a
// question raised on one WS connection can be answered on another, and a child
// agent's question bubbles to the same principal as the parent (the run id chain
// stays visible to the dashboard). =====

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};

/// Default human-wait budget when the caller omits `timeout_secs` (§3.13).
pub const DEFAULT_INTERACTION_TIMEOUT_SECS: u64 = 600;

/// The two interaction kinds. The registry is agnostic to the kind — both park
/// on the same channel — but the kind drives what the dashboard renders (a
/// question card vs a permission grant card) and what a timeout means (sentinel
/// text vs deny).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractionKind {
    /// `core.ask_user` / `ask_user` block — a clarification or missing-data ask.
    Question,
    /// A tool permission grant requested on a NotConfigured deny.
    Permission,
}

impl InteractionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InteractionKind::Question => "question",
            InteractionKind::Permission => "permission",
        }
    }
}

/// A user's reply to a question interaction. `answer` is the free text (or the
/// chosen option's label); the registry does not interpret it.
#[derive(Debug, Clone)]
pub struct QuestionReply {
    pub answer: String,
}

/// A user's decision on a permission grant request (§3.13 B). Scope widens left
/// to right: a single retry, the rest of this run, or a persisted grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// Refuse this call; the tool result becomes a permission-denied error.
    Deny,
    /// Allow this one retried call; nothing is cached.
    AllowOnce,
    /// Allow every call to this tool for the remainder of this run (cached
    /// in-memory, equivalent to Codex `ApprovedForSession`).
    AllowForRun,
    /// Persist the grant through the addon permission engine (principal-scoped;
    /// a global grant is admin-only and decided by the handler, not here).
    Always,
}

impl PermissionDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionDecision::Deny => "deny",
            PermissionDecision::AllowOnce => "allow_once",
            PermissionDecision::AllowForRun => "allow_for_run",
            PermissionDecision::Always => "always",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "deny" => Some(PermissionDecision::Deny),
            "allow_once" => Some(PermissionDecision::AllowOnce),
            "allow_for_run" => Some(PermissionDecision::AllowForRun),
            "always" => Some(PermissionDecision::Always),
            _ => None,
        }
    }

    /// True when this decision lets the call proceed (a retry should run).
    pub fn allows(self) -> bool {
        !matches!(self, PermissionDecision::Deny)
    }
}

/// What a resolved interaction carries back to the awaiting run.
#[derive(Debug, Clone)]
pub enum InteractionReply {
    Question(QuestionReply),
    Permission(PermissionDecision),
}

/// Outcome of awaiting an interaction: a human reply, or a timeout (the human
/// did not respond within the budget). A cancelled run drops the sender, which
/// also surfaces as `TimedOut` so the awaiting tool degrades the same way.
#[derive(Debug, Clone)]
pub enum InteractionOutcome {
    Replied(InteractionReply),
    TimedOut,
}

/// One pending interaction the dashboard can list and answer. `run_id` ties it
/// to the awaiting run for ACL (the run's principal, or admin); `parent_run_id`
/// makes a child's question visibly bubble under the parent (§3.13 A).
#[derive(Debug, Clone, Serialize)]
pub struct PendingInteraction {
    pub id: String,
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub kind: InteractionKind,
    /// Question text (Question kind) or a short human-readable permission prompt.
    pub prompt: String,
    /// Offered options for a question (≤4); empty for an open question or a
    /// permission request (which has its own fixed decision set).
    pub choices: Vec<String>,
    /// Permission target (Permission kind only): the addon + tool + permission
    /// the grant would cover, surfaced so the card can name them.
    pub addon_id: Option<String>,
    pub tool_name: Option<String>,
    pub permission: Option<String>,
    /// Epoch millis the interaction was raised (for the dashboard's "waiting
    /// for N s" display); the authoritative timeout is enforced by the awaiter.
    pub raised_at_ms: i64,
}

/// The awaiting side of one pending interaction: the sender resolves the
/// run's `oneshot`. Held in the registry until a reply arrives or the awaiter
/// drops it (timeout / cancel).
struct PendingSlot {
    info: PendingInteraction,
    reply_tx: oneshot::Sender<InteractionReply>,
}

/// Process-global pending-interaction registry. Maps interaction id → its
/// awaiting slot; also indexes by run id so a cancelled run can drop every
/// question it raised. Behind one std Mutex (held only across map ops, never
/// across an await).
pub struct InteractionRegistry {
    pending: Mutex<HashMap<String, PendingSlot>>,
    /// Per-run `AllowForRun` grant cache (§3.13 B): `run_id` → set of granted
    /// `"addon_id.tool_name"` names. A grant lives only for the run that earned
    /// it (Codex `ApprovedForSession`), so a retried call in a later iteration of
    /// the SAME run skips the permission prompt. Cleared when the run settles.
    run_grants: Mutex<HashMap<String, std::collections::HashSet<String>>>,
}

static GLOBAL: OnceLock<Arc<InteractionRegistry>> = OnceLock::new();

/// Installs the process-global registry (idempotent — first wins). Called once
/// at startup alongside the run manager.
pub fn init_global(registry: Arc<InteractionRegistry>) -> Arc<InteractionRegistry> {
    let _ = GLOBAL.set(registry);
    GLOBAL.get().expect("registry just set").clone()
}

/// The process-global registry, installing a fresh one on first use. Unlike the
/// run manager, the interaction registry has no external dependencies, so a
/// lazily-created default is always correct (a question raised before startup
/// wiring still parks on the same global instance the handlers read).
pub fn global() -> Arc<InteractionRegistry> {
    GLOBAL
        .get_or_init(|| Arc::new(InteractionRegistry::new()))
        .clone()
}

impl Default for InteractionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InteractionRegistry {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            run_grants: Mutex::new(HashMap::new()),
        }
    }

    /// Records an `AllowForRun` grant: every later call to `tool` in `run_id`
    /// proceeds without re-prompting (§3.13 B).
    pub fn grant_for_run(&self, run_id: &str, tool: &str) {
        if let Ok(mut map) = self.run_grants.lock() {
            map.entry(run_id.to_string())
                .or_default()
                .insert(tool.to_string());
        }
    }

    /// True when `tool` already holds an `AllowForRun` grant in `run_id`.
    pub fn run_grant_holds(&self, run_id: &str, tool: &str) -> bool {
        self.run_grants
            .lock()
            .map(|map| map.get(run_id).is_some_and(|s| s.contains(tool)))
            .unwrap_or(false)
    }

    /// Drops a run's per-run grant cache (called when the run settles, so a
    /// finished run leaves no stale grants).
    pub fn clear_run_grants(&self, run_id: &str) {
        if let Ok(mut map) = self.run_grants.lock() {
            map.remove(run_id);
        }
    }

    /// Registers a pending interaction and returns its id plus the receiver the
    /// run awaits. The caller (ask_user tool/block, permission path) registers,
    /// flips the run to `waiting_user` + releases its permit, then awaits the
    /// receiver with its own timeout. The id is also published in a progress
    /// event so the dashboard can render the card.
    pub fn register(&self, info: PendingInteraction) -> oneshot::Receiver<InteractionReply> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let id = info.id.clone();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(id, PendingSlot { info, reply_tx });
        }
        reply_rx
    }

    /// Resolves a pending interaction with a reply. Returns true when an
    /// interaction with that id was waiting (and the awaiter is still alive).
    /// A reply for an unknown / already-resolved id is a no-op returning false
    /// — the handler maps that to "not found / already answered".
    pub fn reply(&self, interaction_id: &str, reply: InteractionReply) -> bool {
        let slot = self
            .pending
            .lock()
            .ok()
            .and_then(|mut map| map.remove(interaction_id));
        match slot {
            Some(slot) => slot.reply_tx.send(reply).is_ok(),
            None => false,
        }
    }

    /// Looks up a pending interaction's public info (for ACL checks in the reply
    /// handler before dispatching the typed reply).
    pub fn info(&self, interaction_id: &str) -> Option<PendingInteraction> {
        self.pending
            .lock()
            .ok()
            .and_then(|map| map.get(interaction_id).map(|s| s.info.clone()))
    }

    /// Drops every pending interaction raised by `run_id` (run cancellation
    /// closes its open questions, §3.13 A). Dropping the sender makes each
    /// awaiter observe a closed channel → `TimedOut`. Returns the count dropped.
    pub fn drop_run(&self, run_id: &str) -> usize {
        let Ok(mut map) = self.pending.lock() else {
            return 0;
        };
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, s)| s.info.run_id == run_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            map.remove(id);
        }
        ids.len()
    }

    /// Snapshot of pending interactions visible to a principal: a non-admin sees
    /// only those whose awaiting run belongs to them, admins see all. `user_ids`
    /// is the set of run ids owned by the caller, resolved by the handler from
    /// the DB (the registry has no DB handle). When `admin` is true the filter
    /// is bypassed.
    pub fn list_for(&self, admin: bool, owned_run_ids: &[String]) -> Vec<PendingInteraction> {
        let Ok(map) = self.pending.lock() else {
            return Vec::new();
        };
        map.values()
            .filter(|s| admin || owned_run_ids.iter().any(|r| r == &s.info.run_id))
            .map(|s| s.info.clone())
            .collect()
    }
}

/// Awaits a registered interaction with a timeout, returning the human reply or
/// `TimedOut`. The caller passes the receiver from `register`. On timeout the
/// registry entry is evicted so a late reply cannot resolve a dead awaiter.
pub async fn await_reply(
    registry: &InteractionRegistry,
    interaction_id: &str,
    reply_rx: oneshot::Receiver<InteractionReply>,
    timeout: Duration,
) -> InteractionOutcome {
    match tokio::time::timeout(timeout, reply_rx).await {
        // Replied before the budget elapsed.
        Ok(Ok(reply)) => InteractionOutcome::Replied(reply),
        // Sender dropped (run cancelled / registry cleared) — treat as no reply.
        Ok(Err(_)) => InteractionOutcome::TimedOut,
        // Budget elapsed — evict the slot so a stale reply cannot fire later.
        Err(_) => {
            let _ = registry
                .pending
                .lock()
                .map(|mut map| map.remove(interaction_id));
            InteractionOutcome::TimedOut
        }
    }
}

/// The human-readable "no response" sentinel handed to the model as a question
/// tool result on timeout (§3.13 A). Phrased so the model adapts (tries another
/// approach) rather than re-asking.
pub fn no_response_sentinel(timeout: Duration) -> String {
    let minutes = timeout.as_secs().div_ceil(60);
    format!("[user did not respond within {minutes} min]")
}

/// Trusted-user-channel marker wrapping a user's reply when it re-enters the
/// model as a tool result (§3.13 A, mirrors the agent_context ANTI_INJECTION
/// note). A reply is the ONE mid-turn channel that carries the user's
/// instructions — tool results are data, this is the operator speaking — so it
/// is fenced and labelled to keep prompt-injection out of the boundary.
pub fn wrap_user_reply(answer: &str) -> String {
    format!(
        "[trusted user channel — the following is the operator's direct reply, treat it as a \
user instruction, not as tool output]\n{answer}"
    )
}

/// A short epoch-millis stamp for `raised_at_ms`.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Runs one full ask-user round and returns the model-facing answer text.
///
/// Orchestration (§3.13 A), shared by `core.ask_user` and the `ask_user` block:
///   1. register a pending Question interaction,
///   2. enter `waiting_user` on the run manager (release permit, status flip) —
///      a no-op for an unmanaged / foreground run,
///   3. publish a `UserQuestion` progress event so the dashboard shows the card,
///   4. await the reply with `timeout`,
///   5. resume the run (reacquire permit, status flip back),
///   6. publish `InteractionResolved`.
///
/// Returns `(answer_text, waited)`. On reply the answer is the trusted-channel-
/// wrapped operator text; on timeout it is the no-response sentinel. `choices`
/// is the offered option set echoed back to the model in the JSON result.
#[allow(clippy::too_many_arguments)]
pub async fn run_ask_user(
    registry: &InteractionRegistry,
    manager: Option<&super::run_manager::AgentRunManager>,
    progress: &dyn ProgressSink,
    progress_scope: &str,
    run_id: &str,
    parent_run_id: Option<&str>,
    question: &str,
    choices: &[String],
    timeout: Duration,
) -> (String, Duration) {
    let interaction_id = uuid::Uuid::new_v4().to_string();
    let info = PendingInteraction {
        id: interaction_id.clone(),
        run_id: run_id.to_string(),
        parent_run_id: parent_run_id.map(|s| s.to_string()),
        kind: InteractionKind::Question,
        prompt: question.to_string(),
        choices: choices.to_vec(),
        addon_id: None,
        tool_name: None,
        permission: None,
        raised_at_ms: now_ms(),
    };
    let rx = registry.register(info);

    // Enter waiting_user (release permit, pause status). A foreground run holds
    // no permit, so this is a no-op and `had_permit` is false.
    let had_permit = manager
        .map(|m| m.enter_waiting_user(run_id))
        .unwrap_or(false);

    progress.emit(
        progress_scope,
        ProgressEvent::UserQuestion {
            run_id: run_id.to_string(),
            interaction_id: interaction_id.clone(),
            question: question.to_string(),
            choices: choices.to_vec(),
        },
    );

    let started = Instant::now();
    let outcome = await_reply(registry, &interaction_id, rx, timeout).await;
    let waited = started.elapsed();

    if let Some(m) = manager {
        // Resume: reacquire the permit (awaits if the pool is saturated) and flip
        // back to running. A closed semaphore would only happen on shutdown.
        let _ = m.exit_waiting_user(run_id, had_permit).await;
    }

    let (answer, outcome_label) = match outcome {
        InteractionOutcome::Replied(InteractionReply::Question(q)) => {
            (wrap_user_reply(&q.answer), "replied")
        }
        // A permission reply to a question interaction cannot occur (the registry
        // is keyed by id), but fold defensively into the sentinel.
        InteractionOutcome::Replied(InteractionReply::Permission(_))
        | InteractionOutcome::TimedOut => (no_response_sentinel(timeout), "timed_out"),
    };

    progress.emit(
        progress_scope,
        ProgressEvent::InteractionResolved {
            run_id: run_id.to_string(),
            interaction_id,
            outcome: outcome_label.to_string(),
        },
    );

    (answer, waited)
}

/// Runs one full permission-grant round and returns the operator's decision plus
/// the time waited (§3.13 B). Orchestration mirrors `run_ask_user` but raises a
/// Permission interaction and publishes a `PermissionRequest` event. On timeout
/// the decision is `Deny` (the caller maps that to a `[TOOL_ERROR] permission
/// denied` result).
#[allow(clippy::too_many_arguments)]
pub async fn run_permission_request(
    registry: &InteractionRegistry,
    manager: Option<&super::run_manager::AgentRunManager>,
    progress: &dyn ProgressSink,
    progress_scope: &str,
    run_id: &str,
    parent_run_id: Option<&str>,
    addon_id: &str,
    tool_name: &str,
    permission: &str,
    timeout: Duration,
) -> (PermissionDecision, Duration) {
    let interaction_id = uuid::Uuid::new_v4().to_string();
    let prompt =
        format!("Allow tool '{tool_name}' (addon '{addon_id}', permission '{permission}')?");
    let info = PendingInteraction {
        id: interaction_id.clone(),
        run_id: run_id.to_string(),
        parent_run_id: parent_run_id.map(|s| s.to_string()),
        kind: InteractionKind::Permission,
        prompt,
        choices: Vec::new(),
        addon_id: Some(addon_id.to_string()),
        tool_name: Some(tool_name.to_string()),
        permission: Some(permission.to_string()),
        raised_at_ms: now_ms(),
    };
    let rx = registry.register(info);

    let had_permit = manager
        .map(|m| m.enter_waiting_user(run_id))
        .unwrap_or(false);

    progress.emit(
        progress_scope,
        ProgressEvent::PermissionRequest {
            run_id: run_id.to_string(),
            interaction_id: interaction_id.clone(),
            addon_id: addon_id.to_string(),
            tool_name: tool_name.to_string(),
            permission: permission.to_string(),
        },
    );

    let started = Instant::now();
    let outcome = await_reply(registry, &interaction_id, rx, timeout).await;
    let waited = started.elapsed();

    if let Some(m) = manager {
        let _ = m.exit_waiting_user(run_id, had_permit).await;
    }

    let (decision, outcome_label) = match outcome {
        InteractionOutcome::Replied(InteractionReply::Permission(d)) => (d, "replied"),
        // Timeout (or a question reply, impossible here) → deny.
        InteractionOutcome::Replied(InteractionReply::Question(_))
        | InteractionOutcome::TimedOut => (PermissionDecision::Deny, "timed_out"),
    };

    progress.emit(
        progress_scope,
        ProgressEvent::InteractionResolved {
            run_id: run_id.to_string(),
            interaction_id,
            outcome: outcome_label.to_string(),
        },
    );

    (decision, waited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str, run_id: &str, kind: InteractionKind) -> PendingInteraction {
        PendingInteraction {
            id: id.into(),
            run_id: run_id.into(),
            parent_run_id: None,
            kind,
            prompt: "do you want X?".into(),
            choices: vec!["yes".into(), "no".into()],
            addon_id: None,
            tool_name: None,
            permission: None,
            raised_at_ms: now_ms(),
        }
    }

    #[tokio::test]
    async fn reply_resolves_the_awaiter() {
        let reg = InteractionRegistry::new();
        let rx = reg.register(pending("i1", "r1", InteractionKind::Question));
        assert!(reg.info("i1").is_some());

        let replied = reg.reply(
            "i1",
            InteractionReply::Question(QuestionReply {
                answer: "yes".into(),
            }),
        );
        assert!(replied);
        // The slot is consumed on reply.
        assert!(reg.info("i1").is_none());

        let outcome = await_reply(&reg, "i1", rx, Duration::from_secs(5)).await;
        match outcome {
            InteractionOutcome::Replied(InteractionReply::Question(q)) => {
                assert_eq!(q.answer, "yes")
            }
            other => panic!("expected question reply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_yields_timed_out_and_evicts() {
        let reg = InteractionRegistry::new();
        let rx = reg.register(pending("i2", "r1", InteractionKind::Question));
        let outcome = await_reply(&reg, "i2", rx, Duration::from_millis(20)).await;
        assert!(matches!(outcome, InteractionOutcome::TimedOut));
        // The slot was evicted on timeout, so a late reply finds nothing.
        assert!(!reg.reply(
            "i2",
            InteractionReply::Question(QuestionReply {
                answer: "late".into()
            })
        ));
    }

    #[tokio::test]
    async fn drop_run_cancels_pending_questions() {
        let reg = InteractionRegistry::new();
        let rx = reg.register(pending("i3", "run-x", InteractionKind::Permission));
        let dropped = reg.drop_run("run-x");
        assert_eq!(dropped, 1);
        // The awaiter sees a closed channel → TimedOut.
        let outcome = await_reply(&reg, "i3", rx, Duration::from_secs(5)).await;
        assert!(matches!(outcome, InteractionOutcome::TimedOut));
    }

    #[test]
    fn reply_for_unknown_id_is_false() {
        let reg = InteractionRegistry::new();
        assert!(!reg.reply(
            "nope",
            InteractionReply::Permission(PermissionDecision::Deny)
        ));
    }

    #[test]
    fn list_for_filters_by_ownership() {
        let reg = InteractionRegistry::new();
        let _a = reg.register(pending("ia", "run-a", InteractionKind::Question));
        let _b = reg.register(pending("ib", "run-b", InteractionKind::Question));
        // Non-admin owning only run-a sees one.
        let mine = reg.list_for(false, &["run-a".to_string()]);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].run_id, "run-a");
        // Admin sees both.
        let all = reg.list_for(true, &[]);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn permission_decision_round_trips() {
        for d in [
            PermissionDecision::Deny,
            PermissionDecision::AllowOnce,
            PermissionDecision::AllowForRun,
            PermissionDecision::Always,
        ] {
            assert_eq!(PermissionDecision::parse(d.as_str()), Some(d));
        }
        assert_eq!(PermissionDecision::parse("bogus"), None);
        assert!(!PermissionDecision::Deny.allows());
        assert!(PermissionDecision::AllowOnce.allows());
    }

    #[test]
    fn sentinel_rounds_seconds_up_to_minutes() {
        assert_eq!(
            no_response_sentinel(Duration::from_secs(600)),
            "[user did not respond within 10 min]"
        );
        assert_eq!(
            no_response_sentinel(Duration::from_secs(90)),
            "[user did not respond within 2 min]"
        );
    }

    #[test]
    fn wrap_user_reply_marks_trusted_channel() {
        let wrapped = wrap_user_reply("delete it");
        assert!(wrapped.contains("trusted user channel"));
        assert!(wrapped.contains("delete it"));
    }

    use crate::flow_engine::dispatchers::NoopProgress;

    /// A reply through `run_ask_user` (manager=None, foreground) returns the
    /// trusted-channel-wrapped answer and reports a non-trivial wait.
    #[tokio::test]
    async fn run_ask_user_returns_wrapped_reply() {
        let reg = Arc::new(InteractionRegistry::new());
        let progress = NoopProgress;
        let reg2 = reg.clone();
        let task = tokio::spawn(async move {
            run_ask_user(
                &reg2,
                None,
                &progress,
                "scope",
                "run-1",
                None,
                "delete file X?",
                &["yes".into(), "no".into()],
                Duration::from_secs(10),
            )
            .await
        });
        // Resolve the single pending question.
        let id = loop {
            let pending = reg.list_for(true, &[]);
            if let Some(p) = pending.first() {
                break p.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert!(reg.reply(
            &id,
            InteractionReply::Question(QuestionReply {
                answer: "yes".into()
            })
        ));
        let (answer, _waited) = task.await.expect("join");
        assert!(answer.contains("trusted user channel"));
        assert!(answer.contains("yes"));
    }

    /// A timed-out ask_user yields the sentinel as the answer.
    #[tokio::test]
    async fn run_ask_user_timeout_yields_sentinel() {
        let reg = InteractionRegistry::new();
        let progress = NoopProgress;
        let (answer, _waited) = run_ask_user(
            &reg,
            None,
            &progress,
            "scope",
            "run-1",
            None,
            "anyone?",
            &[],
            Duration::from_millis(20),
        )
        .await;
        assert!(answer.contains("did not respond"));
    }

    /// AllowForRun via `run_permission_request` returns AllowForRun; the run-grant
    /// cache then short-circuits a later call to the same tool (§3.13 B scoping).
    #[tokio::test]
    async fn permission_allow_for_run_decision_and_cache() {
        let reg = Arc::new(InteractionRegistry::new());
        let progress = NoopProgress;
        let reg2 = reg.clone();
        let task = tokio::spawn(async move {
            run_permission_request(
                &reg2,
                None,
                &progress,
                "scope",
                "run-7",
                None,
                "contacts",
                "contacts.lookup",
                "llm",
                Duration::from_secs(10),
            )
            .await
        });
        let id = loop {
            let pending = reg.list_for(true, &[]);
            if let Some(p) = pending
                .iter()
                .find(|p| p.kind == InteractionKind::Permission)
            {
                break p.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert!(reg.reply(
            &id,
            InteractionReply::Permission(PermissionDecision::AllowForRun)
        ));
        let (decision, _waited) = task.await.expect("join");
        assert_eq!(decision, PermissionDecision::AllowForRun);

        // Caller caches the AllowForRun grant; a later call short-circuits.
        assert!(!reg.run_grant_holds("run-7", "contacts.lookup"));
        reg.grant_for_run("run-7", "contacts.lookup");
        assert!(reg.run_grant_holds("run-7", "contacts.lookup"));
        // Scoping: the grant does NOT leak to another run.
        assert!(!reg.run_grant_holds("run-8", "contacts.lookup"));
        // Settling clears the run's grants.
        reg.clear_run_grants("run-7");
        assert!(!reg.run_grant_holds("run-7", "contacts.lookup"));
    }

    /// A timed-out permission request denies (so tool_exec returns a denial).
    #[tokio::test]
    async fn permission_timeout_denies() {
        let reg = InteractionRegistry::new();
        let progress = NoopProgress;
        let (decision, _waited) = run_permission_request(
            &reg,
            None,
            &progress,
            "scope",
            "run-1",
            None,
            "contacts",
            "contacts.lookup",
            "llm",
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(decision, PermissionDecision::Deny);
    }
}
