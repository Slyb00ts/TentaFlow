// =============================================================================
// Plik: routing/reverse_request.rs
// Opis: Obsluga odwrotnych QUIC requestow od kontenerow. Kontenery moga
//       otwierac strumienie bi-directional na istniejacym polaczeniu, aby
//       wyslac ModelRequest do routera (np. sidecar wola STT/TTS).
// =============================================================================

use crate::net::quic::QuicClient;
use crate::routing::Router;

use anyhow::Context;
use dashmap::DashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

/// Maksymalny rozmiar odwrotnego requestu od kontenera (10 MB)
const MAX_REVERSE_REQUEST_SIZE: usize = 10_000_000;

/// Cache `meeting_key -> session_id` współdzielony przez wszystkie wywołania
/// `persist_meeting_event`. Każdy MeetingEvent (TranscriptEntry, RosterSnapshot,
/// BackendUpdate, …) trafia do reverse handlera setki razy w trakcie spotkania —
/// `get_or_create_session` to synchroniczny rusqlite call (~5–30 ms). Cache redukuje
/// to do ~50 ns DashMap hit po pierwszym uderzeniu w danej sesji.
///
/// Wpisy są ważne do końca procesu (sesje nie są usuwane w trakcie życia routera).
/// Gdy admin usunie meeting w GUI, wywołanie `invalidate_meeting_session` musi
/// wyczyścić wpis — inaczej kolejny event z tym kluczem trafiłby na zerwany
/// foreign-key. Obecnie żadna ścieżka produkcyjna nie kasuje sesji, więc helper
/// czeka na podpięcie przy delete-meeting endpoint.
fn meeting_session_cache() -> &'static DashMap<String, i64> {
    static CACHE: OnceLock<DashMap<String, i64>> = OnceLock::new();
    CACHE.get_or_init(DashMap::new)
}

/// Czyści wpis cache `meeting_key -> session_id`. Wołane przy usunięciu sesji
/// z DB (delete meeting endpoint). Jeśli nic nie ma w cache — no-op.
pub fn invalidate_meeting_session(meeting_key: &str) {
    meeting_session_cache().remove(meeting_key);
}

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

    // Streamujaca sciezka TTS: bot ustawia stream=true + payload Audio(TTS),
    // router odpowiada seria ModelStreamChunk z AudioChunk(pcm) zakonczona
    // Done. Pozwala botowi pchac probki do mikrofonu na biezaco zamiast
    // czekac na pelny WAV.
    //
    // Streamujaca sciezka Completion: stream=true + payload Completion zwraca
    // serie TextDelta zakonczona Done. Bot uzywa tego do tokeno-strumienia
    // odpowiedzi LLM, parsuje granice zdan i odpala TTS na kazde pelne zdanie
    // (sentence-boundary pipeline) — pierwszy chunk audio idzie do mikrofonu
    // dlugo zanim LLM skonczy generowac.
    if request.stream {
        if let tentaflow_protocol::ModelPayload::Audio(ref audio_payload) = request.payload {
            if let tentaflow_protocol::AudioOperation::TTS { .. } = audio_payload.operation {
                handle_tts_stream(send, router, request).await;
                return;
            }
        }
        if let tentaflow_protocol::ModelPayload::Completion(_) = request.payload {
            handle_completion_stream(send, router, request).await;
            return;
        }
    }

    // Routuj request w zaleznosci od typu payload
    let response = dispatch_reverse_request(&router, request).await;

    // Bot side reads the response with tentaflow_transport::framing::read_frame,
    // which expects a 4-byte big-endian length prefix before the rkyv payload.
    // We were writing raw rkyv bytes here, so the bot interpreted the first
    // four bytes of the payload as the length and rejected every response as
    // `frame exceeds 16777216 bytes`. Emit the same [len][rkyv] framing the
    // bot produces on the request side.
    if let Err(e) = tentaflow_transport::framing::write_frame(&mut send, &response).await {
        error!(
            "Reverse '{}': blad wysylania odpowiedzi: {:?}",
            service_name, e
        );
    } else {
        let _ = send.finish();
        debug!(
            "Reverse '{}': odpowiedz wyslana (request_id={})",
            service_name, response.request_id
        );
    }
}

