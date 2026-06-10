// ===== File: flow_engine/node_adapters/compact_context.rs —
// CompactContextNodeAdapter (node_type "compact_context", category transform).
// Context compaction as a Flow Builder block (so the compaction policy is
// editable, not baked into the loop). Below the threshold it is a pure
// passthrough. Above it, this stage does MINIMAL compaction: keep the system
// prompts, the most recent user message, and the newest `protect_last_messages`
// messages in the live tail; replace the dropped middle span with ONE summary
// message produced by an audited LLM call, prefixed as reference-only data. The
// full two-phase Hermes compaction (cheap no-LLM pre-pass, structured template,
// temporal anchoring, iterative re-summarisation, anti-thrashing) is phase 7 —
// this is the threshold + tail-protection + single-summary skeleton it builds
// on. The summary call is audited like any other llm call via the meta
// correlation keys. (Harness §3.5 block 5, §1.2, §3.4.) =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::llm::LlmRequest;
use crate::flow_engine::dispatchers::ProgressEvent;
use crate::flow_engine::envelope::{ChatMessage, ChatRole, FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "compact_context";

const DEFAULT_THRESHOLD_PERCENT: u32 = 50;
const DEFAULT_PROTECT_LAST_MESSAGES: usize = 4;

/// Conservative chars-per-token estimate for the threshold heuristic. The exact
/// model context window is resolved by the runtime, not here; phase 7 wires the
/// real `auto_compact_token_limit`. For now the threshold is a fraction of a
/// fixed reference window so the block triggers proportionally to conversation
/// size without a runtime model lookup.
const CHARS_PER_TOKEN: usize = 4;

/// Reference context window (tokens) the threshold percentage is taken against
/// until the real per-model window is plumbed (phase 7). 8k tokens is a safe
/// floor — a conversation that pushes past 50% of it is already large enough to
/// benefit from compaction regardless of the backing model.
const REFERENCE_CONTEXT_TOKENS: usize = 8192;

const SUMMARY_SYSTEM_PROMPT: &str = "You compress an ongoing conversation into a concise \
handoff summary for the SAME assistant to continue from. Write in the past tense what was \
already done, what decisions were made, and what remains. Be factual and brief. Output only \
the summary text — no preamble.";

/// Prefix marking the injected summary as reference data, not a fresh
/// instruction (anti-injection / temporal-anchoring lite, §1.2 / §3.10).
const SUMMARY_PREFIX: &str = "[conversation summary — reference only, earlier turns were \
compacted]\n";

pub struct CompactContextNodeAdapter;

impl CompactContextNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn threshold_percent(node: &FlowNode) -> u32 {
        node.config
            .get("threshold_percent")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0 && *n <= 100)
            .map(|n| n as u32)
            .unwrap_or(DEFAULT_THRESHOLD_PERCENT)
    }

    fn protect_last_messages(node: &FlowNode) -> usize {
        node.config
            .get("protect_last_messages")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_PROTECT_LAST_MESSAGES)
    }

    /// Summary model: node config `summary_model`, falling back to
    /// `envelope.meta["model"]` (the conversation's own model). Empty/absent
    /// config + no meta model → no model to call, so compaction is skipped
    /// (passthrough) rather than erroring an otherwise-healthy flow.
    fn summary_model(node: &FlowNode, envelope: &FlowEnvelope) -> Option<String> {
        node.config
            .get("summary_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                envelope
                    .meta
                    .get("model")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
    }

    /// Estimated token count of the conversation messages (rough char/4). System
    /// prompts are excluded — they are always preserved and not compactable.
    fn estimated_tokens(messages: &[ChatMessage]) -> usize {
        let chars: usize = messages.iter().map(|m| m.text_or_default().len()).sum();
        chars / CHARS_PER_TOKEN
    }

    fn over_threshold(messages: &[ChatMessage], threshold_percent: u32) -> bool {
        let budget = REFERENCE_CONTEXT_TOKENS * (threshold_percent as usize) / 100;
        Self::estimated_tokens(messages) > budget
    }

    /// Index of the most recent user message, if any — it is always kept in the
    /// live tail (§1.2: the last user message must never be summarised away).
    fn last_user_index(messages: &[ChatMessage]) -> Option<usize> {
        messages.iter().rposition(|m| m.role == ChatRole::User)
    }

    /// Plans the split: which message indices are protected (kept verbatim) vs
    /// dropped (summarised). Protected = the newest `protect_last` messages plus
    /// the most recent user message (which may be older than that window).
    /// Returns `(dropped_indices, protected_indices)` both ascending; an empty
    /// dropped set means there is nothing worth summarising (passthrough).
    fn plan_split(messages: &[ChatMessage], protect_last: usize) -> (Vec<usize>, Vec<usize>) {
        let n = messages.len();
        let tail_start = n.saturating_sub(protect_last);
        let mut protected: std::collections::BTreeSet<usize> = (tail_start..n).collect();
        if let Some(u) = Self::last_user_index(messages) {
            protected.insert(u);
        }
        let dropped: Vec<usize> = (0..n).filter(|i| !protected.contains(i)).collect();
        (dropped, protected.into_iter().collect())
    }

    /// Renders the dropped span into the text the summary model reads.
    fn render_span(messages: &[ChatMessage], dropped: &[usize]) -> String {
        let mut out = String::new();
        for &i in dropped {
            let m = &messages[i];
            let role = match m.role {
                ChatRole::System => "system",
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
                ChatRole::Tool => "tool",
            };
            out.push_str(role);
            out.push_str(": ");
            out.push_str(&m.text_or_default());
            out.push('\n');
        }
        out
    }
}

