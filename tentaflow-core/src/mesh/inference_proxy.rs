// =============================================================================
// Plik: mesh/inference_proxy.rs
// Opis: Obsluga odwrotnych QUIC requestow od kontenerow. Kontenery moga
//       otwierac strumienie bi-directional na istniejacym polaczeniu, aby
//       wyslac ModelRequest do routera (np. sidecar wola STT/TTS).
// =============================================================================

use crate::routing::Router;

use dashmap::DashMap;
use std::sync::OnceLock;
use tracing::{debug, error, info, warn};

pub use crate::meeting::flow_turn::ReverseCaller;

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

/// Sidecar payload policy for the unary reverse path.
///
/// A mesh peer (`caller = None`) is a trust-paired node and keeps full access.
/// A sidecar (`caller = Some`) is a container Core spawned: it may only drive
/// the meeting it was spawned for, so every payload that names a meeting goes
/// through `lookup_owned_session`, and every payload that would turn Core into
/// an anonymous inference proxy (embeddings, vision, rerank, camera-cv, …) is
/// refused outright — the bot has no use for them and they would run without a
/// user context, model ACL or quota.
///
/// `Completion` and `PromptFetch` stay reachable because the meeting
/// summarizer runs inside the sidecar, but both are bound to an owned meeting
/// through the `meeting_id` request metadata.
fn authorize_sidecar_request(
    router: &Router,
    request: &tentaflow_protocol::ModelRequest,
    caller: &ReverseCaller,
) -> Result<(), String> {
    use tentaflow_protocol::ModelPayload;

    let Some(ref db) = router.db else {
        return Err("reverse request from a sidecar needs a database".to_string());
    };
    let meta_meeting_id =
        || -> Option<&str> {
            request.metadata.as_ref()?.iter().find_map(|(k, v)| {
                (k == "meeting_id" && !v.trim().is_empty()).then_some(v.as_str())
            })
        };
    let owned = |meeting_id: Option<&str>| -> Result<(), String> {
        let meeting_id = meeting_id.ok_or_else(|| {
            "reverse request from a sidecar must carry the meeting_id it owns".to_string()
        })?;
        crate::meeting::flow_turn::lookup_owned_session(db, meeting_id, caller).map(|_| ())
    };

    match &request.payload {
        ModelPayload::Audio(_) | ModelPayload::Completion(_) | ModelPayload::PromptFetch(_) => {
            owned(meta_meeting_id())
        }
        ModelPayload::MeetingEvent(event) => owned(Some(event.meeting_key.as_str())),
        other => Err(format!(
            "payload {:?} is not accepted from service '{}'",
            std::mem::discriminant(other),
            caller.service_name
        )),
    }
}

