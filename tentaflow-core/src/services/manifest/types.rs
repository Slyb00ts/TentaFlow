// =============================================================================
// File: types.rs
// Description: Serde types used to deserialize service manifests from TOML.
// They model the schema described in tentaflow-containers/_schema/SCHEMA.md.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Full manifest for a single engine, including its deploy modes and model presets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceManifest {
    pub engine: Engine,
    pub deploy: DeploySection,
    #[serde(default, rename = "model_preset")]
    pub model_presets: Vec<ModelPreset>,
}

/// `[engine]` section with catalog metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Engine {
    pub id: String,
    pub category: Category,
    pub name: String,
    pub description_pl: String,
    pub description_en: String,
    pub homepage: String,
    pub license: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub resource_kind: Option<ResourceKind>,
    #[serde(default)]
    pub requires_model: Option<bool>,
    #[serde(default)]
    pub gpu_supported: Option<bool>,
    pub default_port: u16,
    pub api: ApiKind,
    pub version: String,
}

/// Engine category aligned with the `tentaflow-containers/` directory layout.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Llm,
    Stt,
    Tts,
    Embeddings,
    Reranker,
    Vision,
    ImageGen,
    VideoGen,
    MusicGen,
    Model3dGen,
    Agents,
    Tools,
}

/// API family exposed by the engine.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApiKind {
    OpenaiCompatible,
    OllamaNative,
    SherpaTts,
    SherpaStt,
    Comfyui,
    Custom,
}

/// High-level resource class used by the GUI to distinguish AI runtimes from
/// supporting infrastructure and utility stacks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    Ai,
    Infra,
}

/// `[deploy]` section aggregating optional docker/native/external variants.
/// A manifest must define at least one of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploySection {
    #[serde(default)]
    pub docker: Option<DockerDeploy>,
    #[serde(default)]
    pub native: Option<NativeDeploy>,
    #[serde(default)]
    pub external: Option<ExternalDeploy>,
}

/// `[deploy.docker]` section for Docker deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerDeploy {
    #[serde(default)]
    pub context_path: Option<String>,
    #[serde(default)]
    pub compose_path: Option<String>,
    pub platforms: Vec<TargetOs>,
    #[serde(default)]
    pub download_image: Option<String>,
    #[serde(default)]
    pub download_size_mb: Option<u64>,
}

/// `[deploy.native]` section for native execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeDeploy {
    pub platforms: Vec<TargetOs>,
    pub runtime: NativeRuntime,
    #[serde(default)]
    pub feature_flag: Option<String>,
    #[serde(default)]
    pub binary_path: Option<String>,
    #[serde(default)]
    pub bundle_path: Option<String>,
}

/// Native runtime variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NativeRuntime {
    /// Compiled into `tentaflow` behind a Cargo feature.
    Embedded,
    /// Native binary built by `<binary_path>/build.sh`.
    Binary,
    /// Python bundle managed by TentaFlow.
    PythonBundle,
}

impl NativeRuntime {
    pub fn as_kebab_str(&self) -> &'static str {
        match self {
            NativeRuntime::Embedded => "embedded",
            NativeRuntime::Binary => "binary",
            NativeRuntime::PythonBundle => "python-bundle",
        }
    }
}

/// `[deploy.external]` section for discovering an already running external service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalDeploy {
    pub platforms: Vec<TargetOs>,
    pub detection_binary: String,
    pub detection_endpoint: String,
    #[serde(default = "default_health_path")]
    pub detection_health_path: String,
}

fn default_health_path() -> String {
    "/".to_string()
}

/// Operating system supported by a deploy variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    Linux,
    Macos,
    Windows,
    Ios,
    Android,
}

/// `[[model_preset]]` section with a recommended model entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPreset {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub recommended: bool,
}
