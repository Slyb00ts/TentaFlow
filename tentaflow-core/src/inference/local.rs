// =============================================================================
// Plik: inference/local.rs
// Opis: Adapter konwertujacy OpenAI-compatible requesty na lokalne wywolania
//       InferenceEngine (llama.cpp / MLX). Obsluguje chat completions,
//       streaming SSE i embeddingi.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice, ChunkChoice, Delta,
    EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, GenPerf,
    Message, MessageContent, Usage,
};
use crate::inference::{
    EmbeddingParams, GenerateParams, GenerateResult, InferenceManager, StopReason, StreamToken,
};
use crate::routing::chat_template::{ChatMessage, ChatTemplate};

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};
use uuid::Uuid;

/// Adapter routujacy OpenAI-compatible requesty do lokalnego silnika inferencji.
pub struct LocalInferenceHandler {
    inference_manager: Arc<RwLock<InferenceManager>>,
}

/// Marker recognised by the Qwen-family chat templates as "answer without a
/// thinking block". Harmless to models that do not know it — it reads as a
/// short instruction rather than corrupting the turn.
const NO_THINK_MARKER: &str = "/no_think";

/// Whether the caller asked for no reasoning. `reasoning_effort` is the field
/// the OpenAI wire already carries, so callers do not need a second one; the
/// embedded path simply never read it until now.
fn reasoning_disabled(effort: Option<&str>) -> bool {
    effort.is_some_and(|e| matches!(e.trim().to_ascii_lowercase().as_str(), "none" | "off"))
}

/// Appends the marker to the system turn, or inserts a system turn when the
/// request has none. Appending rather than replacing keeps whatever the caller
/// actually asked the model to do.
fn apply_no_think(messages: &mut Vec<ChatMessage>) {
    if let Some(system) = messages.iter_mut().find(|m| m.role == "system") {
        if !system.content.contains(NO_THINK_MARKER) {
            system.content.push(' ');
            system.content.push_str(NO_THINK_MARKER);
        }
        return;
    }
    messages.insert(
        0,
        ChatMessage {
            role: "system".to_string(),
            content: NO_THINK_MARKER.to_string(),
        },
    );
}


/// Without llama.cpp there is no grammar engine to compile against, and the
/// other backends enforce tool shape themselves. Constraining is a llama.cpp
/// capability, not a platform one.
#[cfg(not(feature = "inference-llamacpp"))]
fn tool_call_grammar(_tools: &[crate::api::openai::types::Tool]) -> Option<String> {
    None
}

#[cfg(all(test, feature = "inference-llamacpp"))]
mod grammar_tests {
    use super::{tool_call_grammar, TOOL_CALL_TRIGGER};
    use crate::api::openai::types::{FunctionDefinition, Tool};

    fn tool(name: &str, params: serde_json::Value) -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: name.to_string(),
                description: None,
                parameters: Some(params),
            },
        }
    }

    #[test]
    fn no_tools_means_no_grammar() {
        assert!(tool_call_grammar(&[]).is_none());
    }

    /// The grammar has to come from llama.cpp's own converter, so it always
    /// matches what that build accepts. If this stops producing a grammar the
    /// constraint is silently gone and malformed calls come back.
    #[test]
    fn schemas_compile_to_a_grammar() {
        let tools = vec![
            tool(
                "search_web",
                serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }),
            ),
            tool("ping", serde_json::json!({"type": "object"})),
        ];

        let grammar = tool_call_grammar(&tools).expect("llama.cpp musi skompilowac schemat");

        assert!(grammar.contains("root ::="), "brak reguly root: {grammar}");
        // The trigger is fed into the grammar when it fires; a root that does
        // not accept it empties the stack and llama.cpp terminates the process.
        assert!(
            grammar.starts_with("root ::= \"<tool_call>\""),
            "root musi zaczynac sie wyzwalaczem: {grammar}"
        );
        assert!(
            grammar.contains("</tool_call>"),
            "domkniecie musi byc w gramatyce: {grammar}"
        );
        // Both tool names must survive into the grammar, or one of them became
        // unreachable for the model.
        assert!(grammar.contains("search_web"), "brak search_web: {grammar}");
        assert!(grammar.contains("ping"), "brak ping: {grammar}");
    }

    #[test]
    fn the_trigger_is_the_opening_tag() {
        assert_eq!(TOOL_CALL_TRIGGER, "<tool_call>");
    }

    /// llama.cpp feeds the grammar the CAPTURE GROUP of the trigger pattern.
    /// Without a group it hands over everything generated so far, the grammar
    /// rejects the prose and the process is terminated by a C++ exception —
    /// twice observed, both times fatal to Core mid-run.
    #[test]
    fn the_trigger_pattern_captures_from_the_opening_tag() {
        use super::TOOL_CALL_TRIGGER_PATTERN as p;

        assert!(p.contains('('), "wzorzec musi miec grupe: {p}");
        let group = &p[p.find('(').unwrap()..];
        assert!(
            group.starts_with("(<tool_call>"),
            "grupa musi zaczynac sie od znacznika: {group}"
        );
    }
}

