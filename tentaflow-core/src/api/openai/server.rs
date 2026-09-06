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
use crate::flow_engine::dispatcher::{FlowActor, FlowOrigin};
use crate::routing::router::Router;
use crate::services::catalog::{CatalogEntry, CatalogEntryKind, CatalogSnapshot};

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

/// §2.5 — the `/v1` actor, minted by the unified server while it verified the
/// API key (the only place the key → user binding is known) and injected into
/// the request extensions. A request that reached a handler without one is not
/// an authenticated external caller (a public path, or an internal call that
/// never went through the HTTP gate), so it degrades to the system actor rather
/// than inventing a user.
pub(crate) fn v1_actor(req: &Request<Incoming>) -> FlowActor {
    req.extensions()
        .get::<FlowActor>()
        .cloned()
        .unwrap_or_else(FlowActor::system)
}

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

/// Tworzy response z dowolnym typem zawartosci (publiczne assety: HTML, JS).
fn asset_response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<OpenAIBody> {
    let stream = futures::stream::once(async move { Ok(Frame::data(Bytes::from(body))) });
    let boxed_stream: Pin<
        Box<dyn Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send>,
    > = Box::pin(stream);
    Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .body(StreamBody::new(boxed_stream))
        .unwrap()
}

/// Tworzy HTML response (publiczna strona /docs).
fn html_response(status: StatusCode, body: Vec<u8>) -> Response<OpenAIBody> {
    asset_response(status, "text/html; charset=utf-8", body)
}

