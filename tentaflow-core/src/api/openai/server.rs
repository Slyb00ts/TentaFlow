// =============================================================================
// Plik: api/openai/server.rs
// Opis: HTTP server obslugujacy OpenAI API protocol. Przyjmuje requesty od klientow
//       na endpointy (/v1/chat/completions, /v1/images/generations, etc.),
//       parsuje je, przekazuje do routera, i zwraca odpowiedzi.
// =============================================================================

use crate::api::openai::types::*;
use crate::auth::acl::Principal;
use crate::config::ProtocolConfig;
use crate::db::DbPool;
use crate::error::{CoreError, Result};
use crate::routing::router::Router;
use crate::services::catalog::{CatalogEntryKind, CatalogSnapshot};

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

/// resource_type string used in `resource_permissions` for a catalog entry,
/// derived from its kind. Canonical resource_id is always `entry.id` (model id,
/// catalog flow id, alias id) — never a published alias/name.
fn resource_type_for_kind(kind: &CatalogEntryKind) -> &'static str {
    match kind {
        CatalogEntryKind::ServiceModel { .. } => "model",
        CatalogEntryKind::Flow { .. } => "flow",
        CatalogEntryKind::Alias { .. } => "alias",
    }
}

/// Outcome of an authorization check. `ModelNotInCatalog` and `Denied` both
/// surface as 404 `model_not_found` to callers — we never reveal whether a
/// resource exists.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthDecision {
    Allow,
    /// Requested model is not advertised in the catalog.
    ModelNotInCatalog,
    /// Resource exists but the principal is not allowed (default-DENY).
    Denied,
}

/// Pure /v1 authorization decision, decoupled from HTTP so it can be unit
/// tested with a synthetic snapshot + in-memory DB. fail-CLOSED:
///   * model absent from catalog → `ModelNotInCatalog`,
///   * alias requires allow on the alias **and** on the resolved target
///     (resource_type of the target depends on what the target is in the
///     catalog); a missing/denied target denies the whole request,
///   * a deny anywhere wins.
pub fn authorize_model(
    snapshot: &CatalogSnapshot,
    db: &DbPool,
    principal: &Principal,
    requested_model: &str,
) -> AuthDecision {
    let entry = match snapshot
        .advertised_entries()
        .find(|e| e.id == requested_model)
    {
        Some(e) => e,
        None => return AuthDecision::ModelNotInCatalog,
    };

    let rt = resource_type_for_kind(&entry.kind);
    if !crate::auth::acl::check_v1_access(db, rt, &entry.id, principal) {
        return AuthDecision::Denied;
    }

    // For aliases the principal must also be allowed on the resolved target
    // (and on any declared fallback) — otherwise an alias would let a caller
    // reach a model/flow whose own ACL denies them.
    if let CatalogEntryKind::Alias {
        target,
        fallback_targets,
        ..
    } = &entry.kind
    {
        for resolved in std::iter::once(target).chain(fallback_targets.iter()) {
            let target_entry = match snapshot.advertised_entries().find(|e| &e.id == resolved) {
                Some(e) => e,
                // A target missing from the catalog cannot be authorized — be
                // conservative and deny rather than silently skip it.
                None => return AuthDecision::Denied,
            };
            let target_rt = resource_type_for_kind(&target_entry.kind);
            if !crate::auth::acl::check_v1_access(db, target_rt, &target_entry.id, principal) {
                return AuthDecision::Denied;
            }
        }
    }

    AuthDecision::Allow
}