#[cfg(test)]
mod reasoning_tests {
    use super::{apply_no_think, reasoning_disabled, NO_THINK_MARKER};
    use crate::routing::chat_template::ChatMessage;

    #[test]
    fn only_an_explicit_off_disables_reasoning() {
        assert!(reasoning_disabled(Some("none")));
        assert!(reasoning_disabled(Some("OFF")));
        assert!(!reasoning_disabled(Some("low")));
        // Absent must not change a model that thinks by default.
        assert!(!reasoning_disabled(None));
    }

    #[test]
    fn the_marker_joins_the_existing_system_turn() {
        let mut messages = vec![ChatMessage {
            role: "system".into(),
            content: "Extract the facts.".into(),
        }];

        apply_no_think(&mut messages);

        assert_eq!(messages.len(), 1, "nie dokladamy drugiej tury systemowej");
        assert!(messages[0].content.starts_with("Extract the facts."));
        assert!(messages[0].content.ends_with(NO_THINK_MARKER));
    }

    #[test]
    fn a_request_without_a_system_turn_gets_one() {
        let mut messages = vec![ChatMessage {
            role: "user".into(),
            content: "hi".into(),
        }];

        apply_no_think(&mut messages);

        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn applying_twice_does_not_repeat_the_marker() {
        let mut messages = vec![ChatMessage {
            role: "system".into(),
            content: "x".into(),
        }];

        apply_no_think(&mut messages);
        apply_no_think(&mut messages);

        assert_eq!(messages[0].content.matches(NO_THINK_MARKER).count(), 1);
    }
}

/// Literal that opens a call. Prose before it is free; everything after it must
/// be a valid call.
const TOOL_CALL_TRIGGER: &str = "<tool_call>";

/// Pattern that switches the grammar on. The CAPTURE GROUP matters: llama.cpp
/// feeds the grammar exactly what the group holds, so it must start at the
/// opening tag. A pattern without one hands over every character generated so
/// far, the grammar rejects the prose, and llama.cpp terminates the process.
const TOOL_CALL_TRIGGER_PATTERN: &str = r"[\s\S]*?(<tool_call>[\s\S]*)";

/// GBNF for "a JSON object naming one of THESE tools with ITS arguments".
///
/// Prompt mode can only ask for the shape in words, and a model asked in words
/// misses constantly — a brace too many, a tag form from another vendor, a tool
/// name that does not exist. Each miss is a call that never runs. Compiled from
/// the real schemas, the wrong output stops being reachable: the sampler cannot
/// emit a token the grammar forbids.
///
/// `None` when there is nothing to constrain or llama.cpp rejects the schema —
/// unconstrained sampling is what happened before, so falling back to it costs
/// nothing that was not already being paid.
#[cfg(feature = "inference-llamacpp")]
fn tool_call_grammar(tools: &[crate::api::openai::types::Tool]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let variants: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            let arguments = t
                .function
                .parameters
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"const": t.function.name},
                    "arguments": arguments,
                },
                "required": ["name", "arguments"],
                "additionalProperties": false,
            })
        })
        .collect();

    let schema = serde_json::json!({ "oneOf": variants });
    let body = match tentaflow_wrappers::llama::json_schema_to_grammar(&schema) {
        Ok(grammar) => grammar,
        Err(e) => {
            warn!("tool-call grammar rejected by llama.cpp, sampling unconstrained: {e}");
            return None;
        }
    };

    // The trigger text is fed INTO the grammar once it fires, so the root has
    // to accept it. A grammar that starts at the JSON leaves the stack empty on
    // `<tool_call>` and llama.cpp raises a C++ exception that terminates the
    // process — this cost a Core crash mid-run to learn.
    //
    // Wrapping also lets the closing tag be part of the constraint, so the
    // block cannot be left unterminated the way models kept leaving it.
    let body = body.replacen("root ::=", "tool-json ::=", 1);
    Some(format!(
        "root ::= \"{TOOL_CALL_TRIGGER}\" tool-json \"</tool_call>\"\n{body}"
    ))
}

