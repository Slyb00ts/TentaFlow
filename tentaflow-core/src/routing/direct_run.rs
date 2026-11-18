// ===== File: routing/direct_run.rs — a direct model call on the Events log =====
//
// A plain model name is answered by the backend with no flow, so the flow
// engine never announces a run for it. This records one by hand, in the shape
// the engine's runs have: `request_started`, ONE `llm` step carrying the full
// request and the complete answer, and `request_finished`. It speaks only
// through the same progress events the engine emits, so the log treats it like
// any other run.
//
// The run closes exactly once. A caller that drops it before the answer
// arrived (a cancelled HTTP call, a stream abandoned between `begin` and the
// first poll) closes it as `cancelled`, or as `failed` when an error was seen.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse,
};
use crate::compliance::ai_gateway::AiGatewayContext;
use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
use crate::flow_engine::dispatchers::{ProgressEvent, ProgressSink};
use crate::flow_engine::progress_broker::{begin_run, RunDescriptor};

/// Step id of a top-level direct run's one step.
const STEP: &str = "llm";

/// One streamed choice, rebuilt from its deltas.
#[derive(Default)]
struct ChoiceAcc {
    role: Option<String>,
    content: String,
    reasoning: String,
    /// Tool-call slots by index: `(id, type, name, arguments)`.
    tool_calls: BTreeMap<u32, (String, String, String, String)>,
    finish_reason: Option<String>,
}

pub struct DirectRun {
    run_id: String,
    /// This call's step id. A child of a flow run gets one of its own, so it
    /// cannot collide with the parent's nodes.
    step: String,
    /// Closes only the step, not the run: the call is a child of a run that
    /// someone else ends.
    child: bool,
    scope: String,
    sink: std::sync::Arc<dyn ProgressSink>,
    requested_model: String,
    input: String,
    /// The complete answer as JSON (`{"choices": [...]}`), set when it is known.
    output: String,
    streamed: BTreeMap<u32, ChoiceAcc>,
    served_model: Option<String>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    failed: bool,
    closed: bool,
}

impl DirectRun {
    /// Opens the run. `None` when the call is not recorded: an origin that is
    /// not recorded, or a call made inside a run that records nothing.
    ///
    /// A call made on behalf of a flow run (`context.parent_run`) is recorded as
    /// a child step of that run, on its sink and scope, and never as a run of its
    /// own.
    pub fn begin(
        request: &ChatCompletionRequest,
        origin: FlowOrigin,
        actor: &FlowActor,
        user: Option<&crate::auth::acl::UserContext>,
        db: Option<&crate::db::DbPool>,
        context: Option<&AiGatewayContext>,
        correlation_id: Option<String>,
    ) -> Option<Self> {
        Self::begin_with(
            &request.model,
            "chat",
            request_json(request),
            origin,
            actor,
            user,
            db,
            context,
            correlation_id,
        )
    }