/// Central /v1 gate. Called at the top of every handler that accepts a `model`
/// field. Returns `Err(Response)` (404 `model_not_found`) on any denial, or 401
/// when no Principal was injected (should not happen — the unified server gate
/// builds one for every accepted key — but fail-CLOSED here too).
fn v1_authorize(
    router: &Router,
    principal: Option<&Principal>,
    requested_model: &str,
) -> std::result::Result<(), Response<OpenAIBody>> {
    let principal = match principal {
        Some(p) => p,
        None => {
            return Err(error_response(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Brak Principal dla zadania /v1".to_string(),
            ));
        }
    };

    let db = match router.db.as_ref() {
        Some(db) => db,
        None => {
            return Err(error_response(
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("model '{}' not found", requested_model),
            ));
        }
    };

    let snapshot = router.catalog_snapshot();
    match authorize_model(&snapshot, db, principal, requested_model) {
        AuthDecision::Allow => Ok(()),
        AuthDecision::ModelNotInCatalog | AuthDecision::Denied => Err(error_response(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("model '{}' not found", requested_model),
        )),
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

        // Audio TTS streaming (TentaFlow-specific, NIE OpenAI-compatible)
        ("POST", "/v1/audio/speech/stream") => handle_audio_tts_stream(req, router).await,

        // Audio flow streaming (Krok 5 — request leci przez flow_engine
        // streaming, audio chunki z `EnvelopeDelta::Audio`. Dla flow z
        // `tts_stream_bridge` audio leci per zdanie; dla blocking flow
        // wychodzi single chunk z całością bytes z BlobStore).
        ("POST", "/v1/audio/speech/flow-stream") => {
            handle_audio_speech_flow_stream(req, router).await
        }

        // Audio STT (Whisper)
        ("POST", "/v1/audio/transcriptions") => handle_audio_transcriptions(req, router).await,

        // Embeddings
        ("POST", "/v1/embeddings") => handle_embeddings(req, router).await,

        // NVIDIA NIM Object-Detection / OCR — reverse-proxy do serwisu wizyjnego
        // (nemotron-ocr, paddle-ocr, detektory YOLOX). Body forwardowane verbatim.
        ("POST", "/v1/infer") => {
            handle_passthrough(
                req,
                router,
                crate::services::catalog::ServiceSurface::Documents,
                &[crate::services::catalog::InputModality::Image],
                "/v1/infer",
            )
            .await
        }

        // Rerank — reverse-proxy do serwisów vLLM `--task score`
        // (nemotron-rerank, nemotron-rerank-vl). Body forwardowane verbatim.
        ("POST", "/v1/rerank") => {
            handle_passthrough(
                req,
                router,
                crate::services::catalog::ServiceSurface::Rerank,
                &[crate::services::catalog::InputModality::Text],
                "/v1/rerank",
            )
            .await
        }

        // NVIDIA NeMo Retriever reranking — kontrakt NVIDIA (query/passages →
        // rankings). Zdeployowane kontenery to czysty vLLM i wystawiają tylko
        // Cohere-style `/v1/rerank`, dlatego Core TŁUMACZY request i response,
        // a nie forwarduje verbatim (inaczej kontener zwróciłby 404 na
        // `/v1/ranking`). Współistnieje z `/v1/rerank` dla klientów Cohere/Jina.
        ("POST", "/v1/ranking") => handle_ranking(req, router).await,

        // Health check (dla load balancerow)
        ("GET", "/health") | ("GET", "/v1/health") => Ok(json_response(
            StatusCode::OK,
            br#"{"status":"ok"}"#.to_vec(),
        )),

        // Readiness check - zwraca 200 jesli >=1 backend zdrowy
        ("GET", "/ready") | ("GET", "/v1/ready") => handle_readiness_check(router).await,

        // Lista dostepnych modeli — filtrowana per-Principal (fail-CLOSED).
        ("GET", "/v1/models") => {
            let principal = req.extensions().get::<Principal>().cloned();
            handle_models_list(router, principal.as_ref()).await
        }

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
    let principal = req.extensions().get::<Principal>().cloned();

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

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &request.model) {
        return Ok(resp);
    }

    let is_streaming = request.stream;
    debug!(
        "Chat completion request: model={}, stream={}",
        request.model, is_streaming
    );

    if is_streaming {
        // === STREAMING MODE: SSE ===
        match router
            .route_chat_completion_stream(
                request,
                user_ctx.clone(),
                None,
                crate::routing::streaming::ChatFlowSelector::Auto,
            )
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
                Ok(core_error_to_response(&e))
            }
        }
    } else {
        // === NON-STREAMING MODE: JSON ===
        match router.route_chat_completion(request, user_ctx, None).await {
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
/// - QUIC TTS (TentaFlow.TTS z CBOR) - preferowany
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
    let want_trailers = wants_trailers(req.headers());
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

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

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &tts_request.model) {
        return Ok(resp);
    }

    // Rozwiazanie pola `language`: body klienta -> preferencja uzytkownika -> "en".
    // Pozwala wymusic jezyk per-request, a jesli brak — backend dostaje jawna
    // wartosc zamiast polegac na ukrytym domyslnym ustawieniu silnika TTS.
    if tts_request.language.is_none() {
        if let (Some(ref ctx), Some(ref db)) = (user_ctx.as_ref(), router.db.as_ref()) {
            if let Ok(Some(lang)) =
                crate::db::repository::get_user_preferred_language(db, &ctx.user_id)
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
            if want_trailers {
                emit_trailer_headers(resp.headers_mut(), &route_result.metadata);
            }
            Ok(resp)
        }
        Err(e) => {
            error!("TTS error: {}", e);
            Ok(core_error_to_response(&e))
        }
    }
}