/// Mapuje dowolny anyhow::Error (potencjalnie CoreError) na error response z odpowiednim HTTP status.
fn core_error_to_response(e: &anyhow::Error) -> Response<OpenAIBody> {
    let core_error = e.downcast_ref::<CoreError>();
    if let Some(err) = core_error {
        let status = StatusCode::from_u16(err.status_code()).unwrap();
        let error_type = match err {
            CoreError::ModelNotFound { .. } => "model_not_found",
            CoreError::InvalidRequest { .. } => "invalid_request_error",
            CoreError::AllBackendsUnavailable { .. } | CoreError::SttServiceUnavailable => {
                "service_unavailable"
            }
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

    if !entry_access_allowed(db, entry, principal) {
        return AuthDecision::Denied;
    }

    // For aliases the principal must also be allowed on every target it could
    // actually resolve to — otherwise an alias would let a caller reach a
    // model/flow whose own ACL denies them. A target that is NOT advertised
    // (its owning node is offline) is unreachable right now, so the resolver
    // cannot pick it and there is nothing to authorize — we skip it instead of
    // denying the whole alias. Denying here defeated the entire point of a
    // fallback chain: the primary going offline (exactly when the fallback
    // matters) would 404 the alias.
    if let CatalogEntryKind::Alias {
        target,
        fallback_targets,
        ..
    } = &entry.kind
    {
        for resolved in std::iter::once(target).chain(fallback_targets.iter()) {
            let Some(target_entry) = snapshot.advertised_entries().find(|e| &e.id == resolved)
            else {
                continue; // offline / unadvertised → not reachable, nothing to authorize
            };
            if !entry_access_allowed(db, target_entry, principal) {
                return AuthDecision::Denied;
            }
        }
    }

    AuthDecision::Allow
}

/// Whether `principal` may reach this catalog entry. For a published flow we
/// accept a grant on EITHER its published model name (`entry.id`, what the
/// catalog/`/v1` advertises) OR its underlying flow id — the dashboard's
/// access-key wizard grants by flow id, so a grant made in the GUI must
/// authorize the same flow when called by its published name.
fn entry_access_allowed(db: &DbPool, entry: &CatalogEntry, principal: &Principal) -> bool {
    match &entry.kind {
        CatalogEntryKind::Flow { flow_id, .. } => {
            crate::auth::acl::check_v1_access(db, "flow", &entry.id, principal)
                || crate::auth::acl::check_v1_access(db, "flow", flow_id, principal)
        }
        other => crate::auth::acl::check_v1_access(
            db,
            resource_type_for_kind(other),
            &entry.id,
            principal,
        ),
    }
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

/// Publiczny wrapper bramy `/v1` dla modulow obok serwera (np. Anthropic
/// Messages API). Reuzywa te sama logike ACL co handlery OpenAI — ten sam
/// 404 `model_not_found` przy braku/odmowie modelu.
pub fn v1_authorize_public(
    router: &Router,
    principal: Option<&Principal>,
    requested_model: &str,
) -> std::result::Result<(), Response<OpenAIBody>> {
    v1_authorize(router, principal, requested_model)
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

        // Anthropic Messages API (zewnetrzne, zgodne z Anthropic SDK). Inna
        // sciezka i naglowek auth (`x-api-key` + `anthropic-version`) niz OpenAI,
        // wiec wspolistnieje z `/v1/chat/completions` bez kolizji.
        ("POST", "/v1/messages") => {
            crate::api::openai::anthropic::handle_messages(req, router).await
        }

        // Anthropic count_tokens — estymacja tokenow wejsciowych.
        ("POST", "/v1/messages/count_tokens") => {
            crate::api::openai::anthropic::handle_count_tokens(req, router).await
        }

        // Image generation
        ("POST", "/v1/images/generations") => handle_image_generation(req, router).await,

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

        // Depth — monocular depth estimation (Depth Anything V3 / MiDaS). Body
        // (image in) forwarded verbatim to the depth service; returns a depth map.
        ("POST", "/v1/depth") => {
            handle_passthrough(
                req,
                router,
                crate::services::catalog::ServiceSurface::Depth,
                &[crate::services::catalog::InputModality::Image],
                "/v1/depth",
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

        // Publiczna dokumentacja REST API — spec OpenAPI 3.1 (bez auth).
        ("GET", "/openapi.json") => {
            let spec = crate::api::openai::openapi::build_spec();
            let body = serde_json::to_vec(&spec).unwrap_or_default();
            Ok(json_response(StatusCode::OK, body))
        }

        // Publiczna strona Scalar API reference (bez auth).
        ("GET", "/docs") | ("GET", "/docs/") => Ok(html_response(
            StatusCode::OK,
            crate::api::openai::openapi::docs_html().into_bytes(),
        )),

        // Zbundlowany Scalar JS — samowystarczalne /docs (offline, bez CDN).
        ("GET", "/docs/scalar.js") => Ok(asset_response(
            StatusCode::OK,
            "application/javascript; charset=utf-8",
            crate::api::openai::openapi::scalar_js().as_bytes().to_vec(),
        )),

        // TentaBus REST endpoint (PLAN §6.5/M4) — the only `/v1/*` shape with
        // a dynamic path segment, so it can't be an exact-match arm above.
        ("POST", p) if crate::api::bus_rest::topic_from_records_path(p).is_some() => {
            let topic = crate::api::bus_rest::topic_from_records_path(p)
                .unwrap()
                .to_string();
            crate::api::bus_rest::handle_publish(req, router, topic).await
        }
        ("GET", p) if crate::api::bus_rest::topic_from_records_path(p).is_some() => {
            let topic = crate::api::bus_rest::topic_from_records_path(p)
                .unwrap()
                .to_string();
            crate::api::bus_rest::handle_consume(req, router, topic).await
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
    let actor = v1_actor(&req);

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
                FlowOrigin::Api,
                actor,
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
        match router
            .route_chat_completion(request, user_ctx, FlowOrigin::Api, actor, None)
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

/// Handler dla `/v1/images/generations` (OpenAI-compatible) — generuje obrazy
/// przez zdeployowany serwis ComfyUI. Resolve modelu idzie ta sama sciezka
/// ACL/resolver co reszta `/v1` (`v1_authorize` + `ServiceSurface::ImageGen`,
/// input `[Text]`, output `[Image]`). Workflow text2img SD1.5 budowany jest
/// programowo; ComfyUI kolejkuje go, a Core czeka na PNG-i i zwraca je jako
/// `b64_json` (domyslnie) — `response_format="url"` nie jest tu wspierany
/// (brak trwalego storage na te bajty), wiec degradujemy do `b64_json`.
async fn handle_image_generation(
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
    use crate::api::openai::comfyui::{ComfyClient, ComfyError, Text2ImgParams, MAX_IMAGES};

    let user_ctx = req
        .extensions()
        .get::<crate::auth::acl::UserContext>()
        .cloned();
    let principal = req.extensions().get::<Principal>().cloned();

    let body_bytes = req.collect().await?.to_bytes();

    let request: ImageGenerationRequest = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!("Image-gen: niepoprawny JSON: {}", e);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Niepoprawny JSON: {}", e),
            ));
        }
    };

    if request.prompt.trim().is_empty() {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "Pole 'prompt' nie moze byc puste".to_string(),
        ));
    }

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &request.model) {
        return Ok(resp);
    }

    // Resolve do lokalnego serwisu (target niesie handle + service_id). Surface
    // ImageGen, wejscie Text, wyjscie Image — zgodnie z manifestem comfyui.
    let context_label = "image-gen /v1/images/generations";
    let target = match resolve_local_v1_target(
        &router,
        &request.model,
        crate::services::catalog::ServiceSurface::ImageGen,
        &[crate::services::catalog::InputModality::Text],
        user_ctx,
        context_label,
    ) {
        Ok(t) => t,
        Err(resp) => return Ok(resp),
    };

    // Tylko serwisy ComfyUI (engine_id == "comfyui") sa obslugiwane. Inne
    // backendy image-gen (np. stable-diffusion-cpp) maja inne API i sa poza
    // zakresem tego endpointu — zwracamy jasny blad zamiast zgadywac protokol.
    let service_id = match &target {
        crate::services::runtime::target::ResolvedExecutionTarget::Local { service_id, .. } => {
            *service_id
        }
        _ => {
            // resolve_http_base_from_target i tak by to odrzucilo, ale dajemy
            // dedykowany komunikat zanim odpytamy DB.
            return Ok(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                format!(
                    "model '{}' nie jest lokalnym serwisem image-gen",
                    request.model
                ),
            ));
        }
    };

    // Odczyt engine_id + ewentualnego override checkpointu z config_json serwisu
    // (ten sam store co kreator deployu). Brak DB → traktujemy jako nieznany
    // backend i odrzucamy (fail-closed).
    let (engine_id, checkpoint_override) = match router.db.as_ref() {
        Some(db) => match db.read() {
            Ok(conn) => match crate::services_repo::services::get(&conn, service_id) {
                Ok(Some(svc)) => {
                    let override_name = serde_json::from_str::<serde_json::Value>(&svc.config_json)
                        .ok()
                        .and_then(|cfg| {
                            cfg.get("checkpoint")
                                .or_else(|| cfg.get("model_file"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        });
                    (svc.engine_id, override_name)
                }
                _ => (String::new(), None),
            },
            Err(_) => (String::new(), None),
        },
        None => (String::new(), None),
    };

    if engine_id != "comfyui" {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "model '{}' jest obslugiwany przez backend '{}', a /v1/images/generations wspiera tylko ComfyUI",
                request.model, engine_id
            ),
        ));
    }

    // Bazowy URL ComfyUI (golе `endpoint_url`, BEZ `/v1`).
    let base = match resolve_http_base_from_target(&target, &request.model, context_label) {
        Ok(b) => b,
        Err(resp) => return Ok(resp),
    };

    // Rozmiar "WxH" → (width, height); domyslnie 512x512 (natywny SD1.5).
    let (width, height) = parse_image_size(request.size.as_deref());

    // Liczba obrazow: clamp do [1, MAX_IMAGES].
    let batch_size = request.n.unwrap_or(1).clamp(1, MAX_IMAGES);

    // Checkpoint: jawny override z configu serwisu LUB nazwa-pliku podana jako
    // `model` w requescie; inaczej klient wykryje zaladowany checkpoint przez
    // /object_info, a w ostatecznosci uzyje domyslnego.
    let checkpoint = checkpoint_override.or_else(|| {
        let m = request.model.as_str();
        if m.ends_with(".safetensors") || m.ends_with(".ckpt") {
            Some(m.to_string())
        } else {
            None
        }
    });

    // Seed losowy per-request (OpenAI nie wystawia seeda w tym kontrakcie).
    let seed = rand::random::<u32>() as u64;

    let params = Text2ImgParams {
        prompt: request.prompt.clone(),
        negative_prompt: String::new(),
        width,
        height,
        batch_size,
        steps: 20,
        cfg: 7.0,
        sampler: "euler".to_string(),
        scheduler: "normal".to_string(),
        seed,
        checkpoint,
    };

    let wants_url = request.response_format.as_deref() == Some("url");
    if wants_url {
        warn!("Image-gen: response_format=url nie jest wspierany dla ComfyUI — zwracam b64_json");
    }

    let client = match ComfyClient::new(base) {
        Ok(c) => c,
        Err(e) => {
            error!("Image-gen: budowa klienta ComfyUI: {}", e);
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            ));
        }
    };

    info!(
        "Image-gen: model={}, {}x{}, n={}",
        request.model, width, height, batch_size
    );

    let images = match client.text2img(&params).await {
        Ok(imgs) => imgs,
        Err(e) => {
            error!("Image-gen: ComfyUI: {}", e);
            let (status, code) = match &e {
                ComfyError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "timeout_error"),
                ComfyError::Http(_) => (StatusCode::BAD_GATEWAY, "service_unavailable"),
                ComfyError::Backend(_) => (StatusCode::BAD_GATEWAY, "service_unavailable"),
            };
            return Ok(error_response(status, code, e.to_string()));
        }
    };

    use base64::Engine;
    let data: Vec<ImageData> = images
        .iter()
        .map(|png| ImageData {
            url: None,
            b64_json: Some(base64::engine::general_purpose::STANDARD.encode(png)),
            revised_prompt: None,
        })
        .collect();

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let response = ImageGenerationResponse { created, data };
    let body = match serde_json::to_vec(&response) {
        Ok(b) => b,
        Err(e) => {
            error!("Image-gen: serializacja odpowiedzi: {}", e);
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                e.to_string(),
            ));
        }
    };
    Ok(json_response(StatusCode::OK, body))
}

