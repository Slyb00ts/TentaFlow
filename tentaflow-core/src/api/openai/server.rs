// =============================================================================
// Plik: api/openai/server.rs
// Opis: HTTP server obslugujacy OpenAI API protocol. Przyjmuje requesty od klientow
//       na endpointy (/v1/chat/completions, /v1/images/generations, etc.),
//       parsuje je, przekazuje do routera, i zwraca odpowiedzi.
// =============================================================================

use crate::api::openai::types::*;
use crate::config::ProtocolConfig;
use crate::error::{CoreError, Result};
use crate::routing::router::Router;

use futures::TryStreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{body::Incoming, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use std::pin::Pin;
use std::sync::Arc;

// Dla SSE streaming
use futures::{Stream, StreamExt};
use hyper::body::{Bytes, Frame};

/// Typ body odpowiedzi OpenAI API (stream SSE lub jednorazowy JSON)
pub type OpenAIBody = StreamBody<
    Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>>,
>;

/// Tworzy error response z podanym statusem, typem bledu i wiadomoscia.
fn error_response(status: StatusCode, error_type: &str, message: String) -> Response<OpenAIBody> {
    let error = ErrorResponse {
        error: ErrorDetail {
            error_type: error_type.to_string(),
            message,
            param: None,
            code: Some(error_type.to_string()),
        },
    };
    let body = serde_json::to_vec(&error).unwrap();
    json_response(status, body)
}

/// Tworzy JSON response z podanym statusem i body.
fn json_response(status: StatusCode, body: Vec<u8>) -> Response<OpenAIBody> {
    let stream = futures::stream::once(async move { Ok(Frame::data(Bytes::from(body))) });
    let boxed_stream: Pin<
        Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(stream);
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(StreamBody::new(boxed_stream))
        .unwrap()
}

/// Mapuje dowolny anyhow::Error (potencjalnie CoreError) na error response z odpowiednim HTTP status.
fn core_error_to_response(e: &anyhow::Error) -> Response<OpenAIBody> {
    let core_error = e.downcast_ref::<CoreError>();
    if let Some(err) = core_error {
        let status = StatusCode::from_u16(err.status_code()).unwrap();
        let error_type = match err {
            CoreError::ModelNotFound { .. } => "model_not_found",
            CoreError::InvalidRequest { .. } => "invalid_request_error",
            CoreError::AllBackendsUnavailable { .. } => "service_unavailable",
            CoreError::Timeout { .. } => "timeout_error",
            _ => "internal_error",
        };
        error_response(status, error_type, err.to_string())
    } else {
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )
    }
}

/// HTTP Server dla OpenAI API Protocol
pub struct OpenAIServer {
    /// Konfiguracja protokolu
    config: ProtocolConfig,

    /// Router do routing requestow
    router: Arc<Router>,
}

impl OpenAIServer {
    /// Tworzy nowy serwer OpenAI API.
    ///
    /// Waliduje konfiguracje (bind address musi byc poprawny).
    ///
    /// Parametry:
    /// - config: Konfiguracja protokolu OpenAI API
    /// - router: Router do routing requestow do backendow
    pub fn new(config: ProtocolConfig, router: Arc<Router>) -> Result<Self> {
        if !config.enabled {
            return Err(CoreError::ConfigError {
                message: "OpenAI API protocol jest wylaczony".to_string(),
                source: anyhow::anyhow!("enabled = false"),
            }
            .into());
        }

        Ok(Self { config, router })
    }

    /// Uruchamia serwer HTTP.
    ///
    /// Funkcja blokuje do momentu otrzymania sygnalu shutdown lub bledu.
    /// Uzywa Hyper 1.x API z TcpListener.
    pub async fn run(self) -> Result<()> {
        let addr = self.config.bind.clone();
        info!("Uruchamianie OpenAI API server na {}", addr);

        // Bind TCP listener
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("Nie mozna zbindowac na adresie {}", addr),
                source: e.into(),
            })?;

        info!("OpenAI API server nasluchuje na {}", addr);

        // Clone router dla kazdego connection (Arc - cheap)
        let router = self.router.clone();

        // Accept loop - przyjmujemy polaczenia
        loop {
            let (stream, remote_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!("Blad akceptowania polaczenia: {}", e);
                    continue;
                }
            };

            debug!("Nowe polaczenie od {}", remote_addr);

            // Clone router dla tego connection
            let router_clone = router.clone();

            // Spawn task dla kazdego polaczenia (concurrency)
            tokio::spawn(async move {
                // Wrap stream dla Hyper
                let io = TokioIo::new(stream);

                // Service function - obsluguje requesty
                // Capture router w closure
                let service = service_fn(move |req| {
                    let router = router_clone.clone();
                    async move { handle_request(req, router).await }
                });

                // Serve connection (HTTP/1.1)
                if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                    error!("Blad obslugi polaczenia: {}", e);
                }
            });
        }
    }
}