/// Obsluguje streamujaca synteze mowy. Wysyla seria ModelStreamChunk:
///   1) AudioChunk(pcm_bytes) — N razy, kazdy chunk to ~100 ms PCM
///   2) Done { final_metrics: None } — terminator
/// W razie bledu syntezy wysyla Error chunk + Done, zeby klient nie
/// zawisl czekajac na ramki ktorych nie bedzie.
async fn handle_tts_stream(
    mut send: iroh::endpoint::SendStream,
    router: Router,
    request: tentaflow_protocol::ModelRequest,
) {
    use tentaflow_protocol::{
        AudioOperation, ErrorInfo, ErrorType, ModelPayload, ModelStreamChunk, StreamChunkType,
    };

    let request_id = request.request_id.clone();

    // Wyluskaj parametry TTS bez clone calego payloadu
    let (model, input, voice, format, speed, language) = match request.payload {
        ModelPayload::Audio(p) => match p.operation {
            AudioOperation::TTS {
                model,
                input,
                voice,
                format,
                speed,
                language,
            } => (model, input, voice, format, speed, language),
            _ => return,
        },
        _ => return,
    };

    let tts_request = crate::api::openai::types::TTSRequest {
        model,
        input,
        voice,
        // Wymuszamy "pcm" — chunki maja byc raw PCM bez WAV headera. Jesli
        // klient prosil o wav/mp3, i tak strippujemy w synthesize_speech_stream,
        // ale "pcm" zaoszczedzi pracy backendowi.
        response_format: Some("pcm".to_string()),
        speed,
        language,
    };
    let _ = format; // honorujemy "pcm" niezaleznie od pola w request

    // Buforujemy chunki w mpsc, zeby synthesize_speech_stream (synchroniczny
    // closure) nie musial trzymac referencji do `send`. Backend produkuje
    // chunki na mpsc, ten task czyta i wysyla po sieci.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let synth_task = tokio::spawn(async move {
        router
            .synthesize_speech_stream(&tts_request, |chunk_bytes| {
                tx.send(chunk_bytes).map_err(|e| {
                    crate::error::CoreError::InternalError {
                        message: format!("TTS chunk_sink kanal zamkniety: {}", e),
                        source: None,
                    }
                    .into()
                })
            })
            .await
    });

    // Konsument: kazdy chunk z mpsc -> wysylka jako AudioChunk frame.
    let mut client_dropped = false;
    while let Some(pcm) = rx.recv().await {
        let chunk = ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::AudioChunk(pcm),
        };
        if let Err(e) = tentaflow_transport::framing::write_frame(&mut send, &chunk).await {
            warn!("TTS stream: blad wysylki AudioChunk: {:?}", e);
            client_dropped = true;
            break;
        }
    }

    // Zaczekaj na wynik syntezy zeby wyslac koncowy chunk (Done lub Error).
    let final_chunk = match synth_task.await {
        Ok(Ok(())) => ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::Done {
                final_metrics: None,
            },
        },
        Ok(Err(e)) => ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::Error(ErrorInfo {
                error_type: ErrorType::InternalError,
                message: format!("Blad TTS streaming: {}", e),
                details: None,
            }),
        },
        Err(join_err) => ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::Error(ErrorInfo {
                error_type: ErrorType::InternalError,
                message: format!("TTS task panika: {}", join_err),
                details: None,
            }),
        },
    };
    if !client_dropped {
        if let Err(e) = tentaflow_transport::framing::write_frame(&mut send, &final_chunk).await {
            warn!("TTS stream: blad wysylki final chunk: {:?}", e);
        }
    }
    let _ = send.finish();
}

