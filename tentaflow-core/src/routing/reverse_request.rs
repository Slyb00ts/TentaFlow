// =============================================================================
// Plik: routing/reverse_request.rs
// Opis: Obsluga odwrotnych QUIC requestow od kontenerow. Kontenery moga
//       otwierac strumienie bi-directional na istniejacym polaczeniu, aby
//       wyslac ModelRequest do routera (np. sidecar wola STT/TTS).
// =============================================================================

use crate::net::quic::QuicClient;
use crate::routing::Router;

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
        // Pobierz aktywne polaczenie iroh (z auto-reconnect).
        let conn = match client.iroh_connection().await {
            Ok(c) => c,
            Err(e) => {
                debug!(
                    "Reverse listener '{}': brak polaczenia: {}",
                    service_name, e
                );
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
                    Err(iroh::endpoint::ConnectionError::ApplicationClosed { .. }) => {
                        info!("Reverse listener '{}': polaczenie zamkniete przez kontener", service_name);
                        break;
                    }
                    Err(iroh::endpoint::ConnectionError::ConnectionClosed { .. }) => {
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
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
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
    let request =
        match rkyv::access::<tentaflow_protocol::ArchivedModelRequest, rkyv::rancor::Error>(&data)
            .context("Blad dostepu do ArchivedModelRequest")
        {
            Ok(archived) => {
                match rkyv::deserialize::<tentaflow_protocol::ModelRequest, rkyv::rancor::Error>(
                    archived,
                ) {
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

    debug!(
        "Reverse '{}': request_id={}, payload={:?}",
        service_name,
        request.request_id,
        std::mem::discriminant(&request.payload)
    );

    // Routuj request w zaleznosci od typu payload
    let response = dispatch_reverse_request(&router, request).await;

    // Serializacja i wyslanie odpowiedzi
    match rkyv::to_bytes::<rkyv::rancor::Error>(&response) {
        Ok(resp_data) => {
            if let Err(e) = send.write_all(&resp_data).await {
                error!(
                    "Reverse '{}': blad wysylania odpowiedzi: {}",
                    service_name, e
                );
                return;
            }
            let _ = send.finish();
            debug!(
                "Reverse '{}': odpowiedz wyslana (request_id={})",
                service_name, response.request_id
            );
        }
        Err(e) => {
            error!(
                "Reverse '{}': blad serializacji odpowiedzi: {}",
                service_name, e
            );
        }
    }
}

/// Dispatchuje odwrotny request przez odpowiednia metode Routera. Dostepne
/// publicznie zeby forward handler mesh mogl uzyc tej samej sciezki.
pub async fn dispatch_reverse_request(
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
            let meeting_id: Option<String> = request.metadata.as_ref().and_then(|kv| {
                kv.iter()
                    .find(|(k, _)| k == "meeting_id")
                    .map(|(_, v)| v.clone())
            });

            // Uruchamiamy diarization *rownolegle* ze STT (nie seryjnie). Diarization
            // zjada kilkaset ms na CPU i bez tej paralelizacji dolozylaby sie wprost
            // do latencji whispera. spawn_blocking bo WeSpeaker forward jest CPU-bound.
            #[cfg(feature = "inference-diarization")]
            let diarization_handle = {
                if let (AudioOperation::STT { audio_data, .. }, Some(ref mid), Some(pool)) =
                    (&audio_payload.operation, &meeting_id, router.db.clone())
                {
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
            let (stt_result, identify_result): (_, Option<()>) = (stt_future.await, None);

            match stt_result {
                Ok(response) => {
                    // Jesli to STT (Text result), zapisz do transcript_store dla GUI Bot Status
                    if let ModelResult::Audio(ref audio_result) = response.result {
                        if let AudioResultData::Text(ref text) = audio_result.data {
                            if !text.trim().is_empty() {
                                let mut builder =
                                    crate::routing::transcript_store::TranscriptBuilder::new(
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
                                info!(
                                    "Transcript [{}][{}]: {}",
                                    display_speaker, audio_result.model, text
                                );
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
                                    finish_reason: route_result
                                        .response
                                        .choices
                                        .first()
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
                        Err(e) => {
                            make_error_response(request_id, &format!("Blad chat completion: {}", e))
                        }
                    }
                }
                Err(e) => make_error_response(request_id, &e),
            }
        }

        ModelPayload::Embeddings(ref emb_payload) => {
            match router
                .route_embeddings_via_quic(&emb_payload.model, emb_payload.input.clone())
                .await
            {
                Ok(response) => response,
                Err(e) => make_error_response(request_id, &format!("Blad embeddings: {}", e)),
            }
        }

        ModelPayload::PromptFetch(req) => {
            // Kontener (np. meeting-bot) pobiera treść promptu z DB routera —
            // jedno źródło prawdy zamiast kopiowania seed-a po stronie obrazu.
            let Some(ref pool) = router.db else {
                return make_error_response(
                    request_id,
                    "PromptFetch: router bez DB",
                );
            };
            handle_prompt_fetch(pool, request_id, req)
        }


        ModelPayload::MeetingEvent(event) => {
            // Bot meetingowy otwiera reverse stream i pcha eventy summary/action
            // items. Router resolvuje meeting_key -> session_id przez get_or_create
            // (bot moze miec inny widok sesji niz DB, np. przy restarcie routera).
            let Some(ref pool) = router.db else {
                return make_error_response(
                    request_id,
                    "MeetingEvent persist: router bez DB",
                );
            };

            match persist_meeting_event(pool, event) {
                Ok(()) => ModelResponse {
                    request_id,
                    result: ModelResult::Completion(CompletionResult {
                        text: String::new(),
                        reasoning_content: None,
                        model: String::new(),
                        finish_reason: Some("stop".to_string()),
                        tool_calls: None,
                        detected_intent: None,
                        detected_tools: None,
                        transcribed_text: None,
                        speaker_id: None,
                        speaker_name: None,
                    }),
                    metrics: None,
                },
                Err(e) => make_error_response(request_id, &e),
            }
        }

        _ => make_error_response(
            request_id,
            &format!(
                "Nieobslugiwany typ payload w reverse request: {:?}",
                std::mem::discriminant(&request.payload)
            ),
        ),
    }
}

/// Tworzy ChatCompletionRequest z CompletionPayload.
fn build_chat_request(
    payload: &tentaflow_protocol::CompletionPayload,
) -> Result<crate::api::openai::types::ChatCompletionRequest, String> {
    use crate::api::openai::types::{ChatCompletionRequest, Message, MessageContent};

    let messages: Vec<Message> = payload
        .messages
        .iter()
        .map(|m| Message {
            role: m.role.clone(),
            content: Some(MessageContent::Text(m.content.clone())),
            ..Default::default()
        })
        .collect();

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

/// Buduje `ModelResponse` dla `PromptFetch`. Wydzielone żeby test mógł uderzyć
/// bezpośrednio w DB bez budowania pełnego Routera (mesh + QUIC to ciężki setup).
fn handle_prompt_fetch(
    pool: &crate::db::DbPool,
    request_id: String,
    req: tentaflow_protocol::PromptFetchRequest,
) -> tentaflow_protocol::ModelResponse {
    use tentaflow_protocol::*;
    match crate::db::repository::find_prompt(pool, &req.prompt_id, &req.language) {
        Ok(Some(prompt)) => ModelResponse {
            request_id,
            result: ModelResult::PromptFetched(PromptFetchResponse {
                content: prompt.content,
                name: prompt.name,
                resolved_language: prompt.language,
            }),
            metrics: None,
        },
        Ok(None) => make_error_response(
            request_id,
            &format!(
                "PromptFetch: prompt '{}' nie istnieje (language={})",
                req.prompt_id, req.language
            ),
        ),
        Err(e) => make_error_response(
            request_id,
            &format!("PromptFetch: blad DB: {}", e),
        ),
    }
}

/// Persistuje pojedynczy MeetingEvent do DB. Wydzielone zeby mozna testowac
/// logike bez budowania calego Routera (Router + QUIC + mesh to ciezkie setup).
fn persist_meeting_event(
    pool: &crate::db::DbPool,
    event: tentaflow_protocol::MeetingEventData,
) -> std::result::Result<(), String> {
    use tentaflow_protocol::MeetingEventPayload;

    let session_id = crate::db::repository::transcripts::get_or_create_session(
        pool,
        &event.meeting_key,
        None,
        None,
    )
    .map_err(|e| format!("MeetingEvent: resolve session '{}' failed: {}", event.meeting_key, e))?;

    match event.payload {
        MeetingEventPayload::SummaryUpdate {
            decisions_text,
            summary_text,
            model,
        } => {
            crate::db::repository::transcripts::insert_meeting_summary(
                pool,
                session_id,
                &decisions_text,
                &summary_text,
                &model,
            )
            .map_err(|e| format!("MeetingEvent: insert_meeting_summary failed: {}", e))?;
            info!(
                "MeetingEvent SummaryUpdate: session_id={} model={} dec_len={} sum_len={}",
                session_id,
                model,
                decisions_text.len(),
                summary_text.len()
            );
        }
        MeetingEventPayload::ActionItemsUpdate { items } => {
            let count = items.len();
            for item in items {
                crate::db::repository::transcripts::upsert_meeting_action_item(
                    pool,
                    session_id,
                    &item.owner,
                    &item.task,
                    item.deadline.as_deref(),
                )
                .map_err(|e| format!("MeetingEvent: upsert_meeting_action_item failed: {}", e))?;
            }
            info!(
                "MeetingEvent ActionItemsUpdate: session_id={} items={}",
                session_id, count
            );
        }
        // TranscriptEntry nie jest persistowany tym handlerem: chunki transkryptu
        // trafiają do DB osobną ścieżką (STT ModelRequest z metadata meeting_id →
        // transcript_store). Ten wariant istnieje wyłącznie po to, żeby dashboard
        // dostał live broadcast (layer dopinany przez subskrybentów eventów;
        // persist DB pozostaje pojedynczy punkt prawdy — transcript_store).
        MeetingEventPayload::TranscriptEntry {
            speaker_id,
            text,
            latency_ms,
            resolved_stt_model,
            ..
        } => {
            info!(
                "MeetingEvent TranscriptEntry: session_id={} speaker={} model={} latency_ms={} text_len={}",
                session_id,
                speaker_id,
                resolved_stt_model,
                latency_ms,
                text.len()
            );
        }
        // ParticipantUpdate: brak tabeli participants per-session. Roster + active
        // speaker to stan runtime'owy trzymany w pamięci bota i broadcastowany
        // live. Zapis do DB nie jest potrzebny — rekonstrukcja możliwa z
        // transcript_entries (DISTINCT speaker_name).
        MeetingEventPayload::ParticipantUpdate {
            speaker_id,
            status,
            ..
        } => {
            info!(
                "MeetingEvent ParticipantUpdate: session_id={} speaker={} status={}",
                session_id, speaker_id, status
            );
        }
        // BackendUpdate: info o modelach sesji jest stanem runtime'owym (zmienia
        // się tylko przy zmianie aliasów w configu bota między sesjami). DB ma już
        // `model` w `meeting_summaries` dla historii, osobna tabela byłaby
        // duplikacją. Ten event służy tylko live broadcastowi do UI.
        MeetingEventPayload::BackendUpdate {
            stt_model,
            tts_model,
            summarization_model,
            diarization_model,
            ..
        } => {
            info!(
                "MeetingEvent BackendUpdate: session_id={} stt={} tts={} sum={} diar={}",
                session_id, stt_model, tts_model, summarization_model, diarization_model
            );
        }
    }
    Ok(())
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

    // =========================================================================
    // Persist MeetingEvent: testy logiki wydzielonej z dispatch_reverse_request.
    // Uzywamy in-memory SQLite, nie potrzeba Routera.
    // =========================================================================

    fn setup_test_db() -> crate::db::DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("init test DB")
    }

    #[test]
    fn persist_handler_summary_insert_row() {
        let db = setup_test_db();
        // Sesja musi istniec zanim wstawimy summary — get_or_create utworzy.
        let event = MeetingEventData {
            meeting_key: "m-summary-1".to_string(),
            timestamp_ms: 1_700_000_000_000,
            payload: MeetingEventPayload::SummaryUpdate {
                decisions_text: "D1".to_string(),
                summary_text: "S1".to_string(),
                model: "qwen".to_string(),
            },
        };
        persist_meeting_event(&db, event).expect("persist summary");

        // Odczyt: session_id z klucza + lista summaries.
        let sid = crate::db::repository::transcripts::get_or_create_session(
            &db,
            "m-summary-1",
            None,
            None,
        )
        .unwrap();
        let rows = crate::db::repository::transcripts::list_summaries_for_meeting(&db, sid, 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decisions_text, "D1");
        assert_eq!(rows[0].summary_text, "S1");
        assert_eq!(rows[0].model, "qwen");
    }

    #[test]
    fn persist_handler_action_items_upsert_dedup() {
        let db = setup_test_db();
        // Dwa razy ten sam owner+task — powinno byc dedup po content_hash.
        let event1 = MeetingEventData {
            meeting_key: "m-actions-1".to_string(),
            timestamp_ms: 1,
            payload: MeetingEventPayload::ActionItemsUpdate {
                items: vec![
                    MeetingActionItemData {
                        owner: "Alice".to_string(),
                        task: "prepare report".to_string(),
                        deadline: Some("2026-05-01".to_string()),
                    },
                    MeetingActionItemData {
                        owner: "Bob".to_string(),
                        task: "ship PR".to_string(),
                        deadline: None,
                    },
                ],
            },
        };
        persist_meeting_event(&db, event1).expect("persist 1");

        // Ponowny push tych samych owner+task — nie tworzy duplikatow.
        let event2 = MeetingEventData {
            meeting_key: "m-actions-1".to_string(),
            timestamp_ms: 2,
            payload: MeetingEventPayload::ActionItemsUpdate {
                items: vec![MeetingActionItemData {
                    owner: "Alice".to_string(),
                    task: "prepare report".to_string(),
                    deadline: Some("2026-05-10".to_string()),
                }],
            },
        };
        persist_meeting_event(&db, event2).expect("persist 2");

        let sid = crate::db::repository::transcripts::get_or_create_session(
            &db,
            "m-actions-1",
            None,
            None,
        )
        .unwrap();
        let rows = crate::db::repository::transcripts::list_action_items_for_meeting(&db, sid, None)
            .unwrap();
        assert_eq!(rows.len(), 2, "dwa unikalne action items (dedup drugiego Alice)");
        let alice = rows.iter().find(|r| r.owner == "Alice").unwrap();
        assert_eq!(alice.deadline.as_deref(), Some("2026-05-10"), "deadline odswiezony");
    }

    // =========================================================================
    // PromptFetch: testy handlera odczytu promptu z seedowanej DB.
    // Świeża DB po `db::init` ma już 5 wariantów `transcription_summarization`.
    // =========================================================================

    #[test]
    fn prompt_fetch_handler_returns_content_for_language() {
        let db = setup_test_db();
        let resp = handle_prompt_fetch(
            &db,
            "rid-1".to_string(),
            PromptFetchRequest {
                prompt_id: "transcription_summarization".to_string(),
                language: "en".to_string(),
            },
        );
        assert_eq!(resp.request_id, "rid-1");
        match resp.result {
            ModelResult::PromptFetched(p) => {
                assert_eq!(p.resolved_language, "en");
                assert!(!p.content.is_empty());
                assert!(!p.name.is_empty());
            }
            _ => panic!("expected PromptFetched"),
        }
    }

    #[test]
    fn prompt_fetch_handler_falls_back_to_pl_when_language_missing() {
        let db = setup_test_db();
        // `it` nie jest seedowany — `find_prompt` ma zwrocic wariant `pl`.
        let resp = handle_prompt_fetch(
            &db,
            "rid-2".to_string(),
            PromptFetchRequest {
                prompt_id: "transcription_summarization".to_string(),
                language: "it".to_string(),
            },
        );
        match resp.result {
            ModelResult::PromptFetched(p) => {
                assert_eq!(p.resolved_language, "pl", "fallback na pl gdy brak wariantu");
                assert!(!p.content.is_empty());
            }
            _ => panic!("expected PromptFetched"),
        }
    }

    #[test]
    fn prompt_fetch_handler_returns_error_for_unknown_prompt() {
        let db = setup_test_db();
        let resp = handle_prompt_fetch(
            &db,
            "rid-3".to_string(),
            PromptFetchRequest {
                prompt_id: "does_not_exist".to_string(),
                language: "pl".to_string(),
            },
        );
        match resp.result {
            ModelResult::Error(info) => {
                assert!(info.message.contains("does_not_exist"));
                assert!(matches!(info.error_type, ErrorType::InternalError));
            }
            _ => panic!("expected Error response for unknown prompt"),
        }
    }

    #[test]
    fn persist_handler_unknown_meeting_key_creates_session() {
        let db = setup_test_db();
        // Klucz ktorego nie ma w DB — handler ma utworzyc nowa sesje idle.
        let event = MeetingEventData {
            meeting_key: "m-new-key".to_string(),
            timestamp_ms: 42,
            payload: MeetingEventPayload::SummaryUpdate {
                decisions_text: "d".to_string(),
                summary_text: "s".to_string(),
                model: "m".to_string(),
            },
        };
        persist_meeting_event(&db, event).expect("persist should create session");

        // Sesja powinna istniec w meeting_sessions po call.
        let conn = db.lock().unwrap();
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM meeting_sessions WHERE meeting_key = ?1",
                rusqlite::params!["m-new-key"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 1);
    }

    // Handler musi akceptować TranscriptEntry bez błędu i bez wpisów do
    // meeting_summaries / meeting_action_items — persist chunków leci przez
    // transcript_store, nie przez ten handler.
    #[test]
    fn persist_handler_transcript_entry_is_noop_but_creates_session() {
        let db = setup_test_db();
        let event = MeetingEventData {
            meeting_key: "m-te-1".to_string(),
            timestamp_ms: 100,
            payload: MeetingEventPayload::TranscriptEntry {
                speaker_id: "SPEAKER_00".to_string(),
                speaker_name: Some("Alice".to_string()),
                is_enrolled: false,
                speaker_confidence: Some(0.5),
                text: "test".to_string(),
                language: Some("pl".to_string()),
                resolved_stt_model: "whisper".to_string(),
                latency_ms: 250,
            },
        };
        persist_meeting_event(&db, event).expect("persist transcript entry");

        let sid = crate::db::repository::transcripts::get_or_create_session(
            &db, "m-te-1", None, None,
        )
        .unwrap();
        let summaries =
            crate::db::repository::transcripts::list_summaries_for_meeting(&db, sid, 10).unwrap();
        let actions =
            crate::db::repository::transcripts::list_action_items_for_meeting(&db, sid, None)
                .unwrap();
        assert!(summaries.is_empty(), "TranscriptEntry nie wpisuje summary");
        assert!(actions.is_empty(), "TranscriptEntry nie wpisuje action items");
    }

    // ParticipantUpdate: handler nie persistuje nigdzie — sprawdzamy że nie
    // zwraca błędu i nie tworzy rekordów w tabelach zapisywanych.
    #[test]
    fn persist_handler_participant_update_is_noop() {
        let db = setup_test_db();
        let event = MeetingEventData {
            meeting_key: "m-pu-1".to_string(),
            timestamp_ms: 100,
            payload: MeetingEventPayload::ParticipantUpdate {
                speaker_id: "SPEAKER_02".to_string(),
                speaker_name: Some("Bob".to_string()),
                status: "active_now".to_string(),
                last_spoken_ago_sec: None,
            },
        };
        persist_meeting_event(&db, event).expect("persist participant update");

        let sid = crate::db::repository::transcripts::get_or_create_session(
            &db, "m-pu-1", None, None,
        )
        .unwrap();
        let summaries =
            crate::db::repository::transcripts::list_summaries_for_meeting(&db, sid, 10).unwrap();
        let actions =
            crate::db::repository::transcripts::list_action_items_for_meeting(&db, sid, None)
                .unwrap();
        assert!(summaries.is_empty());
        assert!(actions.is_empty());
    }

    // BackendUpdate: to samo — stan runtime'owy, nic nie wpada do DB.
    #[test]
    fn persist_handler_backend_update_is_noop() {
        let db = setup_test_db();
        let event = MeetingEventData {
            meeting_key: "m-bu-1".to_string(),
            timestamp_ms: 100,
            payload: MeetingEventPayload::BackendUpdate {
                stt_model: "teams-stt".to_string(),
                tts_model: "teams-tts".to_string(),
                summarization_model: "teams-summarization".to_string(),
                diarization_model: "pyannote-3.1".to_string(),
                streaming_latency_ms: None,
                enrolled_speakers: None,
                total_participants: None,
            },
        };
        persist_meeting_event(&db, event).expect("persist backend update");

        let sid = crate::db::repository::transcripts::get_or_create_session(
            &db, "m-bu-1", None, None,
        )
        .unwrap();
        let summaries =
            crate::db::repository::transcripts::list_summaries_for_meeting(&db, sid, 10).unwrap();
        assert!(summaries.is_empty());
    }
}