/// Obsluguje pojedynczy HTTP request.
///
/// Parsuje method, path, headers, body i kieruje do odpowiedniego handlera.
pub async fn handle_request(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>>,
        >,
    >,
    hyper::Error,
> {
    let method = req.method();
    let path = req.uri().path();

    debug!("{} {}", method, path);

    // Routing na podstawie path
    let response = match (method.as_str(), path) {
        // Chat completions (text & vision)
        ("POST", "/v1/chat/completions") => handle_chat_completions(req, router).await,

        // Image generation
        ("POST", "/v1/images/generations") => handle_image_generation(req).await,

        // Audio TTS
        ("POST", "/v1/audio/speech") => handle_audio_tts(req, router).await,

        // Audio STT (Whisper)
        ("POST", "/v1/audio/transcriptions") => handle_audio_transcriptions(req, router).await,

        // Embeddings
        ("POST", "/v1/embeddings") => handle_embeddings(req, router).await,

        // Health check (dla load balancerow)
        ("GET", "/health") | ("GET", "/v1/health") => Ok(json_response(
            StatusCode::OK,
            br#"{"status":"ok"}"#.to_vec(),
        )),

        // Readiness check - zwraca 200 jesli >=1 backend zdrowy
        ("GET", "/ready") | ("GET", "/v1/ready") => handle_readiness_check(router).await,

        // Lista dostepnych modeli
        ("GET", "/v1/models") => handle_models_list(router).await,

        // Prometheus metrics
        ("GET", "/metrics") => handle_metrics(router).await,

        // 404 Not Found
        _ => {
            warn!("Nieznany endpoint: {} {}", method, path);
            Ok(error_response(
                StatusCode::NOT_FOUND,
                "endpoint_not_found",
                format!("Nieznany endpoint: {} {}", method, path),
            ))
        }
    };

    response
}

/// Handler dla /v1/chat/completions
///
/// Obsluguje zarowno non-streaming (JSON response) jak i streaming (SSE).
async fn handle_chat_completions(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            std::pin::Pin<
                Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
            >,
        >,
    >,
    hyper::Error,
> {
    let debug_route = is_debug_route_openai(req.headers(), req.uri());
    // Etap 2: trailers opt-in. Klient z `X-Want-Trailers: true` dostaje
    // dodatkowe `X-Tentaflow-{Latency-Ms,*Tokens,Finish-Reason}` headery
    // wyciągnięte z `RouteMetadata` po blocking response.
    let want_trailers = wants_trailers(req.headers());
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();

    // Czytamy body
    let body_bytes = req.collect().await?.to_bytes();

    // Parsujemy JSON
    let request: ChatCompletionRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!("Blad parsowania JSON: {}", e);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                format!("Niepoprawny JSON: {}", e),
            ));
        }
    };

    let is_streaming = request.stream;
    debug!(
        "Chat completion request: model={}, stream={}",
        request.model, is_streaming
    );

    if is_streaming {
        // === STREAMING MODE: SSE ===
        match router
            .route_chat_completion_stream(request, user_ctx.clone())
            .await
        {
            Ok(route_result) => {
                let metadata = route_result.metadata;
                let chunk_stream = route_result.response;

                // SSE event route_info przed pierwszym chunkiem (tylko w trybie debug)
                let route_info_event = if debug_route {
                    serde_json::to_string(&metadata)
                        .ok()
                        .map(|json| format!("event: route_info\ndata: {}\n\n", json))
                } else {
                    None
                };

                let prefix_stream = futures::stream::iter(
                    route_info_event
                        .into_iter()
                        .map(|event| Ok(Frame::data(Bytes::from(event)))),
                );

                // Konwertuj Stream<Result<ChatCompletionChunk>> -> Stream SSE
                let sse_stream = prefix_stream
                    .chain(chunk_stream.map(|chunk_result| {
                        match chunk_result {
                            Ok(mut chunk) => {
                                // Normalizuj reasoning_content -> content dla kompatybilnosci z OpenAI API
                                for choice in &mut chunk.choices {
                                    if choice.delta.reasoning_content.is_some()
                                        && choice.delta.content.is_none()
                                    {
                                        choice.delta.content =
                                            choice.delta.reasoning_content.take();
                                    }
                                }

                                let json = serde_json::to_string(&chunk).unwrap();
                                let sse_line = format!("data: {}\n\n", json);
                                Ok(Frame::data(Bytes::from(sse_line)))
                            }
                            Err(e) => {
                                error!("Blad w streaming chunk: {}", e);
                                let error_chunk = format!("data: {{\"error\": \"{}\"}}\n\n", e);
                                Ok(Frame::data(Bytes::from(error_chunk)))
                            }
                        }
                    }))
                    .chain(futures::stream::once(async {
                        Ok(Frame::data(Bytes::from("data: [DONE]\n\n")))
                    }));

                let boxed_stream: Pin<
                    Box<
                        dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send,
                    >,
                > = Box::pin(sse_stream);
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "text/event-stream")
                    .header("Cache-Control", "no-cache")
                    .header("Connection", "keep-alive")
                    .body(StreamBody::new(boxed_stream))
                    .unwrap())
            }
            Err(e) => {
                error!("Blad routing (streaming): {}", e);
                Ok(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    e.to_string(),
                ))
            }
        }
    } else {
        // === NON-STREAMING MODE: JSON ===
        match router
            .route_chat_completion(request, user_ctx)
            .await
        {
            Ok(route_result) => {
                let body = serde_json::to_vec(&route_result.response).unwrap();
                let mut resp = json_response(StatusCode::OK, body);
                if debug_route {
                    if let Ok(meta_json) = serde_json::to_string(&route_result.metadata) {
                        resp.headers_mut().insert(
                            "X-TentaFlow-Route",
                            meta_json
                                .parse()
                                .unwrap_or_else(|_| hyper::http::HeaderValue::from_static("")),
                        );
                    }
                }
                if want_trailers {
                    emit_trailer_headers(resp.headers_mut(), &route_result.metadata);
                }
                Ok(resp)
            }
            Err(e) => {
                error!("Blad routing: {}", e);
                Ok(core_error_to_response(&e))
            }
        }
    }
}

