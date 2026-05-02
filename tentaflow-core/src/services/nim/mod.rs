// =============================================================================
// File: services/nim/mod.rs
// Opis: Klient katalogu NVIDIA NIM — autentykacja NGC, pobieranie kontenerow
//       z https://integrate.api.nvidia.com + walidacja dostepnosci na nvcr.io.
// =============================================================================

use crate::db::{self, DbPool};
use anyhow::{Context, Result};
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

#[derive(Serialize, Clone, Debug)]
pub struct NimContainer {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub image: String,
    pub latest_tag: String,
    pub publisher: String,
    pub category: String,
    pub min_gpu_memory_gb: Option<u32>,
    pub updated_at: Option<String>,
    pub self_hostable: bool,
}

type NimCacheEntry = Option<(Instant, Vec<NimContainer>)>;

static NIM_CACHE: LazyLock<RwLock<NimCacheEntry>> = LazyLock::new(|| RwLock::new(None));

const CACHE_TTL: Duration = Duration::from_secs(3600);

/// Sprawdza ktore modele maja kontener NIM na nvcr.io
/// Uzywa registry auth endpoint — 200 = kontener istnieje, 403 = nie
/// Zwraca tylko modele z dostepnym kontenerem
async fn filter_self_hostable(containers: Vec<NimContainer>, api_key: &str) -> Vec<NimContainer> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(50));
    let mut handles = Vec::new();

    for (i, c) in containers.iter().enumerate() {
        if c.self_hostable {
            handles.push(tokio::spawn(async move { (i, true) }));
            continue;
        }

        let repo = c
            .image
            .strip_prefix("nvcr.io/")
            .unwrap_or(&c.image)
            .to_string();
        let key = api_key.to_string();
        let client = client.clone();
        let sem = sem.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            let url = format!(
                "https://nvcr.io/proxy_auth?scope=repository:{}:pull&service=registry",
                repo
            );
            let resp = client
                .get(&url)
                .basic_auth("$oauthtoken", Some(&key))
                .send()
                .await;
            (i, matches!(resp, Ok(r) if r.status().is_success()))
        }));
    }

    let mut available = vec![false; containers.len()];
    for handle in handles {
        if let Ok((idx, ok)) = handle.await {
            available[idx] = ok;
        }
    }

    containers
        .into_iter()
        .enumerate()
        .filter(|(i, _)| available[*i])
        .map(|(_, mut c)| {
            c.self_hostable = true;
            c
        })
        .collect()
}