impl LocalInferenceHandler {
    pub fn new(manager: Arc<RwLock<InferenceManager>>) -> Self {
        Self {
            inference_manager: manager,
        }
    }

    /// Obsluga /v1/chat/completions przez lokalne LLM (non-streaming).
    pub async fn handle_chat_completion(
        &self,
        request: &ChatCompletionRequest,
        tools: Option<&[crate::api::openai::types::Tool]>,
    ) -> anyhow::Result<ChatCompletionResponse> {
        let template = self.get_chat_template().await;
        let (deploy_params, context_length) = {
            let manager = self.inference_manager.read().await;
            (manager.get_deploy_params(), manager.active_context_length())
        };
        let params =
            Self::request_to_generate_params(request, &template, &deploy_params, context_length, tools);
        let model_name = self
            .loaded_model_name()
            .await
            .unwrap_or_else(|| request.model.clone());

        debug!(
            "Lokalna inferencja chat completion: model={}, max_tokens={}",
            model_name, params.max_tokens
        );

        let result = {
            let manager = self.inference_manager.read().await;
            let engine = manager
                .active_engine()
                .ok_or_else(|| anyhow::anyhow!("Brak zaladowanego modelu lokalnego"))?;
            engine.generate(params).await?
        };

        let response = Self::generate_result_to_response(&result, &model_name);
        Ok(response)
    }

    /// Streaming bezposrednio jako ChatCompletionChunk — zero serde_json hop.
    /// Uzywane przez router::streaming dla LocalLlm; OpenAI HTTP API SSE
    /// endpoint nadal moze uzywac `handle_chat_completion_stream` ktory
    /// owija to w SSE.
    pub async fn stream_chat_chunks(
        &self,
        request: &ChatCompletionRequest,
        tools: Option<&[crate::api::openai::types::Tool]>,
    ) -> anyhow::Result<mpsc::Receiver<ChatCompletionChunk>> {
        let template = self.get_chat_template().await;
        let (deploy_params, context_length) = {
            let manager = self.inference_manager.read().await;
            (manager.get_deploy_params(), manager.active_context_length())
        };
        let params =
            Self::request_to_generate_params(request, &template, &deploy_params, context_length, tools);
        let model_name = self
            .loaded_model_name()
            .await
            .unwrap_or_else(|| request.model.clone());
        let completion_id = format!("chatcmpl-{}", Uuid::new_v4());
        let created = chrono::Utc::now().timestamp() as u64;

        debug!(
            "Lokalna inferencja streaming (binary): model={}, id={}",
            model_name, completion_id
        );

        let token_rx = {
            let manager = self.inference_manager.read().await;
            let engine = manager
                .active_engine()
                .ok_or_else(|| anyhow::anyhow!("Brak zaladowanego modelu lokalnego"))?;
            engine.generate_stream(params).await?
        };

        let (chunk_tx, chunk_rx) = mpsc::channel::<ChatCompletionChunk>(256);

        tokio::spawn(Self::stream_tokens_to_chunks(
            token_rx,
            chunk_tx,
            completion_id,
            model_name,
            created,
        ));

        Ok(chunk_rx)
    }

