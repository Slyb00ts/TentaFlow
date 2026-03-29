// =============================================================================
// Plik: api/dashboard/api_chat.rs
// Opis: Endpointy chat playground - completions, TTS, STT, capabilities.
// =============================================================================

use crate::db::{self, DbPool};
use crate::metrics::RouterMetrics;
use crate::api::openai::types::{
    ChatCompletionRequest, TTSRequest, TranscriptionRequest,
};
use crate::routing::router::Router;

use http_body_util::{Full, Either, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::{Method, Response, StatusCode};
use std::pin::Pin;
use std::sync::Arc;
use futures::{Stream, StreamExt};
use tracing::{error, info, warn};

type SseStream = Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>>;
type DashboardBody = Either<Full<Bytes>, StreamBody<SseStream>>;

/// Routuje requesty chat playground do odpowiednich handlerow
pub async fn route_chat_api(
    method: &Method,
    path: &str,
    router: &Arc<Router>,
    body: Bytes,
    db: &DbPool,
    metrics: &Arc<RouterMetrics>,
    cors_origin: Option<&str>,
) -> Response<DashboardBody> {
    match (method, path) {
        (&Method::POST, "/api/chat/completions") => handle_completions(router, body, db, metrics, cors_origin).await,
        (&Method::POST, "/api/chat/tts") => handle_tts(router, body, cors_origin).await,
        (&Method::POST, "/api/chat/stt") => handle_stt(router, body, cors_origin).await,
        (&Method::GET, "/api/chat/capabilities") => handle_capabilities(router, db, cors_origin).await,
        _ => json_err(404, "Nieznany endpoint chat", cors_origin),
    }
}

/// POST /api/chat/completions - chat completion (streaming lub nie)
async fn handle_completions(router: &Arc<Router>, body: Bytes, _db: &DbPool, metrics: &Arc<RouterMetrics>, cors_origin: Option<&str>) -> Response<DashboardBody> {
    let request: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return json_err(400, &format!("Blad parsowania requestu: {}", e), cors_origin),
    };

    info!("Chat completions: model='{}', stream={}", request.model, request.stream);

    metrics.record_request();

    // Estymacja tokenow wejsciowych (bajty body / 4)
    let estimated_input_tokens = (body.len() / 4).max(1) as u64;
    metrics.record_tokens(estimated_input_tokens, 0);

    let stream = request.stream;

    if stream {
        match router.route_chat_completion_stream(request).await {
            Ok(chunk_stream) => {
                let metrics_done = metrics.clone();
                let metrics_stream = metrics.clone();
                let sse_stream = chunk_stream.map(move |chunk_result| {
                    match chunk_result {
                        Ok(chunk) => {
                            let content_len = chunk.choices.first()
                                .and_then(|c| c.delta.content.as_ref())
                                .map(|s| s.len())
                                .unwrap_or(0);
                            if content_len > 0 {
                                let estimate = (content_len / 4).max(1) as u64;
                                metrics_stream.record_tokens(0, estimate);
                            }
                            let json = serde_json::to_string(&chunk).unwrap_or_default();
                            let sse_line = format!("data: {}\n\n", json);
                            Ok(Frame::data(Bytes::from(sse_line)))
                        }
                        Err(e) => {
                            let error_chunk = format!("data: {{\"error\": \"{}\"}}\n\n", e);
                            Ok(Frame::data(Bytes::from(error_chunk)))
                        }
                    }
                }).chain(futures::stream::once(async move {
                    metrics_done.record_request_done();
                    Ok(Frame::data(Bytes::from("data: [DONE]\n\n")))
                }));

                let boxed_stream: SseStream = Box::pin(sse_stream);
                let mut builder = Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "text/event-stream")
                    .header("Cache-Control", "no-cache")
                    .header("Connection", "keep-alive");
                if let Some(origin) = cors_origin {
                    builder = builder
                        .header("Access-Control-Allow-Origin", origin)
                        .header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
                        .header("Access-Control-Allow-Headers", "Content-Type, Authorization");
                }
                builder
                    .body(Either::Right(StreamBody::new(boxed_stream)))
                    .unwrap()
            }
            Err(e) => {
                metrics.record_request_done();
                metrics.record_error();
                error!("Blad streaming completion: {}", e);
                json_err(500, &format!("Blad streaming: {}", e), cors_origin)
            }
        }
    } else {
        match router.route_chat_completion(request).await {
            Ok(response) => {
                metrics.record_request_done();
                let json = match serde_json::to_string(&response) {
                    Ok(j) => j,
                    Err(e) => return json_err(500, &format!("Blad serializacji odpowiedzi: {}", e), cors_origin),
                };
                // Estymacja tokenow wyjsciowych z odpowiedzi
                let out_estimate = (json.len() / 4).max(1) as u64;
                metrics.record_tokens(0, out_estimate);
                json_resp(200, json, cors_origin)
            }
            Err(e) => {
                metrics.record_request_done();
                metrics.record_error();
                error!("Blad chat completion: {}", e);
                json_err(500, &format!("Blad completion: {}", e), cors_origin)
            }
        }
    }
}

