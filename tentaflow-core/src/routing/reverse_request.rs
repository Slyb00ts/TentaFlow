// =============================================================================
// Plik: routing/reverse_request.rs
// Opis: Obsluga odwrotnych QUIC requestow od kontenerow. Kontenery moga
//       otwierac strumienie bi-directional na istniejacym polaczeniu, aby
//       wyslac ModelRequest do routera (np. sidecar wola STT/TTS).
// =============================================================================

use crate::routing::Router;
use crate::net::quic::QuicClient;

use anyhow::Context;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

/// Maksymalny rozmiar odwrotnego requestu od kontenera (10 MB)
const MAX_REVERSE_REQUEST_SIZE: usize = 10_000_000;

/// Uruchamia petle accept_bi na polaczeniu QUIC do kontenera.
/// Kazdy przychodzacy strumien to ModelRequest od kontenera, ktory
/// zostaje skierowany przez Router i odpowiedz wraca tym samym strumieniem.
pub(crate) fn spawn_reverse_listener(
    client: Arc<QuicClient>,
    router: Router,
    service_name: String,
    shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        reverse_listener_loop(client, router, service_name, shutdown_rx).await;
    })
}

/// Glowna petla nasluchujaca na odwrotne requesty od kontenera.
async fn reverse_listener_loop(
    client: Arc<QuicClient>,
    router: Router,
    service_name: String,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    info!("Reverse listener '{}': uruchomiony", service_name);

    loop {
        // Pobierz connection z klienta
        let conn_arc = client.connection();
        let conn_guard = conn_arc.lock().await;
        let conn = match conn_guard.as_ref() {
            Some(c) => c.clone(),
            None => {
                drop(conn_guard);
                // Polaczenie jeszcze nie gotowe lub utracone — czekaj
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                        continue;
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Reverse listener '{}': shutdown", service_name);
                            return;
                        }
                    }
                }
                continue;
            }
        };
        drop(conn_guard);

        tokio::select! {
            result = conn.accept_bi() => {
                match result {
                    Ok((send, recv)) => {
                        let router_clone = router.clone();
                        let name_clone = service_name.clone();
                        tokio::spawn(async move {
                            handle_reverse_stream(send, recv, router_clone, name_clone).await;
                        });
                    }
                    Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                        info!("Reverse listener '{}': polaczenie zamkniete przez kontener", service_name);
                        break;
                    }
                    Err(quinn::ConnectionError::ConnectionClosed { .. }) => {
                        info!("Reverse listener '{}': polaczenie zamkniete", service_name);
                        break;
                    }
                    Err(e) => {
                        warn!("Reverse listener '{}': blad accept_bi: {}", service_name, e);
                        break;
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Reverse listener '{}': shutdown", service_name);
                    return;
                }
            }
        }
    }

    info!("Reverse listener '{}': zakonczony", service_name);
}

/// Obsluguje pojedynczy odwrotny strumien od kontenera.
/// Czyta ModelRequest, routuje przez Router, odsyla ModelResponse.
async fn handle_reverse_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    router: Router,
    service_name: String,
) {
    // Odczytaj ModelRequest
    let data = match recv.read_to_end(MAX_REVERSE_REQUEST_SIZE).await {
        Ok(d) => d,
        Err(e) => {
            error!("Reverse '{}': blad odczytu requestu: {}", service_name, e);
            return;
        }
    };

    // Deserializacja rkyv
    let request = match rkyv::access::<tentaflow_protocol::ArchivedModelRequest, rkyv::rancor::Error>(&data)
        .context("Blad dostepu do ArchivedModelRequest")
    {
        Ok(archived) => {
            match rkyv::deserialize::<tentaflow_protocol::ModelRequest, rkyv::rancor::Error>(archived) {
                Ok(req) => req,
                Err(e) => {
                    error!("Reverse '{}': blad deserializacji: {}", service_name, e);
                    return;
                }
            }
        }
        Err(e) => {
            error!("Reverse '{}': blad dostepu rkyv: {}", service_name, e);
            return;
        }
    };

    debug!("Reverse '{}': request_id={}, payload={:?}",
        service_name, request.request_id,
        std::mem::discriminant(&request.payload));

    // Routuj request w zaleznosci od typu payload
    let response = dispatch_reverse_request(&router, request).await;

    // Serializacja i wyslanie odpowiedzi
    match rkyv::to_bytes::<rkyv::rancor::Error>(&response) {
        Ok(resp_data) => {
            if let Err(e) = send.write_all(&resp_data).await {
                error!("Reverse '{}': blad wysylania odpowiedzi: {}", service_name, e);
                return;
            }
            let _ = send.finish();
            debug!("Reverse '{}': odpowiedz wyslana (request_id={})",
                service_name, response.request_id);
        }
        Err(e) => {
            error!("Reverse '{}': blad serializacji odpowiedzi: {}", service_name, e);
        }
    }
}