/// Obsluguje streamujace chat completion od kontenera (bot meetingowy):
/// kazdy delta-token z `route_chat_completion_stream` przepakowywany jest
/// w `ModelStreamChunk::TextDelta`. Po wyczerpaniu strumienia wysylamy
/// `Done`. Brak buforowania — token leci do bota natychmiast po wyprodukowaniu
/// przez backend, dzieki czemu sentence-boundary parser bota odpala TTS dla
/// pierwszego zdania zanim LLM dokonczy generowanie reszty odpowiedzi.
async fn handle_completion_stream(
    mut send: iroh::endpoint::SendStream,
    router: Router,
    request: tentaflow_protocol::ModelRequest,
) {
    use futures::StreamExt;
    use tentaflow_protocol::{ErrorInfo, ErrorType, ModelStreamChunk, StreamChunkType};

    let request_id = request.request_id.clone();

    let completion_payload = match &request.payload {
        tentaflow_protocol::ModelPayload::Completion(p) => p,
        _ => return,
    };

    let mut chat_request = match build_chat_request(completion_payload) {
        Ok(mut r) => {
            r.stream = true;
            r
        }
        Err(e) => {
            let err_chunk = ModelStreamChunk {
                request_id: request_id.clone(),
                chunk: StreamChunkType::Error(ErrorInfo {
                    error_type: ErrorType::InvalidRequest,
                    message: e,
                    details: None,
                }),
            };
            let _ = tentaflow_transport::framing::write_frame(&mut send, &err_chunk).await;
            let _ = send.finish();
            return;
        }
    };
    // upewniamy sie ze stream flag jest ustawiona — niektore sciezki
    // route_chat_completion_stream gateuja tryb streaming wlasnie tym polem.
    chat_request.stream = true;

    let route_result = match router.route_chat_completion_stream(chat_request, None).await {
        Ok(r) => r,
        Err(e) => {
            let err_chunk = ModelStreamChunk {
                request_id: request_id.clone(),
                chunk: StreamChunkType::Error(ErrorInfo {
                    error_type: ErrorType::InternalError,
                    message: format!("route_chat_completion_stream: {}", e),
                    details: None,
                }),
            };
            let _ = tentaflow_transport::framing::write_frame(&mut send, &err_chunk).await;
            let _ = send.finish();
            return;
        }
    };

    let mut stream = route_result.response;
    let mut errored = false;
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chat_chunk) => {
                // OpenAI-format chunk -> wyciagamy delta.content; pomijamy
                // chunki bez tekstu (np. role-only first chunk).
                if let Some(choice) = chat_chunk.choices.into_iter().next() {
                    if let Some(text) = choice.delta.content {
                        if !text.is_empty() {
                            let frame = ModelStreamChunk {
                                request_id: request_id.clone(),
                                chunk: StreamChunkType::TextDelta(text),
                            };
                            if let Err(e) =
                                tentaflow_transport::framing::write_frame(&mut send, &frame).await
                            {
                                warn!("Completion stream: blad wysylki TextDelta: {:?}", e);
                                errored = true;
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let err_chunk = ModelStreamChunk {
                    request_id: request_id.clone(),
                    chunk: StreamChunkType::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: format!("Completion stream blad: {}", e),
                        details: None,
                    }),
                };
                let _ = tentaflow_transport::framing::write_frame(&mut send, &err_chunk).await;
                errored = true;
                break;
            }
        }
    }

    if !errored {
        let done = ModelStreamChunk {
            request_id: request_id.clone(),
            chunk: StreamChunkType::Done {
                final_metrics: None,
            },
        };
        if let Err(e) = tentaflow_transport::framing::write_frame(&mut send, &done).await {
            warn!("Completion stream: blad wysylki Done: {:?}", e);
        }
    }
    let _ = send.finish();
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
                    // audio_data jest Vec<u8> z deserializacji rkyv. Tu jest jedyny
                    // klon do diarization — fork odpala sie rownolegle ze STT
                    // (oba widza ten sam buffer; spawn_blocking przejmuje wlasnosc).
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
                    match router.route_chat_completion(chat_request, None).await {
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
                return make_error_response(request_id, "PromptFetch: router bez DB");
            };
            handle_prompt_fetch(pool, request_id, req)
        }

        ModelPayload::MeetingEvent(event) => {
            // Bot meetingowy otwiera reverse stream i pcha eventy summary/action
            // items. Router resolvuje meeting_key -> session_id przez get_or_create
            // (bot moze miec inny widok sesji niz DB, np. przy restarcie routera).
            let Some(ref pool) = router.db else {
                return make_error_response(request_id, "MeetingEvent persist: router bez DB");
            };

            // Zachowujemy kopie do live broadcastu przed move do persist.
            // Persist moze nie zapisywac danego wariantu do DB (TranscriptEntry,
            // RosterSnapshot, BackendUpdate tylko logują), ale broadcastujemy
            // WSZYSTKIE — GUI potrzebuje pełnego stream'u do live view.
            let live_event = tentaflow_protocol::MeetingLiveEvent {
                meeting_key: event.meeting_key.clone(),
                timestamp_ms: event.timestamp_ms,
                payload: event.payload.clone(),
            };
            // VideoFrame wyzwala vision pipeline (face → emotion + age/gender)
            // — wynik leci jako osobny event `ParticipantAttributes` na ten
            // sam broadcast bus. Pipeline ma własny throttle 1 inf/2s per
            // uczestnik, więc bezpiecznie wołamy go na każdy frame.
            if let tentaflow_protocol::MeetingEventPayload::VideoFrame {
                participant_id,
                name,
                ts_ms,
                jpeg,
            } = &live_event.payload
            {
                crate::routing::video_pipeline::maybe_spawn_inference(
                    pool.clone(),
                    live_event.meeting_key.clone(),
                    live_event.timestamp_ms,
                    participant_id.clone(),
                    name.clone(),
                    *ts_ms,
                    jpeg.clone(),
                );
            }
            match persist_meeting_event(pool, event) {
                Ok(()) => {
                    crate::dispatch::meeting_live_broadcast::publish(live_event);
                    ModelResponse {
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
                    }
                }
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

pub async fn dispatch_reverse_stream_request(
    router: &Router,
    request: tentaflow_protocol::ModelRequest,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) {
    use futures::StreamExt;
    use tentaflow_protocol::{ErrorInfo, ErrorType, ModelPayload, ModelStreamChunk, StreamChunkType};

    let request_id = request.request_id.clone();
    let completion_payload = match &request.payload {
        ModelPayload::Completion(p) => p,
        _ => {
            send_stream_chunk_bytes(
                &tx,
                ModelStreamChunk {
                    request_id,
                    chunk: StreamChunkType::Error(ErrorInfo {
                        error_type: ErrorType::InvalidRequest,
                        message: "stream forward supports completion payloads".to_string(),
                        details: None,
                    }),
                },
            );
            return;
        }
    };

    let mut chat_request = match build_chat_request(completion_payload) {
        Ok(req) => req,
        Err(e) => {
            send_stream_chunk_bytes(
                &tx,
                ModelStreamChunk {
                    request_id,
                    chunk: StreamChunkType::Error(ErrorInfo {
                        error_type: ErrorType::InvalidRequest,
                        message: e,
                        details: None,
                    }),
                },
            );
            return;
        }
    };
    chat_request.stream = true;

    let route_result = match router.route_chat_completion_stream(chat_request, None).await {
        Ok(result) => result,
        Err(e) => {
            send_stream_chunk_bytes(
                &tx,
                ModelStreamChunk {
                    request_id,
                    chunk: StreamChunkType::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: format!("route_chat_completion_stream: {}", e),
                        details: None,
                    }),
                },
            );
            return;
        }
    };

    let mut stream = route_result.response;
    let mut errored = false;
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chat_chunk) => {
                if let Some(choice) = chat_chunk.choices.into_iter().next() {
                    if let Some(text) = choice.delta.content {
                        if !text.is_empty() {
                            send_stream_chunk_bytes(
                                &tx,
                                ModelStreamChunk {
                                    request_id: request_id.clone(),
                                    chunk: StreamChunkType::TextDelta(text),
                                },
                            );
                        }
                    }
                }
            }
            Err(e) => {
                errored = true;
                send_stream_chunk_bytes(
                    &tx,
                    ModelStreamChunk {
                        request_id: request_id.clone(),
                        chunk: StreamChunkType::Error(ErrorInfo {
                            error_type: ErrorType::InternalError,
                            message: format!("Completion stream blad: {}", e),
                            details: None,
                        }),
                    },
                );
                break;
            }
        }
    }

    if !errored {
        send_stream_chunk_bytes(
            &tx,
            ModelStreamChunk {
                request_id,
                chunk: StreamChunkType::Done {
                    final_metrics: None,
                },
            },
        );
    }
}