/// Handler dla /v1/images/generations (placeholder)
async fn handle_image_generation(
    _req: Request<Incoming>,
) -> std::result::Result<
    Response<
        StreamBody<
            std::pin::Pin<
                Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
            >,
        >,
    >,
    hyper::Error,
> {
    Ok(error_response(
        StatusCode::NOT_IMPLEMENTED,
        "not_implemented",
        "Image generation nie jest jeszcze zaimplementowane".to_string(),
    ))
}

/// Handler dla /v1/audio/speech (Text-to-Speech)
///
/// Obsluguje backendy:
/// - QUIC TTS (TentaFlow.TTS z rkyv) - preferowany
/// - HTTP TTS (OpenAI API kompatybilny)
async fn handle_audio_tts(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            std::pin::Pin<
                Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
            >,
        >,
    >,
    hyper::Error,
> {
    let debug_route = is_debug_route_openai(req.headers(), req.uri());
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();

    // Parsuj body jako JSON
    let body_bytes = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Nie mozna odczytac body: {}", e),
            ));
        }
    };

    let mut tts_request: TTSRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Niepoprawny format JSON: {}", e),
            ));
        }
    };

    // Rozwiazanie pola `language`: body klienta -> preferencja uzytkownika -> "en".
    // Pozwala wymusic jezyk per-request, a jesli brak — backend dostaje jawna
    // wartosc zamiast polegac na ukrytym domyslnym ustawieniu silnika TTS.
    if tts_request.language.is_none() {
        if let (Some(ref ctx), Some(ref db)) = (user_ctx.as_ref(), router.db.as_ref()) {
            if let Ok(Some(lang)) =
                crate::db::repository::get_user_preferred_language(db, ctx.user_id)
            {
                tts_request.language = Some(lang);
            }
        }
    }
    if tts_request.language.is_none() {
        tts_request.language = Some("en".to_string());
    }

    info!(
        "TTS request: model={}, voice={}, input_len={}, language={:?}",
        tts_request.model,
        tts_request.voice,
        tts_request.input.len(),
        tts_request.language
    );

    // Wywolaj Router.synthesize_speech()
    match router
        .synthesize_speech_for_user(&tts_request, user_ctx)
        .await
    {
        Ok(route_result) => {
            let audio_bytes = route_result.response.bytes;
            // Codex R3b.4 M2: pick Content-Type from the **actual** format
            // the executor reports, not from the request hint. Embedded
            // engines may emit WAV even when the client asked for `mp3`;
            // the requested-format header would be a lie.
            let content_type = match route_result.response.format.as_str() {
                "mp3" => "audio/mpeg",
                "opus" => "audio/opus",
                "aac" => "audio/aac",
                "flac" => "audio/flac",
                "wav" | "pcm" => "audio/wav",
                other => {
                    warn!("Unknown audio format '{}', defaulting to audio/wav", other);
                    "audio/wav"
                }
            };

            info!(
                "TTS response: {} bytes, format={}",
                audio_bytes.len(),
                content_type
            );

            let stream =
                futures::stream::once(async move { Ok(Frame::data(Bytes::from(audio_bytes))) });
            let boxed_stream: Pin<
                Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
            > = Box::pin(stream);

            let mut resp = Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .body(StreamBody::new(boxed_stream))
                .unwrap();
            if debug_route {
                if let Ok(meta_json) = serde_json::to_string(&route_result.metadata) {
                    resp.headers_mut().insert(
                        "X-TentaFlow-Route",
                        meta_json
                            .parse()
                            .unwrap_or_else(|_| hyper::http::HeaderValue::from_static("")),
                    );
                    resp.headers_mut().insert(
                        "Access-Control-Expose-Headers",
                        "X-TentaFlow-Route".parse().unwrap(),
                    );
                }
            }
            Ok(resp)
        }
        Err(e) => {
            error!("TTS error: {}", e);
            Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("TTS synthesis failed: {}", e),
            ))
        }
    }
}

