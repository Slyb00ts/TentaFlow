// ============ File: types.rs — service manifest TOML deserialisation types ============

use serde::{Deserialize, Serialize};

/// Full manifest for a single engine, including its deploy modes and model presets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceManifest {
    pub engine: Engine,
    pub deploy: DeploySection,
    #[serde(default, rename = "model_preset")]
    pub model_presets: Vec<ModelPreset>,
    /// Sha256 of the docker build context tree at compile time. Empty when
    /// the manifest has no buildable docker context. Populated by build.rs.
    #[serde(default)]
    pub docker_source_hash: String,
    /// Sha256 of the native build tree (binary/python-bundle) at compile
    /// time. Empty for embedded/external runtimes. Populated by build.rs.
    #[serde(default)]
    pub native_source_hash: String,
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
    /// Optional explicit override of the catalog surface vocabulary
    /// (`["chat"]`, `["stt"]`, ...). When `None` the surfaces are
    /// derived from `category` via `Category::default_service_surfaces`.
    /// An explicit empty list is invalid — use `None` to mean "default".
    #[serde(default)]
    pub service_surfaces: Option<Vec<String>>,
    /// Optional input modality list (`["text", "image", "audio"]`).
    /// `None` falls through to category defaults; explicit empty list
    /// is rejected by validation.
    #[serde(default)]
    pub input_modalities: Option<Vec<String>>,
    /// Optional output modality list (`["text", "audio", "embedding",
    /// "image"]`). Same fallback rules as the input list.
    #[serde(default)]
    pub output_modalities: Option<Vec<String>>,
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

// Wire-string allow-lists live in `vocabulary.rs` so build.rs can `include!`
// the same source. Re-export here keeps existing call sites unchanged.
pub use super::vocabulary::{
    VALID_INPUT_MODALITIES, VALID_OUTPUT_MODALITIES, VALID_SERVICE_SURFACES,
};

impl Category {
    /// Surface(s) implied by this category when the manifest does not
    /// declare `service_surfaces` explicitly. Categories without a
    /// catalog presence (`VideoGen`, `MusicGen`, `Model3dGen`, `Tools`)
    /// return an empty slice; callers treat that as "no public catalog
    /// entry" rather than "match all".
    pub fn default_service_surfaces(self) -> &'static [&'static str] {
        match self {
            Self::Llm => &["chat"],
            Self::Stt => &["stt"],
            Self::Tts => &["tts"],
            Self::Embeddings => &["embeddings"],
            Self::Reranker => &["rerank"],
            Self::Vision => &["chat"],
            Self::ImageGen => &["image_gen"],
            Self::Agents => &["agents"],
            Self::VideoGen | Self::MusicGen | Self::Model3dGen | Self::Tools => &[],
        }
    }

    /// Default input modality vocabulary. Vision adds Image, STT adds
    /// Audio; everything else defaults to text-in.
    pub fn default_input_modalities(self) -> &'static [&'static str] {
        match self {
            Self::Llm | Self::Tts | Self::Embeddings | Self::Reranker | Self::ImageGen => {
                &["text"]
            }
            Self::Stt => &["audio"],
            Self::Vision => &["text", "image"],
            Self::Agents => &["text"],
            Self::VideoGen | Self::MusicGen | Self::Model3dGen | Self::Tools => &[],
        }
    }

    /// Default output modality vocabulary. STT/Reranker emit text;
    /// TTS emits audio; embeddings/image-gen emit their typed payload.
    pub fn default_output_modalities(self) -> &'static [&'static str] {
        match self {
            Self::Llm | Self::Stt | Self::Reranker | Self::Vision | Self::Agents => &["text"],
            Self::Tts => &["audio"],
            Self::Embeddings => &["embedding"],
            Self::ImageGen => &["image"],
            Self::VideoGen | Self::MusicGen | Self::Model3dGen | Self::Tools => &[],
        }
    }
}

impl Engine {
    /// Resolve the surfaces this engine advertises after applying the
    /// preset > engine > category fallback chain. The argument is the
    /// optional preset that the deploy is targeting; pass `None` for
    /// engine-only views.
    pub fn effective_service_surfaces(&self, preset: Option<&ModelPreset>) -> Vec<String> {
        if let Some(p) = preset {
            if let Some(list) = p.service_surfaces.as_ref() {
                return list.clone();
            }
        }
        if let Some(list) = self.service_surfaces.as_ref() {
            return list.clone();
        }
        self.category
            .default_service_surfaces()
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    pub fn effective_input_modalities(&self, preset: Option<&ModelPreset>) -> Vec<String> {
        if let Some(p) = preset {
            if let Some(list) = p.input_modalities.as_ref() {
                return list.clone();
            }
        }
        if let Some(list) = self.input_modalities.as_ref() {
            return list.clone();
        }
        self.category
            .default_input_modalities()
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    pub fn effective_output_modalities(&self, preset: Option<&ModelPreset>) -> Vec<String> {
        if let Some(p) = preset {
            if let Some(list) = p.output_modalities.as_ref() {
                return list.clone();
            }
        }
        if let Some(list) = self.output_modalities.as_ref() {
            return list.clone();
        }
        self.category
            .default_output_modalities()
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
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
    /// How TentaFlow reaches the container at runtime. `sidecar-quic` wraps the
    /// engine's HTTP API in a QUIC sidecar so mesh peers route through QUIC;
    /// `direct-http` exposes the host-mapped HTTP port directly with no sidecar.
    /// Required for every `[deploy.docker]` section as of Phase 6. Modelled as
    /// `Option<>` here so a missing TOML field surfaces as a typed validation
    /// error rather than a serde "missing field" message.
    #[serde(default)]
    pub transport: Option<DockerTransport>,
}

/// Transport variant declared by `[deploy.docker].transport`. The build-time
/// validator in `build.rs` rejects manifests that omit this field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DockerTransport {
    /// TentaFlow runs a Rust QUIC sidecar in front of the engine's HTTP API.
    SidecarQuic,
    /// TentaFlow speaks HTTP directly to the host-mapped engine port.
    DirectHttp,
}

impl DockerTransport {
    /// Stable kebab-case tag used in TOML and JSON.
    pub fn as_kebab_str(self) -> &'static str {
        match self {
            DockerTransport::SidecarQuic => "sidecar-quic",
            DockerTransport::DirectHttp => "direct-http",
        }
    }
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
///
/// The three modality overrides take priority over the engine-level
/// fields when present. This lets a preset declare extra capabilities
/// the base engine does not advertise (e.g. an omni preset of an LLM
/// engine declaring `input_modalities = ["text", "audio"]`). Empty
/// lists are rejected the same way as on `Engine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPreset {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub service_surfaces: Option<Vec<String>>,
    #[serde(default)]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default)]
    pub output_modalities: Option<Vec<String>>,
}
