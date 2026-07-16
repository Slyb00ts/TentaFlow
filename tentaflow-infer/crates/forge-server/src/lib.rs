// ===== File: lib.rs — forge-server: OpenAI-compatible HTTP frontend over the FORGE engine =====
// PLAN chunk 7 / SPEC §8.1 subset: /v1/chat/completions, /v1/completions,
// /v1/models with SSE streaming and usage accounting, plus /healthz. The
// engine's blocking event receivers are bridged to tokio; admission-control
// rejections surface as 429 + Retry-After.

pub mod api;
pub mod error;
pub mod routes;
pub mod source;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::routing::{get, post};
use axum::Router;
use forge_engine::server::EngineHandle;
use forge_tokenize::Tokenizer;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub model_id: String,
    pub api_key: Option<String>,
}

/// Shared per-server state handed to every handler.
pub struct ServerState {
    pub engine: EngineHandle,
    pub tokenizer: Arc<Tokenizer>,
    pub chat_template: String,
    pub template_vars: serde_json::Map<String, serde_json::Value>,
    pub eos_ids: Vec<u32>,
    pub model_id: String,
    pub api_key: Option<String>,
    pub created: u64,
}

impl ServerState {
    pub fn new(
        cfg: &ServerConfig,
        engine: EngineHandle,
        tokenizer: Arc<Tokenizer>,
        template_vars: serde_json::Map<String, serde_json::Value>,
        eos_ids: Vec<u32>,
        chat_template: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            tokenizer,
            chat_template,
            template_vars,
            eos_ids,
            model_id: cfg.model_id.clone(),
            api_key: cfg.api_key.clone(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        })
    }
}

/// Full application router. /healthz stays outside the API-key gate.
pub fn build_router(state: Arc<ServerState>) -> Router {
    let v1 = Router::new()
        .route("/chat/completions", post(routes::chat_completions))
        .route("/completions", post(routes::completions))
        .route("/models", get(routes::list_models))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            routes::require_api_key,
        ));
    Router::new()
        .nest("/v1", v1)
        .route("/healthz", get(routes::healthz))
        .with_state(state)
}

/// Bind and serve until the process is stopped.
pub async fn serve(state: Arc<ServerState>, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("forge-server listening on {}", listener.local_addr()?);
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}