    /// Obsluga /v1/embeddings przez lokalne modele.
    pub async fn handle_embeddings(
        &self,
        request: &EmbeddingRequest,
    ) -> anyhow::Result<EmbeddingResponse> {
        let texts = match &request.input {
            EmbeddingInput::Single(text) => vec![text.clone()],
            EmbeddingInput::Multiple(texts) => texts.clone(),
        };

        let params = EmbeddingParams {
            texts: texts.clone(),
            normalize: true,
        };

        debug!("Lokalne embeddingi: {} tekstow", params.texts.len());

        let result = {
            let manager = self.inference_manager.read().await;
            let engine = manager
                .active_engine()
                .ok_or_else(|| anyhow::anyhow!("Brak zaladowanego modelu lokalnego"))?;
            engine.embeddings(params).await?
        };

        // Engines that tokenize themselves report the real count; otherwise
        // estimate from whitespace-separated words so usage is never zero.
        let prompt_tokens = result.prompt_tokens.unwrap_or_else(|| {
            texts
                .iter()
                .map(|t| t.split_whitespace().count() as u32)
                .sum::<u32>()
        });

        let data: Vec<EmbeddingData> = result
            .embeddings
            .into_iter()
            .enumerate()
            .map(|(i, embedding)| EmbeddingData {
                object: "embedding".to_string(),
                index: i as u32,
                embedding,
            })
            .collect();

        Ok(EmbeddingResponse {
            object: "list".to_string(),
            data,
            model: request.model.clone(),
            usage: EmbeddingUsage {
                prompt_tokens,
                total_tokens: prompt_tokens,
            },
        })
    }

    /// Czy lokalne LLM jest dostepne i ma zaladowany model?
    pub async fn is_available(&self) -> bool {
        let manager = self.inference_manager.read().await;
        manager
            .active_engine()
            .map(|e| e.is_loaded())
            .unwrap_or(false)
    }

    /// Jaki model jest zaladowany?
    pub async fn loaded_model_name(&self) -> Option<String> {
        let manager = self.inference_manager.read().await;
        manager
            .active_engine()
            .and_then(|e| e.model_info())
            .map(|info| info.name)
    }

    // ========================================================================
    // KONWERSJA TYPOW
    // ========================================================================

    /// Pobiera wykryty szablon chatu z aktywnego silnika inferencji.
    /// Jesli model nie jest zaladowany lub brak info — zwraca Plain.
    async fn get_chat_template(&self) -> ChatTemplate {
        let info = {
            let manager = self.inference_manager.read().await;
            manager.active_engine().and_then(|e| e.model_info())
        };
        let Some(info) = info else {
            warn!("get_chat_template: brak aktywnego silnika/model_info — Plain");
            return ChatTemplate::Plain;
        };

        // Source of truth dla MLX (katalog safetensors): szablon czytany z
        // tokenizer_config.json przy KAZDYM requescie, niezalezny od ewentualnie
        // pustego pola chat_template w cache silnika. llama.cpp ma plik .gguf
        // (nie-katalog) i osadzony template — wtedy ufamy polu model_info.
        let model_dir = std::path::Path::new(&info.path);
        let is_dir = model_dir.is_dir();
        if is_dir {
            let detected = crate::routing::chat_template::detect_chat_template(model_dir);
            debug!(
                "get_chat_template: backend={} path={:?} re-detekcja={:?}",
                info.backend,
                info.path,
                detected.name()
            );
            return detected;
        }

        info.chat_template
            .as_deref()
            .map(|name| match name {
                "chatml" => ChatTemplate::ChatML,
                "llama3" => ChatTemplate::Llama3,
                "mistral" => ChatTemplate::Mistral,
                "alpaca" => ChatTemplate::Alpaca,
                _ => ChatTemplate::Plain,
            })
            .unwrap_or(ChatTemplate::Plain)
    }