/// Etap 3c: TentaFlow-specific (NIE OpenAI-compatible) endpoint streaming
/// TTS. Klient POST'uje tę samą strukturę co `/v1/audio/speech`
/// (`TTSRequest` JSON), dostaje `text/event-stream` z audio chunks:
/// `data: { audio_chunk: "<base64>", mime, sample_rate, finish_reason }\n\n`.
/// Stream kończy `data: [DONE]\n\n`. Cancel propaguje przez
/// `CancelOnDropStream` — klient disconnect zatrzymuje emisję
/// kolejnych chunków (limit: backend blocking syntezę i tak skończy
/// przed cancel — full backend abort wraca z native streaming).
async fn handle_audio_tts_stream(
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
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

    let body_bytes = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("Nie udalo sie odczytac body: {}", e),
            ));
        }
    };
    let api_request: crate::api::openai::types::TTSRequest =
        match serde_json::from_slice(&body_bytes) {
            Ok(r) => r,
            Err(e) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    format!("invalid JSON: {}", e),
                ));
            }
        };

    // Autorytatywna brama /v1 — egzekwuje default-DENY dla user/group/general
    // (zastepuje dawny check_access_safe widoczny tylko dla UserContext).
    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &api_request.model) {
        return Ok(resp);
    }

    let dispatcher = match router.flow_dispatcher.as_ref() {
        Some(d) => d.clone(),
        None => {
            return Ok(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "flow dispatcher not wired".to_string(),
            ));
        }
    };

    if api_request.input.is_empty() {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "TTSRequest.input must not be empty".to_string(),
        ));
    }

    let cancel = tokio_util::sync::CancellationToken::new();
    let req_dto = crate::flow_engine::dispatchers::TtsRequest {
        model: api_request.model.clone(),
        text: api_request.input.clone(),
        voice: Some(api_request.voice.clone()),
        format: api_request.response_format.clone(),
        language: api_request.language.clone(),
        speed: api_request.speed,
        user_id: user_ctx.as_ref().map(|u| u.user_id.clone()),
        user_role: user_ctx.as_ref().map(|u| u.role.clone()),
        cancel_token: cancel.clone(),
    };

    let chunk_stream = match dispatcher.tts().stream_synthesize(req_dto).await {
        Ok(s) => s,
        Err(e) => {
            // Map common error types to typed HTTP status. Default Internal
            // pokrywa nieznane błędy backendu.
            // Mapowanie typowane na podstawie podstringów wytwarzanych
            // przez TtsDispatcherImpl/runtime. "not wired" → backend nie
            // gotowy; "not found" / "unknown model" → 404; "empty text"
            // / "no candidate" / "capability" → 400; reszta → 500.
            let msg = e.to_string();
            let lower = msg.to_ascii_lowercase();
            let (status, code) = if lower.contains("not wired") {
                (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
            } else if lower.contains("not found") || lower.contains("unknown model") {
                (StatusCode::NOT_FOUND, "model_not_found")
            } else if lower.contains("empty text")
                || lower.contains("no candidate")
                || lower.contains("capability")
            {
                (StatusCode::BAD_REQUEST, "invalid_request")
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, "backend_error")
            };
            error!("TTS stream init: {}", e);
            return Ok(error_response(status, code, msg));
        }
    };

    use base64::Engine;
    use futures::StreamExt;
    let sse_chunks = chunk_stream.flat_map(|res| {
        let frames: Vec<std::result::Result<Frame<Bytes>, std::io::Error>> = match res {
            Ok(chunk) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&chunk.bytes_delta);
                let json = serde_json::json!({
                    "audio_chunk": b64,
                    "mime": chunk.mime,
                    "sample_rate": chunk.sample_rate,
                    "finish_reason": chunk
                        .finish_reason
                        .and_then(|f| f.as_openai_str().map(|s| s.to_string())),
                });
                let line = format!("data: {}\n\n", json);
                vec![Ok(Frame::data(Bytes::from(line)))]
            }
            Err(e) => {
                let json = serde_json::json!({ "error": format!("{e}") });
                let line = format!("data: {}\n\n", json);
                vec![Ok(Frame::data(Bytes::from(line)))]
            }
        };
        futures::stream::iter(frames)
    });
    let done = futures::stream::once(async {
        Ok::<_, std::io::Error>(Frame::data(Bytes::from("data: [DONE]\n\n")))
    });
    let combined = sse_chunks.chain(done);

    // CancelOnDropStream: hyper drop body → cancel.cancel() → take_while
    // w `stream_synthesize` widzi cancelled, EOF.
    let wrapped = crate::flow_engine::cancel_on_drop::CancelOnDropStream::new(combined, cancel);

    let body: std::pin::Pin<
        Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(wrapped);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(StreamBody::new(body))
        .unwrap())
}

