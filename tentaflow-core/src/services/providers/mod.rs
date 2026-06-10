// =============================================================================
// File: services/providers/mod.rs
// Opis: Pobieranie zywej listy modeli z zewnetrznych providerow chmurowych
//       (OpenAI-compatible, Anthropic, Azure, ElevenLabs, Soniox, Ollama).
//       Jedno wejscie `list_models` rozgaleziajace per `ApiKind`.
// =============================================================================

use crate::services::manifest::ApiKind;
use anyhow::{Context, Result};
use std::time::Duration;

/// Timeout for a single provider model-list request. Cloud APIs answer fast;
/// 15 s is generous enough to ride out a slow TLS handshake without hanging
/// the admin UI when a provider is unreachable.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// One model entry as advertised by an external provider.
#[derive(Debug, Clone)]
pub struct ProviderModel {
    pub id: String,
    pub display_name: Option<String>,
    /// "chat" | "embedding" | "tts" | "stt" | "image" | "rerank" | "unknown"
    pub modality: String,
    pub context_length: Option<u32>,
}

/// Fetch the live model list for an external provider.
/// `base_url` = the service endpoint_url (already includes /v1 for openai-compatible).
/// `api_key` = already DECRYPTED. `api_version` = Azure only (may be None).
pub async fn list_models(
    api: ApiKind,
    base_url: &str,
    api_key: &str,
    api_version: Option<&str>,
) -> Result<Vec<ProviderModel>> {
    // Azure deployments are not enumerable from the data plane; the
    // `api_version` argument only matters for actual inference calls.
    let _ = api_version;

    let base = base_url.trim_end_matches('/');
    let client = build_client()?;

    match api {
        ApiKind::OpenaiCompatible | ApiKind::Custom => {
            list_openai_compatible(&client, base, api_key).await
        }
        ApiKind::Anthropic => list_anthropic(&client, base, api_key).await,
        // Azure OpenAI cannot enumerate deployments over the data plane; the
        // admin types deployment names manually. Empty list, not an error.
        ApiKind::AzureOpenai => Ok(Vec::new()),
        ApiKind::Elevenlabs => list_elevenlabs(&client, base, api_key).await,
        ApiKind::Soniox => list_soniox(&client, base, api_key).await,
        ApiKind::OllamaNative => list_ollama(&client, base, api_key).await,
        // Local/self-hosted engines, not external cloud providers.
        ApiKind::SherpaTts | ApiKind::SherpaStt | ApiKind::Comfyui => Ok(Vec::new()),
    }
}

fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("failed to build reqwest client for provider model listing")
}

/// OpenAI-compatible `GET {base}/models`. Handles the OpenRouter
/// `architecture` modality hints when present, otherwise classifies by id.
async fn list_openai_compatible(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<Vec<ProviderModel>> {
    let url = format!("{base}/models");
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("openai-compatible model list request to {url} failed"))?;
    ensure_success(resp.status(), "openai-compatible")?;

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse openai-compatible model list response")?;

    let Some(models) = body.get("data").and_then(|d| d.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let Some(id) = model.get("id").and_then(|v| v.as_str()) else {
            continue;
        };

        let context_length = model
            .get("context_length")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as u32);

        let modality = openai_modality(id, model);

        out.push(ProviderModel {
            id: id.to_string(),
            display_name: None,
            modality,
            context_length,
        });
    }

    Ok(out)
}

/// Resolve modality for an OpenAI-compatible entry. Prefers OpenRouter's
/// `architecture` modality lists, falling back to id-based classification.
fn openai_modality(id: &str, model: &serde_json::Value) -> String {
    if let Some(arch) = model.get("architecture") {
        let out_mods = string_array(arch.get("output_modalities"));
        let in_mods = string_array(arch.get("input_modalities"));

        if !out_mods.is_empty() {
            let has_text_out = out_mods.iter().any(|m| m == "text");
            if out_mods.iter().any(|m| m == "audio") {
                // Audio out + audio in = speech recognition style; audio out
                // from text = synthesis.
                return if in_mods.iter().any(|m| m == "audio") {
                    "stt".to_string()
                } else {
                    "tts".to_string()
                };
            }
            if out_mods.iter().any(|m| m == "image") && !has_text_out {
                return "image".to_string();
            }
            // Audio-only input with text output is transcription.
            if has_text_out
                && in_mods.iter().any(|m| m == "audio")
                && !in_mods.iter().any(|m| m == "text")
            {
                return "stt".to_string();
            }
        }
    }

    classify_openai_model_id(id).to_string()
}