/// Handler dla /v1/audio/transcriptions (Speech-to-Text, Whisper)
///
/// Parsuje multipart/form-data request z plikiem audio i parametrami,
/// routuje do odpowiedniego model pool (Whisper) i zwraca transkrypcje.
async fn handle_audio_transcriptions(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            std::pin::Pin<
                Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
            >,
        >,
    >,
    hyper::Error,
> {
    let debug_route = is_debug_route_openai(req.headers(), req.uri());
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();

    // Wyciagnij Content-Type header aby sprawdzic boundary
    let content_type = match req.headers().get("content-type") {
        Some(ct) => match ct.to_str() {
            Ok(s) => s,
            Err(_) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "Niepoprawny Content-Type header".to_string(),
                ));
            }
        },
        None => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Brak Content-Type header. Oczekiwano multipart/form-data".to_string(),
            ));
        }
    };

    // Wyciagnij boundary z Content-Type
    let boundary = match multer::parse_boundary(content_type) {
        Ok(b) => b,
        Err(e) => {
            warn!("Nie mozna sparsowac boundary: {}", e);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Niepoprawny multipart boundary: {}", e),
            ));
        }
    };

    // Konwertuj body stream do formatu kompatybilnego z multer
    let stream = req
        .into_body()
        .into_data_stream()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));

    // Parse multipart
    let mut multipart = multer::Multipart::new(stream, boundary);

    // Zmienne dla pol formularza
    let mut file_data: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut model: Option<String> = None;
    let mut language: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut response_format: Option<String> = None;
    let mut temperature: Option<f32> = None;
    let mut timestamp_granularities: Option<Vec<String>> = None;
    let mut no_speech_threshold: Option<f32> = None;
    let mut avg_logprob_threshold: Option<f32> = None;
    let mut compression_ratio_threshold: Option<f32> = None;

    // Iteruj przez pola
    while let Some(field) = multipart.next_field().await.ok().flatten() {
        let field_name = field.name().unwrap_or("").to_string();

        match field_name.as_str() {
            "file" => {
                filename = field.file_name().map(|s| s.to_string());
                file_data = Some(
                    field
                        .bytes()
                        .await
                        .ok()
                        .map(|b| b.to_vec())
                        .unwrap_or_default(),
                );
            }
            "model" => {
                model = Some(field.text().await.ok().unwrap_or_default());
            }
            "language" => {
                language = Some(field.text().await.ok().unwrap_or_default());
            }
            "prompt" => {
                prompt = Some(field.text().await.ok().unwrap_or_default());
            }
            "response_format" => {
                response_format = Some(field.text().await.ok().unwrap_or_default());
            }
            "temperature" => {
                if let Ok(text) = field.text().await {
                    temperature = text.parse::<f32>().ok();
                }
            }
            "timestamp_granularities[]" | "timestamp_granularities" => {
                if let Ok(text) = field.text().await {
                    let granularities = timestamp_granularities.get_or_insert_with(Vec::new);
                    granularities.push(text);
                }
            }
            "no_speech_threshold" => {
                if let Ok(text) = field.text().await {
                    no_speech_threshold = text.parse::<f32>().ok();
                }
            }
            "avg_logprob_threshold" => {
                if let Ok(text) = field.text().await {
                    avg_logprob_threshold = text.parse::<f32>().ok();
                }
            }
            "compression_ratio_threshold" => {
                if let Ok(text) = field.text().await {
                    compression_ratio_threshold = text.parse::<f32>().ok();
                }
            }
            _ => {
                // Ignoruj nieznane pola
            }
        }
    }

    // Walidacja: file i model sa wymagane
    let file_bytes = match file_data {
        Some(data) => data,
        None => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Pole 'file' jest wymagane".to_string(),
            ));
        }
    };

    let model_name = match model {
        Some(m) => m,
        None => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Pole 'model' jest wymagane".to_string(),
            ));
        }
    };

    // Rozwiazanie pola `language`: form klienta -> preferencja uzytkownika -> brak.
    // Whisper ma w sobie language detection, wiec gdy nikt nie poda zostawiamy
    // None i silnik sam wykryje. Prefer user setting przed auto-detection
    // bo eliminuje cross-language hallucination przy krotkich nagraniach.
    if language.is_none() {
        if let (Some(ref ctx), Some(ref db)) = (user_ctx.as_ref(), router.db.as_ref()) {
            if let Ok(Some(lang)) =
                crate::db::repository::get_user_preferred_language(db, ctx.user_id)
            {
                language = Some(lang);
            }
        }
    }

    let fname = filename.unwrap_or_else(|| "audio.mp3".to_string());

    debug!(
        "Audio transcription request: model={}, file={}, size={} bytes",
        model_name,
        fname,
        file_bytes.len()
    );

    // Utworz TranscriptionRequest
    let transcription_request = TranscriptionRequest {
        file: std::sync::Arc::from(file_bytes.into_boxed_slice()),
        filename: fname,
        model: model_name.clone(),
        language,
        prompt,
        response_format: response_format.clone(),
        temperature,
        timestamp_granularities,
        no_speech_threshold,
        avg_logprob_threshold,
        compression_ratio_threshold,
        // R2d (D.3): pierwszorzedne opcje. Multipart `/v1/audio/transcriptions`
        // jeszcze nie eksponuje `speaker_identification`/`diarization` na wire,
        // wiec startujemy z domyslnymi (oba false) — kompatybilne z OpenAI API.
        options: crate::api::openai::types::SttRequestOptions::default(),
    };

    // Routuj do odpowiedniego backendu
    match router
        .route_audio_transcription_for_user(transcription_request, user_ctx)
        .await
    {
        Ok(route_result) => {
            // Zwroc odpowiedz jako JSON
            let response_json = match serde_json::to_vec(&route_result.response) {
                Ok(json) => json,
                Err(e) => {
                    error!("Blad serializacji odpowiedzi: {}", e);
                    return Ok(error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        format!("Nie mozna serializowac odpowiedzi: {}", e),
                    ));
                }
            };

            let mut resp = json_response(StatusCode::OK, response_json);
            if debug_route {
                if let Ok(meta_json) = serde_json::to_string(&route_result.metadata) {
                    resp.headers_mut().insert(
                        "X-TentaFlow-Route",
                        meta_json
                            .parse()
                            .unwrap_or_else(|_| hyper::http::HeaderValue::from_static("")),
                    );
                    resp.headers_mut().insert(
                        "Access-Control-Expose-Headers",
                        "X-TentaFlow-Route".parse().unwrap(),
                    );
                }
            }
            Ok(resp)
        }
        Err(e) => {
            error!("Blad routingu audio transcription: {}", e);
            Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "backend_error",
                format!("Blad przetwarzania audio: {}", e),
            ))
        }
    }
}

