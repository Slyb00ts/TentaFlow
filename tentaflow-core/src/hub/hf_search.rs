// =============================================================================
// Plik: hub/hf_search.rs
// Opis: Wyszukiwanie modeli w HuggingFace Hub i Ollama library.
//       Uzywa HF REST API do filtrowania modeli per format silnika.
// =============================================================================

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::engine_registry;

/// Wynik wyszukiwania modelu z HuggingFace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfModelResult {
    pub model_id: String,
    pub author: String,
    pub downloads: u64,
    pub likes: u64,
    pub tags: Vec<String>,
    pub pipeline_tag: Option<String>,
}

/// Odpowiedz z HF API (struktura JSON)
/// HF zwraca zarowno `id` ("org/model") jak i `modelId` ("org/model") — oba pola moga wystepowac jednoczesnie.
#[derive(Debug, Deserialize)]
struct HfApiModel {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "modelId", default)]
    model_id: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    likes: Option<u64>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(rename = "pipeline_tag", default)]
    pipeline_tag: Option<String>,
}

/// Wyszukuje modele na HuggingFace Hub filtrowane per silnik
pub async fn search_models(
    query: &str,
    engine_id: &str,
    limit: u32,
) -> Result<Vec<HfModelResult>, String> {
    if engine_id == "ollama" {
        return search_ollama_models(query, limit).await;
    }

    let engine = engine_registry::engine_by_id(engine_id)
        .ok_or_else(|| format!("Nieznany silnik: {}", engine_id))?;

    let filter = match engine.model_format {
        "mlx" => "mlx",
        "gguf" => "gguf",
        "safetensors" => "text-generation",
        _ => "text-generation",
    };

    let url = format!(
        "https://huggingface.co/api/models?search={}&filter={}&sort=downloads&direction=-1&limit={}",
        urlencoding::encode(query),
        urlencoding::encode(filter),
        limit
    );

    debug!(url = %url, "HuggingFace API search");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client
        .get(&url)
        .header("User-Agent", "TentaFlow-AI/1.0")
        .send()
        .await
        .map_err(|e| format!("HF API request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HF API returned status: {}", resp.status()));
    }

    let models: Vec<HfApiModel> = resp
        .json()
        .await
        .map_err(|e| format!("HF API parse error: {}", e))?;

    Ok(models
        .into_iter()
        .map(|m| {
            let model_id = m.model_id.or(m.id).unwrap_or_default();
            let author = m.author.unwrap_or_else(|| {
                model_id.split('/').next().unwrap_or("unknown").to_string()
            });
            HfModelResult {
                model_id,
                author,
                downloads: m.downloads.unwrap_or(0),
                likes: m.likes.unwrap_or(0),
                tags: m.tags.unwrap_or_default(),
                pipeline_tag: m.pipeline_tag,
            }
        })
        .collect())
}

/// Wyszukuje modele Ollama (z wbudowanej biblioteki)
pub async fn search_ollama_models(
    query: &str,
    _limit: u32,
) -> Result<Vec<HfModelResult>, String> {
    // Ollama nie ma publicznego search API — zwracamy statyczna liste
    // filtrowana po zapytaniu
    let all = default_ollama_models();
    let q = query.to_lowercase();

    if q.is_empty() {
        return Ok(all);
    }

    Ok(all
        .into_iter()
        .filter(|m| {
            m.model_id.to_lowercase().contains(&q)
                || m.author.to_lowercase().contains(&q)
        })
        .collect())
}

/// Domyslne modele per silnik (fallback gdy HF niedostepny)
pub fn default_models(engine_id: &str) -> Vec<HfModelResult> {
    match engine_id {
        "sglang" | "vllm" => vec![
            hf_model("speakleash/Bielik-11B-v3.0-Instruct-FP8-Dynamic", "speakleash", "text-generation"),
            hf_model("meta-llama/Llama-3.1-8B-Instruct", "meta-llama", "text-generation"),
            hf_model("Qwen/Qwen2.5-7B-Instruct", "Qwen", "text-generation"),
            hf_model("mistralai/Mistral-7B-Instruct-v0.3", "mistralai", "text-generation"),
        ],
        "llamacpp" => vec![
            hf_model("bartowski/Llama-3.1-8B-Instruct-GGUF", "bartowski", "text-generation"),
            hf_model("TheBloke/Llama-2-7B-GGUF", "TheBloke", "text-generation"),
            hf_model("bartowski/Qwen2.5-7B-Instruct-GGUF", "bartowski", "text-generation"),
            hf_model("bartowski/Mistral-7B-Instruct-v0.3-GGUF", "bartowski", "text-generation"),
        ],
        "mlx" => vec![
            hf_model("mlx-community/Llama-3.1-8B-Instruct-4bit", "mlx-community", "text-generation"),
            hf_model("mlx-community/Qwen2.5-7B-Instruct-4bit", "mlx-community", "text-generation"),
            hf_model("mlx-community/Mistral-7B-Instruct-v0.3-4bit", "mlx-community", "text-generation"),
            hf_model("mlx-community/phi-4-4bit", "mlx-community", "text-generation"),
        ],
        "ollama" => default_ollama_models(),
        _ => vec![],
    }
}

fn default_ollama_models() -> Vec<HfModelResult> {
    vec![
        HfModelResult {
            model_id: "llama3.1".to_string(),
            author: "Meta".to_string(),
            downloads: 0,
            likes: 0,
            tags: vec!["ollama".to_string()],
            pipeline_tag: Some("text-generation".to_string()),
        },
        HfModelResult {
            model_id: "qwen2.5".to_string(),
            author: "Alibaba".to_string(),
            downloads: 0,
            likes: 0,
            tags: vec!["ollama".to_string()],
            pipeline_tag: Some("text-generation".to_string()),
        },
        HfModelResult {
            model_id: "mistral".to_string(),
            author: "Mistral AI".to_string(),
            downloads: 0,
            likes: 0,
            tags: vec!["ollama".to_string()],
            pipeline_tag: Some("text-generation".to_string()),
        },
        HfModelResult {
            model_id: "phi3".to_string(),
            author: "Microsoft".to_string(),
            downloads: 0,
            likes: 0,
            tags: vec!["ollama".to_string()],
            pipeline_tag: Some("text-generation".to_string()),
        },
        HfModelResult {
            model_id: "gemma2".to_string(),
            author: "Google".to_string(),
            downloads: 0,
            likes: 0,
            tags: vec!["ollama".to_string()],
            pipeline_tag: Some("text-generation".to_string()),
        },
    ]
}

fn hf_model(id: &str, author: &str, pipeline: &str) -> HfModelResult {
    HfModelResult {
        model_id: id.to_string(),
        author: author.to_string(),
        downloads: 0,
        likes: 0,
        tags: vec![],
        pipeline_tag: Some(pipeline.to_string()),
    }
}