/// Anthropic `GET {base}/models` with `x-api-key` + `anthropic-version`.
/// All entries are chat models.
async fn list_anthropic(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<Vec<ProviderModel>> {
    let url = format!("{base}/models");
    let mut req = client.get(&url).header("anthropic-version", "2023-06-01");
    if !api_key.is_empty() {
        req = req.header("x-api-key", api_key);
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("anthropic model list request to {url} failed"))?;
    ensure_success(resp.status(), "anthropic")?;

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse anthropic model list response")?;

    let Some(models) = body.get("data").and_then(|d| d.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let Some(id) = model.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let display_name = model
            .get("display_name")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        out.push(ProviderModel {
            id: id.to_string(),
            display_name,
            modality: "chat".to_string(),
            context_length: None,
        });
    }

    Ok(out)
}

/// ElevenLabs `GET {base}/v1/models` with `xi-api-key`. The base is the API
/// root (no `/v1`); guard against a base that already ends in `/v1`. Response
/// is a JSON array of speech models.
async fn list_elevenlabs(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<Vec<ProviderModel>> {
    let url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };

    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.header("xi-api-key", api_key);
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("elevenlabs model list request to {url} failed"))?;
    ensure_success(resp.status(), "elevenlabs")?;

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse elevenlabs model list response")?;

    let Some(models) = body.as_array() else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let Some(id) = model.get("model_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let display_name = model
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        out.push(ProviderModel {
            id: id.to_string(),
            display_name,
            modality: "tts".to_string(),
            context_length: None,
        });
    }

    Ok(out)
}

/// Soniox `GET {base}/models` with `Authorization: Bearer`. The response wraps
/// the list under either `models` or `data`, and entries key the name as `id`
/// or `name`; handle both defensively.
async fn list_soniox(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<Vec<ProviderModel>> {
    let url = format!("{base}/models");
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("soniox model list request to {url} failed"))?;
    ensure_success(resp.status(), "soniox")?;

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse soniox model list response")?;

    let models = body
        .get("models")
        .and_then(|v| v.as_array())
        .or_else(|| body.get("data").and_then(|v| v.as_array()));

    let Some(models) = models else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let id = model
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| model.get("name").and_then(|v| v.as_str()));
        let Some(id) = id else {
            continue;
        };

        out.push(ProviderModel {
            id: id.to_string(),
            display_name: None,
            modality: "stt".to_string(),
            context_length: None,
        });
    }

    Ok(out)
}

/// Ollama daemon `GET {base}/api/tags`. The base is the daemon root. Auth is
/// only sent when an api_key is present (remote/proxied Ollama).
async fn list_ollama(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<Vec<ProviderModel>> {
    let url = format!("{base}/api/tags");
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {api_key}"));
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("ollama model list request to {url} failed"))?;
    ensure_success(resp.status(), "ollama")?;

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse ollama model list response")?;

    let Some(models) = body.get("models").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let Some(name) = model.get("name").and_then(|v| v.as_str()) else {
            continue;
        };

        out.push(ProviderModel {
            id: name.to_string(),
            display_name: None,
            modality: "chat".to_string(),
            context_length: None,
        });
    }

    Ok(out)
}

/// Best-effort modality classification from an OpenAI-style model id.
pub fn classify_openai_model_id(id: &str) -> &'static str {
    let id = id.to_lowercase();

    if id.contains("embedding") {
        return "embedding";
    }
    if id.contains("tts") {
        return "tts";
    }
    if id.contains("whisper") || id.contains("transcribe") {
        return "stt";
    }
    if id.contains("dall-e") || id.contains("gpt-image") || id.contains("image-gen") {
        return "image";
    }
    if id.contains("rerank") {
        return "rerank";
    }
    "chat"
}

/// Collect the string values of a JSON array field, ignoring non-string and
/// missing entries.
fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Map a non-2xx provider response to a typed error naming the provider so the
/// caller can surface it to the admin.
fn ensure_success(status: reqwest::StatusCode, provider: &str) -> Result<()> {
    if !status.is_success() {
        anyhow::bail!("{provider} model list returned HTTP {status}");
    }
    Ok(())
}