fn send_stream_chunk_bytes(
    tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    chunk: tentaflow_protocol::ModelStreamChunk,
) {
    if let Ok(bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&chunk) {
        let _ = tx.send(bytes.into_vec());
    }
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
        Err(e) => make_error_response(request_id, &format!("PromptFetch: blad DB: {}", e)),
    }
}

/// Resolvuje `meeting_key` do `session_id` przez cache; przy miss woła
/// `get_or_create_session` (synchroniczne rusqlite) i zapisuje wynik.
/// Wołane wyłącznie z wariantów które faktycznie zapisują do DB —
/// pure-broadcast warianty (TranscriptEntry/RosterSnapshot) pomijają to
/// całkiem i nie obciążają SQLite.
fn resolve_session_id_cached(
    pool: &crate::db::DbPool,
    meeting_key: &str,
) -> std::result::Result<i64, String> {
    if let Some(cached) = meeting_session_cache().get(meeting_key) {
        return Ok(*cached);
    }
    let id =
        crate::db::repository::transcripts::get_or_create_session(pool, meeting_key, None, None)
            .map_err(|e| {
                format!(
                    "MeetingEvent: resolve session '{}' failed: {}",
                    meeting_key, e
                )
            })?;
    meeting_session_cache().insert(meeting_key.to_string(), id);
    Ok(id)
}