/// Dispatchuje odwrotny request przez odpowiednia metode Routera.
async fn dispatch_reverse_request(
    router: &Router,
    request: tentaflow_protocol::ModelRequest,
) -> tentaflow_protocol::ModelResponse {
    use tentaflow_protocol::*;

    let request_id = request.request_id.clone();

    match request.payload {
        ModelPayload::Audio(audio_payload) => {
            // Meeting context — bot dopisuje "meeting_id" do ModelRequest.metadata
            // przy kazdym STT requescie. Router uzywa go jako klucza do
            // voice_temp_speakers i transcript_store.
            let meeting_id: Option<String> = request
                .metadata
                .as_ref()
                .and_then(|kv| {
                    kv.iter()
                        .find(|(k, _)| k == "meeting_id")
                        .map(|(_, v)| v.clone())
                });

            // Uruchamiamy diarization *rownolegle* ze STT (nie seryjnie). Diarization
            // zjada kilkaset ms na CPU i bez tej paralelizacji dolozylaby sie wprost
            // do latencji whispera. spawn_blocking bo WeSpeaker forward jest CPU-bound.
            #[cfg(feature = "inference-diarization")]
            let diarization_handle = {
                if let (AudioOperation::STT { audio_data, .. }, Some(ref mid), Some(pool)) = (
                    &audio_payload.operation,
                    &meeting_id,
                    router.db.clone(),
                ) {
                    let audio_clone = audio_data.clone();
                    let mid_clone = mid.clone();
                    Some(tokio::task::spawn_blocking(move || {
                        crate::diarization::identify_speaker_with_profiles(
                            &pool,
                            &audio_clone,
                            &mid_clone,
                        )
                    }))
                } else {
                    None
                }
            };

            let stt_future = router.route_audio_via_protocol(&audio_payload.operation);

            #[cfg(feature = "inference-diarization")]
            let (stt_result, identify_result) = {
                let stt_res = stt_future.await;
                let ident = match diarization_handle {
                    Some(h) => h.await.ok().flatten(),
                    None => None,
                };
                (stt_res, ident)
            };
            #[cfg(not(feature = "inference-diarization"))]
            let (stt_result, identify_result): (
                _,
                Option<crate::diarization::service::IdentifyResult>,
            ) = (stt_future.await, None);

            match stt_result {
                Ok(response) => {
                    // Jesli to STT (Text result), zapisz do transcript_store dla GUI Bot Status
                    if let ModelResult::Audio(ref audio_result) = response.result {
                        if let AudioResultData::Text(ref text) = audio_result.data {
                            if !text.trim().is_empty() {
                                let mut builder = crate::routing::transcript_store::TranscriptBuilder::new(
                                    text.clone(),
                                    audio_result.model.clone(),
                                );
                                if let Some(ref mid) = meeting_id {
                                    builder = builder.meeting_id(mid.clone());
                                }
                                #[cfg(feature = "inference-diarization")]
                                {
                                    if let Some(ref ident) = identify_result {
                                        builder = builder.speaker(ident.label.clone());
                                        if let Some(pid) = ident.profile_id {
                                            builder = builder.profile_id(pid);
                                        }
                                        if let Some(c) = ident.confidence {
                                            builder = builder.confidence(c);
                                        }
                                    }
                                }
                                let display_speaker = builder.speaker.clone();
                                crate::routing::transcript_store::push(builder);
                                info!("Transcript [{}][{}]: {}", display_speaker, audio_result.model, text);
                            }
                        }
                    }
                    response
                }
                Err(e) => make_error_response(request_id, &format!("Blad routingu audio: {}", e)),
            }
        }

        ModelPayload::Completion(ref completion_payload) => {
            match build_chat_request(completion_payload) {
                Ok(chat_request) => {
                    match router.route_chat_completion(chat_request).await {
                        Ok(route_result) => {
                            let text = route_result.response.choices.first()
                                .and_then(|c| c.message.content.as_ref())
                                .map(|c| match c {
                                    crate::api::openai::types::MessageContent::Text(t) => t.clone(),
                                    crate::api::openai::types::MessageContent::Parts(parts) => {
                                        parts.iter().filter_map(|p| {
                                            if let crate::api::openai::types::ContentPart::Text { text } = p {
                                                Some(text.as_str())
                                            } else {
                                                None
                                            }
                                        }).collect::<Vec<_>>().join("")
                                    }
                                })
                                .unwrap_or_default();

                            ModelResponse {
                                request_id,
                                result: ModelResult::Completion(CompletionResult {
                                    text,
                                    reasoning_content: None,
                                    model: route_result.response.model,
                                    finish_reason: route_result.response.choices.first()
                                        .and_then(|c| c.finish_reason.clone()),
                                    tool_calls: None,
                                    detected_intent: None,
                                    detected_tools: None,
                                    transcribed_text: None,
                                    speaker_id: None,
                                    speaker_name: None,
                                }),
                                metrics: None,
                            }
                        }
                        Err(e) => make_error_response(request_id, &format!("Blad chat completion: {}", e)),
                    }
                }
                Err(e) => make_error_response(request_id, &e),
            }
        }

        ModelPayload::Embeddings(ref emb_payload) => {
            match router.route_embeddings_via_quic(&emb_payload.model, emb_payload.input.clone()).await {
                Ok(response) => response,
                Err(e) => make_error_response(request_id, &format!("Blad embeddings: {}", e)),
            }
        }

        _ => {
            make_error_response(request_id, &format!(
                "Nieobslugiwany typ payload w reverse request: {:?}",
                std::mem::discriminant(&request.payload)
            ))
        }
    }
}