/// Handler dla /v1/embeddings
async fn handle_embeddings(
    req: Request<Incoming>,
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            std::pin::Pin<
                Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
            >,
        >,
    >,
    hyper::Error,
> {
    let debug_route = is_debug_route_openai(req.headers(), req.uri());
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();

    // Czytamy body
    let body_bytes = req.collect().await?.to_bytes();

    // Parsujemy JSON
    let request: EmbeddingRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!("Blad parsowania JSON: {}", e);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                format!("Niepoprawny JSON: {}", e),
            ));
        }
    };

    debug!("Embeddings request: model={}", request.model);

    // Routuj do odpowiedniego backendu
    match router.route_embeddings_for_user(request, user_ctx).await {
        Ok(route_result) => {
            let body = serde_json::to_vec(&route_result.response).unwrap();
            let mut resp = json_response(StatusCode::OK, body);
            if debug_route {
                if let Ok(meta_json) = serde_json::to_string(&route_result.metadata) {
                    resp.headers_mut().insert(
                        "X-TentaFlow-Route",
                        meta_json
                            .parse()
                            .unwrap_or_else(|_| hyper::http::HeaderValue::from_static("")),
                    );
                    resp.headers_mut().insert(
                        "Access-Control-Expose-Headers",
                        "X-TentaFlow-Route".parse().unwrap(),
                    );
                }
            }
            Ok(resp)
        }
        Err(e) => {
            error!("Blad routing embeddings: {}", e);
            Ok(core_error_to_response(&e))
        }
    }
}
/// Handler dla /v1/documents (document ingestion)
async fn handle_readiness_check(
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>>,
        >,
    >,
    hyper::Error,
> {
    // Sprawdz czy jest dostepny jakikolwiek backend
    let is_ready = router.has_healthy_backends();

    if is_ready {
        Ok(json_response(
            StatusCode::OK,
            br#"{"status":"ready"}"#.to_vec(),
        ))
    } else {
        Ok(json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            br#"{"status":"unavailable","error":"No healthy backends available"}"#.to_vec(),
        ))
    }
}

