// ===== File: flow_engine/node_adapters/patch_review.rs —
// PatchReviewNodeAdapter (node_type "patch_review", category logic, 1-in/1-out)
// plus `InteractionGate`, the ONE bridge between Code Studio's policy layer and
// the operator (§16.4).
//
// The block is optional. A flow that wants the review at a fixed point puts it
// here; a flow that leaves the decision to the agent gets the same review from
// PEP gate 5a the moment `core.git_commit` is called without an accepted set.
// Both call `code_studio::tools::run_review` — there is exactly one
// implementation of "what was accepted", because two would eventually disagree.
//
// `InteractionGate` is also what the tool_exec block hands to every Code Studio
// tool call, so a permission card raised by a tool and a review raised by this
// block travel the same registry, the same progress stream and the same
// deadline accounting.
// =====

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::json;

use crate::agents::{
    await_interaction_reply, interaction_now_ms, AgentRunManager, AgentServiceSlot,
    InteractionKind, InteractionOutcome, InteractionRegistry, InteractionReply,
    PendingInteraction, PermissionDecision,
};
use crate::code_studio::pep::AskKind;
use crate::code_studio::tools::{
    self, Approval, ApprovalDecision, ApprovalGate, ReviewPrompt, ReviewTimeout,
};
use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "patch_review";
const DEFAULT_TIMEOUT_SECS: u64 = 1800;
const MAX_TIMEOUT_SECS: u64 = 86_400;
const DEFAULT_OUTPUT_VARIABLE: &str = "patch_review";

/// The operator bridge. Owns no state beyond the run it speaks for: the
/// registry and the run manager are process-global, and the progress sink comes
/// from the executing context.
pub struct InteractionGate<'a> {
    registry: &'a InteractionRegistry,
    manager: Option<&'a AgentRunManager>,
    progress: &'a dyn ProgressSink,
    progress_scope: &'a str,
    run_id: &'a str,
    parent_run_id: Option<&'a str>,
    /// Human think-time is added back here, so a person deliberating over a
    /// diff never consumes the agent's own timeout (§3.13). `Send + Sync` is
    /// required, not incidental: the gate is handed to an async tool call whose
    /// future must stay `Send`.
    deadline_extension: &'a (dyn Fn(Duration) + Send + Sync),
}

impl<'a> InteractionGate<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: &'a InteractionRegistry,
        manager: Option<&'a AgentRunManager>,
        progress: &'a dyn ProgressSink,
        progress_scope: &'a str,
        run_id: &'a str,
        parent_run_id: Option<&'a str>,
        deadline_extension: &'a (dyn Fn(Duration) + Send + Sync),
    ) -> Self {
        Self {
            registry,
            manager,
            progress,
            progress_scope,
            run_id,
            parent_run_id,
            deadline_extension,
        }
    }

    /// Parks the run, waits for the reply, resumes. Shared by both gate methods
    /// so the permit/deadline handling cannot drift between them.
    async fn ask(
        &self,
        interaction_id: &str,
        rx: tokio::sync::oneshot::Receiver<InteractionReply>,
        timeout: Duration,
    ) -> InteractionOutcome {
        let had_permit = self
            .manager
            .map(|m| m.enter_waiting_user(self.run_id))
            .unwrap_or(false);
        let started = Instant::now();
        let outcome = await_interaction_reply(self.registry, interaction_id, rx, timeout).await;
        let waited = started.elapsed();
        if let Some(m) = self.manager {
            let _ = m.exit_waiting_user(self.run_id, had_permit).await;
        }
        (self.deadline_extension)(waited);
        outcome
    }
}

