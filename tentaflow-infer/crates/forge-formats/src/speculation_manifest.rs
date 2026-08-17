// =============================================================================
// Plik: speculation_manifest.rs
// Opis: Typowany parser i walidator manifestu neuralnych proposerow spekulacyjnych.
// Przykład: SpeculationManifest::load("forge-speculation.json", Some(32))
// =============================================================================

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Component, Path};

use forge_types::{ForgeError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const SPECULATION_FORMAT_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ARTIFACTS: usize = 1024;
const MAX_TENSOR_MAPPINGS: usize = 65_536;
const MAX_FEATURE_LAYERS: usize = 4096;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_DRAFT_NODES: u32 = 65_536;
const MAX_DRAFT_DEPTH: u32 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeuralProposerKind {
    DraftModel,
    Mtp,
    Eagle,
    Dflash,
    Dspark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionMode {
    Standalone,
    Cascade,
    Tree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintAlgorithm {
    Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub algorithm: FingerprintAlgorithm,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInfo {
    pub url: String,
    pub repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LicenseInfo {
    pub code_spdx: String,
    pub weights_spdx: String,
    pub redistribution_allowed: bool,
    pub attribution: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetModel {
    pub model_id: String,
    pub revision: String,
    pub architecture: String,
    pub vocab_size: u32,
    pub hidden_size: u32,
    pub num_layers: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureDimension {
    pub layer_id: u32,
    pub target_size: u32,
    pub proposer_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Weights,
    Config,
    Tokenizer,
    Calibration,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSpec {
    pub role: ArtifactRole,
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TensorMapping {
    pub logical_name: String,
    pub artifact_path: String,
    pub tensor_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedTensor {
    pub proposer_name: String,
    pub target_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeculationDType {
    F32,
    F16,
    Bf16,
    Fp8E4m3,
    Fp8E5m2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantization {
    None,
    Int8,
    Int4,
    Fp8,
    Nvfp4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMode {
    Greedy,
    Multinomial,
    TopK,
    TopP,
    MinP,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceMethod {
    Temperature,
    Logistic,
    Isotonic,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfidenceCalibration {
    pub method: ConfidenceMethod,
    pub acceptance_threshold: f32,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub artifact_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeculationManifest {
    pub format_version: u32,
    pub kind: NeuralProposerKind,
    pub source: SourceInfo,
    pub license: LicenseInfo,
    pub target: TargetModel,
    pub target_fingerprint: Fingerprint,
    pub tokenizer_fingerprint: Fingerprint,
    pub composition: CompositionMode,
    pub max_nodes: u32,
    pub max_depth: u32,
    pub artifacts: Vec<ArtifactSpec>,
    pub tensor_map: Vec<TensorMapping>,
    #[serde(default)]
    pub shared_tensors: Vec<SharedTensor>,
    #[serde(default)]
    pub target_feature_layer_ids: Vec<u32>,
    #[serde(default)]
    pub feature_dimensions: Vec<FeatureDimension>,
    pub dtype: SpeculationDType,
    pub quantization: Quantization,
    #[serde(default)]
    pub block_size: Option<u32>,
    #[serde(default)]
    pub diffusion_steps: Option<u32>,
    #[serde(default)]
    pub confidence_calibration: Option<ConfidenceCalibration>,
    pub supported_sampling_modes: Vec<SamplingMode>,
}

#[derive(Debug)]
pub struct VerifiedArtifact {
    spec: ArtifactSpec,
    file: File,
}

impl VerifiedArtifact {
    pub fn spec(&self) -> &ArtifactSpec {
        &self.spec
    }

    pub fn file(&self) -> &File {
        &self.file
    }
}

#[derive(Debug)]
pub struct VerifiedSpeculationManifest {
    manifest: SpeculationManifest,
    artifacts: Vec<VerifiedArtifact>,
}

impl VerifiedSpeculationManifest {
    pub fn manifest(&self) -> &SpeculationManifest {
        &self.manifest
    }

    pub fn artifacts(&self) -> &[VerifiedArtifact] {
        &self.artifacts
    }
}

impl SpeculationManifest {
    pub fn load(
        path: impl AsRef<Path>,
        target_layer_count: Option<usize>,
    ) -> Result<VerifiedSpeculationManifest> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mut bytes = Vec::new();
        file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return invalid("manifest przekracza limit 1 MiB");
        }
        let manifest = Self::from_slice(&bytes, target_layer_count)?;
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let artifacts = manifest.verify_artifacts(directory)?;
        Ok(VerifiedSpeculationManifest {
            manifest,
            artifacts,
        })
    }

    pub fn from_slice(bytes: &[u8], target_layer_count: Option<usize>) -> Result<Self> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return invalid("manifest przekracza limit 1 MiB");
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| ForgeError::Format(format!("forge-speculation.json: {error}")))?;
        manifest.validate(target_layer_count)?;
        Ok(manifest)
    }

    pub fn validate(&self, target_layer_count: Option<usize>) -> Result<()> {
        if self.format_version != SPECULATION_FORMAT_VERSION {
            return invalid(format!(
                "nieobslugiwana wersja formatu {}",
                self.format_version
            ));
        }
        validate_fingerprint("target_fingerprint", &self.target_fingerprint)?;
        validate_fingerprint("tokenizer_fingerprint", &self.tokenizer_fingerprint)?;
        self.validate_source_and_license()?;
        self.validate_target(target_layer_count)?;
        if self.max_nodes == 0
            || self.max_depth == 0
            || self.max_nodes > MAX_DRAFT_NODES
            || self.max_depth > MAX_DRAFT_DEPTH
        {
            return invalid("max_nodes lub max_depth jest poza obslugiwanym zakresem");
        }
        self.max_nodes
            .checked_mul(self.max_depth)
            .ok_or_else(|| format_error("iloczyn max_nodes i max_depth przekracza zakres u32"))?;
        if self.max_depth > self.max_nodes {
            return invalid("max_depth nie moze byc wieksze od max_nodes");
        }
        if self.artifacts.is_empty() {
            return invalid("manifest musi zawierac co najmniej jeden artefakt");
        }
        if self.tensor_map.is_empty() {
            return invalid("tensor_map nie moze byc pusta");
        }
        if self.supported_sampling_modes.is_empty() {
            return invalid("supported_sampling_modes nie moze byc puste");
        }
        if self.artifacts.len() > MAX_ARTIFACTS
            || self.tensor_map.len() > MAX_TENSOR_MAPPINGS
            || self.shared_tensors.len() > MAX_TENSOR_MAPPINGS
            || self.target_feature_layer_ids.len() > MAX_FEATURE_LAYERS
            || self.feature_dimensions.len() > MAX_FEATURE_LAYERS
            || self.supported_sampling_modes.len() > 16
        {
            return invalid("liczba elementow manifestu przekracza obslugiwany limit");
        }

        self.validate_artifacts()?;
        self.validate_tensor_map()?;
        validate_shared_tensors(&self.shared_tensors)?;
        validate_unique_values(
            "target_feature_layer_ids",
            self.target_feature_layer_ids.iter().copied(),
        )?;
        validate_unique_values(
            "feature_dimensions.layer_id",
            self.feature_dimensions
                .iter()
                .map(|feature| feature.layer_id),
        )?;
        validate_unique_values(
            "supported_sampling_modes",
            self.supported_sampling_modes.iter().copied(),
        )?;

        let layer_count = usize::try_from(self.target.num_layers)
            .map_err(|_| format_error("target.num_layers nie miesci sie w usize"))?;
        {
            for &layer_id in &self.target_feature_layer_ids {
                let layer_id = usize::try_from(layer_id)
                    .map_err(|_| format_error("indeks warstwy nie miesci sie w usize"))?;
                if layer_id >= layer_count {
                    return invalid(format!(
                        "target_feature_layer_ids zawiera warstwe {layer_id} poza modelem"
                    ));
                }
                if matches!(
                    self.kind,
                    NeuralProposerKind::Dspark | NeuralProposerKind::Dflash
                ) && layer_id.checked_add(1) == Some(layer_count)
                {
                    return invalid("DSpark i DFlash nie moga uzywac ostatniej warstwy modelu");
                }
            }
        }

        if let Some(block_size) = self.block_size {
            if block_size == 0 || block_size > self.max_nodes {
                return invalid("block_size musi nalezec do zakresu 1..=max_nodes");
            }
        }
        if self.diffusion_steps == Some(0) {
            return invalid("diffusion_steps musi byc wieksze od zera");
        }
        if let Some(calibration) = &self.confidence_calibration {
            self.validate_calibration(calibration)?;
        }

        self.validate_kind_requirements()
    }

    pub fn verify_artifacts(
        &self,
        manifest_directory: impl AsRef<Path>,
    ) -> Result<Vec<VerifiedArtifact>> {
        let base = std::fs::canonicalize(manifest_directory.as_ref()).map_err(|error| {
            format_error(format!("nie mozna ustalic katalogu manifestu: {error}"))
        })?;
        let mut verified = Vec::with_capacity(self.artifacts.len());
        for artifact in &self.artifacts {
            let candidate = base.join(&artifact.path);
            let canonical = std::fs::canonicalize(&candidate).map_err(|error| {
                format_error(format!(
                    "nie mozna otworzyc artefaktu {}: {error}",
                    artifact.path
                ))
            })?;
            if !canonical.starts_with(&base) {
                return invalid(format!(
                    "artefakt wychodzi poza katalog manifestu: {}",
                    artifact.path
                ));
            }
            let mut file = File::open(&canonical)?;
            validate_open_file_location(&file, &canonical, &base, &artifact.path)?;
            let actual = sha256_file(&mut file)?;
            if !actual.eq_ignore_ascii_case(&artifact.sha256) {
                return invalid(format!(
                    "niezgodny SHA-256 artefaktu {}: oczekiwano {}, otrzymano {actual}",
                    artifact.path, artifact.sha256
                ));
            }
            file.seek(SeekFrom::Start(0))?;
            verified.push(VerifiedArtifact {
                spec: artifact.clone(),
                file,
            });
        }
        Ok(verified)
    }

    fn validate_source_and_license(&self) -> Result<()> {
        validate_http_url("source.url", &self.source.url)?;
        validate_http_url("source.repository", &self.source.repository)?;
        validate_spdx("license.code_spdx", &self.license.code_spdx)?;
        validate_spdx("license.weights_spdx", &self.license.weights_spdx)?;
        validate_name("license.attribution", &self.license.attribution)
    }

    fn validate_target(&self, target_layer_count: Option<usize>) -> Result<()> {
        validate_name("target.model_id", &self.target.model_id)?;
        validate_name("target.revision", &self.target.revision)?;
        validate_name("target.architecture", &self.target.architecture)?;
        if self.target.vocab_size == 0
            || self.target.hidden_size == 0
            || self.target.num_layers == 0
        {
            return invalid("rozmiary targetu i num_layers musza byc wieksze od zera");
        }
        if let Some(layer_count) = target_layer_count {
            let declared = usize::try_from(self.target.num_layers)
                .map_err(|_| format_error("target.num_layers nie miesci sie w usize"))?;
            if layer_count != declared {
                return invalid(format!(
                    "target.num_layers={declared} nie zgadza sie z modelem ({layer_count})"
                ));
            }
        }
        Ok(())
    }

    fn validate_artifacts(&self) -> Result<()> {
        let mut paths = HashSet::with_capacity(self.artifacts.len());
        for artifact in &self.artifacts {
            validate_relative_path("artifacts.path", &artifact.path)?;
            validate_sha256("artifacts.sha256", &artifact.sha256)?;
            if !paths.insert(artifact.path.as_str()) {
                return invalid(format!("zduplikowana sciezka artefaktu: {}", artifact.path));
            }
        }
        if !self
            .artifacts
            .iter()
            .any(|artifact| artifact.role == ArtifactRole::Weights)
        {
            return invalid("brak artefaktu o roli weights");
        }
        Ok(())
    }

    fn validate_tensor_map(&self) -> Result<()> {
        let artifact_paths: HashSet<&str> = self
            .artifacts
            .iter()
            .map(|artifact| artifact.path.as_str())
            .collect();
        let mut logical_names = HashSet::with_capacity(self.tensor_map.len());
        for mapping in &self.tensor_map {
            validate_name("tensor_map.logical_name", &mapping.logical_name)?;
            validate_name("tensor_map.tensor_name", &mapping.tensor_name)?;
            validate_relative_path("tensor_map.artifact_path", &mapping.artifact_path)?;
            if !artifact_paths.contains(mapping.artifact_path.as_str()) {
                return invalid(format!(
                    "tensor_map odwoluje sie do nieznanego artefaktu: {}",
                    mapping.artifact_path
                ));
            }
            if !logical_names.insert(mapping.logical_name.as_str()) {
                return invalid(format!(
                    "zduplikowana nazwa logiczna tensora: {}",
                    mapping.logical_name
                ));
            }
        }
        Ok(())
    }

    fn validate_calibration(&self, calibration: &ConfidenceCalibration) -> Result<()> {
        if !calibration.acceptance_threshold.is_finite()
            || !(0.0..=1.0).contains(&calibration.acceptance_threshold)
        {
            return invalid("acceptance_threshold musi byc skonczone i nalezec do [0, 1]");
        }
        match (calibration.method, calibration.temperature) {
            (ConfidenceMethod::Temperature, Some(value)) if value.is_finite() && value > 0.0 => {}
            (ConfidenceMethod::Temperature, _) => {
                return invalid("kalibracja temperature wymaga dodatniej temperatury")
            }
            (_, Some(_)) => {
                return invalid("temperature jest dozwolone tylko dla metody temperature")
            }
            _ => {}
        }
        if let Some(path) = &calibration.artifact_path {
            validate_relative_path("confidence_calibration.artifact_path", path)?;
            if !self.artifacts.iter().any(|artifact| artifact.path == *path) {
                return invalid("kalibracja odwoluje sie do nieznanego artefaktu");
            }
        }
        if matches!(
            calibration.method,
            ConfidenceMethod::Logistic | ConfidenceMethod::Isotonic
        ) && calibration.artifact_path.is_none()
        {
            return invalid("kalibracja logistic/isotonic wymaga artifact_path");
        }
        Ok(())
    }

    fn validate_kind_requirements(&self) -> Result<()> {
        let requires_block = matches!(
            self.kind,
            NeuralProposerKind::Mtp
                | NeuralProposerKind::Eagle
                | NeuralProposerKind::Dflash
                | NeuralProposerKind::Dspark
        );
        if requires_block && self.block_size.is_none() {
            return invalid("wybrany rodzaj proposera wymaga block_size");
        }
        let requires_features = matches!(
            self.kind,
            NeuralProposerKind::Eagle | NeuralProposerKind::Dflash | NeuralProposerKind::Dspark
        );
        if requires_features && self.target_feature_layer_ids.is_empty() {
            return invalid("wybrany rodzaj proposera wymaga target_feature_layer_ids");
        }
        if requires_features && self.feature_dimensions.is_empty() {
            return invalid("wybrany rodzaj proposera wymaga feature_dimensions");
        }
        let feature_ids: HashSet<u32> = self
            .feature_dimensions
            .iter()
            .map(|feature| feature.layer_id)
            .collect();
        let target_ids: HashSet<u32> = self.target_feature_layer_ids.iter().copied().collect();
        if feature_ids != target_ids {
            return invalid("feature_dimensions nie odpowiada target_feature_layer_ids");
        }
        for feature in &self.feature_dimensions {
            if feature.target_size == 0 || feature.proposer_size == 0 {
                return invalid("wymiary cech musza byc wieksze od zera");
            }
            if feature.target_size != self.target.hidden_size {
                return invalid("target_size cechy nie zgadza sie z target.hidden_size");
            }
            if feature.proposer_size != feature.target_size {
                return invalid("wymiar cechy proposera nie zgadza sie z cecha targetu");
            }
        }
        if self.kind == NeuralProposerKind::Dflash && self.diffusion_steps.is_none() {
            return invalid("DFlash wymaga diffusion_steps");
        }
        if self.kind != NeuralProposerKind::Dflash && self.diffusion_steps.is_some() {
            return invalid("diffusion_steps jest dozwolone tylko dla DFlash");
        }
        if matches!(
            self.kind,
            NeuralProposerKind::Dflash | NeuralProposerKind::Dspark
        ) && self.confidence_calibration.is_none()
        {
            return invalid("DSpark i DFlash wymagaja confidence_calibration");
        }
        Ok(())
    }
}

fn sha256_file(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_open_file_location(
    file: &File,
    _canonical: &Path,
    base: &Path,
    display_path: &str,
) -> Result<()> {
    if !file.metadata()?.is_file() {
        return invalid(format!("artefakt nie jest plikiem: {display_path}"));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        use std::path::PathBuf;

        let descriptor = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        let opened = std::fs::canonicalize(descriptor).map_err(|error| {
            format_error(format!(
                "nie mozna potwierdzic artefaktu {display_path}: {error}"
            ))
        })?;
        if !opened.starts_with(base) {
            return invalid(format!(
                "otwarty artefakt wychodzi poza katalog manifestu: {display_path}"
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let current = std::fs::canonicalize(_canonical)?;
        if !current.starts_with(base) {
            return invalid(format!(
                "otwarty artefakt wychodzi poza katalog manifestu: {display_path}"
            ));
        }
        let before = std::fs::metadata(&current)?;
        let opened = file.metadata()?;
        #[cfg(unix)]
        let same_file = {
            use std::os::unix::fs::MetadataExt;
            before.dev() == opened.dev() && before.ino() == opened.ino()
        };
        #[cfg(not(unix))]
        let same_file =
            before.len() == opened.len() && before.modified().ok() == opened.modified().ok();
        if !same_file {
            return invalid(format!(
                "artefakt zmienil sie podczas otwierania: {display_path}"
            ));
        }
    }
    Ok(())
}

fn validate_http_url(field: &str, value: &str) -> Result<()> {
    if !(value.starts_with("https://") || value.starts_with("http://"))
        || value.len() <= "http://".len()
        || value.len() > MAX_TEXT_BYTES
        || value.chars().any(char::is_whitespace)
    {
        return invalid(format!("{field} musi byc adresem HTTP(S)"));
    }
    Ok(())
}

fn validate_spdx(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.eq_ignore_ascii_case("unknown")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'+'))
    {
        return invalid(format!("{field} nie jest pojedynczym identyfikatorem SPDX"));
    }
    Ok(())
}

fn validate_fingerprint(field: &str, fingerprint: &Fingerprint) -> Result<()> {
    match fingerprint.algorithm {
        FingerprintAlgorithm::Sha256 => validate_sha256(field, &fingerprint.value),
    }
}

fn validate_sha256(field: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return invalid(format!("{field} musi zawierac 64-znakowy SHA-256"));
    }
    Ok(())
}

fn validate_relative_path(field: &str, value: &str) -> Result<()> {
    let has_unsafe_segment = value
        .split(['/', '\\'])
        .any(|segment| segment == "." || segment == "..");
    if value.is_empty()
        || value.len() > MAX_TEXT_BYTES
        || Path::new(value).is_absolute()
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.as_bytes().get(1) == Some(&b':')
        || has_unsafe_segment
        || Path::new(value).components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return invalid(format!(
            "{field} musi byc bezpieczna sciezka wzgledna: {value}"
        ));
    }
    Ok(())
}

fn validate_name(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > MAX_TEXT_BYTES {
        return invalid(format!("{field} nie moze byc puste"));
    }
    Ok(())
}

fn validate_shared_tensors(tensors: &[SharedTensor]) -> Result<()> {
    let mut proposer_names = HashSet::with_capacity(tensors.len());
    let mut target_names = HashSet::with_capacity(tensors.len());
    for tensor in tensors {
        validate_name("shared_tensors.proposer_name", &tensor.proposer_name)?;
        validate_name("shared_tensors.target_name", &tensor.target_name)?;
        if !proposer_names.insert(tensor.proposer_name.as_str()) {
            return invalid(format!(
                "zduplikowany proposer_name w shared_tensors: {}",
                tensor.proposer_name
            ));
        }
        if !target_names.insert(tensor.target_name.as_str()) {
            return invalid(format!(
                "zduplikowany target_name w shared_tensors: {}",
                tensor.target_name
            ));
        }
    }
    Ok(())
}

fn validate_unique_values<T>(field: &str, values: impl Iterator<Item = T>) -> Result<()>
where
    T: Copy + Eq + std::hash::Hash + std::fmt::Debug,
{
    let mut unique = HashSet::new();
    for value in values {
        if !unique.insert(value) {
            return invalid(format!("{field} zawiera duplikat: {value:?}"));
        }
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(format_error(message))
}

fn format_error(message: impl Into<String>) -> ForgeError {
    ForgeError::Format(format!("forge-speculation.json: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest(kind: &str, extra: &str) -> Vec<u8> {
        format!(
            r#"{{
                "format_version": 1,
                "kind": "{kind}",
                "source": {{
                    "url":"https://models.example/proposer",
                    "repository":"https://github.com/example/proposer"
                }},
                "license": {{
                    "code_spdx":"Apache-2.0",
                    "weights_spdx":"Apache-2.0",
                    "redistribution_allowed":true,
                    "attribution":"Example Authors"
                }},
                "target": {{
                    "model_id":"example/target",
                    "revision":"0123456789abcdef",
                    "architecture":"LlamaForCausalLM",
                    "vocab_size":32000,
                    "hidden_size":4096,
                    "num_layers":12
                }},
                "target_fingerprint": {{"algorithm":"sha256","value":"{HASH}"}},
                "tokenizer_fingerprint": {{"algorithm":"sha256","value":"{HASH}"}},
                "composition": "cascade",
                "max_nodes": 8,
                "max_depth": 4,
                "artifacts": [
                    {{"role":"weights","path":"model/proposer.safetensors","sha256":"{HASH}"}},
                    {{"role":"calibration","path":"model/calibration.json","sha256":"{HASH}"}}
                ],
                "tensor_map": [{{
                    "logical_name":"embed_tokens.weight",
                    "artifact_path":"model/proposer.safetensors",
                    "tensor_name":"model.embed_tokens.weight"
                }}],
                "shared_tensors": [{{"proposer_name":"lm_head.weight","target_name":"lm_head.weight"}}],
                "target_feature_layer_ids": [3, 7],
                "feature_dimensions": [
                    {{"layer_id":3,"target_size":4096,"proposer_size":4096}},
                    {{"layer_id":7,"target_size":4096,"proposer_size":4096}}
                ],
                "dtype": "bf16",
                "quantization": "nvfp4",
                "supported_sampling_modes": ["greedy", "top_p"]
                {extra}
            }}"#
        )
        .into_bytes()
    }

    fn parse(kind: &str, extra: &str, layers: Option<usize>) -> Result<SpeculationManifest> {
        SpeculationManifest::from_slice(&manifest(kind, extra), layers)
    }

    #[test]
    fn parses_valid_dspark_manifest() {
        let parsed = parse(
            "dspark",
            r#", "block_size": 4, "confidence_calibration": {
                "method":"temperature", "acceptance_threshold":0.7, "temperature":1.2
            }"#,
            Some(12),
        )
        .unwrap();
        assert_eq!(parsed.kind, NeuralProposerKind::Dspark);
        assert_eq!(parsed.block_size, Some(4));
    }

    #[test]
    fn validates_required_fields_for_each_kind() {
        assert!(parse("draft_model", "", Some(12)).is_ok());
        assert!(parse("mtp", "", Some(12)).is_err());
        assert!(parse("eagle", r#", "block_size": 4"#, Some(12)).is_ok());
        assert!(parse("dflash", r#", "block_size": 4"#, Some(12)).is_err());
        assert!(parse("dspark", r#", "block_size": 4"#, Some(12)).is_err());
    }

    #[test]
    fn rejects_unknown_version_and_zero_or_overflow_limits() {
        let unknown = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace("\"format_version\": 1", "\"format_version\": 2");
        assert!(SpeculationManifest::from_slice(unknown.as_bytes(), None).is_err());

        let zero = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace("\"max_nodes\": 8", "\"max_nodes\": 0");
        assert!(SpeculationManifest::from_slice(zero.as_bytes(), None).is_err());

        let overflow = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace("\"max_nodes\": 8", "\"max_nodes\": 4294967295")
            .replace("\"max_depth\": 4", "\"max_depth\": 4294967295");
        assert!(SpeculationManifest::from_slice(overflow.as_bytes(), None).is_err());
    }

    #[test]
    fn rejects_manifest_larger_than_parser_limit() {
        let input = vec![b' '; MAX_MANIFEST_BYTES as usize + 1];
        assert!(SpeculationManifest::from_slice(&input, None).is_err());
    }

    #[test]
    fn rejects_unsafe_artifact_paths() {
        for path in [
            "../secret",
            "model\\..\\secret",
            "/tmp/model",
            "C:\\model",
            "\\\\server\\share",
        ] {
            let input = String::from_utf8(manifest("draft_model", ""))
                .unwrap()
                .replace("model/proposer.safetensors", path);
            assert!(
                SpeculationManifest::from_slice(input.as_bytes(), None).is_err(),
                "zaakceptowano {path}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_tensor_layers_and_sampling_modes() {
        let duplicate_artifact = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace("model/calibration.json", "model/proposer.safetensors");
        assert!(SpeculationManifest::from_slice(duplicate_artifact.as_bytes(), None).is_err());

        let duplicate_tensor = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace(
                r#""tensor_map": [{
                    "logical_name":"embed_tokens.weight",
                    "artifact_path":"model/proposer.safetensors",
                    "tensor_name":"model.embed_tokens.weight"
                }]"#,
                r#""tensor_map": [
                    {"logical_name":"x","artifact_path":"model/proposer.safetensors","tensor_name":"a"},
                    {"logical_name":"x","artifact_path":"model/proposer.safetensors","tensor_name":"b"}
                ]"#,
            );
        assert!(SpeculationManifest::from_slice(duplicate_tensor.as_bytes(), None).is_err());

        let duplicate_layer = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace("[3, 7]", "[3, 3]");
        assert!(SpeculationManifest::from_slice(duplicate_layer.as_bytes(), None).is_err());

        let duplicate_sampling = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace("[\"greedy\", \"top_p\"]", "[\"greedy\", \"greedy\"]");
        assert!(SpeculationManifest::from_slice(duplicate_sampling.as_bytes(), None).is_err());
    }

    #[test]
    fn rejects_final_target_layer_for_dspark_and_dflash() {
        let calibration = r#", "block_size": 4, "confidence_calibration": {
            "method":"temperature", "acceptance_threshold":0.5, "temperature":1.0
        }"#;
        let dspark = String::from_utf8(manifest("dspark", calibration))
            .unwrap()
            .replace("\"num_layers\":12", "\"num_layers\":8");
        assert!(SpeculationManifest::from_slice(dspark.as_bytes(), Some(8)).is_err());
        let dflash = String::from_utf8(manifest(
            "dflash",
            r#", "block_size":4, "diffusion_steps":3, "confidence_calibration": {
                "method":"temperature", "acceptance_threshold":0.5, "temperature":1.0
            }"#,
        ))
        .unwrap()
        .replace("\"num_layers\":12", "\"num_layers\":8");
        assert!(SpeculationManifest::from_slice(dflash.as_bytes(), Some(8)).is_err());
        assert!(parse("dspark", calibration, None).is_ok());
    }

    #[test]
    fn rejects_invalid_hash_and_calibration() {
        let invalid_hash = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replacen(HASH, "abc", 1);
        assert!(SpeculationManifest::from_slice(invalid_hash.as_bytes(), None).is_err());

        assert!(parse(
            "dspark",
            r#", "block_size":4, "confidence_calibration": {
                "method":"temperature", "acceptance_threshold":1.5, "temperature":0.0
            }"#,
            None,
        )
        .is_err());
    }

    #[test]
    fn rejects_target_feature_and_license_mismatch() {
        let target_layers = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace("\"num_layers\":12", "\"num_layers\":16");
        assert!(SpeculationManifest::from_slice(target_layers.as_bytes(), Some(12)).is_err());

        let feature = String::from_utf8(manifest("eagle", r#", "block_size":4"#))
            .unwrap()
            .replacen("\"proposer_size\":4096", "\"proposer_size\":2048", 1);
        assert!(SpeculationManifest::from_slice(feature.as_bytes(), Some(12)).is_err());

        let license = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replace(
                "\"weights_spdx\":\"Apache-2.0\"",
                "\"weights_spdx\":\"unknown\"",
            );
        assert!(SpeculationManifest::from_slice(license.as_bytes(), Some(12)).is_err());
    }

    #[test]
    fn load_verifies_artifact_hashes() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("model")).unwrap();
        std::fs::write(
            directory.path().join("model/proposer.safetensors"),
            b"weights",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("model/calibration.json"),
            b"calibration",
        )
        .unwrap();
        let weights_hash = format!("{:x}", Sha256::digest(b"weights"));
        let calibration_hash = format!("{:x}", Sha256::digest(b"calibration"));
        let input = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replacen(HASH, &weights_hash, 3)
            .replacen(HASH, &calibration_hash, 1);
        let path = directory.path().join("forge-speculation.json");
        std::fs::write(&path, &input).unwrap();
        assert!(SpeculationManifest::load(&path, Some(12)).is_ok());

        std::fs::write(
            directory.path().join("model/proposer.safetensors"),
            b"tampered",
        )
        .unwrap();
        assert!(SpeculationManifest::load(&path, Some(12)).is_err());
    }

    #[test]
    fn verified_artifact_handle_keeps_hashed_bytes_after_path_replacement() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("model")).unwrap();
        std::fs::write(
            directory.path().join("model/proposer.safetensors"),
            b"weights",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("model/calibration.json"),
            b"calibration",
        )
        .unwrap();
        let weights_hash = format!("{:x}", Sha256::digest(b"weights"));
        let calibration_hash = format!("{:x}", Sha256::digest(b"calibration"));
        let input = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replacen(HASH, &weights_hash, 3)
            .replacen(HASH, &calibration_hash, 1);
        let path = directory.path().join("forge-speculation.json");
        std::fs::write(&path, input).unwrap();
        let loaded = SpeculationManifest::load(&path, Some(12)).unwrap();

        std::fs::remove_file(directory.path().join("model/proposer.safetensors")).unwrap();
        std::fs::write(
            directory.path().join("model/proposer.safetensors"),
            b"tampered",
        )
        .unwrap();
        let weights = loaded
            .artifacts()
            .iter()
            .find(|artifact| artifact.spec().role == ArtifactRole::Weights)
            .unwrap();
        let mut file = weights.file().try_clone().unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"weights");
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("model")).unwrap();
        std::fs::write(outside.path().join("weights"), b"weights").unwrap();
        std::fs::write(
            directory.path().join("model/calibration.json"),
            b"calibration",
        )
        .unwrap();
        symlink(
            outside.path().join("weights"),
            directory.path().join("model/proposer.safetensors"),
        )
        .unwrap();
        let weights_hash = format!("{:x}", Sha256::digest(b"weights"));
        let calibration_hash = format!("{:x}", Sha256::digest(b"calibration"));
        let input = String::from_utf8(manifest("draft_model", ""))
            .unwrap()
            .replacen(HASH, &weights_hash, 3)
            .replacen(HASH, &calibration_hash, 1);
        let path = directory.path().join("forge-speculation.json");
        std::fs::write(&path, input).unwrap();
        assert!(SpeculationManifest::load(&path, Some(12)).is_err());
    }
}