/// Handler dla `POST /v1/audio/speech/flow-stream` (Krok 5).
///
/// Klient POST'uje strukturę `TTSRequest` (taką samą jak `/v1/audio/speech`),
/// dostaje `text/event-stream` z bazą64-zakodowanymi audio chunkami emitowanymi
/// bezpośrednio z flow_engine streaming chain'a:
///
/// - User-defined flow z `tts_stream_bridge` → audio per-zdanie (LLM tokeny
///   buforowane do sentence boundary, każde zdanie osobny TTS synthesize).
/// - User-defined blocking flow z `FlowValue::Audio` na output → single
///   chunk z całością bytes (`wrap_blocking_as_stream` fetchuje BlobStore
///   przed emitem).
/// - Synthetic TTS (gdy admin nie skonfigurował user-defined) → blocking
///   path → single chunk.
///
/// Cancel propaguje przez `CancelOnDropStream` — hyper drop body → token
/// cancel → executor finalizer EOF.
async fn handle_audio_speech_flow_stream(
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
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

    let body_bytes = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("Nie udalo sie odczytac body: {}", e),
            ));
        }
    };
    let api_request: crate::api::openai::types::TTSRequest =
        match serde_json::from_slice(&body_bytes) {
            Ok(r) => r,
            Err(e) => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    format!("invalid JSON: {}", e),
                ));
            }
        };

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &api_request.model) {
        return Ok(resp);
    }

    let dispatcher = match router.flow_dispatcher.as_ref() {
        Some(d) => d.clone(),
        None => {
            return Ok(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "flow dispatcher not wired".to_string(),
            ));
        }
    };

    if api_request.input.is_empty() {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "TTSRequest.input must not be empty".to_string(),
        ));
    }

    let (initial, mut meta) = crate::services::runtime::executor::tts_request_to_initial_envelope(
        &api_request,
        user_ctx.clone(),
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    meta.cancel_token = cancel.clone();

    let stream_exec = match dispatcher
        .try_dispatch_streaming(&api_request.model, "tts", initial, meta)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            let core_err = crate::routing::dispatch_error_to_core(e, &api_request.model);
            let (status, code) = match &core_err {
                crate::error::CoreError::ModelNotFound { .. } => {
                    (StatusCode::NOT_FOUND, "model_not_found")
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "backend_error"),
            };
            error!("audio flow-stream init: {}", msg);
            return Ok(error_response(status, code, msg));
        }
    };

    let body_chunks = crate::routing::audio_stream::envelope_stream_to_audio_chunks(stream_exec);
    let wrapped = crate::flow_engine::cancel_on_drop::CancelOnDropStream::new(body_chunks, cancel);

    let body: std::pin::Pin<
        Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(wrapped);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .body(StreamBody::new(body))
        .unwrap())
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
    let want_trailers = wants_trailers(req.headers());
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

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

    // Authorize BEFORE buffering the (potentially large) `file` part. An
    // unauthenticated/denied caller must not be able to force the server to
    // read the whole upload into memory ahead of the default-DENY gate. We
    // require `model` to be authorized before any `file` bytes are accepted:
    // a `file` part arriving while `model` is still unknown/unauthorized is
    // rejected without buffering. `model` is authorized inline the moment it
    // is parsed.
    let mut model_authorized = false;

    // Iteruj przez pola
    while let Some(field) = multipart.next_field().await.ok().flatten() {
        let field_name = field.name().unwrap_or("").to_string();

        match field_name.as_str() {
            "file" => {
                if !model_authorized {
                    return Ok(error_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        "Pole 'model' musi poprzedzac 'file' w multipart".to_string(),
                    ));
                }
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
                let model_name = field.text().await.ok().unwrap_or_default();
                if let Err(resp) = v1_authorize(&router, principal.as_ref(), &model_name) {
                    return Ok(resp);
                }
                model_authorized = true;
                model = Some(model_name);
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

    // `model` is required and was already authorized inline (before `file`
    // bytes were accepted); see the multipart loop above.
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
                crate::db::repository::get_user_preferred_language(db, &ctx.user_id)
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
            if want_trailers {
                emit_trailer_headers(resp.headers_mut(), &route_result.metadata);
            }
            Ok(resp)
        }
        Err(e) => {
            error!("Blad routingu audio transcription: {}", e);
            Ok(core_error_to_response(&e))
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
    let want_trailers = wants_trailers(req.headers());
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

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

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &request.model) {
        return Ok(resp);
    }

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
            if want_trailers {
                emit_trailer_headers(resp.headers_mut(), &route_result.metadata);
            }
            Ok(resp)
        }
        Err(e) => {
            error!("Blad routing embeddings: {}", e);
            Ok(core_error_to_response(&e))
        }
    }
}
/// Wspólny reverse-proxy dla endpointów, które nie mają typowanej odpowiedzi
/// OpenAI (`/v1/infer` NVIDIA NIM, `/v1/rerank` vLLM score). Rozwiązuje model
/// przez ten sam resolver/ACL co reszta `/v1`, a body przekazuje verbatim do
/// `endpoint_url` rozwiązanego serwisu pod tą samą ścieżką. Odpowiedź (status +
/// body) jest kopiowana bez reserializacji — to przezroczysty passthrough.
async fn handle_passthrough(
    req: Request<Incoming>,
    router: Arc<Router>,
    surface: crate::services::catalog::ServiceSurface,
    input_modalities: &[crate::services::catalog::InputModality],
    forward_path: &str,
) -> std::result::Result<
    Response<
        StreamBody<
            Pin<Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>>,
        >,
    >,
    hyper::Error,