/// Pobiera katalog modeli NIM z NVIDIA API (integrate.api.nvidia.com)
async fn fetch_nim_catalog(api_key: &str) -> Result<Vec<NimContainer>> {
    let client = reqwest::Client::new();

    let resp = client
        .get("https://integrate.api.nvidia.com/v1/models")
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
        .context("Blad polaczenia z NVIDIA API")?;

    if !resp.status().is_success() {
        anyhow::bail!("NVIDIA API catalog zwrocil status {}", resp.status());
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .context("Blad parsowania odpowiedzi NVIDIA API")?;

    let containers = parse_nvidia_models_response(&body);

    // Filtruj — zostaw tylko modele z kontenerem na nvcr.io (~4s, 50 parallel HTTP)
    let containers = filter_self_hostable(containers, api_key).await;

    Ok(containers)
}

/// Metadane znanych modeli — opisy i kategorie (bez VRAM, bo zalezy od context window)
fn known_model_info(id: &str) -> Option<(&'static str, &'static str)> {
    // (opis, kategoria)
    let known: &[(&str, &str, &str)] = &[
        (
            "meta/llama-3.1-8b-instruct",
            "Meta Llama 3.1 8B instruction-tuned LLM",
            "llm",
        ),
        (
            "meta/llama-3.1-70b-instruct",
            "Meta Llama 3.1 70B instruction-tuned LLM",
            "llm",
        ),
        (
            "meta/llama-3.1-405b-instruct",
            "Meta Llama 3.1 405B instruction-tuned LLM",
            "llm",
        ),
        (
            "meta/llama-3.2-1b-instruct",
            "Meta Llama 3.2 1B compact instruction-tuned LLM",
            "llm",
        ),
        (
            "meta/llama-3.2-3b-instruct",
            "Meta Llama 3.2 3B compact instruction-tuned LLM",
            "llm",
        ),
        (
            "meta/llama-3.2-11b-vision-instruct",
            "Meta Llama 3.2 11B vision-language model",
            "vlm",
        ),
        (
            "meta/llama-3.2-90b-vision-instruct",
            "Meta Llama 3.2 90B vision-language model",
            "vlm",
        ),
        (
            "meta/llama-3.3-70b-instruct",
            "Meta Llama 3.3 70B instruct, latest generation",
            "llm",
        ),
        (
            "meta/llama-4-scout-17b-16e-instruct",
            "Meta Llama 4 Scout 17B MoE instruct",
            "llm",
        ),
        (
            "meta/llama-4-maverick-17b-128e-instruct",
            "Meta Llama 4 Maverick 17B MoE instruct",
            "llm",
        ),
        (
            "mistralai/mistral-large-2-instruct",
            "Mistral Large 2 123B instruct",
            "llm",
        ),
        (
            "mistralai/mistral-small-24b-instruct",
            "Mistral Small 24B, efficient instruct",
            "llm",
        ),
        (
            "mistralai/mixtral-8x7b-instruct-v0.1",
            "Mistral MoE 8x7B instruct",
            "llm",
        ),
        (
            "microsoft/phi-3-mini-128k-instruct",
            "Microsoft Phi-3 Mini 3.8B, 128K context",
            "llm",
        ),
        (
            "microsoft/phi-4-mini-instruct",
            "Microsoft Phi-4 Mini instruct",
            "llm",
        ),
        (
            "google/gemma-2-9b-it",
            "Google Gemma 2 9B instruction-tuned",
            "llm",
        ),
        (
            "google/gemma-2-27b-it",
            "Google Gemma 2 27B instruction-tuned",
            "llm",
        ),
        ("qwen/qwq-32b", "Qwen QwQ 32B reasoning model", "llm"),
        (
            "nvidia/nemotron-4-340b-instruct",
            "NVIDIA Nemotron 340B instruct",
            "llm",
        ),
        (
            "nvidia/llama-3.1-nemotron-70b-instruct",
            "NVIDIA Nemotron 70B based on Llama 3.1",
            "llm",
        ),
        (
            "nvidia/nv-embedqa-e5-v5",
            "NVIDIA E5 embedding model for QA",
            "embedding",
        ),
        (
            "nvidia/nv-embedqa-mistral-7b-v2",
            "NVIDIA Mistral 7B embedding for QA",
            "embedding",
        ),
        (
            "nvidia/nv-embed-v1",
            "NVIDIA NV-Embed v1 embedding model",
            "embedding",
        ),
        (
            "nvidia/llama-3.2-nv-embedqa-1b-v2",
            "NVIDIA 1B embedding model",
            "embedding",
        ),
        (
            "nvidia/nvclip",
            "NVIDIA CLIP vision-language embedding",
            "vlm",
        ),
        (
            "snowflake/arctic-embed-l",
            "Snowflake Arctic Embed Large",
            "embedding",
        ),
        (
            "nvidia/neva-22b",
            "NVIDIA NeVA 22B vision-language model",
            "vlm",
        ),
        ("nvidia/vila", "NVIDIA VILA vision-language model", "vlm"),
        (
            "deepseek-ai/deepseek-r1-distill-llama-8b",
            "DeepSeek R1 distilled reasoning 8B",
            "llm",
        ),
        (
            "deepseek-ai/deepseek-v3.1",
            "DeepSeek V3.1 MoE, flagship model",
            "llm",
        ),
    ];
    for (pattern, desc, cat) in known {
        if id == *pattern {
            return Some((desc, cat));
        }
    }
    None
}

/// NIM kontenery niedostepne w /v1/models (STT, TTS, inne)
fn extra_nim_containers() -> Vec<NimContainer> {
    vec![
        NimContainer {
            name: "nvidia/parakeet-ctc-1.1b-asr".into(),
            display_name: "Parakeet CTC 1.1B".into(),
            description: "NVIDIA Parakeet automatic speech recognition, CTC-based".into(),
            image: "nvcr.io/nim/nvidia/parakeet-ctc-1.1b-asr".into(),
            latest_tag: "latest".into(),
            publisher: "nvidia".into(),
            category: "stt".into(),
            min_gpu_memory_gb: Some(4),
            updated_at: None,
            self_hostable: true,
        },
        NimContainer {
            name: "nvidia/parakeet-rnnt-1.1b-asr".into(),
            display_name: "Parakeet RNNT 1.1B".into(),
            description: "NVIDIA Parakeet automatic speech recognition, RNNT-based".into(),
            image: "nvcr.io/nim/nvidia/parakeet-rnnt-1.1b-asr".into(),
            latest_tag: "latest".into(),
            publisher: "nvidia".into(),
            category: "stt".into(),
            min_gpu_memory_gb: Some(4),
            updated_at: None,
            self_hostable: true,
        },
        NimContainer {
            name: "nvidia/canary-1b-flash".into(),
            display_name: "Canary 1B Flash".into(),
            description: "NVIDIA Canary multilingual ASR, fast variant".into(),
            image: "nvcr.io/nim/nvidia/canary-1b-flash".into(),
            latest_tag: "latest".into(),
            publisher: "nvidia".into(),
            category: "stt".into(),
            min_gpu_memory_gb: Some(4),
            updated_at: None,
            self_hostable: true,
        },
        NimContainer {
            name: "nvidia/fastpitch-hifigan-tts".into(),
            display_name: "FastPitch HiFi-GAN TTS".into(),
            description: "NVIDIA text-to-speech with FastPitch + HiFi-GAN vocoder".into(),
            image: "nvcr.io/nim/nvidia/fastpitch-hifigan-tts".into(),
            latest_tag: "latest".into(),
            publisher: "nvidia".into(),
            category: "tts".into(),
            min_gpu_memory_gb: Some(4),
            updated_at: None,
            self_hostable: true,
        },
        NimContainer {
            name: "nvidia/riva-asr".into(),
            display_name: "Riva ASR".into(),
            description: "NVIDIA Riva automatic speech recognition, production-grade".into(),
            image: "nvcr.io/nim/nvidia/riva-asr".into(),
            latest_tag: "latest".into(),
            publisher: "nvidia".into(),
            category: "stt".into(),
            min_gpu_memory_gb: Some(8),
            updated_at: None,
            self_hostable: true,
        },
        NimContainer {
            name: "nvidia/riva-tts".into(),
            display_name: "Riva TTS".into(),
            description: "NVIDIA Riva text-to-speech, production-grade multi-voice".into(),
            image: "nvcr.io/nim/nvidia/riva-tts".into(),
            latest_tag: "latest".into(),
            publisher: "nvidia".into(),
            category: "tts".into(),
            min_gpu_memory_gb: Some(8),
            updated_at: None,
            self_hostable: true,
        },
        NimContainer {
            name: "nvidia/nemo-retriever-reranking".into(),
            display_name: "NeMo Retriever Reranking".into(),
            description: "NVIDIA NeMo reranking model for RAG pipelines".into(),
            image: "nvcr.io/nim/nvidia/nemo-retriever-reranking".into(),
            latest_tag: "latest".into(),
            publisher: "nvidia".into(),
            category: "reranker".into(),
            min_gpu_memory_gb: Some(8),
            updated_at: None,
            self_hostable: true,
        },
    ]
}

/// Parsuje odpowiedz z integrate.api.nvidia.com/v1/models
fn parse_nvidia_models_response(body: &serde_json::Value) -> Vec<NimContainer> {
    let mut containers = Vec::new();

    let models = match body.get("data").and_then(|d| d.as_array()) {
        Some(arr) => arr,
        None => return containers,
    };

    for model in models {
        let id = match model.get("id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => continue,
        };

        let owned_by = model
            .get("owned_by")
            .and_then(|v| v.as_str())
            .unwrap_or("nvidia");

        let model_name = id.split('/').last().unwrap_or(id);

        // Display name: zamien myslniki i podkreslniki na spacje, capitalize slowa
        let display_name = model_name
            .replace('_', " ")
            .split(|c: char| c == '-' || c == ' ')
            .filter(|w| !w.is_empty())
            .map(|w| {
                // Zachowaj uppercase tokeny (np. "7B", "V3", "XTX")
                if w.chars()
                    .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '.')
                {
                    w.to_string()
                } else {
                    let mut chars = w.chars();
                    match chars.next() {
                        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                        None => String::new(),
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        // Uzyj znanych metadanych jesli dostepne
        let (description, category) = match known_model_info(id) {
            Some((desc, cat)) => (desc.to_string(), cat.to_string()),
            None => {
                let cat = categorize_nim(id, &display_name, "");
                let desc = format!("{} by {}", display_name, owned_by);
                (desc, cat)
            }
        };

        let publisher = owned_by.to_string();
        let image = format!("nvcr.io/nim/{}", id);

        containers.push(NimContainer {
            name: id.to_string(),
            display_name,
            description,
            image,
            latest_tag: "latest".to_string(),
            publisher,
            category,
            min_gpu_memory_gb: None,
            updated_at: None,
            self_hostable: false,
        });
    }

    // Dodaj kontenery niedostepne w /v1/models (STT, TTS, reranker)
    containers.extend(extra_nim_containers());

    containers
}

/// Kategoryzuje kontener NIM na podstawie nazwy i opisu
fn categorize_nim(name: &str, display_name: &str, description: &str) -> String {
    let combined = format!("{} {} {}", name, display_name, description).to_lowercase();

    if combined.contains("embed") {
        return "embedding".to_string();
    }
    if combined.contains("rerank") {
        return "reranker".to_string();
    }
    if combined.contains("whisper")
        || combined.contains("stt")
        || combined.contains("asr")
        || combined.contains("speech-to-text")
        || combined.contains("parakeet")
        || combined.contains("canary")
    {
        return "stt".to_string();
    }
    if combined.contains("tts")
        || combined.contains("text-to-speech")
        || combined.contains("fastpitch")
        || combined.contains("hifigan")
    {
        return "tts".to_string();
    }
    if combined.contains("vlm")
        || combined.contains("vision")
        || combined.contains("visual")
        || combined.contains("neva")
        || combined.contains("vila")
    {
        return "vlm".to_string();
    }

    "llm".to_string()
}

/// Wynik fetchu katalogu NIM — containers + opcjonalny symboliczny kod bledu.
pub struct NimCatalogResult {
    pub containers: Vec<NimContainer>,
    pub error: Option<String>,
}

/// Pobiera liste kontenerow NIM z cache lub bezposrednio z NVIDIA API.
/// Przy braku klucza NGC / bledzie fetch zwraca pusta liste z polem `error`,
/// zeby GUI moglo pokazac wskazowke (tak samo jak REST przedtem).
pub async fn fetch_catalog(
    pool: &DbPool,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Result<NimCatalogResult> {
    {
        let cache = NIM_CACHE.read();
        if let Some((created, ref containers)) = *cache {
            if created.elapsed() < CACHE_TTL {
                return Ok(NimCatalogResult {
                    containers: containers.clone(),
                    error: None,
                });
            }
        }
    }

    let api_key = match db::repository::get_setting_secure(pool, "ngc_api_key", settings_cipher) {
        Ok(Some(key)) if !key.is_empty() => key,
        _ => {
            return Ok(NimCatalogResult {
                containers: Vec::new(),
                error: Some("ngc_api_key_not_configured".to_string()),
            });
        }
    };

    match fetch_nim_catalog(&api_key).await {
        Ok(containers) => {
            let mut guard = NIM_CACHE.write();
            *guard = Some((Instant::now(), containers.clone()));
            Ok(NimCatalogResult {
                containers,
                error: None,
            })
        }
        Err(e) => {
            tracing::warn!("NGC catalog fetch error: {}", e);
            Ok(NimCatalogResult {
                containers: Vec::new(),
                error: Some("ngc_fetch_failed".to_string()),
            })
        }
    }
}