// =============================================================================
// MODELS LIST HANDLER
// =============================================================================
// Zwraca liste dostepnych modeli w formacie OpenAI API

async fn handle_models_list(
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>>,
        >,
    >,
    hyper::Error,
> {
    // Pull straight from the catalog so every advertised id keeps its
    // kind-specific `owned_by` tag (`tentaflow-service` / `tentaflow-flow` /
    // `tentaflow-alias`) instead of being flattened to a single string.
    let snapshot = router.catalog_snapshot();

    #[derive(serde::Serialize)]
    struct ModelObject {
        id: String,
        object: String,
        created: i64,
        owned_by: String,
    }

    #[derive(serde::Serialize)]
    struct ModelsListResponse {
        object: String,
        data: Vec<ModelObject>,
    }

    let mut model_objects: Vec<ModelObject> = snapshot
        .advertised_entries()
        .map(|entry| ModelObject {
            id: entry.id.clone(),
            object: "model".to_string(),
            created: 1686935002,
            owned_by: entry.owned_by().to_string(),
        })
        .collect();
    model_objects.sort_by(|a, b| a.id.cmp(&b.id));

    let response = ModelsListResponse {
        object: "list".to_string(),
        data: model_objects,
    };

    let body = serde_json::to_vec(&response).unwrap();
    Ok(json_response(StatusCode::OK, body))
}

// =============================================================================
// PROMETHEUS METRICS HANDLER
// =============================================================================
// Zwraca metryki w formacie Prometheus

async fn handle_metrics(
    router: Arc<Router>,
) -> std::result::Result<
    Response<
        StreamBody<
            Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>>,
        >,
    >,
    hyper::Error,
> {
    let metrics = router.get_metrics();

    // Format Prometheus text format
    let mut output = String::new();
    output.push_str("# HELP tentaflow_router_info Router information\n");
    output.push_str("# TYPE tentaflow_router_info gauge\n");
    output.push_str("tentaflow_router_info{version=\"0.1.0\"} 1\n\n");

    // Backend health metrics
    output.push_str(
        "# HELP tentaflow_ai_backend_healthy Backend health status (1=healthy, 0=unhealthy)\n",
    );
    output.push_str("# TYPE tentaflow_ai_backend_healthy gauge\n");
    for (model_name, backend_metrics) in &metrics.backends {
        for (backend_idx, backend_metric) in backend_metrics.iter().enumerate() {
            let health_value = if backend_metric.is_healthy { 1 } else { 0 };
            output.push_str(&format!(
                "tentaflow_ai_backend_healthy{{model=\"{}\",backend=\"{}\"}} {}\n",
                model_name, backend_idx, health_value
            ));
        }
    }
    output.push_str("\n");

    // Request counters
    output.push_str("# HELP tentaflow_ai_requests_total Total number of requests\n");
    output.push_str("# TYPE tentaflow_ai_requests_total counter\n");
    output.push_str(&format!(
        "tentaflow_ai_requests_total{{}} {}\n\n",
        metrics.total_requests
    ));

    // Active connections
    output
        .push_str("# HELP tentaflow_ai_active_connections Current number of active connections\n");
    output.push_str("# TYPE tentaflow_ai_active_connections gauge\n");
    output.push_str(&format!(
        "tentaflow_ai_active_connections{{}} {}\n\n",
        metrics.active_connections
    ));

    // WSS handler metrics (per MessageBody variant). Lazy-init w
    // dispatch::metrics gdy ktorykolwiek handler bedzie wywolany.
    output.push_str(&crate::dispatch::metrics::render_prometheus());

    let body = hyper::body::Bytes::from(output);
    let stream = futures::stream::once(async move { Ok(Frame::data(body)) });
    let boxed_stream: Pin<
        Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(stream);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain; version=0.0.4")
        .body(StreamBody::new(boxed_stream))
        .unwrap())
}

