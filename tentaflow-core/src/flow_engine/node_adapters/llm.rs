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
    audio_format_from_mime, ChatMessage, ChatRole, EnvelopeDelta, FinishReason, FlowEnvelope,
    FlowValue, LlmToolCall, MessagePart, NodeInput,
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

    /// Zbieranie messages z envelope.context:
    /// 1. system_prompts + inline `system_prompt` z node config → JEDNA
    ///    wiadomość System, sekcje rozdzielone pustą linią (envelope-driven
    ///    idą pierwsze, inline na końcu).
    /// 2. context.messages w kolejności.
    /// 3. Jeśli payload jest Text i ostatnia message ma inny content,
    ///    doklejamy User(payload.text). Empty payload nie produkuje żadnego
    ///    dodatkowego user'a.
    ///
    /// Sklejanie, a nie osobne wiadomości (jak zakładał plan v4.2): ścisłe
    /// szablony czatu — Qwen3, Mistral, Gemma — dopuszczają DOKŁADNIE jedną
    /// wiadomość systemową na pozycji zerowej i przerywają renderowanie
    /// wyjątkiem „System message must be at the beginning" przy drugiej.
    /// `agent_context` sam produkuje ich kilka (prompt agenta, indeks skilli,
    /// nota anty-injection, wyniki delegacji), więc rozdzielone wiadomości
    /// czyniły harness niekompatybilnym z większością lokalnych modeli.
    /// Ogranicznik pozostaje pustą linią, więc fence'y anty-injection
    /// (`<<<PASSAGE>>>`) działają jak wcześniej.
    fn build_messages(node: &FlowNode, envelope: &FlowEnvelope) -> Vec<ChatMessage> {
        let mut out: Vec<ChatMessage> = Vec::new();

        let mut sections: Vec<&str> = envelope
            .context
            .system_prompts
            .iter()
            .map(String::as_str)
            .filter(|s| !s.trim().is_empty())
            .collect();
        let inline = Self::inline_system_prompt(node);
        if let Some(inline) = inline.as_deref() {
            if !inline.trim().is_empty() {
                sections.push(inline);
            }
        }
        if !sections.is_empty() {
            out.push(ChatMessage::system(sections.join("\n\n")));
        }
        out.extend(envelope.context.messages.iter().cloned());

        if let Some(user) = Self::payload_user_message(envelope, &out) {
            out.push(user);
        }
        out
    }

    /// The User message that `build_messages` derives from `envelope.payload`,
    /// or `None` when the payload adds nothing (empty, non-Text, or already the
    /// last message).
    ///
    /// Extracted so `execute` can PERSIST it into the conversation. The user's
    /// prompt arrives in `envelope.payload`, and `execute` overwrites that
    /// payload with the model's answer — so without persisting it the prompt
    /// survives exactly ONE iteration of the harness loop. From the second
    /// iteration on the model reasons over tool results with no idea what it
    /// was asked to do, and a strict chat template (Qwen3) refuses to render a
    /// conversation containing no user turn at all
    /// (`No user query found in messages.`).
    ///
    /// `prior` is the message list the payload would be appended to, so the
    /// "already the last message" check matches whatever the caller is building.
    /// Folds every arriving edge into ONE envelope for the turn.
    ///
    /// With typed ports per modality a multimodal turn legitimately arrives as
    /// several edges — the prompt down `in`, the picture down `image` — and one
    /// of them has to carry the conversation while the others contribute their
    /// media. The text branch wins the base (it holds history, system prompts
    /// and meta accumulated by `agent_context`), and a media payload is folded
    /// into it; when no text edge arrived the media envelope IS the base.
    ///
    /// Media is carried in `meta` rather than in the payload of the base, so a
    /// single turn can hold both a prompt and a picture — `payload` has room
    /// for exactly one value, which is why simply picking `inputs.first()` used
    /// to make the second edge disappear.
    fn merge_inputs(inputs: &[NodeInput]) -> Result<FlowEnvelope> {
        let base_idx = inputs
            .iter()
            .position(|i| matches!(i.envelope.payload, FlowValue::Text(_)))
            .unwrap_or(0);
        let base = inputs
            .get(base_idx)
            .ok_or_else(|| anyhow!("llm adapter: missing input edge"))?;
        let mut out: FlowEnvelope = (*base.envelope).clone();

        for (idx, other) in inputs.iter().enumerate() {
            if idx == base_idx {
                continue;
            }
            match &other.envelope.payload {
                FlowValue::Image { blob_ref, mime, dims } => {
                    out.meta.insert(
                        "llm_image".into(),
                        serde_json::json!({
                            "blob": blob_ref,
                            "mime": mime,
                            "dims": dims,
                        }),
                    );
                }
                FlowValue::Audio { blob_ref, mime, sample_rate } => {
                    out.meta.insert(
                        "llm_audio".into(),
                        serde_json::json!({
                            "blob": blob_ref,
                            "mime": mime,
                            "sample_rate": sample_rate,
                        }),
                    );
                }
                // A second text edge cannot happen (one `in` port) and anything
                // else has no place in a chat turn; dropping it loudly beats
                // pretending it was sent.
                other_kind => {
                    tracing::warn!(
                        from = %other.from_node_id,
                        port = %other.from_port,
                        "llm adapter: input of unsupported kind ignored: {other_kind:?}"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Drops tool calls whose `arguments` are not parseable JSON and says why,
    /// returning the surviving calls plus a note for the model (`None` when
    /// nothing was dropped).
    ///
    /// A generation cut by the token budget ends mid-string, so the backend
    /// hands back a REAL tool call carrying half a JSON object — that is
    /// correct behaviour, not a broken model. The damage starts if we keep it:
    /// the call goes into the conversation, and the next turn replays the whole
    /// history to a server whose template parses `arguments` back into an
    /// object (`supports_object_arguments`). It fails on the unterminated
    /// string and answers 500 — for every following request, because the
    /// poisoned message never leaves the history. One truncated turn kills the
    /// session.
    ///
    /// So the call is dropped at the boundary and the model is told, which is
    /// what it needs to retry with a shorter argument. Silently dropping it
    /// would leave the model waiting for a result that never comes.
    pub(crate) fn sanitize_tool_calls(
        calls: Vec<LlmToolCall>,
        finish: FinishReason,
    ) -> (Vec<LlmToolCall>, Option<String>) {
        let mut kept = Vec::with_capacity(calls.len());
        let mut dropped: Vec<String> = Vec::new();
        for call in calls {
            if serde_json::from_str::<serde_json::Value>(&call.arguments).is_ok() {
                kept.push(call);
            } else {
                dropped.push(call.name.clone());
            }
        }
        if dropped.is_empty() {
            return (kept, None);
        }
        let truncated = finish == FinishReason::Length;
        let names = dropped.join(", ");
        let note = if truncated {
            format!(
                "[System] Odpowiedź została ucięta na limicie tokenów w środku wywołania: \
                 {names}. Argumenty są niekompletne, więc wywołanie zostało odrzucone. \
                 Powtórz je z krótszą treścią — na przykład zapisz plik w częściach."
            )
        } else {
            format!(
                "[System] Wywołanie {names} miało argumenty, których nie da się sparsować \
                 jako JSON, więc zostało odrzucone. Powtórz je z poprawnym JSON-em."
            )
        };
        (kept, Some(note))
    }

    pub(crate) fn payload_user_message(
        envelope: &FlowEnvelope,
        prior: &[ChatMessage],
    ) -> Option<ChatMessage> {
        // An Image payload becomes a multimodal user turn instead of vanishing.
        // Before this, a graph that fed a picture into `llm` dropped it in
        // silence: `build_messages` only ever looked at `FlowValue::Text`, so
        // the model was asked about an image it never received and answered
        // from thin air. The caption rides along when the flow put one in
        // `meta.image_prompt`; without it the picture stands alone and the
        // instruction comes from the system prompt or the history.
        // The picture is either THIS turn's payload (a graph whose only input is
        // an image) or one folded in from a second edge by `merge_inputs`.
        let image: Option<crate::flow_engine::blob_store::BlobRef> = match &envelope.payload {
            FlowValue::Image { blob_ref, .. } => Some(blob_ref.clone()),
            _ => envelope
                .meta
                .get("llm_image")
                .and_then(|v| v.get("blob"))
                .and_then(|b| serde_json::from_value(b.clone()).ok()),
        };
        let audio: Option<(crate::flow_engine::blob_store::BlobRef, String)> =
            match &envelope.payload {
                FlowValue::Audio { blob_ref, mime, .. } => {
                    Some((blob_ref.clone(), audio_format_from_mime(mime)))
                }
                _ => envelope.meta.get("llm_audio").and_then(|v| {
                    let blob: crate::flow_engine::blob_store::BlobRef =
                        serde_json::from_value(v.get("blob")?.clone()).ok()?;
                    let mime = v.get("mime").and_then(|m| m.as_str()).unwrap_or("audio/wav");
                    Some((blob, audio_format_from_mime(mime)))
                }),
            };

        if image.is_some() || audio.is_some() {
            let mut parts = Vec::with_capacity(3);
            // The prompt for a picture is the turn's text when there is one —
            // that is the whole point of wiring both edges — and falls back to
            // an explicit caption in meta.
            let caption = match &envelope.payload {
                FlowValue::Text(t) if !t.trim().is_empty() => Some(t.clone()),
                _ => envelope
                    .meta
                    .get("image_prompt")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string),
            };
            if let Some(caption) = caption {
                parts.push(MessagePart::Text { text: caption });
            }
            if let Some(blob_ref) = image {
                parts.push(MessagePart::Image {
                    blob_ref,
                    detail: envelope
                        .meta
                        .get("image_detail")
                        .and_then(|v| v.as_str())
                        .unwrap_or("auto")
                        .to_string(),
                });
            }
            if let Some((blob_ref, format)) = audio {
                parts.push(MessagePart::Audio { blob_ref, format });
            }
            return Some(ChatMessage::user_multimodal(parts));
        }
        let FlowValue::Text(t) = &envelope.payload else {
            return None;
        };
        if t.is_empty() {
            return None;
        }
        // Comparison only when the last message is plain Text. Parts
        // (multimodal) always count as "different" — payload.Text is then
        // appended as another user message.
        let last_matches = prior
            .last()
            .map(|m| m.role == ChatRole::User && m.text() == Some(t.as_str()))
            .unwrap_or(false);
        if last_matches {
            None
        } else {
            Some(ChatMessage::user(t.clone()))
        }
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
            // Audio out is per-block config, not a model guess: the operator
            // decides that THIS step should speak. A model without the
            // capability is rejected by the resolver, not silently muted.
            audio_out: node
                .config
                .get("audio_output")
                .and_then(|v| v.as_object())
                .map(|o| crate::flow_engine::dispatchers::AudioOut {
                    voice: o
                        .get("voice")
                        .and_then(|v| v.as_str())
                        .unwrap_or("alloy")
                        .to_string(),
                    format: o
                        .get("format")
                        .and_then(|v| v.as_str())
                        .unwrap_or("wav")
                        .to_string(),
                }),
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
    /// Typed ports for every modality a chat model can be fed, mirroring how
    /// `output` carries six of them: a port is a promise about the DATA, not
    /// about the model, so the block advertises what it can carry and the
    /// resolver rejects a model that cannot take it (`required_input_modalities`).
    ///
    /// Declaring only `in: Text` made an image impossible to WIRE: the edge
    /// failed R6 before anyone could ask whether the model has vision. The
    /// Flow Builder greys out the ports the selected model does not accept,
    /// reading `input_modalities` off the catalog entry — so what changes with
    /// the model is the port's availability, not its type.
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("in", FlowDataType::Text),
            PortSpec::new("image", FlowDataType::Image),
            PortSpec::new("audio", FlowDataType::Audio),
        ]
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
            // An omni model SPEAKS AND WRITES in one turn, so `audio` is not an
            // alternative to the text ports — it lights up alongside them. The
            // recording rides in `artifacts["audio"]` because an envelope has
            // one payload and that payload is the answer's text; a consumer
            // reads the artifact. `active_output_ports` keeps the port dark on
            // turns that produced no sound, so a text-only model never drags a
            // dead branch behind it.
            PortSpec::new("audio", FlowDataType::Audio),
        ]
    }

    /// Only light `audio` when this turn actually produced sound.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<std::collections::HashSet<String>> {
        let mut ports: std::collections::HashSet<String> =
            ["stream", "full"].iter().map(|s| s.to_string()).collect();
        if result.artifacts.contains_key("audio") {
            ports.insert("audio".to_string());
        }
        Some(ports)
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let merged = Self::merge_inputs(inputs)?;
        let envelope: &FlowEnvelope = &merged;

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
        let mut out: FlowEnvelope = envelope.clone();
        // Persist the prompt BEFORE overwriting the payload with the answer:
        // it lives only in `payload`, so a harness loop would otherwise drop
        // the user's request after the first iteration (see
        // `payload_user_message`).
        if let Some(user) = Self::payload_user_message(envelope, &envelope.context.messages) {
            out.context.messages.push(user);
        }
        out.payload = FlowValue::Text(response.content.clone());
        // Speech from an omni turn: the bytes go to the blob store and the
        // artifact bag, because `payload` already holds the answer's text and a
        // turn that both speaks and writes needs somewhere for the second half.
        // The `audio` output port lights up off this artifact
        // (`active_output_ports`).
        if let Some(audio) = &response.audio {
            match ctx.blobs.put(audio.bytes.clone(), &audio.mime).await {
                Ok(blob_ref) => {
                    out.artifacts.insert(
                        "audio".to_string(),
                        FlowValue::Audio {
                            blob_ref,
                            mime: audio.mime.clone(),
                            sample_rate: None,
                        },
                    );
                    if let Some(transcript) = &audio.transcript {
                        out.meta.insert(
                            "audio_transcript".into(),
                            serde_json::Value::String(transcript.clone()),
                        );
                    }
                }
                // Losing the recording must not lose the answer: the text half
                // of the turn is already valid and useful on its own.
                Err(e) => tracing::warn!(node = %node.id, "storing model audio failed: {e}"),
            }
        }
        let mut assistant = ChatMessage::assistant(response.content);
        assistant.reasoning_content = response.reasoning_content;
        // A truncated generation yields half a JSON object; keeping it poisons
        // every later turn (`sanitize_tool_calls`).
        let (calls, note) =
            Self::sanitize_tool_calls(response.tool_calls, response.finish_reason);
        if !calls.is_empty() {
            // Tool calls ride on the assistant message so downstream nodes
            // (tool executor, converter) see them in conversation context.
            assistant.tool_calls = Some(calls);
        }
        out.context.messages.push(assistant);
        if let Some(note) = note {
            tracing::warn!(node = %node.id, "{note}");
            // As a user turn, so the loop keeps a valid alternation and the
            // model reads it as an instruction rather than as its own words.
            out.context.messages.push(ChatMessage::user(note));
        }
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
                audio_out: None,
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

    /// Sekcje systemowe lądują w JEDNEJ wiadomości System na pozycji zerowej.
    /// Ścisłe szablony (Qwen3/Mistral/Gemma) przerywają na drugiej wiadomości
    /// systemowej, a `agent_context` produkuje ich kilka — rozdzielone
    /// wiadomości wywracały każdą turę na lokalnym modelu.
    #[test]
    fn build_messages_merges_system_prompts_into_one() {
        let mut env = FlowEnvelope::empty();
        env.context = ConversationContext {
            messages: vec![ChatMessage::user("ping")],
            system_prompts: vec!["sp1".into(), "sp2".into()],
        };
        let n = node(json!({"system_prompt": "inline"}));
        let msgs = LlmNodeAdapter::build_messages(&n, &env);
        assert_eq!(msgs.len(), 2, "system sections must collapse into one message");
        assert_eq!(msgs[0].role, ChatRole::System);
        assert_eq!(msgs[0].text(), Some("sp1\n\nsp2\n\ninline"));
        assert_eq!(msgs[1].role, ChatRole::User);
        assert!(
            msgs.iter().skip(1).all(|m| m.role != ChatRole::System),
            "no system message may follow a non-system one"
        );
    }

    /// Regresja złapana na żywym modelu: prompt użytkownika żyje wyłącznie
    /// w `payload`, który `execute` nadpisuje odpowiedzią. Bez utrwalenia go
    /// w rozmowie druga iteracja pętli harnessu widziała już tylko
    /// [system, assistant(tool_calls), tool] — model rozumował nad wynikami
    /// narzędzi, nie wiedząc, o co go poproszono, a ścisłe szablony odmawiały
    /// renderowania („No user query found in messages.").
    #[test]
    fn payload_user_message_survives_a_loop_iteration() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("napisz hello.py".into());

        // Iteracja 1: prompt trafia do żądania…
        let msgs = LlmNodeAdapter::build_messages(&node(json!({})), &env);
        assert_eq!(msgs.last().map(|m| m.role), Some(ChatRole::User));

        // …i musi zostać utrwalony w rozmowie, zanim payload zostanie nadpisany.
        let user = LlmNodeAdapter::payload_user_message(&env, &env.context.messages)
            .expect("prompt must be persistable");
        assert_eq!(user.text(), Some("napisz hello.py"));

        // Iteracja 2: payload to już odpowiedź modelu, ale prompt został.
        let mut next = env.clone();
        next.context.messages.push(user);
        next.context.messages.push(ChatMessage::assistant("wołam narzędzie"));
        next.payload = FlowValue::Text("wołam narzędzie".into());
        let msgs2 = LlmNodeAdapter::build_messages(&node(json!({})), &next);
        assert!(
            msgs2.iter().any(|m| m.role == ChatRole::User && m.text() == Some("napisz hello.py")),
            "the user's request must still be in the conversation on iteration 2"
        );
    }

    /// Regresja zmierzona na rig24: wiadomość `assistant` z niedokończonym
    /// JSON-em w `arguments` wywraca llama.cpp błędem 500 („invalid string:
    /// missing closing quote") przy RENDEROWANIU historii — zanim model cokolwiek
    /// wygeneruje. Zapisana raz, zatruwa sesję na stałe, bo wraca w każdym
    /// kolejnym żądaniu.
    #[test]
    fn a_truncated_tool_call_is_dropped_and_explained() {
        let calls = vec![
            LlmToolCall {
                id: "ok".into(),
                name: "core.fs_read".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
            },
            LlmToolCall {
                id: "cut".into(),
                name: "core.fs_write".into(),
                arguments: r#"{"path":"a.py","content":"niedokonczony"#.into(),
            },
        ];
        let (kept, note) = LlmNodeAdapter::sanitize_tool_calls(calls, FinishReason::Length);
        assert_eq!(kept.len(), 1, "only the parseable call survives");
        assert_eq!(kept[0].id, "ok");
        let note = note.expect("the model must be told why its call vanished");
        assert!(note.contains("core.fs_write"), "the note names the dropped call");
        assert!(note.contains("ucięta"), "a length cut is explained as a cut");
    }

    /// Obraz w payloadzie musi stać się multimodalną turą, a nie zniknąć.
    /// Wcześniej `build_messages` patrzył wyłącznie na `FlowValue::Text`, więc
    /// graf podający zdjęcie do bloku `llm` gubił je bez śladu i model
    /// odpowiadał o obrazie, którego nigdy nie dostał.
    #[test]
    fn an_image_payload_becomes_a_multimodal_turn() {
        use crate::flow_engine::blob_store::BlobRef;
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Image {
            blob_ref: BlobRef {
                id: "b1".into(),
                size_bytes: 1024,
                mime: "image/png".into(),
                sha256: "abc".into(),
            },
            mime: "image/png".into(),
            dims: Some((800, 600)),
        };
        env.meta.insert("image_prompt".into(), serde_json::json!("Co tu widać?"));

        let msg = LlmNodeAdapter::payload_user_message(&env, &[])
            .expect("an image must reach the model");
        assert_eq!(msg.role, ChatRole::User);
        match &msg.content {
            crate::flow_engine::envelope::ChatMessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2, "caption + image");
                assert!(matches!(parts[0], MessagePart::Text { .. }));
                assert!(matches!(parts[1], MessagePart::Image { .. }));
            }
            other => panic!("expected multimodal parts, got {other:?}"),
        }

        // …i trafia do żądania, nie tylko do rozmowy.
        let msgs = LlmNodeAdapter::build_messages(&node(json!({})), &env);
        assert!(
            msgs.iter().any(|m| matches!(
                &m.content,
                crate::flow_engine::envelope::ChatMessageContent::Parts(p)
                    if p.iter().any(|x| matches!(x, MessagePart::Image { .. }))
            )),
            "the request must carry the image"
        );
    }

    /// Poprawne wywołania przechodzą nietknięte i bez noty — inaczej każda tura
    /// dokładałaby modelowi szum.
    #[test]
    fn valid_tool_calls_pass_through_untouched() {
        let calls = vec![LlmToolCall {
            id: "a".into(),
            name: "core.exec".into(),
            arguments: r#"{"argv":["ls"]}"#.into(),
        }];
        let (kept, note) = LlmNodeAdapter::sanitize_tool_calls(calls, FinishReason::ToolCalls);
        assert_eq!(kept.len(), 1);
        assert!(note.is_none());
    }

    /// Brak jakiejkolwiek sekcji systemowej nie może wyprodukować pustej
    /// wiadomości System — część backendów odrzuca pustą treść.
    #[test]
    fn build_messages_emits_no_system_when_there_is_nothing_to_say() {
        let mut env = FlowEnvelope::empty();
        env.context = ConversationContext {
            messages: vec![ChatMessage::user("ping")],
            system_prompts: vec!["   ".into()],
        };
        let msgs = LlmNodeAdapter::build_messages(&node(json!({})), &env);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, ChatRole::User);
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
