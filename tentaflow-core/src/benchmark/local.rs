// ===== File: benchmark/local.rs — in-process benchmark path: drives ModelRuntimeExecutor::stream_chat for backends with no dialable HTTP chat API =====

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;

use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent, StreamOptions};
use crate::auth::acl::UserContext;
use crate::services::runtime::context::ExecutionContext;
use crate::services::runtime::executor::ModelRuntimeExecutor;

use super::client::StreamObservation;

/// Runs benchmark requests through the runtime executor instead of a socket.
/// Cloning is cheap (one `Arc`), which the concurrency scenarios rely on the
/// same way they rely on a cloneable `reqwest::Client`.
///
/// Every backend the catalog can reach becomes measurable: embedded llama.cpp /
/// MLX (no endpoint at all), QUIC sidecars, coding-agent bridges (custom RPC),
/// HTTP services, external cloud providers and models owned by another mesh
/// node. Timing is stamped identically to the HTTP client — at chunk ARRIVAL in
/// the drain loop — so numbers from both paths sit in the same result table.
#[derive(Clone)]
pub struct LocalRunner {
    executor: Arc<ModelRuntimeExecutor>,
    /// Identity of the operator who started the run. Model-level ACL is
    /// enforced by the resolver against this context, so a benchmark cannot
    /// reach a model its author may not use.
    user: Option<UserContext>,
}

/// Which backend actually served an in-process request. The executor resolves
/// by model NAME, so when several instances serve the same name the answer is
/// not knowable up front — the runner logs this once per target instead of
/// pretending the picked row is where the tokens came from.
#[derive(Debug, Clone, Default)]
pub struct RouteNote {
    pub backend: Option<String>,
    pub node: Option<String>,
}

impl LocalRunner {
    pub fn new(executor: Arc<ModelRuntimeExecutor>, user: Option<UserContext>) -> Self {
        Self { executor, user }
    }

    /// Executes one streamed generation and returns the raw observation plus
    /// the route the executor took. `include_usage` is always requested: token
    /// counts come exclusively from `usage`, never from estimation, exactly as
    /// on the HTTP path.
    pub(super) async fn stream(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> anyhow::Result<(StreamObservation, RouteNote)> {
        let request = ChatCompletionRequest {
            reasoning_effort: None,
            modalities: None,
            audio: None,
            model: model.to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text(prompt.to_string())),
                ..Default::default()
            }],
            temperature: Some(0.0),
            max_tokens: Some(max_tokens),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            memory_options: None,
            audio_input: None,
            extra: Default::default(),
        };

        // §2.5 — a benchmark run is core measurement work; the operator who
        // started it (when there is one) stays the actor for attribution.
        let actor = match self.user.as_ref() {
            Some(u) => crate::flow_engine::dispatcher::FlowActor::user(u.user_id.clone()),
            None => crate::flow_engine::dispatcher::FlowActor::system_component("benchmark"),
        };
        let mut exec_ctx = ExecutionContext::new(
            self.user.clone(),
            crate::flow_engine::dispatcher::FlowOrigin::System,
            actor,
        );
        let mut stream = self
            .executor
            .stream_chat(request, &mut exec_ctx)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut obs = StreamObservation::empty();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            // Reasoning tokens are decode work too — a chain-of-thought model
            // streams reasoning_content before content, and ignoring it would
            // report a TTFT that never happened.
            let has_content = chunk.choices.iter().any(|c| {
                c.delta.content.as_deref().is_some_and(|s| !s.is_empty())
                    || c.delta
                        .reasoning_content
                        .as_deref()
                        .is_some_and(|s| !s.is_empty())
            });
            if has_content {
                let now = Instant::now();
                if obs.first_token_at.is_none() {
                    obs.first_token_at = Some(now);
                }
                obs.last_token_at = Some(now);
            }
            if let Some(usage) = chunk.usage {
                obs.prompt_tokens = usage.prompt_tokens;
                obs.completion_tokens = usage.completion_tokens;
                obs.usage_seen = true;
            }
        }
        obs.stream_end_at = Some(Instant::now());

        let route = RouteNote {
            backend: exec_ctx.route_metadata.backend_type.clone(),
            node: exec_ctx.route_metadata.served_by_node.clone(),
        };
        Ok((obs, route))
    }
}