> {
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

    let body_bytes = req.collect().await?.to_bytes();

    // Parsujemy tylko po to, by odczytać pole `model`; reszta leci verbatim.
    let parsed: serde_json::Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!("Passthrough {}: niepoprawny JSON: {}", forward_path, e);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Niepoprawny JSON: {}", e),
            ));
        }
    };
    let model = match parsed.get("model").and_then(|m| m.as_str()) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Brak wymaganego pola 'model'".to_string(),
            ));
        }
    };

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &model) {
        return Ok(resp);
    }

    let base = match resolve_local_v1_base(
        &router,
        &model,
        surface,
        input_modalities,
        user_ctx,
        &format!("passthrough {}", forward_path),
    ) {
        Ok(b) => b,
        Err(resp) => return Ok(resp),
    };

    // `endpoint_url` zwykle kończy się na `/v1` — sklejamy z gołą ścieżką
    // (`/infer`, `/rerank`), żeby nie zdublować segmentu `/v1`.
    let base = base.as_str();
    let suffix = forward_path.strip_prefix("/v1").unwrap_or(forward_path);
    let target_url = if base.ends_with("/v1") {
        format!("{}{}", base, suffix)
    } else {
        format!("{}{}", base, forward_path)
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("Passthrough {}: budowa klienta HTTP: {}", forward_path, e);
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            ));
        }
    };

    let upstream = client
        .post(&target_url)
        .header("Content-Type", "application/json")
        .body(body_bytes.to_vec())
        .send()
        .await;

    match upstream {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            match resp.bytes().await {
                Ok(bytes) => Ok(json_response(status, bytes.to_vec())),
                Err(e) => {
                    error!("Passthrough {}: odczyt body z upstream: {}", forward_path, e);
                    Ok(error_response(
                        StatusCode::BAD_GATEWAY,
                        "service_unavailable",
                        format!("błąd odczytu odpowiedzi z serwisu: {}", e),
                    ))
                }
            }
        }
        Err(e) => {
            error!("Passthrough {} → {}: {}", forward_path, target_url, e);
            Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "service_unavailable",
                format!("błąd forwardu do serwisu: {}", e),
            ))
        }
    }
}

