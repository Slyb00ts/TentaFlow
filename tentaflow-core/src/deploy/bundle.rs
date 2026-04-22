// =============================================================================
// Plik: deploy/bundle.rs
// Opis: Embedowany bundle kontenerow (wbudowany przez build.rs jako tar.gz).
//       extract_to(target) rozpakowuje tentaflow-containers/ oraz wspolne
//       crate'y Rust wymagane przez wybrane Dockerfile do podanego katalogu
//       — typowo tmpdir w trakcie deployu.
// =============================================================================

use anyhow::{Context, Result};
use std::path::Path;

/// tar.gz wbudowany przez build.rs (patrz: pack_container_contexts).
const CONTAINER_BUNDLE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/container_bundle.tar.gz"));

/// Rozpakowuje wbudowany kontekst kontenerow do podanego katalogu.
/// Po rozpakowaniu w `target` znajdziesz `tentaflow-containers/`,
/// `tentaflow-protocol/`, `tentaflow-transport/` i `tentaflow-voice/`.
/// Bezpieczne dla deploy do tmpdir.
pub fn extract_to(target: &Path) -> Result<()> {
    if CONTAINER_BUNDLE.is_empty() {
        anyhow::bail!(
            "Bundle kontenerow jest pusty — build.rs nie spakowal go (sprawdz logi cargo build)"
        );
    }
    std::fs::create_dir_all(target)
        .with_context(|| format!("nie mozna utworzyc {}", target.display()))?;

    let decoder = flate2::read::GzDecoder::new(CONTAINER_BUNDLE);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(target)
        .with_context(|| format!("rozpakowanie bundle do {}", target.display()))?;
    Ok(())
}

/// Informacja o jednym kontenerze dostepnym do deployu.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContainerInfo {
    /// Nazwa kontenera (= nazwa folderu w tentaflow-containers/, np. "llm-vllm")
    pub name: String,
    /// Opis (z config.default.toml jesli mozna sparsowac, w przeciwnym razie pusty)
    pub description: String,
    /// Kategoria do grupowania w GUI: "llm" / "stt" / "tts" / "embeddings" / "image" / "meeting"
    pub category: String,
}

/// Skanuje wbudowany bundle i zwraca liste kontenerow do deployu (bez
/// faktycznego rozpakowywania na dysk).
pub fn list_containers() -> Result<Vec<ContainerInfo>> {
    if CONTAINER_BUNDLE.is_empty() {
        return Ok(Vec::new());
    }
    let decoder = flate2::read::GzDecoder::new(CONTAINER_BUNDLE);
    let mut archive = tar::Archive::new(decoder);

    let mut names = std::collections::BTreeSet::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        // Szukamy plikow tentaflow-containers/<NAZWA>/Dockerfile
        let comps: Vec<_> = path.components().collect();
        if comps.len() >= 3 {
            let root = comps[0].as_os_str().to_string_lossy();
            let name = comps[1].as_os_str().to_string_lossy().to_string();
            let leaf = comps[2].as_os_str().to_string_lossy();
            if root == "tentaflow-containers" && leaf == "Dockerfile" && name != "sidecar" {
                names.insert(name);
            }
        }
    }

    Ok(names
        .into_iter()
        .map(|n| {
            let category = categorize(&n);
            ContainerInfo {
                description: default_description(&n),
                category,
                name: n,
            }
        })
        .collect())
}

fn categorize(name: &str) -> String {
    if name.starts_with("llm-") {
        "llm".into()
    } else if name.starts_with("stt-") {
        "stt".into()
    } else if name.starts_with("tts-") {
        "tts".into()
    } else if name == "embeddings" {
        "embeddings".into()
    } else if name == "reranker" {
        "reranker".into()
    } else if name == "comfyui" {
        "image".into()
    } else if name == "teams-bot" {
        "meeting".into()
    } else {
        "other".into()
    }
}

fn default_description(name: &str) -> String {
    match name {
        "llm-llamacpp" => "Lokalny LLM (llama.cpp + CUDA)".into(),
        "llm-vllm" => "vLLM serwer (z git HEAD, najszybszy LLM HF)".into(),
        "llm-sglang" => "SGLang serwer (structured outputs)".into(),
        "llm-ollama" => "Ollama (latest)".into(),
        "stt-whisper" => "STT z whisper.cpp (CUDA)".into(),
        "stt-parakeet" => "STT z NVIDIA Parakeet-TDT-0.6B-v3 (najszybszy)".into(),
        "stt-qwen-asr" => "STT z Qwen3-ASR-1.7B (jakosc PL)".into(),
        "tts-sherpa" => "TTS z sherpa-onnx (jarvis, justyna)".into(),
        "tts-xtts" => "TTS XTTS v2 (voice cloning)".into(),
        "tts-voxcpm" => "TTS VoxCPM2".into(),
        "embeddings" => "Embeddings (HF text-embeddings-inference)".into(),
        "reranker" => "Reranker BGE (TEI)".into(),
        "comfyui" => "ComfyUI (image generation)".into(),
        "teams-bot" => "Sidecar Teams meeting bot".into(),
        _ => format!("Kontener {}", name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_containers_returns_known_names() {
        let containers = list_containers().expect("list");
        // Po zbudowaniu z pelnym bundle powinno byc co najmniej kilka
        if !containers.is_empty() {
            let names: Vec<_> = containers.iter().map(|c| c.name.as_str()).collect();
            assert!(names.contains(&"llm-llamacpp") || names.contains(&"embeddings"));
        }
    }

    #[test]
    fn extract_to_tmpdir_works() {
        if CONTAINER_BUNDLE.is_empty() {
            return; // build bez bundle — pomijamy
        }
        let dir = tempfile::tempdir().unwrap();
        extract_to(dir.path()).expect("extract");
        assert!(dir.path().join("tentaflow-containers").exists());
        assert!(dir.path().join("tentaflow-protocol").exists());
        assert!(dir.path().join("tentaflow-transport").exists());
        assert!(dir.path().join("tentaflow-voice").exists());
    }
}
