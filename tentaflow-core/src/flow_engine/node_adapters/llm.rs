// =============================================================================
// Plik: flow_engine/node_adapters/llm.rs
// Opis: LlmNodeAdapter — adapter LLM nowego stacku (plan v4.2). Implementuje
//       NodeAdapter (blocking execute) i LlmAdapter (typed accessor
//       prepare_llm_request używany przez streaming executor). Czyta state
//       wyłącznie z `inputs[0].envelope` zgodnie z hard rule 1 (1-input edge).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::{LlmRequest, LlmToolSpec};
use crate::flow_engine::envelope::{
    ChatMessage, ChatRole, EnvelopeDelta, FlowEnvelope, FlowValue, NodeInput,
};
use crate::flow_engine::node_adapter::{
    ExecutionContext, LlmAdapter, NodeAdapter, PortSpec, StreamProducerAdapter,
};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use futures::stream::{BoxStream, StreamExt};

const NODE_TYPE: &str = "llm";

pub struct LlmNodeAdapter;

impl LlmNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        // 1. Override z node config — najwyższy priorytet (operator pin'uje
        //    konkretny backend dla tej ścieżki flow).
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        // 2. Model z envelope.meta — trigger seed'uje go z requestu.
        if let Some(m) = envelope
            .meta
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        Err(anyhow!(
            "llm adapter: no model. The agent has none, the block has none, and \
             the node has no model a service can serve — deploy a model or set \
             one on the agent (Agents → the agent → Model)."
        ))
    }

    /// `node.config[key]` ma priorytet, fallback do `envelope.meta[key]`
    /// (request seed). Etap 2 — symetrycznie do `pick_model`.
    fn pick_optional_f32(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<f32> {
        node.config
            .get(key)
            .and_then(|v| v.as_f64())
            .or_else(|| envelope.meta.get(key).and_then(|v| v.as_f64()))
            .map(|f| f as f32)
    }

    fn pick_optional_u32(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<u32> {
        node.config
            .get(key)
            .and_then(|v| v.as_u64())
            .or_else(|| envelope.meta.get(key).and_then(|v| v.as_u64()))
            .map(|u| u as u32)
    }

    fn pick_stop(node: &FlowNode) -> Vec<String> {
        node.config
            .get("stop")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn inline_system_prompt(node: &FlowNode) -> Option<String> {
        node.config
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Tools offered for this request. The harness writes `harness_tools` into
    /// `envelope.meta` (a JSON array of `{name, description, parameters}`); when
    /// present and the node is not on the final pass, the LLM request carries
    /// them so the model can call tools (§3.1, §3.4). Empty/absent = plain chat.
    /// The grace-summary iteration (`loop_final_pass`) drops tools so the model
    /// produces a final answer (§1.1).
    fn pick_tools(envelope: &FlowEnvelope) -> (Vec<LlmToolSpec>, Option<String>) {
        let final_pass = envelope
            .meta
            .get("loop_final_pass")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if final_pass {
            return (Vec::new(), None);
        }
        let tools = envelope
            .meta
            .get("harness_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| {
                        let name = t.get("name").and_then(|v| v.as_str())?;
                        Some(LlmToolSpec {
                            name: name.to_string(),
                            description: t
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            parameters: t
                                .get("parameters")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tool_choice = if tools.is_empty() {
            None
        } else {
            envelope
                .meta
                .get("tool_choice")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        (tools, tool_choice)
    }

    /// Reads a string audit-correlation key from `envelope.meta` (set by the
    /// harness / trigger). Empty strings collapse to `None`.
    fn meta_string(envelope: &FlowEnvelope, key: &str) -> Option<String> {
        envelope
            .meta
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Zbieranie messages z envelope.context. Plan v4.2:
    /// 1. system_prompts → osobne System messages (nie sklejać).
    /// 2. inline `system_prompt` z node config → osobny System message
    ///    dopisany ZA system_prompts (envelope-driven idą pierwsze).
    /// 3. context.messages w kolejności.
    /// 4. Jeśli payload jest Text i ostatnia message ma inny content,
    ///    doklejamy User(payload.text). Empty payload nie produkuje żadnego
    ///    dodatkowego user'a.
    fn build_messages(node: &FlowNode, envelope: &FlowEnvelope) -> Vec<ChatMessage> {
        let mut out: Vec<ChatMessage> = Vec::new();

        for sp in &envelope.context.system_prompts {
            out.push(ChatMessage::system(sp.clone()));
        }
        if let Some(inline) = Self::inline_system_prompt(node) {
            out.push(ChatMessage::system(inline));
        }
        out.extend(envelope.context.messages.iter().cloned());

        if let FlowValue::Text(t) = &envelope.payload {
            if !t.is_empty() {
                // Etap 3b: porównanie tylko gdy ostatnia message to czysty
                // Text. Parts (multimodal) zawsze potraktowane jako "różne"
                // — payload.Text będzie dodany jako kolejny user message.
                let last_matches = out
                    .last()
                    .map(|m| m.role == ChatRole::User && m.text() == Some(t.as_str()))
                    .unwrap_or(false);
                if !last_matches {
                    out.push(ChatMessage::user(t.clone()));
                }
            }
        }
        out
    }

    fn build_llm_request(
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
    ) -> Result<LlmRequest> {
        let model = Self::pick_model(node, envelope)?;
        // Named here rather than at the call sites: this is the one point where
        // both the node override and the envelope fallback have been applied,
        // and it is shared by the blocking and the streaming path.
        ctx.usage_sink.record_model(&model);
        let messages = Self::build_messages(node, envelope);
        let (tools, tool_choice) = Self::pick_tools(envelope);
        Ok(LlmRequest {
            model,
            messages,
            temperature: Self::pick_optional_f32(node, envelope, "temperature"),
            max_tokens: Self::pick_optional_u32(node, envelope, "max_tokens"),
            top_p: Self::pick_optional_f32(node, envelope, "top_p"),
            frequency_penalty: Self::pick_optional_f32(node, envelope, "frequency_penalty"),
            presence_penalty: Self::pick_optional_f32(node, envelope, "presence_penalty"),
            stop: Self::pick_stop(node),
            tools,
            tool_choice,
            deadline: ctx.deadline,
            cancel_token: ctx.cancel_token.clone(),
            user_id: ctx.user_id.clone(),
            user_role: ctx.user_role.clone(),
            flow_id: Self::meta_string(envelope, "flow_id"),
            flow_node_id: Some(node.id.clone()),
            agent_id: Self::meta_string(envelope, "agent_id"),
            agent_run_id: Self::meta_string(envelope, "agent_run_id"),
            correlation_id: Self::meta_string(envelope, "correlation_id"),
        })
    }
}

impl Default for LlmNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for LlmNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        // Adapter umie wyprodukować obie formy outputu — streaming
        // (przez prepare_llm_request + ctx.llm.stream_chat) i blocking
        // (execute → ctx.llm.execute_chat). Wybór ścieżki zależy od
        // executora (compiled.is_streaming); end-shape validation
        // przyjdzie razem z executor rewrite w stage 1d.
        // Zarówno `stream` jak i `full` produkują Text. Multimodal LLM
        // (Vision/Omni) jest osobnym node type w Etap 3.
        vec![
            PortSpec::new("stream", FlowDataType::Text),
            PortSpec::new("full", FlowDataType::Text),
        ]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("llm adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let request = Self::build_llm_request(node, envelope, ctx)?;
        let response = ctx
            .llm
            .execute_chat(request)
            .await
            .map_err(|e| anyhow!("llm adapter: dispatcher failed: {e}"))?;

        ctx.usage_sink.record(&node.id, response.usage);

        // Output envelope: klon input + nadpisany payload + dopisana
        // assistant message. Provenance trzymamy tylko dla artifacts
        // bag (envelope.artifacts) — payload jest głównym slotem flow,
        // jego pochodzenie wynika z trace (executor produkuje TraceStep).
        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Text(response.content.clone());
        let mut assistant = ChatMessage::assistant(response.content);
        assistant.reasoning_content = response.reasoning_content;
        if !response.tool_calls.is_empty() {
            // Tool calls ride on the assistant message so downstream nodes
            // (tool executor, converter) see them in conversation context.
            assistant.tool_calls = Some(response.tool_calls);
        }
        out.context.messages.push(assistant);
        Ok(out)
    }
}

impl LlmAdapter for LlmNodeAdapter {
    fn prepare_llm_request(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> LlmRequest {
        // prepare_llm_request jest sync — używany przez streaming branch
        // executora po wykonaniu wszystkich pre-LLM nodów. Brakujący input
        // albo brak modelu zwracają minimalny fallback z pustym modelem;
        // executor i tak złapie błąd w stream_chat (LlmDispatcher zwróci
        // 'no candidates' / 'model not found').
        let envelope_owned: FlowEnvelope;
        let envelope: &FlowEnvelope = match inputs.first() {
            Some(i) => &i.envelope,
            None => {
                envelope_owned = FlowEnvelope::empty();
                &envelope_owned
            }
        };
        Self::build_llm_request(node, envelope, ctx).unwrap_or_else(|_| {
            let (tools, tool_choice) = Self::pick_tools(envelope);
            LlmRequest {
                model: String::new(),
                messages: Self::build_messages(node, envelope),
                temperature: Self::pick_optional_f32(node, envelope, "temperature"),
                max_tokens: Self::pick_optional_u32(node, envelope, "max_tokens"),
                top_p: Self::pick_optional_f32(node, envelope, "top_p"),
                frequency_penalty: Self::pick_optional_f32(node, envelope, "frequency_penalty"),
                presence_penalty: Self::pick_optional_f32(node, envelope, "presence_penalty"),
                stop: Self::pick_stop(node),
                tools,
                tool_choice,
                deadline: ctx.deadline,
                cancel_token: ctx.cancel_token.clone(),
                user_id: ctx.user_id.clone(),
                user_role: ctx.user_role.clone(),
                flow_id: Self::meta_string(envelope, "flow_id"),
                flow_node_id: Some(node.id.clone()),
                agent_id: Self::meta_string(envelope, "agent_id"),
                agent_run_id: Self::meta_string(envelope, "agent_run_id"),
                correlation_id: Self::meta_string(envelope, "correlation_id"),
            }
        })
    }
}

/// §3.11 B — LLM jest jednym z producentów strumienia. Ten impl owija
/// dotychczasową ścieżkę streamingu (`prepare_llm_request` + `ctx.llm.
/// stream_chat`) bez duplikacji budowania requestu — executor woła
/// `produce_stream` zamiast inline'owego `ctx.llm.stream_chat`, a slot
/// producenta jest teraz uniwersalny (`AdapterRegistry::stream_producer`).
#[async_trait]
impl StreamProducerAdapter for LlmNodeAdapter {
    async fn produce_stream(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let request = self.prepare_llm_request(node, inputs, ctx);
        let adapter_stream = ctx
            .llm
            .stream_chat(request)
            .await
            .map_err(|e| anyhow!("stream_chat failed: {e}"))?;
        Ok(adapter_stream
            .map(|res| res.map(EnvelopeDelta::Llm))
            .boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::ConversationContext;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "llm1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(envelope: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(envelope),
        }
    }

    #[test]
    fn build_messages_stitches_system_prompts_then_messages() {
        let mut env = FlowEnvelope::empty();
        env.context = ConversationContext {
            messages: vec![ChatMessage::user("ping")],
            system_prompts: vec!["sp1".into(), "sp2".into()],
        };
        let n = node(json!({"system_prompt": "inline"}));
        let msgs = LlmNodeAdapter::build_messages(&n, &env);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, ChatRole::System);
        assert_eq!(msgs[0].text(), Some("sp1"));
        assert_eq!(msgs[1].role, ChatRole::System);
        assert_eq!(msgs[1].text(), Some("sp2"));
        assert_eq!(msgs[2].role, ChatRole::System);
        assert_eq!(msgs[2].text(), Some("inline"));
        assert_eq!(msgs[3].role, ChatRole::User);
    }

    #[test]
    fn payload_text_appended_when_last_message_differs() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user("old")];
        env.payload = FlowValue::Text("new question".into());
        let msgs = LlmNodeAdapter::build_messages(&node(json!({})), &env);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].text(), Some("new question"));
    }

    #[test]
    fn payload_text_skipped_when_last_user_message_matches() {
        let mut env = FlowEnvelope::empty();
        env.context.messages = vec![ChatMessage::user("same")];
        env.payload = FlowValue::Text("same".into());
        let msgs = LlmNodeAdapter::build_messages(&node(json!({})), &env);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn pick_model_prefers_node_config_then_meta() {
        let mut env = FlowEnvelope::empty();
        env.meta.insert("model".into(), json!("envelope-model"));
        let n = node(json!({"model": "node-model"}));
        assert_eq!(LlmNodeAdapter::pick_model(&n, &env).unwrap(), "node-model");
        let n = node(json!({}));
        assert_eq!(
            LlmNodeAdapter::pick_model(&n, &env).unwrap(),
            "envelope-model"
        );
    }

    #[test]
    fn pick_model_errors_when_neither_source_has_value() {
        let env = FlowEnvelope::empty();
        let n = node(json!({}));
        assert!(LlmNodeAdapter::pick_model(&n, &env).is_err());
    }

    #[test]
    fn harness_tools_pass_through_to_request() {
        let mut env = FlowEnvelope::empty();
        env.meta.insert("model".into(), json!("m"));
        env.meta.insert(
            "harness_tools".into(),
            json!([
                {"name": "core.skill_view", "description": "load a skill", "parameters": {"type": "object"}},
                {"name": "memory.memory_store", "description": "store", "parameters": {"type": "object"}}
            ]),
        );
        env.meta.insert("tool_choice".into(), json!("auto"));
        let req = LlmNodeAdapter::build_llm_request(&node(json!({})), &env, &stub_ctx()).unwrap();
        assert_eq!(req.tools.len(), 2);
        assert_eq!(req.tools[0].name, "core.skill_view");
        assert_eq!(req.tools[1].name, "memory.memory_store");
        assert_eq!(req.tool_choice.as_deref(), Some("auto"));
    }

    #[test]
    fn final_pass_drops_tools() {
        let mut env = FlowEnvelope::empty();
        env.meta.insert("model".into(), json!("m"));
        env.meta.insert(
            "harness_tools".into(),
            json!([{"name": "core.skill_view", "description": "x", "parameters": {}}]),
        );
        env.meta.insert("loop_final_pass".into(), json!(true));
        let req = LlmNodeAdapter::build_llm_request(&node(json!({})), &env, &stub_ctx()).unwrap();
        assert!(
            req.tools.is_empty(),
            "final pass must drop tools (grace summary)"
        );
        assert!(req.tool_choice.is_none());
    }

    #[test]
    fn prepare_llm_request_passes_temp_and_stop() {
        let mut env = FlowEnvelope::empty();
        env.meta.insert("model".into(), json!("m"));
        let n = node(json!({"temperature": 0.7, "max_tokens": 128, "stop": ["\n"]}));
        let inputs = vec![input(env)];
        let ctx = stub_ctx();
        let adapter = LlmNodeAdapter::new();
        let req = adapter.prepare_llm_request(&n, &inputs, &ctx);
        assert_eq!(req.model, "m");
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(128));
        assert_eq!(req.stop, vec!["\n".to_string()]);
    }
}