    /// Konwertuje ChatCompletionRequest na GenerateParams.
    /// Formatuje prompt zgodnie z wykrytym szablonem chatu modelu.
    fn request_to_generate_params(
        request: &ChatCompletionRequest,
        template: &ChatTemplate,
        deploy_params: &super::DeployParamsSnapshot,
        context_length: Option<u32>,
        tools: Option<&[crate::api::openai::types::Tool]>,
    ) -> GenerateParams {
        // Konwertuj wiadomosci OpenAI na ChatMessage
        let chat_messages: Vec<ChatMessage> = request
            .messages
            .iter()
            .filter_map(|msg| {
                let text = match &msg.content {
                    Some(MessageContent::Text(t)) => t.clone(),
                    Some(MessageContent::Parts(parts)) => parts
                        .iter()
                        .filter_map(|p| {
                            if let crate::api::openai::types::ContentPart::Text { text } = p {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    None => return None,
                };

                Some(ChatMessage {
                    role: msg.role.clone(),
                    content: text,
                })
            })
            .collect();

        // Reasoning off: a request that asks for no thinking gets the marker the
        // Qwen-family templates recognise, appended to the system turn. Embedded
        // models build their prompt HERE — there is no server-side template to
        // pass `enable_thinking` to — so this is the one place the switch can
        // exist. Extraction work (summarise this page, classify this text) pays
        // the full cost of a thinking block for no benefit.
        let mut chat_messages = chat_messages;
        if reasoning_disabled(request.reasoning_effort.as_deref()) {
            apply_no_think(&mut chat_messages);
        }

        // Sformatuj prompt wedlug szablonu chatu
        let prompt = template.format_messages(&chat_messages, true);

        // Dodaj stop sequences z szablonu
        let mut stop_sequences = request.stop.clone().unwrap_or_default();
        stop_sequences.extend(template.stop_tokens());

        // Deploy-time defaults (z manifest [[parameter]] z bindingiem
        // mlx_field) jako baseline. Request override z OpenAI API (max_tokens,
        // temperature, top_p) ma priorytet. Llama-cpp deploy params idą
        // load-time przez `LlamaCppEngine::load_model`, tu czytamy tylko
        // `mlx` mape — llama-cpp request-time używa `Default::default()`.
        let defaults = GenerateParams::from_mlx_deploy_defaults(&deploy_params.mlx);
        // Only an EXPLICIT deploy-time value counts as a configured cap; the
        // struct default must not masquerade as one.
        let configured_max_tokens = deploy_params
            .mlx
            .get("default_max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        debug!(
            "Sformatowano prompt szablonem {:?}: {} znakow, {} stop sequences",
            template.name(),
            prompt.len(),
            stop_sequences.len(),
        );

        // OpenAI `frequency_penalty` (additive, [-2.0, 2.0]) ma INNE semantyki
        // niz MLX/llama.cpp `repeat_penalty` (multiplicative, > 0). Bezposrednie
        // mapowanie powodowalo `repeat_penalty=0.0` przy `frequency_penalty=0.0`,
        // co dzielilo logits przez zero w apply_repeat_penalty_gpu — sampler
        // wracal pad token (id=0) i model produkowal pusty tekst po 1-szym
        // tokenie (Bielik 4-bit MLX wisial w nieskonczonosc).
        // Konwersja: clamp do dodatniej multiplicative skali, gdzie 0 → defaults.
        let repeat_penalty = match request.frequency_penalty {
            Some(fp) if fp.abs() > f32::EPSILON => (1.0 + fp.abs() * 0.1).max(1.0),
            _ => defaults.repeat_penalty,
        };

        // Prompt mode takes the tools off the request and describes them in
        // prose; the schemas come back in here so the sampler can enforce what
        // the prose only asks for.
        // OFF unless the deploy asks for it. The grammar works — llama.cpp
        // compiles it and it contains the right rules — but the LAZY trigger
        // semantics are not understood well enough yet: whatever the trigger
        // pattern captures, llama.cpp reports "Unexpected empty grammar stack
        // after accepting piece: <tool_call>" and answers it with a C++
        // exception that TERMINATES THE PROCESS. Three runs died that way.
        //
        // A feature that kills the server when it misfires does not belong on
        // by default, whatever it fixes when it works. The parser stays the
        // working defence until the trigger semantics are pinned down.
        let grammar_enabled = deploy_params
            .llamacpp
            .get("tool_call_grammar")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tools_present = grammar_enabled && tools.is_some_and(|t| !t.is_empty());
        let tool_call_grammar = if grammar_enabled {
            tools.and_then(tool_call_grammar)
        } else {
            None
        };

        GenerateParams {
            prompt,
            // Precedence: what the caller asked for, else what the deploy
            // configured, else the model's own context. `defaults.max_tokens`
            // is NOT usable as the last resort — `GenerateParams::default()`
            // fills it with a constant, and that constant used to cap every
            // answer from an engine with no deploy-time override (llama.cpp).
            max_tokens: request
                .max_tokens
                .or(configured_max_tokens)
                .unwrap_or_else(|| super::default_response_budget(context_length)),
            // Tool calls get a grammar so a malformed one cannot be generated.
            // Empty when the request carries no tools — ordinary chat samples
            // freely, as it always has.
            grammar: tool_call_grammar.unwrap_or_default(),
            grammar_triggers: if tools_present {
                vec![TOOL_CALL_TRIGGER_PATTERN.to_string()]
            } else {
                Vec::new()
            },
            temperature: request.temperature.unwrap_or(defaults.temperature),
            top_p: request.top_p.unwrap_or(defaults.top_p),
            top_k: defaults.top_k,
            repeat_penalty,
            stop_sequences,
            system_prompt: None, // system prompt jest juz wbudowany w sformatowany prompt
            // Deploy-time caps (request_override=false) — pinned by the wizard,
            // enforced by the MLX runtime guard. Not overridable per request.
            max_context_tokens: defaults.max_context_tokens,
            memory_budget_mb: defaults.memory_budget_mb,
        }
    }

    /// Konwertuje GenerateResult na ChatCompletionResponse.
    fn generate_result_to_response(
        result: &GenerateResult,
        model_name: &str,
    ) -> ChatCompletionResponse {
        let finish_reason = match &result.stop_reason {
            StopReason::MaxTokens => "length",
            StopReason::StopSequence(_) => "stop",
            StopReason::EndOfText => "stop",
        };

        ChatCompletionResponse {
            id: format!("chatcmpl-{}", Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp() as u64,
            model: model_name.to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message {
                    audio: None,
                    role: "assistant".to_string(),
                    content: Some(MessageContent::Text(result.text.clone())),
                    reasoning_content: None,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(finish_reason.to_string()),
                logprobs: None,
            }],
            usage: Some(Usage {
                prompt_tokens: result.prompt_tokens,
                completion_tokens: result.tokens_generated,
                total_tokens: result.prompt_tokens + result.tokens_generated,
            }),
            system_fingerprint: Some("local-inference".to_string()),
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            speaker_confidence: None,
            detected_intent: None,
            detected_tools: None,
        }
    }

    /// Przetwarza stream tokenow na chunki SSE w formacie OpenAI.
    /// Hot-path streaming dla ws_binary path. Zero JSON hop — bezposrednio
    /// emituje `ChatCompletionChunk` strukt do mpsc, ktory streaming.rs
    /// konsumuje i przekazuje do CBOR-encoded WS frames.
    async fn stream_tokens_to_chunks(
        mut token_rx: mpsc::Receiver<StreamToken>,
        chunk_tx: mpsc::Sender<ChatCompletionChunk>,
        completion_id: String,
        model_name: String,
        created: u64,
    ) {
        // Metryki wall-clock liczone w tym konsumencie: start przed pierwszym
        // tokenem, znacznik pierwszego tokena z niepustym contentem.
        let start = Instant::now();
        let mut first_token_at: Option<Instant> = None;

        // Pierwszy chunk — wysyla role bez contentu.
        let first = ChatCompletionChunk {
            id: completion_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created,
            model: model_name.clone(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: Delta {
                    role: Some("assistant".to_string()),
                    content: None,
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            system_fingerprint: Some("local-inference".to_string()),
            audio: None,
            detected_intent: None,
            detected_tools: None,
            transcribed_text: None,
            speaker_id: None,
            speaker_name: None,
            usage: None,
            perf: None,
        };
        if chunk_tx.send(first).await.is_err() {
            return;
        }

        while let Some(token) = token_rx.recv().await {
            // CR-003: na finale mapujemy REALNY powód zakończenia silnika; twardy
            // błąd silnika sygnalizujemy jako finish_reason "error" zamiast cichego
            // "stop", żeby konsument SSE nie traktował awarii jak normalnego końca.
            let finish_reason = if token.is_final {
                if let Some(err) = &token.error {
                    warn!("Strumień inferencji zakończony błędem silnika: {err}");
                    Some("error".to_string())
                } else {
                    Some(
                        match &token.finish_reason {
                            Some(StopReason::MaxTokens) => "length",
                            Some(StopReason::StopSequence(_)) => "stop",
                            Some(StopReason::EndOfText) | None => "stop",
                        }
                        .to_string(),
                    )
                }
            } else {
                None
            };
            let content = if token.text.is_empty() && token.is_final {
                None
            } else {
                Some(token.text)
            };
            // Pierwszy token z realnym contentem wyznacza TTFT.
            if first_token_at.is_none()
                && content.as_deref().map(|c| !c.is_empty()).unwrap_or(false)
            {
                first_token_at = Some(Instant::now());
            }
            // Usage jedzie na tokenie finalnym z realnymi licznikami silnika; tym
            // chunkiem token accounting (AiGateway) zlicza zużycie. Silnik, który
            // liczb nie podał, daje 0 → pomijamy, by nie wpisywać zer.
            let usage =
                if token.is_final && (token.prompt_tokens > 0 || token.completion_tokens > 0) {
                    Some(Usage {
                        prompt_tokens: token.prompt_tokens,
                        completion_tokens: token.completion_tokens,
                        total_tokens: token.prompt_tokens + token.completion_tokens,
                    })
                } else {
                    None
                };
            // Metryki przepustowości: preferujemy pomiar silnika (realne granice faz
            // prefill/dekodowanie), wall-clock jest tylko fallbackiem gdy silnik nie
            // podał wartości (0.0). TTFT zostaje zawsze wall-clock — to realny,
            // user-facing czas do pierwszego tokena.
            let engine_prefill_tps = token.prefill_tps;
            let engine_completion_tps = token.completion_tps;
            let engine_ttft_ms = token.ttft_ms;
            let perf = usage.as_ref().map(|u| {
                // TTFT z silnika (granica faz slotu) jest dokładniejszy niż zegar
                // ścienny tego mostu, który łapie bufor kanału i kolejkę schedulera;
                // wall-clock zostaje tylko gdy silnik nie zmierzył (np. MLX → 0).
                let ttft_ms = if engine_ttft_ms > 0 {
                    engine_ttft_ms
                } else {
                    first_token_at
                        .map(|t| t.duration_since(start).as_millis() as u32)
                        .unwrap_or(0)
                };
                let now = Instant::now();
                let decode_secs = first_token_at
                    .map(|t| now.duration_since(t).as_secs_f32())
                    .unwrap_or(0.0);
                let prefill_secs = (ttft_ms as f32) / 1000.0;
                let prefill_tps =
                    super::prefill_tps(engine_prefill_tps, u.prompt_tokens, prefill_secs);
                let decode_tps =
                    super::decode_tps(engine_completion_tps, u.completion_tokens, decode_secs);
                // total_ms: pełny czas od startu strumienia do tego (ostatniego
                // przed usage) tokena. `now` zmierzony powyżej domyka okno.
                let total_ms = now.duration_since(start).as_millis() as u32;
                GenPerf {
                    ttft_ms,
                    prefill_tps,
                    decode_tps,
                    total_ms,
                }
            });
            let chunk = ChatCompletionChunk {
                id: completion_id.clone(),
                object: "chat.completion.chunk".to_string(),
                created,
                model: model_name.clone(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: Delta {
                        role: None,
                        content,
                        reasoning_content: None,
                        tool_calls: None,
                    },
                    finish_reason,
                    logprobs: None,
                }],
                system_fingerprint: Some("local-inference".to_string()),
                audio: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
                usage,
                perf,
            };

            if chunk_tx.send(chunk).await.is_err() {
                warn!("Odbiorca chunk channel rozlaczony");
                return;
            }
            if token.is_final {
                break;
            }
        }
    }
}

#[cfg(all(test, feature = "inference-llamacpp"))]
mod grammar_dump {
    use super::tool_call_grammar;
    use crate::api::openai::types::{FunctionDefinition, Tool};

    #[test]
    #[ignore = "diagnostic: prints the generated grammar"]
    fn dump() {
        let tools = vec![Tool {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "search_web".into(),
                description: None,
                parameters: Some(serde_json::json!({
                    "type":"object",
                    "properties":{"query":{"type":"string"}},
                    "required":["query"]
                })),
            },
        }];
        println!("=== GRAMATYKA ===\n{}", tool_call_grammar(&tools).unwrap());
    }
}