/// Sprawdza czy request ma wlaczony debug routing (header lub query param)
fn is_debug_route_openai(headers: &hyper::header::HeaderMap, uri: &hyper::Uri) -> bool {
    let has_header = headers
        .get("x-tentaflow-debug")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |v| v == "true");
    let has_query = uri.query().map_or(false, |q| q.contains("debug=route"));
    has_header || has_query
}

/// Etap 2: czy klient prosi o trailery (`X-Want-Trailers: true`)? Streaming
/// SSE ignoruje to dziś (HTTP/2 trailers wraca w stage 3) — używane tylko
/// dla blocking response (chat / embeddings non-stream).
fn wants_trailers(headers: &hyper::header::HeaderMap) -> bool {
    headers
        .get("x-want-trailers")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Emituje `X-Tentaflow-*` trailer-friendly headery z `RouteMetadata` na
/// response. Używane tylko gdy `wants_trailers(req)` zwróci true.
fn emit_trailer_headers(
    headers: &mut hyper::header::HeaderMap,
    metadata: &crate::routing::RouteMetadata,
) {
    if let Some(latency) = metadata.latency_ms {
        if let Ok(v) = (latency as u64).to_string().parse() {
            headers.insert("x-tentaflow-latency-ms", v);
        }
    }
    if let Some(usage) = metadata.usage.as_ref() {
        if let Ok(v) = usage.prompt_tokens.to_string().parse() {
            headers.insert("x-tentaflow-prompt-tokens", v);
        }
        if let Ok(v) = usage.completion_tokens.to_string().parse() {
            headers.insert("x-tentaflow-completion-tokens", v);
        }
        if let Ok(v) = usage.total_tokens.to_string().parse() {
            headers.insert("x-tentaflow-total-tokens", v);
        }
    }
    if let Some(fr) = metadata.finish_reason.as_deref() {
        if let Ok(v) = fr.parse() {
            headers.insert("x-tentaflow-finish-reason", v);
        }
    }
}