impl Default for CompactContextNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for CompactContextNodeAdapter {
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
            .ok_or_else(|| anyhow!("compact_context: missing input edge"))?;
        let envelope = &input.envelope;

        let threshold = Self::threshold_percent(node);
        let protect_last = Self::protect_last_messages(node);

        let mut out: FlowEnvelope = (**envelope).clone();

        // Below the threshold: pure passthrough — no LLM call, no mutation.
        if !Self::over_threshold(&out.context.messages, threshold) {
            return Ok(out);
        }

        let (dropped, protected) = Self::plan_split(&out.context.messages, protect_last);
        // Nothing to drop (conversation shorter than the protected window even
        // though the byte estimate tripped): leave it untouched.
        if dropped.is_empty() {
            return Ok(out);
        }

        let Some(model) = Self::summary_model(node, envelope) else {
            // No model resolvable → cannot summarise; pass through rather than
            // failing a healthy flow.
            return Ok(out);
        };

        let span = Self::render_span(&out.context.messages, &dropped);

        let mut req = LlmRequest::new(model);
        req.messages = vec![
            ChatMessage::system(SUMMARY_SYSTEM_PROMPT),
            ChatMessage::user(format!("Conversation so far:\n{span}")),
        ];
        req.temperature = Some(0.2);
        req.deadline = ctx.deadline;
        req.cancel_token = ctx.cancel_token.clone();
        req.user_id = ctx.user_id.clone();
        req.user_role = ctx.user_role.clone();
        req.flow_node_id = Some(node.id.clone());
        req.flow_id = envelope
            .meta
            .get("flow_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        req.agent_id = envelope
            .meta
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        req.agent_run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        req.correlation_id = envelope
            .meta
            .get("correlation_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let response = ctx.llm.execute_chat(req).await?;

        // Rebuild messages: a single summary message (reference-prefixed) in
        // place of the dropped span, followed by the protected tail in order.
        let summary = ChatMessage::assistant(format!("{SUMMARY_PREFIX}{}", response.content));
        let mut rebuilt: Vec<ChatMessage> = Vec::with_capacity(protected.len() + 1);
        rebuilt.push(summary);
        for &i in &protected {
            rebuilt.push(out.context.messages[i].clone());
        }
        out.context.messages = rebuilt;

        ctx.progress.emit(
            &ctx.progress_scope,
            ProgressEvent::Compaction {
                node_id: node.id.clone(),
            },
        );

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::llm::LlmDispatcher;
    use crate::flow_engine::dispatchers::llm::{LlmRequest as Req, LlmResponse};
    use crate::flow_engine::envelope::{FinishReason, LlmStreamChunk, TokenUsage};
    use crate::flow_engine::node_adapter::test_support::{stub_ctx, CapturingProgress};
    use futures::stream::BoxStream;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Mock LLM that returns a fixed summary and counts calls (to prove the
    /// passthrough path makes ZERO calls).
    struct CountingLlm {
        summary: String,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmDispatcher for CountingLlm {
        async fn execute_chat(&self, _req: Req) -> Result<LlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(LlmResponse {
                content: self.summary.clone(),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
                tool_calls: Vec::new(),
            })
        }
        async fn stream_chat(
            &self,
            _req: Req,
        ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
            unreachable!("compact_context uses execute_chat only")
        }
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "cc1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "history".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    fn big_message(role: ChatRole, fill: char, len: usize) -> ChatMessage {
        let text: String = std::iter::repeat(fill).take(len).collect();
        let mut m = ChatMessage::user(text);
        m.role = role;
        m
    }

    #[tokio::test]
    async fn passthrough_below_threshold_makes_no_llm_call() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
        let llm = Arc::new(CountingLlm {
            summary: "SUMMARY".into(),
            calls: AtomicUsize::new(0),
        });
        let mut ctx = stub_ctx();
        ctx.llm = llm.clone();

        let out = CompactContextNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .expect("execute");

        // Unchanged + zero LLM calls.
        assert_eq!(out.context.messages.len(), 2);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn summarizes_above_threshold_with_tail_protection() {
        // Build a conversation well over 50% of the reference window: each
        // message is ~4k chars (~1k tokens), 6 messages ≈ 6k tokens > 4k budget.
        let mut env = FlowEnvelope::empty();
        env.context.system_prompts.push("system rules".into());
        env.context.messages = vec![
            big_message(ChatRole::User, 'a', 4000),      // 0 oldest
            big_message(ChatRole::Assistant, 'b', 4000), // 1
            big_message(ChatRole::User, 'c', 4000),      // 2
            big_message(ChatRole::Assistant, 'd', 4000), // 3
            big_message(ChatRole::User, 'e', 4000),      // 4 most recent user
            big_message(ChatRole::Assistant, 'f', 4000), // 5 newest
        ];
        env.meta.insert("model".into(), json!("summary-model"));
        let llm = Arc::new(CountingLlm {
            summary: "did A, B; decided X; remaining Y".into(),
            calls: AtomicUsize::new(0),
        });
        let mut ctx = stub_ctx();
        ctx.llm = llm.clone();

        let out = CompactContextNodeAdapter::new()
            .execute(
                &node(json!({"protect_last_messages": 2})),
                &[input(env)],
                &ctx,
            )
            .await
            .expect("execute");

        // Exactly one summary call happened.
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
        // Result: 1 summary + protected tail. protect_last=2 keeps msgs 4,5;
        // the most-recent-user rule also keeps msg 4 (already in tail). So
        // protected = {4,5} → 1 summary + 2 messages = 3.
        assert_eq!(out.context.messages.len(), 3);
        // First message is the reference-prefixed summary.
        let first = out.context.messages[0].text_or_default();
        assert!(first.starts_with(SUMMARY_PREFIX), "got: {first}");
        assert!(first.contains("did A, B"));
        // The newest two messages survive verbatim (tail protection).
        assert!(out.context.messages[1]
            .text_or_default()
            .starts_with("eeee"));
        assert!(out.context.messages[2]
            .text_or_default()
            .starts_with("ffff"));
        // System prompts untouched (never compactable).
        assert_eq!(out.context.system_prompts, vec!["system rules".to_string()]);
    }

    #[tokio::test]
    async fn keeps_recent_user_message_even_when_older_than_tail_window() {
        // Tail window of 1 would only protect the newest assistant; the most
        // recent user message (older) must still be kept.
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![
            big_message(ChatRole::User, 'a', 4000),      // 0
            big_message(ChatRole::Assistant, 'b', 4000), // 1
            big_message(ChatRole::User, 'q', 4000),      // 2 recent user
            big_message(ChatRole::Assistant, 'r', 4000), // 3
            big_message(ChatRole::Assistant, 's', 4000), // 4 newest
        ];
        env.meta.insert("model".into(), json!("m"));
        let llm = Arc::new(CountingLlm {
            summary: "S".into(),
            calls: AtomicUsize::new(0),
        });
        let mut ctx = stub_ctx();
        ctx.llm = llm.clone();

        let out = CompactContextNodeAdapter::new()
            .execute(
                &node(json!({"protect_last_messages": 1})),
                &[input(env)],
                &ctx,
            )
            .await
            .expect("execute");

        // protected = {newest=4} ∪ {recent user=2} → summary + msg2 + msg4 = 3.
        assert_eq!(out.context.messages.len(), 3);
        // The recent user message is present in the protected tail.
        assert!(out
            .context
            .messages
            .iter()
            .any(|m| m.role == ChatRole::User && m.text_or_default().starts_with("qqqq")));
    }

    #[tokio::test]
    async fn passthrough_when_no_model_resolvable() {
        // Over threshold but no model anywhere → passthrough (no panic, no call).
        let mut env = FlowEnvelope::empty();
        env.context.messages = (0..6)
            .map(|i| big_message(ChatRole::User, (b'a' + i) as char, 4000))
            .collect();
        let llm = Arc::new(CountingLlm {
            summary: "S".into(),
            calls: AtomicUsize::new(0),
        });
        let mut ctx = stub_ctx();
        ctx.llm = llm.clone();

        let out = CompactContextNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .expect("execute");
        assert_eq!(out.context.messages.len(), 6);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn emits_compaction_progress_event() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = (0..6)
            .map(|i| big_message(ChatRole::User, (b'a' + i) as char, 4000))
            .collect();
        env.meta.insert("model".into(), json!("m"));
        let progress = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(CountingLlm {
            summary: "S".into(),
            calls: AtomicUsize::new(0),
        });
        ctx.progress = progress.clone();

        CompactContextNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &ctx)
            .await
            .expect("execute");

        assert!(progress
            .events()
            .iter()
            .any(|(_, e)| matches!(e, ProgressEvent::Compaction { .. })));
    }
}