/// Parsuje OpenAI pole `size` ("WxH") na (width, height). Akceptuje tylko
/// dodatnie wymiary; przy braku/niepoprawnym formacie wraca natywne 512x512
/// SD1.5. Wymiary zaokraglane w dol do wielokrotnosci 8 (wymog VAE SD).
fn parse_image_size(size: Option<&str>) -> (u32, u32) {
    let default = (512u32, 512u32);
    let s = match size {
        Some(s) if !s.is_empty() => s,
        _ => return default,
    };
    let (w, h) = match s.split_once(['x', 'X']) {
        Some((w, h)) => (w.trim().parse::<u32>(), h.trim().parse::<u32>()),
        None => return default,
    };
    match (w, h) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => ((w / 8) * 8, (h / 8) * 8),
        _ => default,
    }
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
    let actor = v1_actor(&req);

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
        .synthesize_speech_for_user(&tts_request, user_ctx, FlowOrigin::Api, actor)
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
    // Read before the body is consumed — `into_body` moves the request.
    let actor = v1_actor(&req);

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
    // §2.5 — external `/v1` integration; the actor was minted while the API key
    // was verified (`v1_actor`), so a service key stays a service key here.
    let provenance = crate::flow_engine::dispatcher::CallProvenance::new(
        crate::flow_engine::dispatcher::FlowOrigin::Api,
        actor,
    );
    let req_dto = crate::flow_engine::dispatchers::TtsRequest {
        provenance,
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
/// - Direct TTS (gdy admin nie skonfigurował jawnego flow) → blocking path →
///   single chunk.
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
    let actor = v1_actor(&req);

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
        FlowOrigin::Api,
        actor,
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
    let actor = v1_actor(&req);

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
        .route_audio_transcription_for_user(transcription_request, user_ctx, FlowOrigin::Api, actor)
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

/// Parses the `/v1/embeddings` body and rejects shapes no backend can serve
/// with a message that names the field. OpenAI accepts `input` as a string,
/// an array of strings, an array of token ids or an array of token arrays;
/// every backend behind this gateway (HTTP OpenAI-compatible, embedded
/// llama.cpp/MLX, QUIC/mesh engines, flow nodes) is text-in, so token-id
/// inputs are refused explicitly instead of surfacing a serde untagged-enum
/// error. Empty inputs are refused like OpenAI does.
pub fn parse_embedding_request(body: &[u8]) -> std::result::Result<EmbeddingRequest, String> {
    let raw: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    match raw.get("input") {
        None => return Err("missing required field 'input'".to_string()),
        Some(serde_json::Value::String(s)) if s.is_empty() => {
            return Err("'input' must not be an empty string".to_string());
        }
        Some(serde_json::Value::Array(items)) => {
            if items.is_empty() {
                return Err("'input' must not be an empty array".to_string());
            }
            if items.iter().any(|v| v.is_number() || v.is_array()) {
                return Err(
                    "'input' as token ids (array of integers or array of integer \
                            arrays) is not supported by this gateway; send a string or an \
                            array of strings"
                        .to_string(),
                );
            }
            if let Some(pos) = items.iter().position(|v| !v.is_string()) {
                return Err(format!("'input[{pos}]' must be a string"));
            }
            if items
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s.is_empty()))
            {
                return Err("'input' must not contain empty strings".to_string());
            }
        }
        Some(serde_json::Value::String(_)) => {}
        Some(_) => {
            return Err("'input' must be a string or an array of strings".to_string());
        }
    }
    serde_json::from_value(raw).map_err(|e| format!("invalid request: {e}"))
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
    let actor = v1_actor(&req);

    // Czytamy body
    let body_bytes = req.collect().await?.to_bytes();

    let request = match parse_embedding_request(&body_bytes) {
        Ok(r) => r,
        Err(message) => {
            warn!("Embeddings request rejected: {}", message);
            return Ok(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                message,
            ));
        }
    };
    let Some(encoding) = EmbeddingEncoding::parse(request.encoding_format.as_deref()) else {
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "encoding_format must be 'float' or 'base64'".to_string(),
        ));
    };

    if let Err(resp) = v1_authorize(&router, principal.as_ref(), &request.model) {
        return Ok(resp);
    }

    debug!("Embeddings request: model={}", request.model);

    // Routuj do odpowiedniego backendu
    match router
        .route_embeddings_for_user(request, user_ctx, FlowOrigin::Api, actor)
        .await
    {
        Ok(route_result) => {
            let body = route_result.response.to_wire_json(encoding);
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
    // Read before the body is consumed — `collect` moves the request.
    let actor = v1_actor(&req);

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

    let context_label = format!("passthrough {}", forward_path);
    let target = match resolve_local_v1_target(
        &router,
        &model,
        surface,
        input_modalities,
        user_ctx,
        &context_label,
    ) {
        Ok(t) => t,
        Err(resp) => return Ok(resp),
    };

    // Embedded vision (`/v1/infer`): silniki wkompilowane w binarkę (detekcja
    // twarzy/pozy/emocji + OCR) nie wystawiają HTTP — uruchamiamy je in-process
    // przez `crate::vision`, zamiast forwardować po sieci. Reszta (`/v1/rerank`,
    // kontenery HTTP) idzie niezmienioną ścieżką passthrough poniżej.
    if let crate::services::runtime::target::ResolvedExecutionTarget::Local {
        handle: crate::services::handles_cache::BackendHandle::Embedded { engine_id, .. },
        model_name,
        ..
    } = &target
    {
        if forward_path == "/v1/infer" {
            return Ok(infer_embedded_vision(
                &parsed,
                model_name,
                engine_id,
                router.executor.clone(),
                actor,
            )
            .await);
        }
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            format!(
                "model '{}' jest serwisem embedded — {} obsługuje tylko serwisy HTTP",
                model, context_label
            ),
        ));
    }

    let base = match resolve_http_base_from_target(&target, &model, &context_label) {
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
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            match resp.bytes().await {
                Ok(bytes) => Ok(json_response(status, bytes.to_vec())),
                Err(e) => {
                    error!(
                        "Passthrough {}: odczyt body z upstream: {}",
                        forward_path, e
                    );
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
    resolve_local_v1_base_url(
        router,
        model,
        surface,
        input_modalities,
        user_ctx,
        context_label,
    )
    .map_err(|msg| error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", msg))
}

/// Rozwiązuje `ResolvedExecutionTarget` przez ten sam resolver/ACL co reszta
/// `/v1` (autoryzacja MUSI być sprawdzona wcześniej przez `v1_authorize`).
/// Współdzielone przez passthrough, który dla `/v1/infer` musi rozróżnić
/// serwis HTTP od silnika embedded — `resolve_local_v1_base_url` tej różnicy
/// nie zachowuje (zwraca tylko URL albo błąd).
fn resolve_local_v1_target(
    router: &Router,
    model: &str,
    surface: crate::services::catalog::ServiceSurface,
    input_modalities: &[crate::services::catalog::InputModality],
    user_ctx: Option<crate::auth::acl::UserContext>,
    context_label: &str,
) -> std::result::Result<
    crate::services::runtime::target::ResolvedExecutionTarget,
    Response<OpenAIBody>,
> {
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

    // §2.5 — /v1 target resolution only. This context resolves a backend and
    // never dispatches a flow, so no event carries its actor; the authenticated
    // /v1 actor is minted in `api::unified_server` and travels the dispatch path.
    let mut ctx = crate::services::runtime::context::ExecutionContext::new(
        user_ctx,
        crate::flow_engine::dispatcher::FlowOrigin::Api,
        crate::flow_engine::dispatcher::FlowActor::system_component("v1_target_resolve"),
    );
    match executor.resolve_proxy_target(model, surface, input_modalities, &mut ctx) {
        Ok(t) => Ok(t),
        Err(e) => {
            warn!("{} resolve dla '{}': {}", context_label, model, e);
            Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                format!("model '{}' niedostępny: {}", model, e),
            ))
        }
    }
}

/// Wyciąga bazowy `<...>/v1` URL z już rozwiązanego targetu HTTP. Embedded jest
/// obsłużony wcześniej przez caller, więc tu trafia tylko Local(HTTP/QUIC) lub
/// zdalny/flow (oba dają błąd jak w poprzedniej, czysto-HTTP ścieżce).
fn resolve_http_base_from_target(
    target: &crate::services::runtime::target::ResolvedExecutionTarget,
    model: &str,
    context_label: &str,
) -> std::result::Result<String, Response<OpenAIBody>> {
    use crate::services::runtime::target::ResolvedExecutionTarget;
    match target {
        ResolvedExecutionTarget::Local { handle, .. } => {
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
        ResolvedExecutionTarget::MeshForward { node_id, .. } => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            format!(
                "model '{}' żyje tylko na zdalnym węźle '{}' — {} obsługuje wyłącznie lokalne serwisy",
                model, node_id, context_label
            ),
        )),
        ResolvedExecutionTarget::Flow { .. } => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            format!("model '{}' rozwiązał się do flow, nie do serwisu HTTP", model),
        )),
    }
}