/// Rozwiązuje lokalny `<base>/v1` endpoint serwisu dla danego modelu przez ten
/// sam resolver/ACL co reszta `/v1` (autoryzacja MUSI być już sprawdzona przez
/// `v1_authorize` przed wywołaniem). Zwraca bazowy URL zakończony na `/v1`
/// (bez trailing slash) gotowy do doklejenia gołej ścieżki (`/rerank`), albo
/// gotowy error-response gdy serwis jest zdalny / nie jest serwisem HTTP /
/// nie ma endpointu. Współdzielony przez passthrough i tłumaczące handlery.
fn resolve_local_v1_base(
    router: &Router,
    model: &str,
    surface: crate::services::catalog::ServiceSurface,
    input_modalities: &[crate::services::catalog::InputModality],
    user_ctx: Option<crate::auth::acl::UserContext>,
    context_label: &str,
) -> std::result::Result<String, Response<OpenAIBody>> {
    let executor = match router.executor() {
        Some(e) => e,
        None => {
            return Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Executor niedostępny".to_string(),
            ));
        }
    };

    let mut ctx = crate::services::runtime::context::ExecutionContext::new(user_ctx);
    let target = match executor.resolve_proxy_target(model, surface, input_modalities, &mut ctx) {
        Ok(t) => t,
        Err(e) => {
            warn!("{} resolve dla '{}': {}", context_label, model, e);
            return Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                format!("model '{}' niedostępny: {}", model, e),
            ));
        }
    };

    // Endpoint bierzemy WPROST z żywego `handle` zwróconego przez resolver, a nie
    // ze snapshotu `service_manager`: `service_id` z resolvera (przestrzeń
    // katalogu) nie pokrywa się z kluczami `services_by_id`, a serwis tuż po
    // reconcile (status `starting`) bywa chwilowo nieobecny w snapshocie mimo
    // żywego handle. `client.url()` to bazowy `endpoint_url` (np. `.../v1`).
    match &target {
        crate::services::runtime::target::ResolvedExecutionTarget::Local { handle, .. } => {
            match crate::services::runtime::transport_client::resolve_http_client(
                handle,
                context_label,
            ) {
                Ok(resolved) => Ok(resolved.client.url().trim_end_matches('/').to_string()),
                Err(e) => Err(error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service_unavailable",
                    format!("model '{}' nie jest lokalnym serwisem HTTP: {}", model, e),
                )),
            }
        }
        crate::services::runtime::target::ResolvedExecutionTarget::MeshForward { node_id, .. } => {
            Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                format!(
                    "model '{}' żyje tylko na zdalnym węźle '{}' — {} obsługuje wyłącznie lokalne serwisy",
                    model, node_id, context_label
                ),
            ))
        }
        crate::services::runtime::target::ResolvedExecutionTarget::Flow { .. } => {
            Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                format!("model '{}' rozwiązał się do flow, nie do serwisu HTTP", model),
            ))
        }
    }
}

