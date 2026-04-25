// =============================================================================
// Plik: routing/local_inference.rs
// Opis: Adapter konwertujacy OpenAI-compatible requesty na lokalne wywolania
//       InferenceEngine (llama.cpp / MLX). Obsluguje chat completions,
//       streaming SSE i embeddingi.
// =============================================================================

use crate::api::openai::types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, Choice, ChunkChoice, Delta,
    EmbeddingData, EmbeddingInput, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage, Message,
    MessageContent, Usage,
};
use crate::inference::{
    EmbeddingParams, GenerateParams, GenerateResult, InferenceManager, StopReason, StreamToken,
};
use crate::routing::chat_template::{ChatMessage, ChatTemplate};

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};
use uuid::Uuid;

/// Adapter routujacy OpenAI-compatible requesty do lokalnego silnika inferencji.
pub struct LocalInferenceHandler {
    inference_manager: Arc<RwLock<InferenceManager>>,
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
    ) -> anyhow::Result<ChatCompletionResponse> {
        let template = self.get_chat_template().await;
        let params = Self::request_to_generate_params(request, &template);
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
    ) -> anyhow::Result<mpsc::Receiver<ChatCompletionChunk>> {
        let template = self.get_chat_template().await;
        let params = Self::request_to_generate_params(request, &template);
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

        let prompt_tokens = texts
            .iter()
            .map(|t| t.split_whitespace().count() as u32)
            .sum::<u32>();

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
        let manager = self.inference_manager.read().await;
        manager
            .active_engine()
            .and_then(|e| e.model_info())
            .and_then(|info| info.chat_template)
            .map(|name| match name.as_str() {
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

        // Sformatuj prompt wedlug szablonu chatu
        let prompt = template.format_messages(&chat_messages, true);

        // Dodaj stop sequences z szablonu
        let mut stop_sequences = request.stop.clone().unwrap_or_default();
        stop_sequences.extend(template.stop_tokens());

        let defaults = GenerateParams::default();

        debug!(
            "Sformatowano prompt szablonem {:?}: {} znakow, {} stop sequences",
            template.name(),
            prompt.len(),
            stop_sequences.len(),
        );

        GenerateParams {
            prompt,
            max_tokens: request.max_tokens.unwrap_or(defaults.max_tokens),
            temperature: request.temperature.unwrap_or(defaults.temperature),
            top_p: request.top_p.unwrap_or(defaults.top_p),
            top_k: defaults.top_k,
            repeat_penalty: request.frequency_penalty.unwrap_or(defaults.repeat_penalty),
            stop_sequences,
            system_prompt: None, // system prompt jest juz wbudowany w sformatowany prompt
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
    /// konsumuje i przekazuje do rkyv encoded WS frames.
    async fn stream_tokens_to_chunks(
        mut token_rx: mpsc::Receiver<StreamToken>,
        chunk_tx: mpsc::Sender<ChatCompletionChunk>,
        completion_id: String,
        model_name: String,
        created: u64,
    ) {
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
        };
        if chunk_tx.send(first).await.is_err() {
            return;
        }

        while let Some(token) = token_rx.recv().await {
            let finish_reason = if token.is_final {
                Some("stop".to_string())
            } else {
                None
            };
            let content = if token.text.is_empty() && token.is_final {
                None
            } else {
                Some(token.text)
            };
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
