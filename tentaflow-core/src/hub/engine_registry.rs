// =============================================================================
// Plik: hub/engine_registry.rs
// Opis: Centralny rejestr silnikow LLM z kompatybilnoscia per platforma/OS.
//       Okresla dostepne silniki, tryb wdrazania (Docker/Native) i formaty modeli.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Definicja silnika LLM
#[derive(Debug, Clone)]
pub struct EngineDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub supported_platforms: &'static [Platform],
    pub model_format: &'static str,
    pub hf_filter_tags: &'static [&'static str],
    pub default_port: u16,
    pub shared_model_formats: &'static [&'static str],
}

/// Platforma docelowa
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    Linux,
    MacOS,
    Windows,
    IOS,
    Android,
}

/// Tryb wdrazania silnika
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeployMode {
    Docker,
    Native,
}

/// Statyczna tablica wszystkich silnikow
static ENGINES: &[EngineDefinition] = &[
    EngineDefinition {
        id: "sglang",
        name: "SGLang",
        description: "High-performance inference with RadixAttention, continuous batching, tensor parallelism",
        supported_platforms: &[Platform::Linux],
        model_format: "safetensors",
        hf_filter_tags: &["text-generation"],
        default_port: 5010,
        shared_model_formats: &["safetensors"],
    },
    EngineDefinition {
        id: "vllm",
        name: "vLLM",
        description: "High-throughput inference with PagedAttention, continuous batching",
        supported_platforms: &[Platform::Linux],
        model_format: "safetensors",
        hf_filter_tags: &["text-generation"],
        default_port: 5010,
        shared_model_formats: &["safetensors"],
    },
    EngineDefinition {
        id: "ollama",
        name: "Ollama",
        description: "Easy-to-use LLM runner with built-in model library",
        supported_platforms: &[Platform::Linux, Platform::MacOS, Platform::Windows],
        model_format: "ollama",
        hf_filter_tags: &[],
        default_port: 11434,
        shared_model_formats: &["gguf"],
    },
    EngineDefinition {
        id: "llamacpp",
        name: "LLama.cpp",
        description: "Lightweight C/C++ inference for GGUF models, runs on CPU and GPU",
        supported_platforms: &[Platform::Linux, Platform::MacOS, Platform::Windows, Platform::IOS, Platform::Android],
        model_format: "gguf",
        hf_filter_tags: &["gguf"],
        default_port: 5010,
        shared_model_formats: &["gguf"],
    },
    EngineDefinition {
        id: "mlx",
        name: "MLX",
        description: "Apple MLX framework for Apple Silicon inference",
        supported_platforms: &[Platform::MacOS, Platform::IOS],
        model_format: "mlx",
        hf_filter_tags: &["mlx"],
        default_port: 5010,
        shared_model_formats: &["mlx", "safetensors"],
    },
];

/// Zwraca wszystkie zdefiniowane silniki
pub fn all_engines() -> &'static [EngineDefinition] {
    ENGINES
}

/// Zwraca silniki dostepne na danej platformie
pub fn engines_for_platform(platform: &Platform) -> Vec<&'static EngineDefinition> {
    ENGINES
        .iter()
        .filter(|e| e.supported_platforms.contains(platform))
        .collect()
}

/// Zwraca silnik po ID
pub fn engine_by_id(id: &str) -> Option<&'static EngineDefinition> {
    ENGINES.iter().find(|e| e.id == id)
}

/// Parsuje string OS na Platform
pub fn parse_platform(os_info: &str) -> Platform {
    let lower = os_info.to_lowercase();
    if lower.contains("linux") {
        Platform::Linux
    } else if lower.contains("ios") || lower.contains("ipad") || lower.contains("iphone") {
        Platform::IOS
    } else if lower.contains("mac") || lower.contains("darwin") {
        Platform::MacOS
    } else if lower.contains("android") {
        Platform::Android
    } else if lower.contains("windows") || lower.contains("win") {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

/// Okresla tryb wdrazania dla silnika na platformie
pub fn deploy_mode_for(engine_id: &str, platform: &Platform) -> DeployMode {
    match (engine_id, platform) {
        ("sglang", Platform::Linux) => DeployMode::Docker,
        ("vllm", Platform::Linux) => DeployMode::Docker,
        ("ollama", Platform::Linux) => DeployMode::Docker,
        ("llamacpp", Platform::Linux) => DeployMode::Docker,
        _ => DeployMode::Native,
    }
}

/// Zwraca platforme biezacego hosta
pub fn current_platform() -> Platform {
    #[cfg(target_os = "macos")]
    { Platform::MacOS }
    #[cfg(target_os = "linux")]
    { Platform::Linux }
    #[cfg(target_os = "windows")]
    { Platform::Windows }
    #[cfg(target_os = "ios")]
    { Platform::IOS }
    #[cfg(target_os = "android")]
    { Platform::Android }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows", target_os = "ios", target_os = "android")))]
    { Platform::Linux }
}

/// Serializowalna wersja EngineDefinition do odpowiedzi JSON
#[derive(Debug, Serialize)]
pub struct EngineInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub model_format: String,
    pub hf_filter_tags: Vec<String>,
    pub default_port: u16,
    pub deploy_mode: String,
    pub supported_platforms: Vec<String>,
}

impl EngineDefinition {
    /// Konwertuje na EngineInfo z trybu wdrazania dla danej platformy
    pub fn to_info(&self, platform: &Platform) -> EngineInfo {
        let mode = deploy_mode_for(self.id, platform);
        EngineInfo {
            id: self.id.to_string(),
            name: self.name.to_string(),
            description: self.description.to_string(),
            model_format: self.model_format.to_string(),
            hf_filter_tags: self.hf_filter_tags.iter().map(|s| s.to_string()).collect(),
            default_port: self.default_port,
            deploy_mode: match mode {
                DeployMode::Docker => "docker".to_string(),
                DeployMode::Native => "native".to_string(),
            },
            supported_platforms: self.supported_platforms.iter().map(|p| format!("{:?}", p)).collect(),
        }
    }
}