/// Handler dla `POST /v1/ranking` (kontrakt NVIDIA NeMo Retriever reranking).
///
/// Zdeployowane kontenery rerank to czysty vLLM i wystawiają wyłącznie
/// Cohere-style `/v1/rerank` (`/v1/ranking` na kontenerze → 404), dlatego Core
/// musi TŁUMACZYĆ, a nie forwardować:
/// - request NVIDIA `{query:{text}|"q", passages:[{text}], truncate}` →
///   vLLM `{model, query:"q", documents:[...]}` (`truncate` nie jest
///   forwardowane — vLLM je ignoruje),
/// - response vLLM `{results:[{index, relevance_score, ...}]}` →
///   NVIDIA `{rankings:[{index, logit}]}` z zachowaniem oryginalnego `index`
///   i sortowania malejąco po score.
async fn handle_ranking(
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
    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

    let body_bytes = req.collect().await?.to_bytes();

    let parsed: serde_json::Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!("/v1/ranking: niepoprawny JSON: {}", e);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Niepoprawny JSON: {}", e),
            ));
        }
    };

    let model = match parsed.get("model").and_then(|m| m.as_str()) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Brak wymaganego pola 'model'".to_string(),
            ));
        }
    };

    // `query` jest leniwy: kontrakt NVIDIA wysyła `{text:"..."}`, ale przyjmujemy
    // też gołego stringa dla nietypowych klientów.
    let query = match parsed.get("query") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Object(o)) => match o.get("text").and_then(|t| t.as_str()) {
            Some(t) => t.to_string(),
            None => {
                return Ok(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "Pole 'query' musi mieć 'text' lub być stringiem".to_string(),
                ));
            }
        },
        _ => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Brak wymaganego pola 'query'".to_string(),
            ));
        }
    };

    let documents: Vec<String> = match parsed.get("passages").and_then(|p| p.as_array()) {
        Some(arr) => {
            let mut docs = Vec::with_capacity(arr.len());
            for passage in arr {
                let text = match passage {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Object(o) => {
                        o.get("text").and_then(|t| t.as_str()).map(|t| t.to_string())
                    }
                    _ => None,
                };
                match text {
                    Some(t) => docs.push(t),
                    None => {
                        return Ok(error_response(
                            StatusCode::BAD_REQUEST,
                            "invalid_request_error",
                            "Każdy element 'passages' musi mieć 'text' lub być stringiem"
                                .to_string(),
                        ));
                    }
                }
            }
            docs
        }
        None => {
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Brak wymaganego pola 'passages'".to_string(),
            ));
        }
    };

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &model) {
        return Ok(resp);
    }

    let base = match resolve_local_v1_base(
        &router,
        &model,
        crate::services::catalog::ServiceSurface::Rerank,
        &[crate::services::catalog::InputModality::Text],
        user_ctx,
        "/v1/ranking",
    ) {
        Ok(b) => b,
        Err(resp) => return Ok(resp),
    };

    // Forward leci ZAWSZE na Cohere-style `/v1/rerank` (kontener nie zna
    // `/v1/ranking`). `endpoint_url` zwykle kończy się na `/v1`.
    let target_url = if base.ends_with("/v1") {
        format!("{}/rerank", base)
    } else {
        format!("{}/v1/rerank", base)
    };

    let upstream_body = serde_json::json!({
        "model": model,
        "query": query,
        "documents": documents,
    });

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("/v1/ranking: budowa klienta HTTP: {}", e);
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            ));
        }
    };

    let upstream = client.post(&target_url).json(&upstream_body).send().await;

    let resp = match upstream {
        Ok(r) => r,
        Err(e) => {
            error!("/v1/ranking → {}: {}", target_url, e);
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "service_unavailable",
                format!("błąd forwardu do serwisu: {}", e),
            ));
        }
    };

    let upstream_status = resp.status();
    let upstream_bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            error!("/v1/ranking: odczyt body z upstream: {}", e);
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "service_unavailable",
                format!("błąd odczytu odpowiedzi z serwisu: {}", e),
            ));
        }
    };

    // Upstream zwrócił błąd — przekazujemy jego treść w OpenAI-style error,
    // żeby klient zobaczył powód (np. zły model / kontekst za długi).
    if !upstream_status.is_success() {
        let detail = String::from_utf8_lossy(&upstream_bytes);
        warn!(
            "/v1/ranking: upstream {} zwrócił {}: {}",
            target_url, upstream_status, detail
        );
        let status =
            StatusCode::from_u16(upstream_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return Ok(error_response(
            status,
            "service_unavailable",
            format!("serwis rerank zwrócił {}: {}", upstream_status, detail),
        ));
    }

    let vllm: serde_json::Value = match serde_json::from_slice(&upstream_bytes) {
        Ok(v) => v,
        Err(e) => {
            error!("/v1/ranking: niepoprawny JSON z upstream: {}", e);
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "service_unavailable",
                format!("serwis rerank zwrócił niepoprawny JSON: {}", e),
            ));
        }
    };

    // vLLM zwraca `results` już posortowane malejąco po score; zachowujemy ten
    // porządek i oryginalny `index`, mapując `relevance_score` → `logit`.
    let rankings: Vec<serde_json::Value> = match vllm.get("results").and_then(|r| r.as_array()) {
        Some(results) => results
            .iter()
            .filter_map(|item| {
                let index = item.get("index").and_then(|i| i.as_u64())?;
                let score = item.get("relevance_score").and_then(|s| s.as_f64())?;
                Some(serde_json::json!({ "index": index, "logit": score }))
            })
            .collect(),
        None => {
            error!("/v1/ranking: upstream nie zwrócił 'results'");
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "service_unavailable",
                "serwis rerank nie zwrócił pola 'results'".to_string(),
            ));
        }
    };

    let nvidia_response = serde_json::json!({ "rankings": rankings });
    let body = match serde_json::to_vec(&nvidia_response) {
        Ok(b) => b,
        Err(e) => {
            error!("/v1/ranking: serializacja odpowiedzi NVIDIA: {}", e);
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            ));
        }
    };

    Ok(json_response(StatusCode::OK, body))
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
    principal: Option<&Principal>,
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

    // fail-CLOSED: no Principal → empty list. Otherwise keep only entries the
    // Principal is actually allowed to reach via /v1 (resource_type per kind).
    let mut model_objects: Vec<ModelObject> = match (principal, router.db.as_ref()) {
        (Some(principal), Some(db)) => snapshot
            .advertised_entries()
            .filter(|entry| {
                matches!(
                    authorize_model(&snapshot, db, principal, &entry.id),
                    AuthDecision::Allow
                )
            })
            .map(|entry| ModelObject {
                id: entry.id.clone(),
                object: "model".to_string(),
                created: 1686935002,
                owned_by: entry.owned_by().to_string(),
            })
            .collect(),
        _ => Vec::new(),
    };
    model_objects.sort_by(|a, b| a.id.cmp(&b.id));

    let response = ModelsListResponse {
        object: "list".to_string(),
        data: model_objects,
    };

    let body = serde_json::to_vec(&response).unwrap();
    Ok(json_response(StatusCode::OK, body))
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
