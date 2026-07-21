// ===== File: lib.rs — forge-server: OpenAI-compatible HTTP frontend over the FORGE engine =====
// PLAN chunk 7 / SPEC §8.1 subset: /v1/chat/completions, /v1/completions,
// /v1/models with SSE streaming and usage accounting, plus /healthz. The
// engine's blocking event receivers are bridged to tokio; admission-control
// rejections surface as 429 + Retry-After. /v1/audio/transcriptions is served
// when a Whisper model is configured (`forge serve --whisper-model`).

pub mod anthropic;
pub mod api;
pub mod error;
pub mod grammar;
pub mod metrics;
pub mod routes;
pub mod source;
pub mod toolcall;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::routing::{get, post};
use axum::Router;
use forge_engine::model::Model;
use forge_engine::server::EngineHandle;
use forge_formats::PoolingType;
use forge_tokenize::Tokenizer;

/// Whisper STT shared across handlers; the mutex serializes transcriptions
/// (single sequence per model, one at a time on the device).
pub type SharedWhisper = Arc<tokio::sync::Mutex<forge_whisper::WhisperModel>>;

/// Embedding model backing /v1/embeddings. Owns its own `Model` on a dedicated
/// device (like Whisper), with a mutex serializing forward passes: one
/// single-sequence pooled encode at a time on that device.
pub struct EmbedModel {
    pub model: tokio::sync::Mutex<Model>,
    pub tokenizer: Arc<Tokenizer>,
    pub pooling: PoolingType,
    /// Whether to L2-normalize the pooled vector (sentence-transformers
    /// `Normalize` module / default for retrieval embeddings).
    pub normalize: bool,
    /// Native embedding width (model hidden size).
    pub dim: usize,
    /// Hard token budget per input (model context ceiling).
    pub max_context: usize,
    /// Served id reported in the response `model` field.
    pub model_id: String,
}

pub type SharedEmbed = Arc<EmbedModel>;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub model_id: String,
    pub api_key: Option<String>,
    /// Tool-call output syntax override: "hermes" | "llama3" | "none".
    /// `None` auto-detects from the chat template and model architecture.
    pub tool_call_parser: Option<String>,
}

/// Shared per-server state handed to every handler.
pub struct ServerState {
    pub engine: EngineHandle,
    pub tokenizer: Arc<Tokenizer>,
    pub chat_template: String,
    pub template_vars: serde_json::Map<String, serde_json::Value>,
    pub eos_ids: Vec<u32>,
    /// Hard token budget per request (prompt + completion).
    pub max_context: usize,
    /// Bounds concurrently bridged requests: each in-flight generation holds
    /// one blocking-pool thread, so admission is capped at the engine's
    /// active slots plus a small queue instead of the pool's global limit.
    pub slots: Arc<tokio::sync::Semaphore>,
    pub model_id: String,
    pub api_key: Option<String>,
    pub created: u64,
    /// Which tool-call syntax to parse out of this model's output.
    pub tool_parser: toolcall::ToolParserKind,
    /// Constrained-decoding engine (SPEC §8.1.2): compiles `response_format` /
    /// `tool_choice` / `grammar` into shared automata + the vocab byte table.
    pub grammar: grammar::GrammarEngine,
    /// Optional Whisper STT model backing /v1/audio/transcriptions.
    pub whisper: Option<SharedWhisper>,
    /// Admission cap for transcription requests: each admitted request may
    /// buffer a multi-MB upload while the single-sequence Whisper model works
    /// through the queue, so unbounded concurrency would exhaust memory.
    pub stt_slots: tokio::sync::Semaphore,
    /// Optional embedding model backing /v1/embeddings.
    pub embed: Option<SharedEmbed>,
    /// Admission cap for embedding requests: each holds one blocking-pool
    /// thread while the single-sequence embedding model works its batch.
    pub embed_slots: tokio::sync::Semaphore,
    /// HTTP-level request counts (route + status) for /metrics; the engine's
    /// own counters/gauges/histograms are read live via `engine.metrics()`.
    pub http_metrics: metrics::HttpMetrics,
}

impl ServerState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: &ServerConfig,
        engine: EngineHandle,
        tokenizer: Arc<Tokenizer>,
        template_vars: serde_json::Map<String, serde_json::Value>,
        eos_ids: Vec<u32>,
        chat_template: String,
        max_context: usize,
        max_active: usize,
        tool_parser: toolcall::ToolParserKind,
        whisper: Option<SharedWhisper>,
        embed: Option<SharedEmbed>,
    ) -> Arc<Self> {
        let queue_limit = max_active.saturating_mul(4).clamp(16, 256);
        let grammar = grammar::GrammarEngine::new(&tokenizer, &eos_ids);
        Arc::new(Self {
            engine,
            tokenizer,
            chat_template,
            template_vars,
            eos_ids,
            max_context,
            slots: Arc::new(tokio::sync::Semaphore::new(
                max_active.saturating_add(queue_limit),
            )),
            model_id: cfg.model_id.clone(),
            api_key: cfg.api_key.clone(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            tool_parser,
            grammar,
            whisper,
            stt_slots: tokio::sync::Semaphore::new(4),
            embed,
            embed_slots: tokio::sync::Semaphore::new(4),
            http_metrics: metrics::HttpMetrics::default(),
        })
    }
}

/// Full application router. /healthz and /metrics stay outside the API-key gate
/// (standard health-check + Prometheus scrape reachability).
pub fn build_router(state: Arc<ServerState>) -> Router {
    let v1 = Router::new()
        .route("/chat/completions", post(routes::chat_completions))
        .route("/completions", post(routes::completions))
        // Anthropic-compatible Messages API (SPEC §8.1).
        .route("/messages", post(anthropic::messages))
        // Raised body limit: a 30 s stereo f32 WAV is well past axum's 2 MB
        // default multipart cap.
        .route(
            "/audio/transcriptions",
            post(routes::audio_transcriptions)
                .layer(axum::extract::DefaultBodyLimit::max(64 << 20)),
        )
        .route("/embeddings", post(routes::embeddings))
        .route("/models", get(routes::list_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            routes::require_api_key,
        ));
    Router::new()
        .nest("/v1", v1)
        .route("/healthz", get(routes::healthz))
        .route("/metrics", get(routes::metrics_endpoint))
        // Records route + status for every request into HttpMetrics, applied to
        // the whole router (including /healthz and /metrics scrapes).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            routes::record_http_metrics,
        ))
        .with_state(state)
}

/// Bind and serve until the process is stopped.
pub async fn serve(state: Arc<ServerState>, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("forge-server listening on {}", listener.local_addr()?);
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}