/// POST /api/chat/tts - synteza mowy
async fn handle_tts(router: &Arc<Router>, body: Bytes, cors_origin: Option<&str>) -> Response<DashboardBody> {
    let request: TTSRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return json_err(400, &format!("Blad parsowania requestu TTS: {}", e), cors_origin),
    };

    // Okresl content-type na podstawie response_format
    let content_type = match request.response_format.as_deref() {
        Some("opus") => "audio/opus",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("pcm") => "audio/pcm",
        _ => "audio/mpeg",
    };

    match router.synthesize_speech(&request).await {
        Ok(audio_bytes) => binary_resp(200, content_type, audio_bytes, cors_origin),
        Err(e) => {
            error!("Blad syntezy mowy: {}", e);
            json_err(500, &format!("Blad TTS: {}", e), cors_origin)
        }
    }
}

/// POST /api/chat/stt - transkrypcja mowy na tekst
async fn handle_stt(router: &Arc<Router>, body: Bytes, cors_origin: Option<&str>) -> Response<DashboardBody> {
    // Deserializacja JSON z danymi audio w base64
    #[derive(serde::Deserialize)]
    struct SttPayload {
        audio: String,
        model: String,
        #[serde(default = "default_language")]
        language: String,
    }

    fn default_language() -> String {
        "pl".to_string()
    }

    let payload: SttPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return json_err(400, &format!("Blad parsowania requestu STT: {}", e), cors_origin),
    };

    // Dekodowanie base64 audio
    use base64::Engine;
    let decoded_bytes = match base64::engine::general_purpose::STANDARD.decode(&payload.audio) {
        Ok(bytes) => bytes,
        Err(e) => return json_err(400, &format!("Blad dekodowania base64 audio: {}", e), cors_origin),
    };

    let request = TranscriptionRequest {
        file: decoded_bytes,
        filename: "audio.webm".to_string(),
        model: payload.model,
        language: Some(payload.language),
        prompt: None,
        response_format: None,
        temperature: None,
        timestamp_granularities: None,
        no_speech_threshold: None,
        avg_logprob_threshold: None,
        compression_ratio_threshold: None,
    };

    match router.route_audio_transcription(request).await {
        Ok(transcription) => {
            let json = match serde_json::to_string(&transcription) {
                Ok(j) => j,
                Err(e) => return json_err(500, &format!("Blad serializacji transkrypcji: {}", e), cors_origin),
            };
            json_resp(200, json, cors_origin)
        }
        Err(e) => {
            error!("Blad transkrypcji audio: {}", e);
            json_err(500, &format!("Blad STT: {}", e), cors_origin)
        }
    }
}

/// GET /api/chat/capabilities - dostepne uslugi i modele
async fn handle_capabilities(router: &Arc<Router>, db: &DbPool, cors_origin: Option<&str>) -> Response<DashboardBody> {
    let sm = router.service_manager();

    let has_llm = sm.has_service_backends();
    let has_tts = sm.get_first_tts_service_name().is_some();
    let has_stt = sm.get_first_stt_service_name().is_some();

    // Pobierz modele z bazy danych
    let models = match db::repository::list_model_entries(db, 0, 200) {
        Ok(entries) => entries
            .into_iter()
            .filter(|m| m.is_active)
            .map(|m| serde_json::json!({
                "id": m.id,
                "model_name": m.model_name,
                "display_name": m.display_name,
                "service_type": m.service_type,
                "is_active": m.is_active,
            }))
            .collect::<Vec<_>>(),
        Err(e) => {
            warn!("Nie udalo sie pobrac modeli: {}", e);
            vec![]
        }
    };

    // Pobierz serwisy z bazy danych
    let services = match db::repository::list_services(db) {
        Ok(svcs) => svcs
            .into_iter()
            .map(|s| serde_json::json!({
                "id": s.id,
                "name": s.name,
                "service_type": s.service_type,
                "status": s.status,
            }))
            .collect::<Vec<_>>(),
        Err(e) => {
            warn!("Nie udalo sie pobrac serwisow: {}", e);
            vec![]
        }
    };

    let capabilities = serde_json::json!({
        "llm": has_llm,
        "tts": has_tts,
        "stt": has_stt,
        "vision": true,
        "models": models,
        "services": services,
    });

    json_resp(200, capabilities.to_string(), cors_origin)
}

// =============================================================================
// Helpery do tworzenia odpowiedzi HTTP
// =============================================================================

fn json_resp(status: u16, body: String, cors_origin: Option<&str>) -> Response<DashboardBody> {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("Content-Type", "application/json");
    if let Some(origin) = cors_origin {
        builder = builder
            .header("Access-Control-Allow-Origin", origin)
            .header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
            .header("Access-Control-Allow-Headers", "Content-Type, Authorization");
    }
    builder
        .body(Either::Left(Full::new(Bytes::from(body.into_bytes()))))
        .unwrap()
}

fn json_err(status: u16, msg: &str, cors_origin: Option<&str>) -> Response<DashboardBody> {
    json_resp(status, serde_json::json!({"error": msg}).to_string(), cors_origin)
}

fn binary_resp(status: u16, content_type: &str, data: Vec<u8>, cors_origin: Option<&str>) -> Response<DashboardBody> {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("Content-Type", content_type);
    if let Some(origin) = cors_origin {
        builder = builder.header("Access-Control-Allow-Origin", origin);
    }
    builder
        .body(Either::Left(Full::new(Bytes::from(data))))
        .unwrap()
}