/// In-process inference dla wkompilowanych silników vision na `/v1/infer`.
/// Dekoduje pierwszy obraz z `input[0].url` (data-URL → RGB8), wybiera ścieżkę:
/// OCR (onnx-ocr/apple-ocr/plate-ocr) przez `VisionDispatcher`, a detekcję
/// twarzy/pozy/emocji przez `crate::vision::infer`, i mapuje wynik na kontrakt
/// NVIDIA NIM (z rozszerzeniami dla pozy/emocji, których NIM nie definiuje).
async fn infer_embedded_vision(
    parsed: &serde_json::Value,
    model_name: &str,
    engine_id: &str,
    runtime_slot: crate::flow_engine::dispatchers_impl::ModelRuntimeSlot,
    // §2.5 — the authenticated `/v1` caller, minted while the API key was verified.
    actor: FlowActor,
) -> Response<OpenAIBody> {
    let url = match parsed
        .get("input")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|item| item.get("url"))
        .and_then(|u| u.as_str())
    {
        Some(u) if !u.is_empty() => u,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "pole 'input[0].url' (data-URL obrazu) jest wymagane".to_string(),
            );
        }
    };

    let (bytes, _mime) = match crate::routing::decode_data_url(url) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("dekodowanie data-URL: {}", e),
            );
        }
    };
    let images_size_mb = bytes.len() as f64 / (1024.0 * 1024.0);

    // Dekodujemy do RGB8 tym samym wzorcem co `dispatch/handlers.rs` (ścieżka
    // Encoded) — `image` rozpoznaje format z nagłówka pliku.
    let (rgb, width, height) = match image::load_from_memory(&bytes) {
        Ok(img) => {
            use image::GenericImageView;
            let (w, h) = img.dimensions();
            (img.to_rgb8().into_raw(), w, h)
        }
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("dekodowanie obrazu: {}", e),
            );
        }
    };

    // OCR embedded (onnx-ocr/apple-ocr/plate-ocr) nie ma odpowiednika w
    // `VisionEngineKind` — rozpoznajemy go po engine_id i obsługujemy przez
    // `VisionDispatcher` (in-process runner / Burn PlateOcr), zwracając kontrakt
    // OCR NVIDIA z pojedynczą detekcją obejmującą cały kadr (jak paddle-ocr).
    if matches!(engine_id, "onnx-ocr" | "apple-ocr" | "plate-ocr") {
        let dispatcher =
            crate::flow_engine::dispatchers_impl::VisionDispatcherImpl::new(runtime_slot);
        let req = crate::flow_engine::dispatchers::VisionOcrRequest {
            rgb,
            width,
            height,
            alias: model_name.to_string(),
            caller_addon_id: None,
            // §2.5 — external `/v1` integration, with the key's own actor.
            provenance: crate::flow_engine::dispatcher::CallProvenance::new(
                crate::flow_engine::dispatcher::FlowOrigin::Api,
                actor,
            ),
        };
        use crate::flow_engine::dispatchers::VisionDispatcher;
        return match dispatcher.ocr(req).await {
            Ok(text) => json_response(
                StatusCode::OK,
                ocr_response_json(text.unwrap_or_default(), images_size_mb),
            ),
            Err(e) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("embedded OCR '{}': {:#}", model_name, e),
            ),
        };
    }

    // Detekcja: silnik wybierany leniwie po nazwie modelu/serwisu w registry.
    let out = match crate::vision::infer(model_name, &rgb, width, height) {
        Ok(o) => o,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("embedded vision '{}': {:#}", model_name, e),
            );
        }
    };

    let body = vision_infer_to_json(out, width as f32, height as f32);
    json_response(StatusCode::OK, serde_json::to_vec(&body).unwrap())
}

