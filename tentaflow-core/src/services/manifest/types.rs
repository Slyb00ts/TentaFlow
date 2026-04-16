// =============================================================================
// Plik: types.rs
// Opis: Typy serde dla deserializacji service manifestow (TOML).
//       Modeluja schemat opisany w tentaflow-containers/_schema/SCHEMA.md.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Pelny manifest pojedynczego silnika wraz z trybami deploymentu i presetami modeli.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceManifest {
    pub engine: Engine,
    pub deploy: DeploySection,
    #[serde(default, rename = "model_preset")]
    pub model_presets: Vec<ModelPreset>,
}

/// Sekcja `[engine]` — metadane silnika.
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
    pub default_port: u16,
    pub api: ApiKind,
    pub version: String,
}

/// Kategoria silnika (zgodna z layoutem katalogow `tentaflow-containers/`).
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

/// Sekcja `[deploy]` — agreguje opcjonalne sub-sekcje docker/native/external.
/// Manifest MUSI miec przynajmniej jedna z trzech zdefiniowana.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploySection {
    #[serde(default)]
    pub docker: Option<DockerDeploy>,
    #[serde(default)]
    pub native: Option<NativeDeploy>,
    #[serde(default)]
    pub external: Option<ExternalDeploy>,
}

/// Sekcja `[deploy.docker]` — build kontenera + opcjonalny prebuilt image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerDeploy {
    pub context_path: String,
    pub platforms: Vec<TargetOs>,
    #[serde(default)]
    pub download_image: Option<String>,
    #[serde(default)]
    pub download_size_mb: Option<u64>,
}

/// Sekcja `[deploy.native]` — natywne uruchomienie (embedded/binary/python-bundle).
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

/// Wariant uruchomienia natywnego silnika.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NativeRuntime {
    /// Wkompilowane w `tentaflow` przez Cargo feature.
    Embedded,
    /// Natywna binarka kompilowana skryptem `<binary_path>/build.sh`.
    Binary,
    /// Bundle Pythona (venv + server.py) zarzadzany przez TentaFlow.
    PythonBundle,
}

/// Sekcja `[deploy.external]` — wykrycie zewnetrznego serwisu w PATH.
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

/// System operacyjny obslugiwany przez wariant deploymentu.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    Linux,
    Macos,
    Windows,
    Ios,
    Android,
}

/// Sekcja `[[model_preset]]` — rekomendowany model dla silnika.
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
