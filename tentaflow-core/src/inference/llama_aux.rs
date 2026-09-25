// ===== File: inference/llama_aux.rs — in-process llama.cpp embedding and reranker models =====
//
// `InferenceManager` holds exactly one active generation engine, so an
// embedding or reranker GGUF loaded there would evict the chat model (and the
// next one evicts it). These models are small and serve different surfaces, so
// each gets its own `LlamaRuntime` here, keyed by the service's engine id — the
// one key the stop and eviction paths (`model_residency::unload_engine`) know.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use tentaflow_wrappers::llama::{Embedding, LlamaLoadConfig, LlamaRuntime, Rerank};
use tokio::sync::RwLock;
use tracing::info;

type Registry = RwLock<HashMap<String, Arc<LlamaRuntime>>>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Loads `path` under `engine_id`, replacing whatever that engine held.
pub async fn load(engine_id: &str, path: &Path, config: LlamaLoadConfig) -> Result<()> {
    let load_path = path.to_path_buf();
    let runtime = tokio::task::spawn_blocking(move || LlamaRuntime::load(&load_path, config))
        .await
        .context("llama.cpp load task panicked")?
        .with_context(|| format!("llama.cpp could not load {}", path.display()))?;
    registry()
        .write()
        .await
        .insert(engine_id.to_string(), Arc::new(runtime));
    info!(engine_id, path = %path.display(), "llama.cpp auxiliary model loaded");
    Ok(())
}

/// Drops the model of `engine_id`. Requests already holding the runtime finish
/// first; the weights are freed with the last reference.
pub async fn unload(engine_id: &str) {
    if registry().write().await.remove(engine_id).is_some() {
        info!(engine_id, "llama.cpp auxiliary model unloaded");
    }
}

pub async fn is_loaded(engine_id: &str) -> bool {
    registry().read().await.contains_key(engine_id)
}

async fn runtime(engine_id: &str) -> Result<Arc<LlamaRuntime>> {
    registry()
        .read()
        .await
        .get(engine_id)
        .cloned()
        .with_context(|| format!("llama.cpp model of engine '{engine_id}' is not loaded"))
}

pub async fn embeddings(engine_id: &str, texts: Vec<String>, normalize: bool) -> Result<Vec<Embedding>> {
    let runtime = runtime(engine_id).await?;
    tokio::task::spawn_blocking(move || {
        texts
            .iter()
            .map(|text| runtime.embeddings(text, normalize))
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .context("llama.cpp embeddings task panicked")?
    .context("llama.cpp embeddings failed")
}

pub async fn rerank(engine_id: &str, query: String, documents: Vec<String>) -> Result<Rerank> {
    let runtime = runtime(engine_id).await?;
    tokio::task::spawn_blocking(move || runtime.rerank(&query, &documents))
        .await
        .context("llama.cpp rerank task panicked")?
        .context("llama.cpp rerank failed")
}