/// Tworzy ChatCompletionRequest z CompletionPayload.
fn build_chat_request(
    payload: &tentaflow_protocol::CompletionPayload,
) -> Result<crate::api::openai::types::ChatCompletionRequest, String> {
    use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};

    let messages: Vec<Message> = payload.messages.iter().map(|m| {
        Message {
            role: m.role.clone(),
            content: Some(MessageContent::Text(m.content.clone())),
            ..Default::default()
        }
    }).collect();

    if messages.is_empty() {
        return Err("Brak wiadomosci w CompletionPayload".to_string());
    }

    Ok(ChatCompletionRequest {
        model: payload.model.clone(),
        messages,
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
        stream: false,
        top_p: payload.top_p,
        frequency_penalty: None,
        presence_penalty: payload.presence_penalty,
        stop: payload.stop.clone(),
        user: None,
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        rag_options: None,
        memory_options: None,
        audio_input: None,
    })
}

/// Tworzy ModelResponse z bledem.
fn make_error_response(request_id: String, message: &str) -> tentaflow_protocol::ModelResponse {
    use tentaflow_protocol::*;
    error!("Reverse request error: {}", message);
    ModelResponse {
        request_id,
        result: ModelResult::Error(ErrorInfo {
            error_type: ErrorType::InternalError,
            message: message.to_string(),
            details: None,
        }),
        metrics: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::*;

    #[test]
    fn build_chat_request_from_completion_payload() {
        // Poprawne budowanie ChatCompletionRequest z CompletionPayload
        let payload = CompletionPayload {
            model: "gpt-4".to_string(),
            prompt: None,
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: "Jestes asystentem.".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: "Czesc!".to_string(),
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(1024),
            top_p: Some(0.9),
            stop: Some(vec!["STOP".to_string()]),
            presence_penalty: Some(0.5),
            frequency_penalty: None,
            tts_options: None,
            memory_options: None,
            audio_input: None,
            prefix_cache_id: None,
            prefix_text: None,
        };

        let result = build_chat_request(&payload);
        assert!(result.is_ok());

        let req = result.unwrap();
        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(req.top_p, Some(0.9));
        assert_eq!(req.presence_penalty, Some(0.5));
        assert!(!req.stream);
    }

    #[test]
    fn build_chat_request_empty_messages_returns_error() {
        // Brak wiadomosci — powinno zwrocic blad
        let payload = CompletionPayload {
            model: "gpt-4".to_string(),
            prompt: None,
            messages: vec![],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tts_options: None,
            memory_options: None,
            audio_input: None,
            prefix_cache_id: None,
            prefix_text: None,
        };

        let result = build_chat_request(&payload);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Brak wiadomosci"));
    }

    #[test]
    fn make_error_response_contains_message() {
        // Sprawdzenie ze error response zawiera podany komunikat
        let resp = make_error_response("req-42".to_string(), "Blad testowy");
        assert_eq!(resp.request_id, "req-42");
        match resp.result {
            ModelResult::Error(info) => {
                assert_eq!(info.message, "Blad testowy");
                assert!(matches!(info.error_type, ErrorType::InternalError));
                assert!(info.details.is_none());
            }
            _ => panic!("Oczekiwano ModelResult::Error"),
        }
        assert!(resp.metrics.is_none());
    }

    #[test]
    fn build_chat_request_single_message() {
        // Jedna wiadomosc — minimalna poprawna konfiguracja
        let payload = CompletionPayload {
            model: "meeting-bot".to_string(),
            prompt: None,
            messages: vec![Message {
                role: "user".to_string(),
                content: "Podsumuj spotkanie".to_string(),
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tts_options: None,
            memory_options: None,
            audio_input: None,
            prefix_cache_id: None,
            prefix_text: None,
        };

        let result = build_chat_request(&payload);
        assert!(result.is_ok());

        let req = result.unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.model, "meeting-bot");
        assert!(req.temperature.is_none());
        assert!(req.max_tokens.is_none());
    }

    #[test]
    fn build_chat_request_message_content_is_text() {
        // Sprawdzenie ze content wiadomosci jest poprawnie opakowany w MessageContent::Text
        let payload = CompletionPayload {
            model: "test".to_string(),
            prompt: None,
            messages: vec![Message {
                role: "user".to_string(),
                content: "Tresc wiadomosci".to_string(),
            }],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tts_options: None,
            memory_options: None,
            audio_input: None,
            prefix_cache_id: None,
            prefix_text: None,
        };

        let req = build_chat_request(&payload).unwrap();
        match req.messages[0].content.as_ref().unwrap() {
            crate::api::openai::types::MessageContent::Text(t) => {
                assert_eq!(t, "Tresc wiadomosci");
            }
            _ => panic!("Oczekiwano MessageContent::Text"),
        }
    }
}