/// Buduje kontrakt OCR NVIDIA NIM z pojedynczą detekcją obejmującą cały kadr
/// (PaddleOCR/Apple Vision transkrybują całą stronę, bez ramek per-region) —
/// identyczny kształt jak kontener `paddle-ocr`.
fn ocr_response_json(text: String, images_size_mb: f64) -> Vec<u8> {
    let body = serde_json::json!({
        "data": [{
            "index": 0,
            "text_detections": [{
                "text_prediction": { "text": text, "confidence": 1.0 },
                "bounding_box": { "points": [
                    { "x": 0.0, "y": 0.0 },
                    { "x": 1.0, "y": 0.0 },
                    { "x": 1.0, "y": 1.0 },
                    { "x": 0.0, "y": 1.0 },
                ] }
            }]
        }],
        "usage": { "images_size_mb": images_size_mb }
    });
    serde_json::to_vec(&body).unwrap()
}

/// Mapuje `InferOutput` na JSON `/v1/infer`. Współrzędne normalizujemy do 0..1
/// względem oryginalnych wymiarów (`w`/`h`), jak kontener nemotron-yolox.
fn vision_infer_to_json(out: crate::vision::InferOutput, w: f32, h: f32) -> serde_json::Value {
    use crate::vision::InferOutput;
    let norm_x = |x: f32| (x / w).clamp(0.0, 1.0);
    let norm_y = |y: f32| (y / h).clamp(0.0, 1.0);
    match out {
        InferOutput::Faces(faces) => {
            // Kontrakt NVIDIA NIM Object Detection: ramki grupowane po nazwie
            // klasy. Detektory twarzy mają jedną klasę → grupa "face".
            let boxes: Vec<serde_json::Value> = faces
                .iter()
                .map(|f| {
                    let (x1, y1, x2, y2) = f.bbox;
                    let mut obj = serde_json::json!({
                        "x_min": norm_x(x1),
                        "y_min": norm_y(y1),
                        "x_max": norm_x(x2),
                        "y_max": norm_y(y2),
                        "confidence": f.score,
                    });
                    // Keypointy to rozszerzenie poza NIM — dołączamy je gdy
                    // detektor ma głowę keypointów (np. yolov8-face/scrfd).
                    if let Some(kps) = f.keypoints {
                        obj["keypoints"] = serde_json::Value::Array(
                            kps.iter()
                                .map(|(x, y)| {
                                    serde_json::json!({ "x": norm_x(*x), "y": norm_y(*y) })
                                })
                                .collect(),
                        );
                    }
                    obj
                })
                .collect();
            serde_json::json!({
                "data": [{ "index": 0, "bounding_boxes": { "face": boxes } }]
            })
        }
        InferOutput::Poses(poses) => {
            // NIM nie ma kontraktu pozy — emitujemy jawny, rozszerzony kształt.
            let arr: Vec<serde_json::Value> = poses
                .iter()
                .map(|p| {
                    let (x1, y1, x2, y2) = p.bbox;
                    serde_json::json!({
                        "bbox": {
                            "x_min": norm_x(x1),
                            "y_min": norm_y(y1),
                            "x_max": norm_x(x2),
                            "y_max": norm_y(y2),
                            "confidence": p.score,
                        },
                        "keypoints": p.keypoints.iter().map(|k| serde_json::json!({
                            "name": k.name,
                            "x": norm_x(k.x),
                            "y": norm_y(k.y),
                            "confidence": k.score,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            serde_json::json!({ "data": [{ "index": 0, "poses": arr }] })
        }
        InferOutput::Emotion(em) => {
            let probabilities: serde_json::Map<String, serde_json::Value> = em
                .probabilities
                .iter()
                .map(|(label, p)| (label.clone(), serde_json::Value::from(*p)))
                .collect();
            serde_json::json!({
                "data": [{
                    "index": 0,
                    "emotion": {
                        "label": em.label,
                        "probabilities": probabilities,
                        "valence": em.valence,
                        "arousal": em.arousal,
                    }
                }]
            })
        }
    }
}

/// Rdzeń rozwiązywania lokalnego `<base>/v1` — bez warstwy HTTP. Zwraca czysty
/// `Err(String)` z gotowym komunikatem, żeby mógł go użyć zarówno HTTP handler
/// (`resolve_local_v1_base` owija to w `error_response`) jak i handler
/// protokołu binarnego (`ProtocolError`). Współdzielona, jedyna implementacja
/// resolve dla obu tierów — bez duplikacji logiki katalogu/ACL.
pub fn resolve_local_v1_base_url(
    router: &Router,
    model: &str,
    surface: crate::services::catalog::ServiceSurface,
    input_modalities: &[crate::services::catalog::InputModality],
    user_ctx: Option<crate::auth::acl::UserContext>,
    context_label: &str,
) -> std::result::Result<String, String> {
    let executor = match router.executor() {
        Some(e) => e,
        None => return Err("Executor niedostępny".to_string()),
    };

    // §2.5 — /v1 target resolution only. This context resolves a backend and
    // never dispatches a flow, so no event carries its actor; the authenticated
    // /v1 actor is minted in `api::unified_server` and travels the dispatch path.
    let mut ctx = crate::services::runtime::context::ExecutionContext::new(
        user_ctx,
        crate::flow_engine::dispatcher::FlowOrigin::Api,
        crate::flow_engine::dispatcher::FlowActor::system_component("v1_target_resolve"),
    );
    let target = match executor.resolve_proxy_target(model, surface, input_modalities, &mut ctx) {
        Ok(t) => t,
        Err(e) => {
            warn!("{} resolve dla '{}': {}", context_label, model, e);
            return Err(format!("model '{}' niedostępny: {}", model, e));
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
                Err(e) => Err(format!(
                    "model '{}' nie jest lokalnym serwisem HTTP: {}",
                    model, e
                )),
            }
        }
        crate::services::runtime::target::ResolvedExecutionTarget::MeshForward {
            node_id, ..
        } => Err(format!(
            "model '{}' żyje tylko na zdalnym węźle '{}' — {} obsługuje wyłącznie lokalne serwisy",
            model, node_id, context_label
        )),
        crate::services::runtime::target::ResolvedExecutionTarget::Flow { .. } => Err(format!(
            "model '{}' rozwiązał się do flow, nie do serwisu HTTP",
            model
        )),
    }
}

/// Współdzielona ścieżka forwardu rerankingu (resolve serwisu Rerank + POST na
/// Cohere-style `<base>/v1/rerank` + parsowanie odpowiedzi vLLM). Używana przez
/// HTTP `/v1/ranking` (Tier 2) ORAZ natywny handler `RerankRequest` (Tier 1) —
/// jedna implementacja forwardu, zero duplikacji logiki HTTP.
///
/// Autoryzacja modelu (`v1_authorize` / `#[policy]`) MUSI być sprawdzona przez
/// wywołującego przed wejściem tu.
/// Embedded MLX reranker (jina-rerank-mlx) — liczy score'y IN-PROCESS przez
/// MLXBridge zamiast forwardu HTTP (spójnie z embedded embeddings/vision). Zwraca
/// `None` gdy serwis rerank NIE jest embedded (caller idzie ścieżką HTTP).
#[cfg(feature = "inference-mlx")]
async fn try_embedded_rerank(
    router: &Router,
    model: &str,
    query: &str,
    documents: &[String],
    top_n: Option<u32>,
    return_documents: bool,
    user_ctx: Option<crate::auth::acl::UserContext>,
    context_label: &str,
) -> Option<std::result::Result<tentaflow_protocol::RerankResult, String>> {
    let executor = router.executor()?;
    // §2.5 — /v1 target resolution only. This context resolves a backend and
    // never dispatches a flow, so no event carries its actor; the authenticated
    // /v1 actor is minted in `api::unified_server` and travels the dispatch path.
    let mut ctx = crate::services::runtime::context::ExecutionContext::new(
        user_ctx,
        crate::flow_engine::dispatcher::FlowOrigin::Api,
        crate::flow_engine::dispatcher::FlowActor::system_component("v1_target_resolve"),
    );
    let target = executor
        .resolve_proxy_target(
            model,
            crate::services::catalog::ServiceSurface::Rerank,
            &[crate::services::catalog::InputModality::Text],
            &mut ctx,
        )
        .ok()?;
    // Tylko embedded local handle idzie in-process; reszta (HTTP/mesh/flow) -> None.
    match &target {
        crate::services::runtime::target::ResolvedExecutionTarget::Local {
            handle: crate::services::handles_cache::BackendHandle::Embedded { .. },
            ..
        } => {}
        _ => return None,
    }
    // Model rerankera zaladowany przez embedded deploy (load_embedder_model).
    let scores = match crate::inference::mlx_swift_bridge::rerank(query, documents).await {
        Ok(s) => s,
        Err(e) => {
            return Some(Err(format!(
                "{}: embedded MLX rerank: {}",
                context_label, e
            )))
        }
    };
    let mut ranked: Vec<(usize, f32)> = scores.into_iter().enumerate().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let n = top_n
        .map(|n| n as usize)
        .unwrap_or(ranked.len())
        .min(ranked.len());
    let results = ranked
        .into_iter()
        .take(n)
        .map(|(idx, score)| tentaflow_protocol::RerankResultItem {
            index: idx,
            relevance_score: score,
            document: if return_documents {
                documents.get(idx).cloned()
            } else {
                None
            },
        })
        .collect();
    Some(Ok(tentaflow_protocol::RerankResult {
        results,
        model: model.to_string(),
    }))
}

#[cfg(not(feature = "inference-mlx"))]
async fn try_embedded_rerank(
    _router: &Router,
    _model: &str,
    _query: &str,
    _documents: &[String],
    _top_n: Option<u32>,
    _return_documents: bool,
    _user_ctx: Option<crate::auth::acl::UserContext>,
    _context_label: &str,
) -> Option<std::result::Result<tentaflow_protocol::RerankResult, String>> {
    None
}

pub async fn rerank_forward(
    router: &Router,
    model: &str,
    query: &str,
    documents: &[String],
    top_n: Option<u32>,
    return_documents: bool,
    user_ctx: Option<crate::auth::acl::UserContext>,
    context_label: &str,
) -> std::result::Result<tentaflow_protocol::RerankResult, String> {
    // Embedded MLX reranker liczy in-process; brak (None) -> forward HTTP nizej.
    if let Some(result) = try_embedded_rerank(
        router,
        model,
        query,
        documents,
        top_n,
        return_documents,
        user_ctx.clone(),
        context_label,
    )
    .await
    {
        return result;
    }

    let base = resolve_local_v1_base_url(
        router,
        model,
        crate::services::catalog::ServiceSurface::Rerank,
        &[crate::services::catalog::InputModality::Text],
        user_ctx,
        context_label,
    )?;

    // Forward leci ZAWSZE na Cohere-style `/v1/rerank` (kontener vLLM nie zna
    // `/v1/ranking`). `endpoint_url` zwykle kończy się na `/v1`.
    let target_url = if base.ends_with("/v1") {
        format!("{}/rerank", base)
    } else {
        format!("{}/v1/rerank", base)
    };

    // vLLM przyjmuje `top_n` jako opcjonalne; przekazujemy gdy podane.
    let mut upstream_body = serde_json::json!({
        "model": model,
        "query": query,
        "documents": documents,
        "return_documents": return_documents,
    });
    if let Some(n) = top_n {
        upstream_body["top_n"] = serde_json::json!(n);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("budowa klienta HTTP: {}", e))?;

    let resp = client
        .post(&target_url)
        .json(&upstream_body)
        .send()
        .await
        .map_err(|e| {
            error!("{} → {}: {}", context_label, target_url, e);
            format!("błąd forwardu do serwisu: {}", e)
        })?;

    let upstream_status = resp.status();
    let upstream_bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("błąd odczytu odpowiedzi z serwisu: {}", e))?;

    if !upstream_status.is_success() {
        let detail = String::from_utf8_lossy(&upstream_bytes);
        warn!(
            "{}: upstream {} zwrócił {}: {}",
            context_label, target_url, upstream_status, detail
        );
        return Err(format!(
            "serwis rerank zwrócił {}: {}",
            upstream_status, detail
        ));
    }

    let vllm: serde_json::Value = serde_json::from_slice(&upstream_bytes)
        .map_err(|e| format!("serwis rerank zwrócił niepoprawny JSON: {}", e))?;

    let results_arr = vllm
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "serwis rerank nie zwrócił pola 'results'".to_string())?;

    let results = results_arr
        .iter()
        .filter_map(|item| {
            let index = item.get("index").and_then(|i| i.as_u64())? as usize;
            let relevance_score = item.get("relevance_score").and_then(|s| s.as_f64())? as f32;
            let document = item
                .get("document")
                .and_then(|d| {
                    d.get("text")
                        .and_then(|t| t.as_str())
                        .or_else(|| d.as_str())
                })
                .map(|s| s.to_string());
            Some(tentaflow_protocol::RerankResultItem {
                index,
                relevance_score,
                document,
            })
        })
        .collect();

    Ok(tentaflow_protocol::RerankResult {
        results,
        model: model.to_string(),
    })
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
                    serde_json::Value::Object(o) => o
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|t| t.to_string()),
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

    // Resolve serwisu + forward na Cohere-style `/v1/rerank` współdzielony z
    // natywnym handlerem `RerankRequest` — Tier 2 dokleja tylko tłumaczenie
    // kontraktu NVIDIA (request `passages` → `documents`, response `results`
    // → `rankings` z `logit`).
    let rerank = match rerank_forward(
        &router,
        &model,
        &query,
        &documents,
        None,
        false,
        user_ctx,
        "/v1/ranking",
    )
    .await
    {
        Ok(r) => r,
        Err(msg) => {
            return Ok(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                msg,
            ));
        }
    };

    // vLLM zwraca `results` już posortowane malejąco po score; zachowujemy ten
    // porządek i oryginalny `index`, mapując `relevance_score` → `logit`.
    let rankings: Vec<serde_json::Value> = rerank
        .results
        .iter()
        .map(|item| serde_json::json!({ "index": item.index, "logit": item.relevance_score }))
        .collect();

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
    let local_services = router.service_manager.current_snapshot();
    let local_node_id = router
        .service_manager
        .mesh_services_registry
        .read()
        .as_ref()
        .map(|r| r.local().node_id.clone())
        .unwrap_or_default();

    #[derive(serde::Serialize)]
    struct ModelObject {
        id: String,
        object: String,
        created: i64,
        owned_by: String,
        supports_embeddings: bool,
        supports_structured_output: bool,
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
                supports_embeddings: entry
                    .service_surfaces
                    .contains(&crate::services::catalog::ServiceSurface::Embeddings),
                supports_structured_output: supports_structured_output(
                    &snapshot,
                    &local_services,
                    &local_node_id,
                    entry,
                    0,
                ),
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

/// Capability flag for `/v1/models`: whether `response_format` (json_object /
/// json_schema) reaches the engine intact. True only when the chat surface is
/// served by a LOCAL service over plain HTTP (vLLM, SGLang, llama-server,
/// ollama, OpenAI-compatible upstreams) — `BackendClient` serializes the full
/// `ChatCompletionRequest`. Embedded engines (llama.cpp in-process, MLX),
/// QUIC sidecars, mesh-forwarded instances and published flows all rebuild the
/// request without `response_format`, so they report false. An alias inherits
/// the verdict of its primary target.
fn supports_structured_output(
    snapshot: &CatalogSnapshot,
    local_services: &crate::services::supervisor::ServicesSnapshot,
    local_node_id: &str,
    entry: &CatalogEntry,
    depth: usize,
) -> bool {
    use crate::services::catalog::ServiceSurface;
    use crate::services::transport::Transport;

    if depth > 4 || !entry.service_surfaces.contains(&ServiceSurface::Chat) {
        return false;
    }
    match &entry.kind {
        CatalogEntryKind::ServiceModel { instances } => instances.iter().any(|inst| {
            inst.node_id == local_node_id
                && local_services
                    .services_by_id
                    .get(&inst.service_id)
                    .and_then(|idx| local_services.services.get(*idx))
                    .is_some_and(|svc| {
                        matches!(
                            svc.transport,
                            Transport::HttpDirect | Transport::ExternalHttp
                        )
                    })
        }),
        CatalogEntryKind::Alias { target, .. } => snapshot
            .entries
            .iter()
            .find(|e| &e.id == target)
            .is_some_and(|t| {
                supports_structured_output(snapshot, local_services, local_node_id, t, depth + 1)
            }),
        CatalogEntryKind::Flow { .. } => false,
    }
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

#[cfg(test)]
mod error_mapping_tests {
    use super::*;

    /// A missing STT service must reach external clients as 503
    /// `service_unavailable`, not a generic 500.
    #[test]
    fn stt_service_unavailable_maps_to_503() {
        let err: anyhow::Error = CoreError::SttServiceUnavailable.into();
        let resp = core_error_to_response(&err);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// The flow-dispatch edge keeps the typed error through `DispatchError`.
    #[test]
    fn dispatch_error_keeps_stt_service_unavailable_typed() {
        let inner: anyhow::Error = CoreError::SttServiceUnavailable.into();
        let wrapped = inner.context("stt adapter: dispatcher failed");
        let dispatch = crate::flow_engine::dispatcher::DispatchError::from(wrapped);
        let core = crate::routing::dispatch_error_to_core(dispatch, "whisper-1");
        assert!(matches!(core, CoreError::SttServiceUnavailable));
        assert_eq!(core.status_code(), 503);
    }
}