#[async_trait]
impl ApprovalGate for InteractionGate<'_> {
    async fn request(&self, ask: &Approval) -> ApprovalDecision {
        let info = PendingInteraction {
            id: ask.interaction_id.clone(),
            run_id: self.run_id.to_string(),
            parent_run_id: self.parent_run_id.map(|s| s.to_string()),
            kind: InteractionKind::Permission,
            prompt: ask.summary.clone(),
            choices: Vec::new(),
            addon_id: Some(crate::agents::CORE_ADDON_ID.to_string()),
            tool_name: Some(format!("code_studio.{}", ask.capability.slug())),
            permission: Some(ask.capability.slug().to_string()),
            raised_at_ms: interaction_now_ms(),
        };
        let rx = self.registry.register(info);
        self.progress.emit(
            self.progress_scope,
            ProgressEvent::PermissionRequest {
                run_id: self.run_id.to_string(),
                interaction_id: ask.interaction_id.clone(),
                addon_id: crate::agents::CORE_ADDON_ID.to_string(),
                tool_name: format!("code_studio.{}", ask.capability.slug()),
                permission: ask.capability.slug().to_string(),
            },
        );
        // A capability whose whole point is the human's presence gets a generous
        // budget; a plain write does not park a run for a day.
        let timeout = match ask.kind {
            AskKind::PatchReview => Duration::from_secs(MAX_TIMEOUT_SECS),
            AskKind::Permission => Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        };
        match self.ask(&ask.interaction_id, rx, timeout).await {
            InteractionOutcome::Replied(InteractionReply::Permission(decision)) => match decision {
                PermissionDecision::Deny => ApprovalDecision::Deny,
                PermissionDecision::AllowOnce => ApprovalDecision::AllowOnce,
                PermissionDecision::AllowForRun => ApprovalDecision::AllowForRun,
                PermissionDecision::Always => ApprovalDecision::Always,
            },
            // No answer is not consent, and neither is an answer of the wrong
            // shape for the card that was raised.
            _ => ApprovalDecision::Deny,
        }
    }

    async fn present_review(&self, prompt: &ReviewPrompt) -> Option<String> {
        let interaction_id = uuid::Uuid::new_v4().to_string();
        let question = format!(
            "Review the change set {} before it is committed.\n\n{}",
            prompt.patch_set_id, prompt.detail
        );
        let choices = vec!["accept".to_string(), "reject".to_string()];
        let info = PendingInteraction {
            id: interaction_id.clone(),
            run_id: self.run_id.to_string(),
            parent_run_id: self.parent_run_id.map(|s| s.to_string()),
            kind: InteractionKind::Question,
            prompt: question.clone(),
            choices: choices.clone(),
            addon_id: None,
            tool_name: None,
            permission: None,
            raised_at_ms: interaction_now_ms(),
        };
        let rx = self.registry.register(info);
        self.progress.emit(
            self.progress_scope,
            ProgressEvent::UserQuestion {
                run_id: self.run_id.to_string(),
                interaction_id: interaction_id.clone(),
                question,
                choices,
            },
        );
        match self.ask(&interaction_id, rx, prompt.timeout).await {
            InteractionOutcome::Replied(InteractionReply::Question(q)) => Some(q.answer),
            // Neither silence nor an answer of the wrong shape is a decision,
            // and the review turns "no decision" into "not accepted".
            InteractionOutcome::Replied(InteractionReply::Permission(_))
            | InteractionOutcome::TimedOut => None,
        }
    }
}

pub struct PatchReviewNodeAdapter {
    service: AgentServiceSlot,
}

impl PatchReviewNodeAdapter {
    pub fn new(service: AgentServiceSlot) -> Self {
        Self { service }
    }

    /// Which review this block runs.
    ///
    /// A merge review names the merge operation it decides: the scope of a
    /// patch set is what a finalize resolves its decision from, so a merge
    /// review that cannot say which merge it belongs to is a decision another
    /// merge could spend.
    fn scope(
        node: &FlowNode,
        pool: &crate::db::DbPool,
        session_id: &str,
    ) -> Result<crate::code_studio::patch::PatchScope> {
        match node.config.get("scope").and_then(|v| v.as_str()) {
            Some("merge") => {
                let op_id = tools::held_merge_op(pool, session_id)?.ok_or_else(|| {
                    anyhow!(
                        "patch_review: this session holds no merge to review; \
                         run core.git_merge first"
                    )
                })?;
                Ok(crate::code_studio::patch::PatchScope::Merge { op_id })
            }
            _ => Ok(crate::code_studio::patch::PatchScope::Work),
        }
    }

    fn granularity(node: &FlowNode) -> String {
        match node.config.get("granularity").and_then(|v| v.as_str()) {
            Some("file") => "file".to_string(),
            _ => "hunk".to_string(),
        }
    }

    fn timeout(node: &FlowNode) -> Duration {
        Duration::from_secs(
            node.config
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .min(MAX_TIMEOUT_SECS),
        )
    }

    fn on_timeout(node: &FlowNode) -> ReviewTimeout {
        ReviewTimeout::from_slug(
            node.config
                .get("on_timeout")
                .and_then(|v| v.as_str())
                .unwrap_or("reject"),
        )
    }

    fn output_variable(node: &FlowNode) -> String {
        node.config
            .get("output_variable")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_OUTPUT_VARIABLE)
            .to_string()
    }
}

#[async_trait]
impl NodeAdapter for PatchReviewNodeAdapter {
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
            .ok_or_else(|| anyhow!("patch_review: missing input edge"))?;
        let envelope = &input.envelope;

        // The review closes a change set into ACCEPTED blobs — the exact half of
        // `core.git_commit` that gate 5a performs. It therefore passes the same
        // first sieve (§10): an agent without the commit verb cannot obtain an
        // accepted set by putting this block in its graph.
        let service = self
            .service
            .read()
            .clone()
            .ok_or_else(|| anyhow!("patch_review: AgentService slot not wired"))?;
        service.require_core_tool(
            envelope.meta.get("agent_id").and_then(|v| v.as_str()),
            crate::agents::CoreToolName::GitCommit,
        )?;

        let binding = tools::binding_from_meta(&envelope.meta).ok_or_else(|| {
            anyhow!(
                "patch_review: this run carries no Code Studio session binding \
                 (meta.code_session)"
            )
        })?;
        let user_id = ctx
            .user_id
            .clone()
            .ok_or_else(|| anyhow!("patch_review: a review needs a user identity to attribute"))?;
        let run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let pool = crate::code_studio::workspace_db::open(&binding.workspace_id)?;
        let registry = crate::agents::interaction_registry_global();
        let manager = crate::agents::agent_run_manager_global();
        let extend = |waited: Duration| ctx.extend_deadline(waited);
        let gate = InteractionGate::new(
            &registry,
            manager.as_deref(),
            ctx.progress.as_ref(),
            &ctx.progress_scope,
            &run_id,
            None,
            &extend,
        );