    /// [`Self::begin`] for an embeddings call: the input is the text sent to be
    /// embedded, whole.
    pub fn begin_embeddings(
        request: &crate::api::openai::types::EmbeddingRequest,
        origin: FlowOrigin,
        actor: &FlowActor,
        user: Option<&crate::auth::acl::UserContext>,
        db: Option<&crate::db::DbPool>,
        context: Option<&AiGatewayContext>,
        correlation_id: Option<String>,
    ) -> Option<Self> {
        Self::begin_with(
            &request.model,
            "embedding",
            embeddings_json(request),
            origin,
            actor,
            user,
            db,
            context,
            correlation_id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_with(
        model: &str,
        service_type: &str,
        input: String,
        origin: FlowOrigin,
        actor: &FlowActor,
        user: Option<&crate::auth::acl::UserContext>,
        db: Option<&crate::db::DbPool>,
        context: Option<&AiGatewayContext>,
        correlation_id: Option<String>,
    ) -> Option<Self> {
        if !super::records_runs(origin) {
            return None;
        }
        if let Some(parent) = context.and_then(|ctx| ctx.parent_run.as_ref()) {
            return Self::begin_child(model, input, parent);
        }
        let mut meta =
            FlowRequestMeta::new(uuid::Uuid::new_v4().to_string(), origin, actor.clone());
        if let Some(u) = user {
            meta.user_id = Some(u.user_id.clone());
            meta.user_role = Some(u.role.clone());
            meta.org_id = u.org_id.clone();
        }
        meta.correlation_id = correlation_id;
        super::start_recording(&mut meta, db, context.and_then(|ctx| ctx.org_id.as_deref()));
        begin_run(&meta, RunDescriptor::resolved(model, service_type, "text"));
        let sink = meta.progress_sink.clone()?;
        let scope = meta.progress_scope();
        sink.emit(
            &scope,
            ProgressEvent::NodeStarted {
                node_id: STEP.into(),
                node_type: STEP.into(),
            },
        );
        Some(Self {
            run_id: meta.request_id.clone(),
            step: STEP.to_string(),
            child: false,
            scope,
            sink,
            requested_model: model.to_string(),
            input,
            output: String::new(),
            streamed: BTreeMap::new(),
            served_model: None,
            prompt_tokens: None,
            completion_tokens: None,
            failed: false,
            closed: false,
        })
    }

    /// A step of the parent run's own log. `None` when the parent records
    /// nothing (a Bus or Mesh run, a sink that keeps no content).
    fn begin_child(
        model: &str,
        input: String,
        parent: &crate::flow_engine::dispatchers::ParentRun,
    ) -> Option<Self> {
        let sink = parent.sink.clone()?;
        let step = format!("llm-call-{}", uuid::Uuid::new_v4());
        sink.emit(
            &parent.scope,
            ProgressEvent::NodeStarted {
                node_id: step.clone(),
                node_type: "llm".into(),
            },
        );
        Some(Self {
            run_id: String::new(),
            step,
            child: true,
            scope: parent.scope.clone(),
            sink,
            requested_model: model.to_string(),
            input,
            output: String::new(),
            streamed: BTreeMap::new(),
            served_model: None,
            prompt_tokens: None,
            completion_tokens: None,
            failed: false,
            closed: false,
        })
    }

    fn emit(&self, event: ProgressEvent) {
        self.sink.emit(&self.scope, event);
    }

    /// Closes the run once; later calls (the drop after an explicit close) do nothing.
    fn close(&mut self, step_status: &str, run_status: &str) {
        if self.closed {
            return;
        }
        self.closed = true;
        if self.output.is_empty() && !self.streamed.is_empty() {
            self.output = streamed_output(&self.streamed);
        }
        self.emit(ProgressEvent::NodeModel {
            node_id: self.step.clone(),
            requested: Some(self.requested_model.clone()),
            served: self.served_model.clone(),
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
        });
        let input = crate::events::step_io::limit_side(std::mem::take(&mut self.input));
        let output = crate::events::step_io::limit_side(std::mem::take(&mut self.output));
        self.emit(ProgressEvent::NodeIo {
            node_id: self.step.clone(),
            input: Some(input),
            output: Some(output),
        });
        self.emit(ProgressEvent::NodeFinished {
            node_id: self.step.clone(),
            status: step_status.into(),
        });
        if !self.child {
            self.emit(ProgressEvent::RunFinished {
                run_id: self.run_id.clone(),
                status: run_status.into(),
            });
        }
    }

    pub fn finish_response(mut self, response: &ChatCompletionResponse) {
        self.served_model = Some(response.model.clone());
        if let Some(usage) = response.usage.as_ref() {
            self.prompt_tokens = Some(u64::from(usage.prompt_tokens));
            self.completion_tokens = Some(u64::from(usage.completion_tokens));
        }
        self.output = response_output(response);
        self.close("ok", "completed");
    }

    /// Replaces the recorded input, for a call that turned out to be another
    /// kind than it was opened as (an alias that serves embeddings, not chat).
    pub fn set_embeddings_input(&mut self, request: &crate::api::openai::types::EmbeddingRequest) {
        self.input = embeddings_json(request);
    }

    /// Closes the run with the summary of an embeddings answer: how many
    /// vectors of what size came back, never the vectors themselves.
    pub fn finish_embeddings(mut self, response: &crate::api::openai::types::EmbeddingResponse) {
        self.served_model = Some(response.model.clone());
        self.prompt_tokens = Some(u64::from(response.usage.prompt_tokens));
        self.output = embeddings_summary(response);
        self.close("ok", "completed");
    }

    /// Closes the run with the raw answer of a service whose reply is not a
    /// chat completion.
    pub fn finish_output(mut self, output: String) {
        self.output = output;
        self.close("ok", "completed");
    }

    pub fn finish_failed(mut self) {
        self.failed = true;
        self.close("error", "failed");
    }

    /// Wraps the chunk stream so the run closes when the stream does.
    pub fn record_stream(
        self,
        inner: Pin<Box<dyn Stream<Item = crate::error::Result<ChatCompletionChunk>> + Send>>,
    ) -> Pin<Box<dyn Stream<Item = crate::error::Result<ChatCompletionChunk>> + Send>> {
        Box::pin(RecordedStream { inner, run: self })
    }

    fn absorb(&mut self, chunk: &ChatCompletionChunk) {
        if !chunk.model.is_empty() {
            self.served_model = Some(chunk.model.clone());
        }
        if let Some(usage) = chunk.usage.as_ref() {
            self.prompt_tokens = Some(u64::from(usage.prompt_tokens));
            self.completion_tokens = Some(u64::from(usage.completion_tokens));
        }
        for choice in &chunk.choices {
            let acc = self.streamed.entry(choice.index).or_default();
            if let Some(role) = choice.delta.role.as_ref() {
                acc.role = Some(role.clone());
            }
            if let Some(text) = choice.delta.content.as_deref() {
                acc.content.push_str(text);
            }
            if let Some(text) = choice.delta.reasoning_content.as_deref() {
                acc.reasoning.push_str(text);
            }
            for call in choice.delta.tool_calls.iter().flatten() {
                let slot = acc.tool_calls.entry(call.index).or_default();
                if let Some(id) = call.id.as_ref() {
                    slot.0 = id.clone();
                }
                if let Some(kind) = call.tool_type.as_ref() {
                    slot.1 = kind.clone();
                }
                if let Some(function) = call.function.as_ref() {
                    if let Some(name) = function.name.as_ref() {
                        slot.2.push_str(name);
                    }
                    if let Some(arguments) = function.arguments.as_ref() {
                        slot.3.push_str(arguments);
                    }
                }
            }
            if let Some(reason) = choice.finish_reason.as_ref() {
                acc.finish_reason = Some(reason.clone());
            }
        }
    }
}

/// Runs one chat completion straight on the runtime executor and records it as
/// a run of its own. For internal callers that reach the executor without the
/// router (summaries, ML Studio evaluation): they are AI runs like any other.
pub async fn execute_chat_recorded(
    executor: &crate::services::runtime::executor::ModelRuntimeExecutor,
    request: ChatCompletionRequest,
    exec_ctx: &mut crate::services::runtime::context::ExecutionContext,
    db: Option<&crate::db::DbPool>,
    attribution: Option<&crate::auth::acl::UserContext>,
) -> Result<ChatCompletionResponse, crate::services::runtime::executor::ExecutorError> {
    let context = AiGatewayContext {
        org_id: exec_ctx.org_id.clone(),
        ..Default::default()
    };
    // `attribution` names the person the work is done for, so the run is theirs
    // to see (and their organisation's admins'). It is a label on the record
    // only: it is not the identity the model call is authorised under.
    let actor = match attribution {
        Some(user) => FlowActor::user(user.user_id.clone()),
        None => exec_ctx.actor.clone(),
    };
    let run = DirectRun::begin(
        &request,
        exec_ctx.origin,
        &actor,
        attribution.or(exec_ctx.user.as_ref()),
        db,
        Some(&context),
        None,
    );
    let result = executor.execute_chat(request, exec_ctx).await;
    if let Some(run) = run {
        match &result {
            Ok(response) => run.finish_response(response),
            Err(_) => run.finish_failed(),
        }
    }
    result
}

impl Drop for DirectRun {
    fn drop(&mut self) {
        // Reached unclosed only when the caller went away first: a dropped
        // future or an abandoned stream.
        if self.failed {
            self.close("error", "failed");
        } else {
            self.close("error", "cancelled");
        }
    }
}

/// The whole request as the backend was asked: messages, tools, parameters.
/// A raw audio payload is reduced to its size; its bytes are not text.
fn request_json(request: &ChatCompletionRequest) -> String {
    let mut value = serde_json::to_value(request).unwrap_or_default();
    if let (Some(object), Some(audio)) = (value.as_object_mut(), request.audio_input.as_ref()) {
        object.insert(
            "audio_input".into(),
            serde_json::json!({ "bytes": audio.len() }),
        );
    }
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

/// The text sent to be embedded, with the parameters that shaped the call.
fn embeddings_json(request: &crate::api::openai::types::EmbeddingRequest) -> String {
    serde_json::to_string_pretty(request).unwrap_or_default()
}

/// What an embeddings call returned, as a summary: the vectors are not text.
pub fn embeddings_summary(response: &crate::api::openai::types::EmbeddingResponse) -> String {
    format!(
        "{} embedding(s) of {} dimension(s) from {}",
        response.data.len(),
        response.data.first().map_or(0, |d| d.embedding.len()),
        response.model
    )
}

/// Reduces a generated audio payload to its size and transcript.
fn without_audio_bytes(mut choices: serde_json::Value) -> serde_json::Value {
    if let Some(list) = choices.as_array_mut() {
        for choice in list {
            let audio = choice
                .pointer_mut("/message/audio")
                .and_then(|a| a.as_object_mut());
            if let Some(audio) = audio {
                if let Some(data) = audio.remove("data") {
                    let len = data.as_str().map(str::len).unwrap_or(0);
                    audio.insert("data_bytes".into(), len.into());
                }
            }
        }
    }
    choices
}

/// Every choice of the answer in full: content, reasoning, tool calls, finish reason.
fn response_output(response: &ChatCompletionResponse) -> String {
    let choices = without_audio_bytes(serde_json::to_value(&response.choices).unwrap_or_default());
    serde_json::to_string_pretty(&serde_json::json!({ "choices": choices })).unwrap_or_default()
}

/// The same shape as [`response_output`], rebuilt from the streamed deltas.
fn streamed_output(streamed: &BTreeMap<u32, ChoiceAcc>) -> String {
    let choices: Vec<serde_json::Value> = streamed
        .iter()
        .map(|(index, acc)| {
            let mut message = serde_json::json!({
                "role": acc.role.clone().unwrap_or_else(|| "assistant".into()),
                "content": acc.content,
            });
            if !acc.reasoning.is_empty() {
                message["reasoning_content"] = acc.reasoning.clone().into();
            }
            if !acc.tool_calls.is_empty() {
                message["tool_calls"] = acc
                    .tool_calls
                    .values()
                    .map(|(id, kind, name, arguments)| {
                        serde_json::json!({
                            "id": id,
                            "type": kind,
                            "function": { "name": name, "arguments": arguments },
                        })
                    })
                    .collect::<Vec<_>>()
                    .into();
            }
            serde_json::json!({
                "index": index,
                "message": message,
                "finish_reason": acc.finish_reason,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "choices": choices })).unwrap_or_default()
}

struct RecordedStream {
    inner: Pin<Box<dyn Stream<Item = crate::error::Result<ChatCompletionChunk>> + Send>>,
    run: DirectRun,
}

impl Stream for RecordedStream {
    type Item = crate::error::Result<ChatCompletionChunk>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.as_mut().poll_next(cx);
        match &polled {
            Poll::Ready(Some(Ok(chunk))) => self.run.absorb(chunk),
            Poll::Ready(Some(Err(_))) => self.run.failed = true,
            Poll::Ready(None) => {
                if self.run.failed {
                    self.run.close("error", "failed");
                } else {
                    self.run.close("ok", "completed");
                }
            }
            Poll::Pending => {}
        }
        polled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::AddonCallProvenance;
    use crate::flow_engine::dispatchers::ParentRun;

    struct Recording(parking_lot::Mutex<Vec<(String, ProgressEvent)>>);

    impl ProgressSink for Recording {
        fn emit(&self, scope: &str, event: ProgressEvent) {
            self.0.lock().push((scope.to_string(), event));
        }

        fn wants_io(&self) -> bool {
            true
        }
    }

    fn request() -> ChatCompletionRequest {
        serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "the addon's prompt"}],
        }))
        .expect("request")
    }

    /// The context an addon host function builds from the worker's provenance.
    fn context_of(provenance: &AddonCallProvenance) -> AiGatewayContext {
        AiGatewayContext {
            parent_run: provenance.parent.clone(),
            ..Default::default()
        }
    }

    fn begin_in(parent: ParentRun) -> Option<DirectRun> {
        let provenance = AddonCallProvenance::nested(parent);
        DirectRun::begin(
            &request(),
            provenance.origin,
            &provenance.actor_for("notes"),
            None,
            None,
            Some(&context_of(&provenance)),
            None,
        )
    }

    /// A model call made by a block inside a non-Bus run is a child step of that
    /// run: its own step id, the full prompt and answer, and no end of the run.
    #[test]
    fn a_nested_call_in_a_chat_run_is_a_child_step() {
        let sink = std::sync::Arc::new(Recording(Default::default()));
        let parent = ParentRun::new(FlowOrigin::Chat, sink.clone(), "chat-run-1".to_string());
        let run = begin_in(parent).expect("a chat-nested call is recorded");
        run.finish_output(r#"{"choices":[{"message":{"content":"answer"}}]}"#.to_string());

        let events = sink.0.lock();
        assert!(events.iter().all(|(scope, _)| scope == "chat-run-1"));
        let step = match &events[0].1 {
            ProgressEvent::NodeStarted { node_id, node_type } => {
                assert_eq!(node_type, "llm");
                node_id.clone()
            }
            other => panic!("first event is not a step start: {other:?}"),
        };
        assert!(step.starts_with("llm-call-"));
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            ProgressEvent::NodeIo { node_id, input, output }
                if *node_id == step
                    && input.as_deref().is_some_and(|i| i.contains("the addon's prompt"))
                    && output.as_deref().is_some_and(|o| o.contains("answer"))
        )));
        assert!(events.iter().any(
            |(_, e)| matches!(e, ProgressEvent::NodeFinished { node_id, .. } if *node_id == step)
        ));
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, ProgressEvent::RunFinished { .. })),
            "the parent run is ended by its owner, not by the child step"
        );
    }

    /// A model call made by a block inside a Bus run stores nothing, even though
    /// the parent run has a sink that would keep content.
    #[test]
    fn a_call_nested_in_a_bus_run_stores_nothing() {
        let sink = std::sync::Arc::new(Recording(Default::default()));
        let parent = ParentRun::new(FlowOrigin::Bus, sink.clone(), "bus-run-1".to_string());
        assert!(begin_in(parent).is_none());
        assert!(sink.0.lock().is_empty());
    }
}