/// Persistuje pojedynczy MeetingEvent do DB. Wydzielone zeby mozna testowac
/// logike bez budowania calego Routera (Router + QUIC + mesh to ciezkie setup).
///
/// Każdy wariant decyduje sam, czy potrzebuje `session_id`. Warianty które
/// tylko logują (TranscriptEntry, RosterSnapshot) nie odpytują DB w ogóle —
/// SQLite hit dla setek per-meeting eventów byłby pasożytniczy. Warianty
/// zapisujące (Summary, ActionItems, Backend, Lifecycle) idą przez
/// `resolve_session_id_cached`, więc po pierwszym evencie sesja siedzi
/// w DashMap i kolejne eventy nie dotykają SQLite na resolve.
fn persist_meeting_event(
    pool: &crate::db::DbPool,
    event: tentaflow_protocol::MeetingEventData,
) -> std::result::Result<(), String> {
    use tentaflow_protocol::MeetingEventPayload;

    match event.payload {
        MeetingEventPayload::SummaryUpdate {
            decisions_text,
            summary_text,
            model,
        } => {
            let session_id = resolve_session_id_cached(pool, &event.meeting_key)?;
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
            let session_id = resolve_session_id_cached(pool, &event.meeting_key)?;
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
        // dostał live broadcast — broadcast woła caller z `meeting_key`, więc
        // `session_id` nie jest tu potrzebny i pomijamy SQLite hit całkowicie.
        MeetingEventPayload::TranscriptEntry {
            speaker_id,
            text,
            latency_ms,
            resolved_stt_model,
            ..
        } => {
            info!(
                "MeetingEvent TranscriptEntry: meeting_key={} speaker={} model={} latency_ms={} text_len={}",
                event.meeting_key,
                speaker_id,
                resolved_stt_model,
                latency_ms,
                text.len()
            );
        }
        // RosterSnapshot: brak tabeli participants per-session. Roster to stan
        // runtime'owy trzymany w pamięci bota i broadcastowany live. Zapis do
        // DB nie jest potrzebny — rekonstrukcja możliwa z transcript_entries
        // (DISTINCT speaker_name). Pomijamy session resolve.
        MeetingEventPayload::RosterSnapshot { entries } => {
            info!(
                "MeetingEvent RosterSnapshot: meeting_key={} count={}",
                event.meeting_key,
                entries.len()
            );
            // Per-entry trace — debug-level zeby zweryfikowac ze speaker_id
            // i nazwa rzeczywiscie sa w payload (frontend filtrowal entries
            // bez speakerId i nigdy nie pokazywal listy uczestnikow).
            for e in entries.iter() {
                tracing::debug!(
                    "  roster entry: speaker_id={} name={:?} status={} has_video={} has_audio={}",
                    e.speaker_id,
                    e.speaker_name,
                    e.status,
                    e.has_video,
                    e.has_audio
                );
            }
        }
        // BackendUpdate: persisted on meeting_sessions so a live view mounted
        // after the broadcast still sees the BACKEND panel populated. The same
        // event is broadcast to dashboards; this branch only owns DB durability.
        // `update_session_backend` operuje po `meeting_key` (a nie session_id),
        // ale i tak rozgrzewamy cache, żeby kolejne warianty zapisujące miały
        // ścieżkę bez SQLite na resolve.
        MeetingEventPayload::BackendUpdate {
            stt_model,
            tts_model,
            summarization_model,
            diarization_model,
            streaming_latency_ms,
            enrolled_speakers,
            total_participants,
        } => {
            let session_id = resolve_session_id_cached(pool, &event.meeting_key)?;
            if let Err(e) = crate::db::repository::transcripts::update_session_backend(
                pool,
                &event.meeting_key,
                &stt_model,
                &tts_model,
                &summarization_model,
                &diarization_model,
                streaming_latency_ms.map(|v| v as i64),
                enrolled_speakers.map(|v| v as i64),
                total_participants.map(|v| v as i64),
            ) {
                warn!("update_session_backend failed: {}", e);
            }
            info!(
                "MeetingEvent BackendUpdate: session_id={} stt={} tts={} sum={} diar={}",
                session_id, stt_model, tts_model, summarization_model, diarization_model
            );
        }
        // Lifecycle stage z bota — persistuje do meeting_sessions.lifecycle_stage
        // żeby reload GUI w trakcie joiningu zobaczył aktualny etap bez zależności
        // od tego, czy WSS broadcast już trafił. Broadcast i tak idzie równolegle
        // przez publish() w callerze.
        // VideoFrame: per-uczestnik klatka wideo do live broadcastu. Nie
        // persistujemy do DB — frames lecą wyłącznie do dashboard subscribers
        // przez ten sam kanał co pozostałe MeetingEventPayload (publish() w
        // callerze). Trzymanie histori klatek w SQLite zalałoby bazę
        // (1 fps × 320x180 JPEG q=0.6 ≈ 15 KB → 54 MB / godzinę / uczestnika).
        MeetingEventPayload::VideoFrame {
            participant_id,
            name,
            ts_ms,
            jpeg,
        } => {
            // VideoFrame leci 1 fps per uczestnik — info-level spamowal logi.
            // Debug zostawiony zeby diagnozowac pipeline gdy potrzeba.
            tracing::debug!(
                "MeetingEvent VideoFrame: meeting_key={} participant={} name={:?} ts_ms={} bytes={}",
                event.meeting_key,
                participant_id,
                name,
                ts_ms,
                jpeg.len()
            );
        }
        // ParticipantAttributes: w obecnym pipeline emitowany WYŁĄCZNIE przez
        // `routing::video_pipeline` po inferencji vision modeli, czyli nigdy
        // nie wpada tutaj jako reverse request od bota. Branch zachowany
        // wyłącznie dla wyczerpania match'a — bot nie pcha takich eventów,
        // a debug log byłby logiem-fantomem.
        MeetingEventPayload::ParticipantAttributes { participant_id, .. } => {
            debug!(
                "MeetingEvent ParticipantAttributes (nieoczekiwany od bota): meeting_key={} participant={}",
                event.meeting_key, participant_id
            );
        }
        MeetingEventPayload::LifecycleUpdate { stage, details } => {
            let session_id = resolve_session_id_cached(pool, &event.meeting_key)?;
            if let Err(e) = crate::db::repository::transcripts::update_session_lifecycle(
                pool,
                &event.meeting_key,
                &stage,
                details.as_deref(),
            ) {
                warn!("update_session_lifecycle failed: {}", e);
            }
            info!(
                "MeetingEvent LifecycleUpdate: meeting_key={} session_id={} stage={}",
                event.meeting_key, session_id, stage
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
        let rows =
            crate::db::repository::transcripts::list_summaries_for_meeting(&db, sid, 10).unwrap();
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
        let rows =
            crate::db::repository::transcripts::list_action_items_for_meeting(&db, sid, None)
                .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "dwa unikalne action items (dedup drugiego Alice)"
        );
        let alice = rows.iter().find(|r| r.owner == "Alice").unwrap();
        assert_eq!(
            alice.deadline.as_deref(),
            Some("2026-05-10"),
            "deadline odswiezony"
        );
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
                assert_eq!(
                    p.resolved_language, "pl",
                    "fallback na pl gdy brak wariantu"
                );
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

    // Handler musi akceptować TranscriptEntry bez błędu i nie wpisywać niczego do
    // DB — persist chunków leci przez transcript_store, a sam wariant istnieje
    // wyłącznie dla live broadcastu. Po optymalizacji R-3/R-4 handler nawet nie
    // resolvuje session_id (skip SQLite hit dla setek per-meeting eventów).
    #[test]
    fn persist_handler_transcript_entry_is_noop_and_skips_session_resolve() {
        let db = setup_test_db();
        invalidate_meeting_session("m-te-1");
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

        // TranscriptEntry NIE tworzy session row — meeting_sessions zostaje puste
        // dopóki nie przyjdzie wariant zapisujący (Summary, ActionItems, …) albo
        // STT chunk (transcript_store).
        let sid_opt =
            crate::db::repository::transcripts::session_id_by_meeting_key(&db, "m-te-1").unwrap();
        assert!(
            sid_opt.is_none(),
            "TranscriptEntry nie powinien tworzyć session row"
        );
    }

    // RosterSnapshot: handler nie persistuje nigdzie — sprawdzamy że nie
    // zwraca błędu i nie dotyka SQLite (po optymalizacji R-3/R-4 pomijamy
    // session resolve całkowicie). Snapshot z N entries traktujemy tak samo
    // jak pojedynczy event — koszt persist O(0) niezależnie od N.
    #[test]
    fn persist_handler_roster_snapshot_is_noop_and_skips_session_resolve() {
        let db = setup_test_db();
        invalidate_meeting_session("m-rs-1");
        let event = MeetingEventData {
            meeting_key: "m-rs-1".to_string(),
            timestamp_ms: 100,
            payload: MeetingEventPayload::RosterSnapshot {
                entries: vec![
                    RosterEntry {
                        speaker_id: "SPEAKER_01".to_string(),
                        speaker_name: Some("Alice".to_string()),
                        status: "joined".to_string(),
                        last_spoken_ago_sec: None,
                        has_video: true,
                        has_audio: true,
                        in_stage: true,
                        in_roster: true,
                    },
                    RosterEntry {
                        speaker_id: "SPEAKER_02".to_string(),
                        speaker_name: Some("Bob".to_string()),
                        status: "speaking".to_string(),
                        last_spoken_ago_sec: Some(2),
                        has_video: false,
                        has_audio: true,
                        in_stage: false,
                        in_roster: true,
                    },
                ],
            },
        };
        persist_meeting_event(&db, event).expect("persist roster snapshot");

        let sid_opt =
            crate::db::repository::transcripts::session_id_by_meeting_key(&db, "m-rs-1").unwrap();
        assert!(
            sid_opt.is_none(),
            "RosterSnapshot nie powinien tworzyć session row"
        );
    }

    // Cache hit: pierwszy event z meeting_key idzie przez get_or_create_session,
    // drugi z tym samym kluczem trafia w DashMap. Sprawdzamy przez wstawienie
    // ręcznie nieistniejącego id do cache i obserwację, że handler go używa
    // bez tworzenia nowej sesji w DB.
    #[test]
    fn meeting_session_cache_hits_skip_db() {
        let db = setup_test_db();
        let key = "m-cache-hit-1";
        invalidate_meeting_session(key);

        // Pierwszy event populuje cache realnym session_id z DB.
        let event1 = MeetingEventData {
            meeting_key: key.to_string(),
            timestamp_ms: 1,
            payload: MeetingEventPayload::SummaryUpdate {
                decisions_text: "d1".to_string(),
                summary_text: "s1".to_string(),
                model: "m".to_string(),
            },
        };
        persist_meeting_event(&db, event1).expect("first persist");

        // Cache musi mieć teraz wpis.
        let cached = meeting_session_cache().get(key).map(|v| *v);
        assert!(
            cached.is_some(),
            "cache nie został zapełniony po pierwszym evencie"
        );
        let real_sid = cached.unwrap();

        // Kasujemy sesję bezpośrednio z DB (cascade FK usunie summary). Cache
        // nadal trzyma stary id — gdyby handler szedł do DB, get_or_create_session
        // utworzyłby nowe id. Jeśli używa cache, drugi insert poleci na stare id
        // i FK error potwierdzi cache hit.
        {
            let conn = db.lock().unwrap();
            conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
            conn.execute(
                "DELETE FROM meeting_sessions WHERE id = ?1",
                rusqlite::params![real_sid],
            )
            .unwrap();
        }

        let event2 = MeetingEventData {
            meeting_key: key.to_string(),
            timestamp_ms: 2,
            payload: MeetingEventPayload::SummaryUpdate {
                decisions_text: "d2".to_string(),
                summary_text: "s2".to_string(),
                model: "m".to_string(),
            },
        };
        let res = persist_meeting_event(&db, event2);
        assert!(
            res.is_err(),
            "cache hit musi reużyć stary session_id; insert powinien fail-FK po DELETE sesji"
        );

        // Po użytku tego testu czyścimy cache, żeby nie dziedziczyć stanu.
        invalidate_meeting_session(key);
    }

    // Po `invalidate_meeting_session` kolejny event musi ponownie odpytać DB
    // i utworzyć/znaleźć sesję — czyli faktycznie zapisać do meeting_sessions.
    #[test]
    fn meeting_session_cache_invalidate_forces_db_resolve() {
        let db = setup_test_db();
        let key = "m-cache-inv-1";
        invalidate_meeting_session(key);

        let event1 = MeetingEventData {
            meeting_key: key.to_string(),
            timestamp_ms: 1,
            payload: MeetingEventPayload::SummaryUpdate {
                decisions_text: "d".to_string(),
                summary_text: "s".to_string(),
                model: "m".to_string(),
            },
        };
        persist_meeting_event(&db, event1).expect("first persist");
        let sid_first = meeting_session_cache().get(key).map(|v| *v).unwrap();

        // Kasujemy sesję i invalidujemy cache — kolejny event musi utworzyć nowy
        // wpis w meeting_sessions z nowym id i odświeżyć cache.
        {
            let conn = db.lock().unwrap();
            conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
            conn.execute(
                "DELETE FROM meeting_sessions WHERE id = ?1",
                rusqlite::params![sid_first],
            )
            .unwrap();
        }
        invalidate_meeting_session(key);

        let event2 = MeetingEventData {
            meeting_key: key.to_string(),
            timestamp_ms: 2,
            payload: MeetingEventPayload::SummaryUpdate {
                decisions_text: "d2".to_string(),
                summary_text: "s2".to_string(),
                model: "m".to_string(),
            },
        };
        persist_meeting_event(&db, event2).expect("second persist after invalidate");
        let sid_second = meeting_session_cache().get(key).map(|v| *v).unwrap();
        assert_ne!(
            sid_first, sid_second,
            "po invalidate handler musi pobrać świeży session_id z DB"
        );

        invalidate_meeting_session(key);
    }

    // BackendUpdate: persisted on meeting_sessions so a live view mounted
    // after the broadcast still sees the BACKEND panel populated.
    #[test]
    fn persist_handler_backend_update_writes_models() {
        let db = setup_test_db();
        // Session must exist before the bot's BackendUpdate, mirroring host flow.
        crate::db::repository::transcripts::get_or_create_session(&db, "m-bu-1", None, None)
            .unwrap();
        let event = MeetingEventData {
            meeting_key: "m-bu-1".to_string(),
            timestamp_ms: 100,
            payload: MeetingEventPayload::BackendUpdate {
                stt_model: "teams-stt".to_string(),
                tts_model: "teams-tts".to_string(),
                summarization_model: "teams-summarization".to_string(),
                diarization_model: "pyannote-3.1".to_string(),
                streaming_latency_ms: Some(180),
                enrolled_speakers: Some(2),
                total_participants: Some(5),
            },
        };
        persist_meeting_event(&db, event).expect("persist backend update");

        let sid = crate::db::repository::transcripts::session_id_by_meeting_key(&db, "m-bu-1")
            .unwrap()
            .expect("session id");
        let row = crate::db::repository::transcripts::get_session(&db, sid)
            .unwrap()
            .expect("session row");
        assert_eq!(row.backend_stt_model.as_deref(), Some("teams-stt"));
        assert_eq!(row.backend_tts_model.as_deref(), Some("teams-tts"));
        assert_eq!(
            row.backend_summarization_model.as_deref(),
            Some("teams-summarization")
        );
        assert_eq!(
            row.backend_diarization_model.as_deref(),
            Some("pyannote-3.1")
        );
        assert_eq!(row.backend_streaming_latency_ms, Some(180));
        assert_eq!(row.backend_enrolled_speakers, Some(2));
        assert_eq!(row.backend_total_participants, Some(5));
    }
}