        let report = tools::run_review(
            &pool,
            &binding.workspace_id,
            &binding.session_id,
            &Self::scope(node, &pool, &binding.session_id)?,
            &Self::granularity(node),
            &user_id,
            &gate,
            Self::timeout(node),
            Self::on_timeout(node),
        )
        .await?;

        let mut out: FlowEnvelope = (**envelope).clone();
        out.variables.insert(
            Self::output_variable(node),
            FlowValue::Json(report.to_json()),
        );
        // A rejection is a RESULT, not a failure: the run continues and the
        // agent sees why nothing was committed, which is what lets §16.3 open a
        // revision run instead of dying here.
        out.meta
            .insert("patch_review_status".into(), json!(report.status.clone()));
        out.payload = FlowValue::Text(format!(
            "review {}: {} accepted, {} rejected, {} conflicted",
            report.status,
            report.accepted.len(),
            report.rejected.len(),
            report.conflicted.len()
        ));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::FlowEnvelope;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json as j;
    use std::sync::Arc;

    fn seeded_slot() -> (crate::db::DbPool, AgentServiceSlot) {
        let pool = crate::db::init(std::path::Path::new(":memory:")).expect("init db");
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let addons =
            Arc::new(crate::addon::AddonManager::new(pool.clone(), cipher).expect("addon mgr"));
        let service = Arc::new(crate::agents::AgentService::new(pool.clone(), addons));
        (pool, Arc::new(parking_lot::RwLock::new(Some(service))))
    }

    fn agent_id(pool: &crate::db::DbPool, name: &str) -> String {
        crate::db::repository::get_agent_by_name(pool, name)
            .expect("query agent")
            .unwrap_or_else(|| panic!("seeded agent '{name}' is missing"))
            .id
    }

    fn inputs_for(agent_id: &str) -> Vec<NodeInput> {
        let mut envelope = FlowEnvelope::empty();
        envelope
            .meta
            .insert("agent_id".into(), serde_json::json!(agent_id));
        envelope.meta.insert(
            tools::SESSION_META_KEY.to_string(),
            tools::binding_meta_value("wsreview", "sessreview"),
        );
        vec![NodeInput {
            from_node_id: "upstream".to_string(),
            from_port: "full".to_string(),
            envelope: Arc::new(envelope),
        }]
    }

    /// A9 — the review closes a change set into ACCEPTED blobs, which is gate 5a
    /// of `core.git_commit`. `code-tester` holds no commit verb (§15 gives it
    /// exec and nothing that publishes), so it must not obtain an accepted set
    /// by putting the block in its graph. Before the fix the block called
    /// `tools::run_review` with no allowlist check whatsoever.
    #[tokio::test]
    async fn an_agent_without_core_git_commit_cannot_close_a_change_set() {
        let (pool, slot) = seeded_slot();
        let tester = agent_id(&pool, "code-tester");
        let err = PatchReviewNodeAdapter::new(slot)
            .execute(&node(j!({})), &inputs_for(&tester), &stub_ctx())
            .await
            .expect_err("a tester must not be able to accept a patch set");
        let message = format!("{err:#}");
        assert!(message.contains("core.git_commit"), "{message}");
        assert!(message.contains("allowlist"), "{message}");
    }

    /// And the committer, which does hold the verb, gets past the sieve and
    /// stops at the next gate — so the check is a sieve, not a wall.
    #[tokio::test]
    async fn the_committer_passes_the_sieve_and_stops_at_the_next_gate() {
        let (pool, slot) = seeded_slot();
        let committer = agent_id(&pool, "code-committer");
        let err = PatchReviewNodeAdapter::new(slot)
            .execute(&node(j!({})), &inputs_for(&committer), &stub_ctx())
            .await
            .expect_err("no user identity in the stub context");
        let message = format!("{err:#}");
        assert!(
            !message.contains("allowlist"),
            "the allowlist must not be what stopped this run: {message}"
        );
        assert!(message.contains("user identity"), "{message}");
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "pr1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    #[test]
    fn config_defaults_are_the_safe_ones() {
        let n = node(j!({}));
        assert_eq!(PatchReviewNodeAdapter::granularity(&n), "hunk");
        assert_eq!(
            PatchReviewNodeAdapter::timeout(&n),
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );
        // Silence is not consent.
        assert_eq!(
            PatchReviewNodeAdapter::on_timeout(&n),
            ReviewTimeout::Reject
        );
    }

    #[test]
    fn timeout_is_clamped_and_scope_is_explicit() {
        let n = node(j!({"timeout_secs": 999_999, "scope": "merge", "granularity": "file"}));
        assert_eq!(
            PatchReviewNodeAdapter::timeout(&n),
            Duration::from_secs(MAX_TIMEOUT_SECS)
        );
        assert_eq!(PatchReviewNodeAdapter::granularity(&n), "file");
    }
}