/// Dispatchuje odwrotny request przez odpowiednia metode Routera. Dostepne
/// publicznie zeby forward handler mesh mogl uzyc tej samej sciezki.
/// `caller` identifies a sidecar that opened the stream back to Core; mesh
/// forwarding passes `None` (trust-paired peer, no sidecar policy).
pub async fn dispatch_reverse_request(
    router: &Router,
    request: tentaflow_protocol::ModelRequest,
    caller: Option<&ReverseCaller>,
) -> tentaflow_protocol::ModelResponse {
    use tentaflow_protocol::*;

    let request_id = request.request_id.clone();

    if let Some(caller) = caller {
        if let Err(message) = authorize_sidecar_request(router, &request, caller) {
            warn!(
                request_id = %request_id,
                service = %caller.service_name,
                "reverse request refused: {message}"
            );
            return ModelResponse {
                request_id,
                result: ModelResult::Error(ErrorInfo {
                    error_type: ErrorType::Unauthorized,
                    message,
                    details: None,
                }),
                metrics: None,
            };
        }
    }

    // Codex R3b.7 H2: anti-loop. Forwarding peer carries hop count in
    // metadata (key `x-tentaflow-mesh-hop`). Refuse when we are at or
    // past `MAX_HOP_COUNT` — A→B→A cycles would otherwise reset hop
    // tracking on each node and run forever.
    if let Some(meta) = request.metadata.as_ref() {
        for (k, v) in meta {
            if k == crate::services::runtime::context::MESH_HOP_HEADER {
                if let Ok(received_hop) = v.parse::<u8>() {
                    if received_hop >= crate::services::runtime::context::MAX_HOP_COUNT {
                        warn!(
                            request_id = %request_id,
                            hop = received_hop,
                            limit = crate::services::runtime::context::MAX_HOP_COUNT,
                            "rejecting reverse mesh request: hop limit exceeded"
                        );
                        return ModelResponse {
                            request_id: request_id.clone(),
                            result: ModelResult::Error(ErrorInfo {
                                error_type: ErrorType::InvalidRequest,
                                message: format!(
                                    "mesh hop limit {} reached — refusing re-forward",
                                    crate::services::runtime::context::MAX_HOP_COUNT
                                ),
                                details: None,
                            }),
                            metrics: None,
                        };
                    }
                }
                break;
            }
        }
    }

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

            // Diarization runs concurrently with STT (both read the same
            // segment); the join happens once the transcript is known.
            let diarization = match (&audio_payload.operation, meeting_id.as_deref()) {
                (AudioOperation::STT { audio_data, .. }, Some(mid)) => {
                    crate::meeting::flow_turn::spawn_diarization(
                        router.db.clone(),
                        Some(audio_data.as_slice()),
                        mid,
                    )
                }
                _ => crate::meeting::flow_turn::spawn_diarization(None, None, ""),
            };

            match router
                .route_audio_via_protocol(&audio_payload.operation)
                .await
            {
                Ok(response) => {
                    // Jesli to STT (Text result), zapisz do transcript_store dla GUI Bot Status
                    if let ModelResult::Audio(ref audio_result) = response.result {
                        if let AudioResultData::Text(ref text) = audio_result.data {
                            if let Some(ref mid) = meeting_id {
                                crate::meeting::flow_turn::record_transcript(
                                    mid,
                                    text,
                                    &audio_result.model,
                                    diarization,
                                )
                                .await;
                            } else if !text.trim().is_empty() {
                                crate::routing::transcript_store::push(
                                    crate::routing::transcript_store::TranscriptBuilder::new(
                                        text.clone(),
                                        audio_result.model.clone(),
                                    ),
                                );
                                // Metrics only — a transcript is participant
                                // speech and must not land in the log.
                                info!(
                                    model = %audio_result.model,
                                    chars = text.chars().count(),
                                    "transcript recorded without meeting context"
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
                    // Codex R3b.8 round 2 H1: dispatch through executor
                    // directly with `hop_count = MAX_HOP_COUNT` so a peer
                    // cannot push us into A→B→A bouncing. `route_chat_completion`
                    // would build a fresh `ExecutionContext` with hop=0,
                    // which loses the cross-mesh hop boundary.
                    let executor_snapshot = router.executor().clone();
                    let Some(executor) = executor_snapshot else {
                        return make_error_response(
                            request_id,
                            "router executor not wired for mesh-reverse chat",
                        );
                    };
                    // §2.5 — the originating user stays on the initiator's
                    // node; the acting identity here is the mesh peer.
                    let mut exec_ctx = crate::services::runtime::context::ExecutionContext::new(
                        None,
                        crate::flow_engine::dispatcher::FlowOrigin::Mesh,
                        crate::flow_engine::dispatcher::FlowActor::system_component("mesh_peer"),
                    );
                    exec_ctx.hop_count = crate::services::runtime::context::MAX_HOP_COUNT;
                    // EXEMPT-MESH-INBOUND (stage 3d v1.5): mesh reverse chat —
                    // peer forwarduje request, my wykonujemy direct executor
                    // żeby zachować ultra-low latency LAN budżet. Flow żyje
                    // po stronie inicjatora (Node A), peer = remote backend
                    // call. Plan v1.5: jeden z 3 dozwolonych wyjątków
                    // (chat tutaj + STT routing/stt.rs:301 + embeddings
                    // routing/embeddings.rs:222), wszystkie wywoływane
                    // wyłącznie z mesh reverse path.
                    let chat_response =
                        match executor.execute_chat(chat_request, &mut exec_ctx).await {
                            Ok(r) => r,
                            Err(e) => {
                                return make_error_response(
                                    request_id,
                                    &format!("Blad chat completion: {}", e),
                                );
                            }
                        };
                    let text = chat_response
                        .choices
                        .first()
                        .and_then(|c| c.message.content.as_ref())
                        .map(|c| match c {
                            crate::api::openai::types::MessageContent::Text(t) => t.clone(),
                            crate::api::openai::types::MessageContent::Parts(parts) => parts
                                .iter()
                                .filter_map(|p| {
                                    if let crate::api::openai::types::ContentPart::Text { text } = p
                                    {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(""),
                        })
                        .unwrap_or_default();
                    let reasoning_content = chat_response
                        .choices
                        .first()
                        .and_then(|c| c.message.reasoning_content.clone());

                    ModelResponse {
                        request_id,
                        result: ModelResult::Completion(CompletionResult {
                            text,
                            reasoning_content,
                            model: chat_response.model,
                            finish_reason: chat_response
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

        ModelPayload::Rerank(ref rerank_payload) => {
            match router.route_rerank_via_quic(rerank_payload).await {
                Ok(response) => response,
                Err(e) => make_error_response(request_id, &format!("Blad rerank: {}", e)),
            }
        }

        // Documents (typed surface `/v1/infer`, detektory struktury) — peer bez
        // lokalnego serwisu Documents forwarduje obraz strony w DocumentInferPayload.
        // Mesh hop binarny (image_bytes serde_bytes); route_documents_via_protocol
        // gada z lokalnym serwisem przez REST `/v1/infer`. Fundament flow-ingestu RAG.
        ModelPayload::Documents(ref documents_payload) => {
            match router.route_documents_via_protocol(documents_payload).await {
                Ok(response) => response,
                Err(e) => make_error_response(request_id, &format!("Blad documents: {}", e)),
            }
        }

        // Vision-chat (VLM przez /v1/chat/completions, np. nemotron-parse) — peer
        // forwarduje obraz strony w VisionPayload. Bez tego ramienia parse PDF z
        // węzła bez lokalnego modelu vision padał (catch-all). Mesh hop binarny;
        // route_vision_via_protocol gada z lokalnym serwisem przez REST.
        ModelPayload::Vision(ref vision_payload) => {
            match router.route_vision_via_protocol(vision_payload).await {
                Ok(response) => response,
                Err(e) => make_error_response(request_id, &format!("Blad vision: {}", e)),
            }
        }

        // CameraCv (typed surface, operacje CV na klatkach z kamer) — peer bez
        // lokalnego modelu CV forwarduje klatki w CameraCvPayload (serde_bytes,
        // mesh hop binarny). Wykonujemy przez executor z hop_count na limicie
        // (jak ramię Completion) — resolver trafi Local(Embedded) na tym węźle,
        // a ewentualna próba re-forwardu pada na hop-limit zamiast krążyć A→B→A.
        ModelPayload::CameraCv(ref cv_payload) => {
            let Some(executor) = router.executor() else {
                return make_error_response(
                    request_id,
                    "router executor not wired for mesh-reverse camera-cv",
                );
            };
            match build_camera_cv_request(cv_payload) {
                Ok(cv_request) => {
                    // §2.5 — the originating user stays on the initiator's
                    // node; the acting identity here is the mesh peer.
                    let mut exec_ctx = crate::services::runtime::context::ExecutionContext::new(
                        None,
                        crate::flow_engine::dispatcher::FlowOrigin::Mesh,
                        crate::flow_engine::dispatcher::FlowActor::system_component("mesh_peer"),
                    );
                    exec_ctx.hop_count = crate::services::runtime::context::MAX_HOP_COUNT;
                    match executor.execute_camera_cv(cv_request, &mut exec_ctx).await {
                        Ok(result) => ModelResponse {
                            request_id,
                            result: ModelResult::CameraCv(result),
                            metrics: None,
                        },
                        Err(e) => {
                            make_error_response(request_id, &format!("Blad camera-cv: {}", e))
                        }
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

    let mut messages: Vec<Message> = payload
        .messages
        .iter()
        .map(|m| Message {
            role: m.role.clone(),
            content: Some(MessageContent::Text(m.content.clone())),
            reasoning_content: m.reasoning_content.clone(),
            ..Default::default()
        })
        .collect();

    // Codex R3b.8 round 2 M2: prompt-only `CompletionPayload` (legacy
    // peers that still send `prompt: Some(_)` with empty `messages`).
    // Pre-cutover `route_completion_via_protocol` accepted both shapes;
    // now we synthesise a single `user`-role message from the prompt so
    // the executor receives chat-shaped input.
    if messages.is_empty() {
        if let Some(prompt) = payload.prompt.as_ref().filter(|p| !p.is_empty()) {
            messages.push(Message {
                role: "user".to_string(),
                content: Some(MessageContent::Text(prompt.clone())),
                ..Default::default()
            });
        } else {
            return Err("CompletionPayload has neither messages nor prompt".to_string());
        }
    }

    Ok(ChatCompletionRequest {
        reasoning_effort: None,
        modalities: None,
        audio: None,
        model: payload.model.clone(),
        messages,
        temperature: payload.temperature,
        max_tokens: payload.max_tokens,
        stream: false,
        stream_options: None,
        top_p: payload.top_p,
        frequency_penalty: None,
        presence_penalty: payload.presence_penalty,
        stop: payload.stop.clone(),
        user: None,
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
        extra: Default::default(),
    })
}

/// Buduje lokalny `CameraCvRequest` z mesh-owego `CameraCvPayload`.
/// Klatki Rgb24 przechodzą wprost do `Arc<[u8]>` (jedna kopia z bufora CBOR);
/// Jpeg jest dekodowany do RGB24 crate'em `image` — wymiary bierzemy z
/// dekodera, nie z pól payloadu.
fn build_camera_cv_request(
    payload: &tentaflow_protocol::CameraCvPayload,
) -> std::result::Result<crate::services::runtime::local_cv::CameraCvRequest, String> {
    use crate::services::runtime::local_cv::{CameraCvOpLocal, CameraCvRequest};
    use tentaflow_protocol::CameraCvOp;

    let op = match &payload.op {
        CameraCvOp::Detect { frames, threshold } => CameraCvOpLocal::Detect {
            frames: frames
                .iter()
                .map(decode_cv_frame)
                .collect::<std::result::Result<Vec<_>, _>>()?,
            threshold: *threshold,
        },
        CameraCvOp::ClassifyState { crop } => CameraCvOpLocal::ClassifyState {
            crop: decode_cv_frame(crop)?,
        },
        CameraCvOp::Ocr { crop, mode } => CameraCvOpLocal::Ocr {
            crop: decode_cv_frame(crop)?,
            mode: mode.clone(),
        },
    };

    Ok(CameraCvRequest {
        model: payload.model.clone(),
        op,
    })
}

/// Twardy limit rozmiaru pojedynczej zdekodowanej klatki RGB przychodzącej
/// z mesh: 1080p RGB to ~6.2 MB, więc 32 MiB to bezpieczny sufit dla
/// pojedynczej ramki — większe wymiary to błędny lub złośliwy payload.
const MAX_CV_RGB_BYTES: usize = 32 * 1024 * 1024;

/// Liczy rozmiar bufora RGB24 (`width * height * 3`) dla klatki z mesh —
/// to granica zdalnego inputu, więc odrzuca wymiary zerowe, przepełnienie
/// mnożenia (`checked_mul`) i przekroczenie `MAX_CV_RGB_BYTES`.
fn checked_cv_rgb_bytes(width: u32, height: u32) -> std::result::Result<usize, String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "camera-cv: klatka o zerowym wymiarze {}x{}",
            width, height
        ));
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(3))
        .ok_or_else(|| {
            format!(
                "camera-cv: wymiary klatki {}x{} przepelniaja usize",
                width, height
            )
        })?;
    if bytes > MAX_CV_RGB_BYTES {
        return Err(format!(
            "camera-cv: klatka {}x{} ({} bajtow RGB) przekracza limit {} bajtow",
            width, height, bytes, MAX_CV_RGB_BYTES
        ));
    }
    Ok(bytes)
}

/// Dekoduje pojedynczą klatkę mesh (`CvFrame`) do lokalnego wariantu RGB24.
/// Rgb24 waliduje rozmiar bufora (`width * height * 3`); Jpeg najpierw
/// sprawdza wymiary z nagłówka (ochrona przed decompression bomb), potem
/// dekompresuje strumień do surowych pikseli RGB. Obie ścieżki podlegają
/// limitowi `MAX_CV_RGB_BYTES`.
fn decode_cv_frame(
    frame: &tentaflow_protocol::CvFrame,
) -> std::result::Result<crate::services::runtime::local_cv::CvFrameLocal, String> {
    use crate::services::runtime::local_cv::CvFrameLocal;
    use tentaflow_protocol::CvFrameEncoding;

    match frame.encoding {
        CvFrameEncoding::Rgb24 => {
            let expected = checked_cv_rgb_bytes(frame.width, frame.height)?;
            if frame.data.len() != expected {
                return Err(format!(
                    "camera-cv: klatka Rgb24 {}x{} wymaga {} bajtow, otrzymano {}",
                    frame.width,
                    frame.height,
                    expected,
                    frame.data.len()
                ));
            }
            Ok(CvFrameLocal {
                data: std::sync::Arc::from(frame.data.as_slice()),
                width: frame.width,
                height: frame.height,
            })
        }
        CvFrameEncoding::Jpeg => {
            // Wymiary z samego nagłówka JPEG, bez dekodowania pikseli —
            // limit rozmiaru musi zadziałać ZANIM zaalokujemy bufor RGB.
            let (header_w, header_h) = image::ImageReader::new(std::io::Cursor::new(&frame.data))
                .with_guessed_format()
                .map_err(|e| format!("camera-cv: odczyt naglowka JPEG nieudany: {}", e))?
                .into_dimensions()
                .map_err(|e| format!("camera-cv: wymiary z naglowka JPEG nieudane: {}", e))?;
            checked_cv_rgb_bytes(header_w, header_h)?;
            let img = image::load_from_memory_with_format(&frame.data, image::ImageFormat::Jpeg)
                .map_err(|e| format!("camera-cv: dekodowanie JPEG nieudane: {}", e))?
                .to_rgb8();
            let (width, height) = img.dimensions();
            Ok(CvFrameLocal {
                data: std::sync::Arc::from(img.into_raw()),
                width,
                height,
            })
        }
    }
}

/// Streaming reverse dispatch. `caller` identifies the sidecar that opened the
/// stream (set by the per-service reverse listener); mesh forward handlers
/// pass `None`, which keeps `FlowInvoke` — a meeting-bot-only payload —
/// unreachable from a peer node.
pub async fn dispatch_reverse_stream_request(
    router: &Router,
    request: tentaflow_protocol::ModelRequest,
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    caller: Option<&ReverseCaller>,
) {
    use futures::StreamExt;
    use tentaflow_protocol::{
        ErrorInfo, ErrorType, ModelPayload, ModelStreamChunk, StreamChunkType,
    };

    let request_id = request.request_id.clone();
    // A sidecar streams exactly one thing: a flow turn for its own meeting.
    // Streaming a raw completion would make Core a free inference proxy with
    // no user context, model ACL or quota behind it.
    if let (Some(caller), false) = (
        caller,
        matches!(request.payload, ModelPayload::FlowInvoke(_)),
    ) {
        send_stream_chunk_bytes(
            &tx,
            ModelStreamChunk {
                request_id,
                chunk: StreamChunkType::Error(ErrorInfo {
                    error_type: ErrorType::Unauthorized,
                    message: format!(
                        "only FlowInvoke may be streamed from service '{}'",
                        caller.service_name
                    ),
                    details: None,
                }),
            },
        );
        return;
    }
    let completion_payload = match &request.payload {
        ModelPayload::Completion(p) => p,
        ModelPayload::FlowInvoke(payload) => {
            crate::meeting::flow_turn::run_flow_turn(
                router,
                request_id,
                payload.clone(),
                caller,
                &tx,
            )
            .await;
            return;
        }
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

    // Diagnostic anchor: every mesh-forwarded chat stream logs which model it
    // asked for and how it ended on THIS node — without it, errors travel back
    // to the initiator as opaque strings and the serving node leaves no trace.
    tracing::info!(
        target: "mesh::reverse",
        request_id = %request_id,
        model = %completion_payload.model,
        "mesh reverse chat stream start"
    );
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

    // Mesh reverse stream = the INITIATOR's flow already ran; its llm node
    // forwarded a model call here. A RAW model MUST stream straight from the
    // backend on this node — no flow re-entry, no is_default "Default Chat"
    // fallback, no hidden PII redaction (all of which live only inside the
    // flow-engine path of route_chat_completion_stream). The forwarding node
    // owns the flow; this node provides raw model compute only. Only a model
    // that IS an explicit published flow on this node runs as a flow — the
    // forwarded name resolves to that flow's definition, which is what the model
    // means. `model_resolves_to_flow` is true ONLY for an explicit Flow catalog
    // entry, never for the is_default fallback, so it is the exact raw-vs-flow
    // discriminator. Mirrors the direct-executor rule of the non-streaming
    // Completion branch (`executor.execute_chat`).
    let model_name = chat_request.model.clone();
    let mut stream: std::pin::Pin<
        Box<
            dyn futures::Stream<
                    Item = crate::error::Result<crate::api::openai::types::ChatCompletionChunk>,
                > + Send,
        >,
    > = if crate::routing::chat::model_resolves_to_flow(&router.catalog_snapshot(), &model_name) {
        // Published flow-as-model: execute THAT flow on this node.
        match router
            .route_chat_completion_stream(
                chat_request,
                None,
                // A peer node forwarded this request over the mesh; there is no
                // local session behind it.
                crate::flow_engine::dispatcher::FlowOrigin::Mesh,
                crate::flow_engine::dispatcher::FlowActor::system(),
                None,
                crate::routing::streaming::ChatFlowSelector::Auto,
            )
            .await
        {
            Ok(result) => result.response,
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
        }
    } else {
        // Raw model: direct executor stream. `hop_count = MAX_HOP_COUNT` is the
        // same A→B→A re-forward guard the non-streaming branch applies.
        let Some(executor) = router.executor() else {
            send_stream_chunk_bytes(
                &tx,
                ModelStreamChunk {
                    request_id,
                    chunk: StreamChunkType::Error(ErrorInfo {
                        error_type: ErrorType::InternalError,
                        message: "router executor not wired for mesh-reverse chat stream"
                            .to_string(),
                        details: None,
                    }),
                },
            );
            return;
        };
        // §2.5 — the originating user stays on the initiator's node; the
        // acting identity here is the mesh peer.
        let mut exec_ctx = crate::services::runtime::context::ExecutionContext::new(
            None,
            crate::flow_engine::dispatcher::FlowOrigin::Mesh,
            crate::flow_engine::dispatcher::FlowActor::system_component("mesh_peer"),
        );
        exec_ctx.hop_count = crate::services::runtime::context::MAX_HOP_COUNT;
        match executor.stream_chat(chat_request, &mut exec_ctx).await {
            Ok(s) => s,
            Err(e) => {
                send_stream_chunk_bytes(
                    &tx,
                    ModelStreamChunk {
                        request_id,
                        chunk: StreamChunkType::Error(ErrorInfo {
                            error_type: ErrorType::InternalError,
                            message: format!("mesh reverse direct stream: {}", e),
                            details: None,
                        }),
                    },
                );
                return;
            }
        }
    };
    let mut errored = false;
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chat_chunk) => relay_chat_chunk(&request_id, chat_chunk, &tx),
            Err(e) => {
                errored = true;
                tracing::warn!(
                    target: "mesh::reverse",
                    request_id = %request_id,
                    "mesh reverse chat stream error on this node: {e}"
                );
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

/// Map one local OpenAI backend chunk onto 0..n wire chunks. Thinking must ride
/// its own `ReasoningDelta` variant — the requester maps it back onto
/// `delta.reasoning_content` (routing::stream_helpers), so folding it into
/// `TextDelta` would corrupt both channels. Split out of the reverse-stream
/// loop so the mapping stays unit-testable without a Router.
fn relay_chat_chunk(
    request_id: &str,
    chat_chunk: crate::api::openai::types::ChatCompletionChunk,
    tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) {
    let Some(choice) = chat_chunk.choices.into_iter().next() else {
        return;
    };
    if let Some(reasoning) = choice.delta.reasoning_content {
        if !reasoning.is_empty() {
            send_stream_chunk_bytes(
                tx,
                ModelStreamChunk {
                    request_id: request_id.to_string(),
                    chunk: StreamChunkType::ReasoningDelta(reasoning),
                },
            );
        }
    }
    if let Some(text) = choice.delta.content {
        if !text.is_empty() {
            send_stream_chunk_bytes(
                tx,
                ModelStreamChunk {
                    request_id: request_id.to_string(),
                    chunk: StreamChunkType::TextDelta(text),
                },
            );
        }
    }
}

fn send_stream_chunk_bytes(
    tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    chunk: tentaflow_protocol::ModelStreamChunk,
) {
    if let Ok(bytes) = crate::mesh::cbor::encode(&chunk) {
        let _ = tx.send(bytes);
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
            // The bot no longer knows its STT/TTS (Core resolves them per turn
            // from the session row), so empty names are filled from there.
            let (stt_model, tts_model) = if stt_model.is_empty() || tts_model.is_empty() {
                let pipeline = crate::db::repository::transcripts::get_session_by_meeting_key(
                    pool,
                    &event.meeting_key,
                )
                .ok()
                .flatten()
                .map(|row| crate::meeting::flow_turn::SessionPipeline::from_row(&row));
                match pipeline {
                    Some(p) => (
                        if stt_model.is_empty() {
                            p.stt_alias
                        } else {
                            stt_model
                        },
                        if tts_model.is_empty() {
                            p.tts_alias
                        } else {
                            tts_model
                        },
                    ),
                    None => (stt_model, tts_model),
                }
            } else {
                (stt_model, tts_model)
            };
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
    fn relay_chat_chunk_splits_reasoning_from_content() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        relay_chat_chunk(
            "req-1",
            crate::api::openai::types::ChatCompletionChunk {
                id: "c-1".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 1,
                model: "m".to_string(),
                choices: vec![crate::api::openai::types::ChunkChoice {
                    index: 0,
                    delta: crate::api::openai::types::Delta {
                        role: None,
                        content: Some("answer".to_string()),
                        reasoning_content: Some("thinking".to_string()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                system_fingerprint: None,
                audio: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
                usage: None,
                perf: None,
            },
            &tx,
        );

        let decode =
            |rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>| {
                crate::mesh::cbor::decode::<ModelStreamChunk>(&rx.try_recv().unwrap()).unwrap()
            };
        assert!(matches!(
            decode(&mut rx).chunk,
            StreamChunkType::ReasoningDelta(ref t) if t == "thinking"
        ));
        assert!(matches!(
            decode(&mut rx).chunk,
            StreamChunkType::TextDelta(ref t) if t == "answer"
        ));
        assert!(rx.try_recv().is_err(), "exactly two wire chunks expected");
    }

    #[test]
    fn relay_chat_chunk_drops_empty_delta_and_choiceless_chunks() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // Usage tail chunk (no choices) must not produce a wire chunk.
        relay_chat_chunk(
            "req-2",
            crate::api::openai::types::ChatCompletionChunk {
                choices: vec![],
                usage: None,
                perf: None,
                id: String::new(),
                object: String::new(),
                created: 0,
                model: String::new(),
                system_fingerprint: None,
                audio: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            },
            &tx,
        );
        // Empty delta strings are dropped like they always were for content.
        relay_chat_chunk(
            "req-2",
            crate::api::openai::types::ChatCompletionChunk {
                choices: vec![crate::api::openai::types::ChunkChoice {
                    index: 0,
                    delta: crate::api::openai::types::Delta {
                        role: None,
                        content: Some(String::new()),
                        reasoning_content: Some(String::new()),
                        tool_calls: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                usage: None,
                perf: None,
                id: String::new(),
                object: String::new(),
                created: 0,
                model: String::new(),
                system_fingerprint: None,
                audio: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            },
            &tx,
        );
        assert!(rx.try_recv().is_err());
    }

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
                    reasoning_content: None,
                },
                Message {
                    role: "user".to_string(),
                    content: "Czesc!".to_string(),
                    reasoning_content: None,
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
    fn build_chat_request_empty_messages_and_no_prompt_returns_error() {
        // Codex R3b.8 round 2 M2: prompt-only fallback synthesises a
        // user-role message; only when both are missing do we reject.
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
        assert!(result.unwrap_err().to_lowercase().contains("neither"));
    }

    #[test]
    fn build_chat_request_prompt_only_synthesises_user_message() {
        let payload = CompletionPayload {
            model: "gpt-4".to_string(),
            prompt: Some("Hello, world".to_string()),
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

        let req = build_chat_request(&payload).expect("prompt-only should synthesise");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        match &req.messages[0].content {
            Some(crate::api::openai::types::MessageContent::Text(t)) => {
                assert_eq!(t, "Hello, world");
            }
            other => panic!("expected Text content, got {other:?}"),
        }
    }

    #[test]
    fn build_camera_cv_request_rgb24_zachowuje_bajty() {
        // Klatka Rgb24 przechodzi do wariantu lokalnego bez modyfikacji danych
        let data: Vec<u8> = (0..12).collect(); // 2x2 piksele RGB
        let payload = CameraCvPayload {
            model: "rfdetr-adr".to_string(),
            op: CameraCvOp::Detect {
                frames: vec![CvFrame {
                    data: data.clone(),
                    width: 2,
                    height: 2,
                    encoding: CvFrameEncoding::Rgb24,
                }],
                threshold: Some(0.6),
            },
        };

        let req = build_camera_cv_request(&payload).expect("Rgb24 powinno przejsc");
        assert_eq!(req.model, "rfdetr-adr");
        match req.op {
            crate::services::runtime::local_cv::CameraCvOpLocal::Detect { frames, threshold } => {
                assert_eq!(threshold, Some(0.6));
                assert_eq!(frames.len(), 1);
                assert_eq!(&frames[0].data[..], data.as_slice());
                assert_eq!(frames[0].width, 2);
                assert_eq!(frames[0].height, 2);
            }
            _ => panic!("Oczekiwano CameraCvOpLocal::Detect"),
        }
    }

    #[test]
    fn build_camera_cv_request_rgb24_zly_rozmiar_zwraca_blad() {
        // Bufor krotszy niz width*height*3 → walidacja odrzuca klatke
        let payload = CameraCvPayload {
            model: "rfdetr-adr".to_string(),
            op: CameraCvOp::ClassifyState {
                crop: CvFrame {
                    data: vec![0u8; 5],
                    width: 2,
                    height: 2,
                    encoding: CvFrameEncoding::Rgb24,
                },
            },
        };

        let err = build_camera_cv_request(&payload).unwrap_err();
        assert!(
            err.contains("Rgb24"),
            "blad powinien wskazywac Rgb24: {err}"
        );
    }

    #[test]
    fn build_camera_cv_request_jpeg_niepoprawny_zwraca_blad() {
        // Uszkodzony strumien JPEG → dekoder image zglasza blad
        let payload = CameraCvPayload {
            model: "plate-ocr".to_string(),
            op: CameraCvOp::Ocr {
                crop: CvFrame {
                    data: vec![1, 2, 3, 4],
                    width: 1,
                    height: 1,
                    encoding: CvFrameEncoding::Jpeg,
                },
                mode: CvOcrMode::Plate,
            },
        };

        let err = build_camera_cv_request(&payload).unwrap_err();
        assert!(err.contains("JPEG"), "blad powinien wskazywac JPEG: {err}");
    }

    #[test]
    fn build_camera_cv_request_jpeg_dekoduje_do_rgb24() {
        // Poprawny JPEG (4x4, jednolity kolor) dekoduje sie do surowych pikseli
        // RGB o wymiarach z dekodera
        let mut jpeg_buf = Vec::new();
        {
            use image::ImageEncoder;
            let pixels = vec![128u8; 4 * 4 * 3];
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, 90);
            encoder
                .write_image(&pixels, 4, 4, image::ExtendedColorType::Rgb8)
                .expect("kodowanie JPEG w tescie");
        }

        let payload = CameraCvPayload {
            model: "nalepka-stan".to_string(),
            op: CameraCvOp::ClassifyState {
                crop: CvFrame {
                    data: jpeg_buf,
                    width: 4,
                    height: 4,
                    encoding: CvFrameEncoding::Jpeg,
                },
            },
        };

        let req = build_camera_cv_request(&payload).expect("JPEG powinien sie zdekodowac");
        match req.op {
            crate::services::runtime::local_cv::CameraCvOpLocal::ClassifyState { crop } => {
                assert_eq!(crop.width, 4);
                assert_eq!(crop.height, 4);
                assert_eq!(crop.data.len(), 4 * 4 * 3);
            }
            _ => panic!("Oczekiwano CameraCvOpLocal::ClassifyState"),
        }
    }

    #[test]
    fn checked_cv_rgb_bytes_waliduje_wymiary() {
        // Poprawne wymiary → dokladny rozmiar bufora RGB
        assert_eq!(checked_cv_rgb_bytes(2, 2), Ok(12));
        assert_eq!(checked_cv_rgb_bytes(1920, 1080), Ok(1920 * 1080 * 3));

        // Wymiary zerowe → odrzucone
        assert!(checked_cv_rgb_bytes(0, 100).is_err());
        assert!(checked_cv_rgb_bytes(100, 0).is_err());

        // Przepelnienie mnozenia (u32::MAX * u32::MAX * 3) → odrzucone
        assert!(checked_cv_rgb_bytes(u32::MAX, u32::MAX).is_err());

        // Tuz ponad limit 32 MiB → odrzucone; tuz pod limitem → OK
        // 3400x3300*3 = 33 660 000 > 33 554 432 (32 MiB)
        let err = checked_cv_rgb_bytes(3400, 3300).unwrap_err();
        assert!(err.contains("przekracza limit"), "blad limitu: {err}");
        assert!(checked_cv_rgb_bytes(3300, 3300).is_ok());
    }

    #[test]
    fn decode_cv_frame_jpeg_ponad_limit_odrzucony_z_naglowka() {
        // JPEG o wymiarach przekraczajacych MAX_CV_RGB_BYTES musi zostac
        // odrzucony na podstawie naglowka — przed pelnym dekodowaniem
        let mut jpeg_buf = Vec::new();
        {
            use image::ImageEncoder;
            let pixels = vec![128u8; 3400 * 3300 * 3];
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, 30);
            encoder
                .write_image(&pixels, 3400, 3300, image::ExtendedColorType::Rgb8)
                .expect("kodowanie JPEG w tescie");
        }

        let frame = CvFrame {
            data: jpeg_buf,
            width: 3400,
            height: 3300,
            encoding: CvFrameEncoding::Jpeg,
        };
        let err = decode_cv_frame(&frame).unwrap_err();
        assert!(err.contains("przekracza limit"), "blad limitu: {err}");
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
                reasoning_content: None,
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
                reasoning_content: None,
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
    // Sidecar policy (CR-002): a service that opened a reverse stream may only
    // drive the meeting it was spawned for, and may not use Core as an
    // anonymous inference proxy. Mesh peers (`caller = None`) are unaffected.
    // =========================================================================

    /// Router with a database and a session owned by `meeting-bot-1`.
    fn policy_router() -> (Router, crate::db::DbPool) {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = crate::db::init(&path).expect("init db");
        let id =
            crate::db::repository::transcripts::get_or_create_session(&db, "mtg-owned", None, None)
                .expect("session");
        crate::db::repository::transcripts::update_session_spawned_native(
            &db,
            id,
            "meeting-bot-1",
            41000,
            "endpoint",
            "secret",
            "teams",
            None,
        )
        .expect("mark spawned");
        let router =
            Router::new(crate::config::RouterConfig::default(), Some(db.clone())).expect("router");
        (router, db)
    }

    /// Summaries stored for a meeting key (0 when the session does not exist).
    fn summary_count(db: &crate::db::DbPool, meeting_key: &str) -> usize {
        let Some(session) =
            crate::db::repository::transcripts::get_session_by_meeting_key(db, meeting_key)
                .expect("session lookup")
        else {
            return 0;
        };
        crate::db::repository::transcripts::list_summaries_for_meeting(db, session.id, 100)
            .expect("list summaries")
            .len()
    }

    fn bot_caller() -> ReverseCaller {
        ReverseCaller {
            service_name: "meeting-bot-1".to_string(),
        }
    }

    fn meeting_event_request(meeting_key: &str) -> ModelRequest {
        ModelRequest {
            request_id: "ev-1".to_string(),
            payload: ModelPayload::MeetingEvent(MeetingEventData {
                meeting_key: meeting_key.to_string(),
                timestamp_ms: 1_700_000_000_000,
                payload: MeetingEventPayload::SummaryUpdate {
                    decisions_text: "D".to_string(),
                    summary_text: "S".to_string(),
                    model: "qwen".to_string(),
                },
            }),
            stream: false,
            metadata: None,
            session_id: None,
        }
    }

    fn error_of(response: &ModelResponse) -> &ErrorInfo {
        match &response.result {
            ModelResult::Error(e) => e,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    // A sidecar pushing an event onto somebody else's meeting is an injection
    // attempt: the meeting exists (or not) — either way it is not this bot's.
    #[tokio::test]
    async fn sidecar_meeting_event_for_foreign_meeting_is_refused() {
        let (router, db) = policy_router();
        crate::db::repository::transcripts::get_or_create_session(&db, "mtg-other", None, None)
            .expect("other session");
        let response = dispatch_reverse_request(
            &router,
            meeting_event_request("mtg-other"),
            Some(&bot_caller()),
        )
        .await;
        let err = error_of(&response);
        assert!(matches!(err.error_type, ErrorType::Unauthorized));
        assert!(
            err.message.contains("does not belong to service"),
            "got: {}",
            err.message
        );
        assert_eq!(
            summary_count(&db, "mtg-other"),
            0,
            "the foreign meeting must stay untouched"
        );
    }

    // Its own meeting still works — the gate is ownership, not a blanket ban.
    #[tokio::test]
    async fn sidecar_meeting_event_for_own_meeting_is_accepted() {
        let (router, db) = policy_router();
        let response = dispatch_reverse_request(
            &router,
            meeting_event_request("mtg-owned"),
            Some(&bot_caller()),
        )
        .await;
        assert!(
            matches!(response.result, ModelResult::Completion(_)),
            "own meeting must pass: {:?}",
            response.result
        );
        assert_eq!(summary_count(&db, "mtg-owned"), 1);
    }

    // Completion without the owned meeting_id = Core as a free inference proxy.
    #[tokio::test]
    async fn sidecar_completion_without_meeting_binding_is_refused() {
        let (router, _db) = policy_router();
        let request = ModelRequest {
            request_id: "cmp-1".to_string(),
            payload: ModelPayload::Completion(CompletionPayload {
                model: "any-model".to_string(),
                prompt: Some("leak everything".to_string()),
                messages: Vec::new(),
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
            }),
            stream: false,
            metadata: None,
            session_id: None,
        };
        let response = dispatch_reverse_request(&router, request, Some(&bot_caller())).await;
        let err = error_of(&response);
        assert!(matches!(err.error_type, ErrorType::Unauthorized));
        assert!(err.message.contains("meeting_id"), "got: {}", err.message);
    }

    // Payloads the bot never sends are refused outright, with no backend call.
    #[tokio::test]
    async fn sidecar_embeddings_payload_is_refused() {
        let (router, _db) = policy_router();
        let request = ModelRequest {
            request_id: "emb-1".to_string(),
            payload: ModelPayload::Embeddings(EmbeddingsPayload {
                model: "any-embed".to_string(),
                input: vec!["free compute".to_string()],
                normalize: true,
            }),
            stream: false,
            metadata: Some(vec![("meeting_id".to_string(), "mtg-owned".to_string())]),
            session_id: None,
        };
        let response = dispatch_reverse_request(&router, request, Some(&bot_caller())).await;
        let err = error_of(&response);
        assert!(matches!(err.error_type, ErrorType::Unauthorized));
        assert!(
            err.message.contains("is not accepted from service"),
            "got: {}",
            err.message
        );
    }

    // Mesh forwarding keeps the pre-policy behaviour: no caller, no gate.
    #[tokio::test]
    async fn mesh_caller_none_keeps_meeting_event_behaviour() {
        let (router, db) = policy_router();
        let response =
            dispatch_reverse_request(&router, meeting_event_request("mtg-from-peer"), None).await;
        assert!(
            matches!(response.result, ModelResult::Completion(_)),
            "mesh peer must not be gated: {:?}",
            response.result
        );
        assert_eq!(
            summary_count(&db, "mtg-from-peer"),
            1,
            "mesh path still persists the event"
        );
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
        let conn = db.read().unwrap();
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
            let conn = db.write().unwrap();
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
            let conn = db.write().unwrap();
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
