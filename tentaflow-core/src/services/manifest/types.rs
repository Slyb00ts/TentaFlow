// =============================================================================
// Plik: types.rs
// Opis: Typy serde dla deserializacji service manifestow (TOML).
//       Modeluja schemat opisany w tentaflow-containers/_schema/SCHEMA.md.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pelny manifest pojedynczego silnika (engine) wraz z wariantami i presetami.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceManifest {
    pub engine: Engine,
    #[serde(default, rename = "variant")]
    pub variants: Vec<Variant>,
    #[serde(default, rename = "model_preset")]
    pub model_presets: Vec<ModelPreset>,
}

/// Sekcja [engine] — metadane silnika.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Engine {
    pub id: String,
    pub category: Category,
    pub name: String,
    pub description_pl: String,
    pub description_en: String,
    pub homepage: String,
    pub license: String,
    pub api: ApiKind,
    pub default_port: u16,
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub also_serves: Vec<Category>,
    #[serde(default)]
    pub docs_url: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

/// Kategoria silnika (zgodna z layoutem katalogow tentaflow-containers/).
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

/// Rodzaj API udostepnianego przez silnik.
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

/// Pojedynczy wariant deploymentu silnika (np. linux-cuda, embedded-metal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub id: String,
    pub deploy_mode: DeployMode,
    pub target_os: OsList,
    pub target_arch: ArchList,
    pub gpu_backend: GpuBackendList,
    pub status: Status,
    #[serde(default)]
    pub vram_gb_min: Option<u16>,
    #[serde(default)]
    pub ram_gb_min: Option<u16>,
    #[serde(default)]
    pub disk_gb_min: Option<u16>,
    #[serde(default)]
    pub notes_pl: Option<String>,
    #[serde(default)]
    pub notes_en: Option<String>,
    #[serde(default)]
    pub build: Option<BuildOption>,
    #[serde(default)]
    pub download: Option<DownloadOption>,
    #[serde(default)]
    pub feature_flag: Option<FeatureFlagSpec>,
    #[serde(default)]
    pub detection: Option<DetectionSpec>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeployMode {
    Native,
    Docker,
    PythonBundle,
    Embedded,
    External,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    Linux,
    Macos,
    Windows,
    Ios,
    Android,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TargetArch {
    X86_64,
    Aarch64,
    Any,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum GpuBackend {
    Cpu,
    Cuda,
    Rocm,
    Vulkan,
    Metal,
    Mlx,
    Xpu,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Stable,
    Experimental,
    Planned,
    Deprecated,
}

/// Sekcja [variant.build] — sposob lokalnego buildu wariantu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOption {
    pub context_path: String,
    #[serde(default = "default_dockerfile")]
    pub dockerfile: String,
    #[serde(default)]
    pub build_args: HashMap<String, String>,
    #[serde(default)]
    pub tags: Vec<String>,
}
fn default_dockerfile() -> String {
    "Dockerfile".to_string()
}

/// Sekcja [variant.download] — prebuilt artefakt do pobrania.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadOption {
    pub image: String,
    pub digest: String,
    #[serde(default)]
    pub size_mb: Option<u64>,
    #[serde(default = "default_license_required")]
    pub license_required: LicenseTier,
    #[serde(default)]
    pub enabled: bool,
}
fn default_license_required() -> LicenseTier {
    LicenseTier::Pro
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LicenseTier {
    Pro,
    Enterprise,
}

/// Sekcja [variant.feature_flag] — wymagana dla deploy_mode = embedded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlagSpec {
    pub name: String,
}

/// Sekcja [variant.detection] — wymagana dla deploy_mode = external.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionSpec {
    pub binary: String,
    pub endpoint: String,
    #[serde(default = "default_health_path")]
    pub health_path: String,
}
fn default_health_path() -> String {
    "/".to_string()
}

/// Sekcja [[model_preset]] — rekomendowany model dla silnika.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPreset {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub vram_gb_min: Option<u16>,
    #[serde(default)]
    pub recommended: bool,
}

// =============================================================================
// Listy "scalar lub array" — TOML pozwala podawac string albo tablice stringow
// dla pol target_os / target_arch / gpu_backend.
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OsList {
    Single(TargetOs),
    Multi(Vec<TargetOs>),
}
impl OsList {
    pub fn as_vec(&self) -> Vec<TargetOs> {
        match self {
            OsList::Single(o) => vec![*o],
            OsList::Multi(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArchList {
    Single(TargetArch),
    Multi(Vec<TargetArch>),
}
impl ArchList {
    pub fn as_vec(&self) -> Vec<TargetArch> {
        match self {
            ArchList::Single(a) => vec![*a],
            ArchList::Multi(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GpuBackendList {
    Single(GpuBackend),
    Multi(Vec<GpuBackend>),
}
impl GpuBackendList {
    pub fn as_vec(&self) -> Vec<GpuBackend> {
        match self {
            GpuBackendList::Single(b) => vec![*b],
            GpuBackendList::Multi(v) => v.clone(),
        }
    }
}
