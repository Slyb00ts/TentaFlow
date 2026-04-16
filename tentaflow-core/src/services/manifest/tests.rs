// =============================================================================
// Plik: tests.rs
// Opis: Testy jednostkowe modulu service manifest — parsowanie TOML, walidacja
//       semantyczna 9 regul, ManifestRegistry oraz integracja z LicenseChecker.
// =============================================================================

#![cfg(test)]

use super::types::*;
use super::validate::{validate_engine, validate_engine_id, ValidationError};
use crate::license::{LicenseChecker, LicenseError, StaticLicenseChecker};
use std::collections::HashMap;
use tempfile::TempDir;

// =============================================================================
// Helpery — fabryki obiektow testowych
// =============================================================================

/// Buduje minimalny prawidlowy [Engine] dla testow.
fn make_engine(id: &str, category: Category) -> Engine {
    Engine {
        id: id.to_string(),
        category,
        name: format!("Engine {id}"),
        description_pl: "opis".to_string(),
        description_en: "desc".to_string(),
        homepage: "https://example.com".to_string(),
        license: "MIT".to_string(),
        api: ApiKind::OpenaiCompatible,
        default_port: 8000,
        version: "1.0.0".to_string(),
        tags: Vec::new(),
        also_serves: Vec::new(),
        docs_url: None,
        icon: None,
    }
}

/// Buduje wariant Docker (ma build, brak feature_flag/detection).
fn make_docker_variant(
    id: &str,
    os: OsList,
    arch: ArchList,
    gpu: GpuBackendList,
) -> Variant {
    Variant {
        id: id.to_string(),
        deploy_mode: DeployMode::Docker,
        target_os: os,
        target_arch: arch,
        gpu_backend: gpu,
        status: Status::Stable,
        vram_gb_min: None,
        ram_gb_min: None,
        disk_gb_min: None,
        notes_pl: None,
        notes_en: None,
        build: Some(BuildOption {
            context_path: "engines/test".to_string(),
            dockerfile: "Dockerfile".to_string(),
            build_args: HashMap::new(),
            tags: Vec::new(),
        }),
        download: None,
        feature_flag: None,
        detection: None,
    }
}

/// Buduje wariant Embedded (ma feature_flag, brak build).
fn make_embedded_variant(
    id: &str,
    os: OsList,
    arch: ArchList,
    gpu: GpuBackendList,
) -> Variant {
    Variant {
        id: id.to_string(),
        deploy_mode: DeployMode::Embedded,
        target_os: os,
        target_arch: arch,
        gpu_backend: gpu,
        status: Status::Stable,
        vram_gb_min: None,
        ram_gb_min: None,
        disk_gb_min: None,
        notes_pl: None,
        notes_en: None,
        build: None,
        download: None,
        feature_flag: Some(FeatureFlagSpec {
            name: "test-feature".to_string(),
        }),
        detection: None,
    }
}

/// Buduje wariant Native (ma build).
fn make_native_variant(
    id: &str,
    os: OsList,
    arch: ArchList,
    gpu: GpuBackendList,
) -> Variant {
    Variant {
        id: id.to_string(),
        deploy_mode: DeployMode::Native,
        target_os: os,
        target_arch: arch,
        gpu_backend: gpu,
        status: Status::Stable,
        vram_gb_min: None,
        ram_gb_min: None,
        disk_gb_min: None,
        notes_pl: None,
        notes_en: None,
        build: Some(BuildOption {
            context_path: "engines/test-native".to_string(),
            dockerfile: "Dockerfile".to_string(),
            build_args: HashMap::new(),
            tags: Vec::new(),
        }),
        download: None,
        feature_flag: None,
        detection: None,
    }
}

/// Buduje wariant External (ma detection).
fn make_external_variant(
    id: &str,
    os: OsList,
    arch: ArchList,
    gpu: GpuBackendList,
) -> Variant {
    Variant {
        id: id.to_string(),
        deploy_mode: DeployMode::External,
        target_os: os,
        target_arch: arch,
        gpu_backend: gpu,
        status: Status::Stable,
        vram_gb_min: None,
        ram_gb_min: None,
        disk_gb_min: None,
        notes_pl: None,
        notes_en: None,
        build: None,
        download: None,
        feature_flag: None,
        detection: Some(DetectionSpec {
            binary: "test".to_string(),
            endpoint: "http://localhost:8080".to_string(),
            health_path: "/health".to_string(),
        }),
    }
}

/// Skladaja manifest z [Engine] i listy wariantow.
fn make_manifest(engine: Engine, variants: Vec<Variant>) -> ServiceManifest {
    ServiceManifest {
        engine,
        variants,
        model_presets: Vec::new(),
    }
}

/// Lekka kopia ManifestRegistry uzywana w testach (poniewaz pole `engines`
/// jest prywatne, a publiczny konstruktor nie istnieje). Replikuje API.
struct TestRegistry {
    engines: Vec<ServiceManifest>,
}

impl TestRegistry {
    fn new(engines: Vec<ServiceManifest>) -> Self {
        Self { engines }
    }

    fn by_id(&self, id: &str) -> Option<&ServiceManifest> {
        self.engines.iter().find(|e| e.engine.id == id)
    }

    fn by_category(&self, cat: Category) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|e| e.engine.category == cat || e.engine.also_serves.contains(&cat))
            .collect()
    }

    fn compatible_for(
        &self,
        os: TargetOs,
        arch: TargetArch,
        gpu: GpuBackend,
    ) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|m| {
                m.variants.iter().any(|v| {
                    let arch_list = v.target_arch.as_vec();
                    v.target_os.as_vec().contains(&os)
                        && (arch_list.contains(&arch) || arch_list.contains(&TargetArch::Any))
                        && v.gpu_backend.as_vec().contains(&gpu)
                })
            })
            .collect()
    }
}

/// Buduje rejestr z 4 sztucznych manifestow pokrywajacych rozne OS/arch/GPU.
fn make_sample_registry() -> TestRegistry {
    // 1. LLM Linux/Win + Cuda + Docker
    let mut llm_cuda = make_engine("test-llm-cuda", Category::Llm);
    llm_cuda.also_serves = vec![Category::Embeddings];
    let llm_cuda_manifest = make_manifest(
        llm_cuda,
        vec![make_docker_variant(
            "linux-cuda",
            OsList::Multi(vec![TargetOs::Linux, TargetOs::Windows]),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Cuda),
        )],
    );

    // 2. STT macOS + Metal + Embedded
    let stt_metal_manifest = make_manifest(
        make_engine("test-stt-metal", Category::Stt),
        vec![make_embedded_variant(
            "macos-metal",
            OsList::Single(TargetOs::Macos),
            ArchList::Single(TargetArch::Aarch64),
            GpuBackendList::Single(GpuBackend::Metal),
        )],
    );

    // 3. TTS dowolny OS + CPU + External
    let tts_cpu_manifest = make_manifest(
        make_engine("test-tts-cpu", Category::Tts),
        vec![make_external_variant(
            "any-cpu",
            OsList::Multi(vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows]),
            ArchList::Single(TargetArch::Any),
            GpuBackendList::Single(GpuBackend::Cpu),
        )],
    );

    // 4. Embeddings Linux + ROCm + Docker
    let emb_rocm_manifest = make_manifest(
        make_engine("test-emb-rocm", Category::Embeddings),
        vec![make_docker_variant(
            "linux-rocm",
            OsList::Single(TargetOs::Linux),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Rocm),
        )],
    );

    TestRegistry::new(vec![
        llm_cuda_manifest,
        stt_metal_manifest,
        tts_cpu_manifest,
        emb_rocm_manifest,
    ])
}

// =============================================================================
// GRUPA A: Parsowanie TOML
// =============================================================================

/// A1: Minimalny TOML (engine + 1 variant embedded) deserializuje sie poprawnie.
#[test]
fn parse_minimal_engine() {
    let toml_src = r#"
[engine]
id = "minimal"
category = "llm"
name = "Minimal"
description_pl = "p"
description_en = "e"
homepage = "https://example.com"
license = "MIT"
api = "openai-compatible"
default_port = 8000
version = "0.1.0"

[[variant]]
id = "v1"
deploy_mode = "embedded"
target_os = "linux"
target_arch = "x86_64"
gpu_backend = "cpu"
status = "stable"

[variant.feature_flag]
name = "minimal-flag"
"#;
    let parsed: ServiceManifest = toml::from_str(toml_src).expect("deserializacja minimalnego manifestu");
    assert_eq!(parsed.engine.id, "minimal");
    assert_eq!(parsed.variants.len(), 1);
    assert_eq!(parsed.variants[0].id, "v1");
    assert_eq!(parsed.variants[0].deploy_mode, DeployMode::Embedded);
}

/// A2: Pelny TOML z wszystkimi opcjonalnymi polami i model_presets.
#[test]
fn parse_full_engine_with_model_presets() {
    let toml_src = r#"
[engine]
id = "full"
category = "llm"
name = "Full"
description_pl = "pelny"
description_en = "full"
homepage = "https://example.com"
license = "Apache-2.0"
api = "openai-compatible"
default_port = 8001
version = "1.2.3"
tags = ["fast", "gpu"]
also_serves = ["embeddings"]
docs_url = "https://docs.example.com"
icon = "icon.png"

[[variant]]
id = "linux-cuda"
deploy_mode = "docker"
target_os = ["linux", "windows"]
target_arch = "x86_64"
gpu_backend = "cuda"
status = "stable"
vram_gb_min = 8
ram_gb_min = 16
disk_gb_min = 20
notes_pl = "notatka"
notes_en = "note"

[variant.build]
context_path = "engines/full"
dockerfile = "Dockerfile"
tags = ["full:latest"]

[variant.download]
image = "ghcr.io/example/full"
digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
size_mb = 1024
license_required = "pro"
enabled = true

[[model_preset]]
id = "qwen-3b"
display_name = "Qwen 3B"
repo = "Qwen/Qwen3.5-0.8B"
quantization = "Q4_K_M"
vram_gb_min = 4
recommended = true
"#;
    let parsed: ServiceManifest = toml::from_str(toml_src).expect("deserializacja pelnego manifestu");
    assert_eq!(parsed.engine.id, "full");
    assert_eq!(parsed.engine.tags, vec!["fast".to_string(), "gpu".to_string()]);
    assert_eq!(parsed.engine.also_serves, vec![Category::Embeddings]);
    assert_eq!(parsed.engine.docs_url.as_deref(), Some("https://docs.example.com"));

    let v = &parsed.variants[0];
    assert_eq!(v.vram_gb_min, Some(8));
    assert_eq!(v.ram_gb_min, Some(16));
    assert!(v.build.is_some());
    let dl = v.download.as_ref().expect("download powinien istniec");
    assert!(dl.enabled);
    assert_eq!(dl.size_mb, Some(1024));

    assert_eq!(parsed.model_presets.len(), 1);
    assert_eq!(parsed.model_presets[0].id, "qwen-3b");
    assert!(parsed.model_presets[0].recommended);
}

/// A3: target_os jako tablica deserializuje sie do OsList::Multi.
#[test]
fn parse_variant_with_array_target_os() {
    let toml_src = r#"
[engine]
id = "arr"
category = "llm"
name = "A"
description_pl = "p"
description_en = "e"
homepage = "https://e.com"
license = "MIT"
api = "openai-compatible"
default_port = 8000
version = "1"

[[variant]]
id = "v"
deploy_mode = "embedded"
target_os = ["linux", "windows"]
target_arch = "x86_64"
gpu_backend = "cpu"
status = "stable"

[variant.feature_flag]
name = "f"
"#;
    let parsed: ServiceManifest = toml::from_str(toml_src).unwrap();
    match &parsed.variants[0].target_os {
        OsList::Multi(v) => assert_eq!(v, &vec![TargetOs::Linux, TargetOs::Windows]),
        OsList::Single(_) => panic!("oczekiwano OsList::Multi"),
    }
}

/// A4: target_os jako string deserializuje sie do OsList::Single.
#[test]
fn parse_variant_with_single_target_os() {
    let toml_src = r#"
[engine]
id = "single"
category = "llm"
name = "S"
description_pl = "p"
description_en = "e"
homepage = "https://e.com"
license = "MIT"
api = "openai-compatible"
default_port = 8000
version = "1"

[[variant]]
id = "v"
deploy_mode = "embedded"
target_os = "linux"
target_arch = "x86_64"
gpu_backend = "cpu"
status = "stable"

[variant.feature_flag]
name = "f"
"#;
    let parsed: ServiceManifest = toml::from_str(toml_src).unwrap();
    match &parsed.variants[0].target_os {
        OsList::Single(o) => assert_eq!(*o, TargetOs::Linux),
        OsList::Multi(_) => panic!("oczekiwano OsList::Single"),
    }
}

/// A5: Pusty TOML zwraca blad bo brakuje sekcji [engine].
#[test]
fn parse_invalid_toml_returns_error() {
    let result: Result<ServiceManifest, _> = toml::from_str("");
    assert!(result.is_err(), "pusty TOML powinien byc bledem");
}

// =============================================================================
// GRUPA B: Walidacja semantyczna — 9 regul
// =============================================================================

/// B1.+: Reguła 1 — Metal na macOS przechodzi.
#[test]
fn validate_metal_with_macos_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_embedded_variant(
            "v",
            OsList::Single(TargetOs::Macos),
            ArchList::Single(TargetArch::Aarch64),
            GpuBackendList::Single(GpuBackend::Metal),
        )],
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B1.-: Reguła 1 — Metal na Linux daje InvalidGpuOsCombo.
#[test]
fn validate_metal_with_linux_fails() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_embedded_variant(
            "v",
            OsList::Single(TargetOs::Linux),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Metal),
        )],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidGpuOsCombo {
                backend: GpuBackend::Metal,
                ..
            }
        )),
        "oczekiwano InvalidGpuOsCombo dla metal: {errs:?}"
    );
}

/// B2.+: Reguła 2 — MLX z deploy_mode = embedded przechodzi.
#[test]
fn validate_mlx_embedded_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_embedded_variant(
            "v",
            OsList::Single(TargetOs::Macos),
            ArchList::Single(TargetArch::Aarch64),
            GpuBackendList::Single(GpuBackend::Mlx),
        )],
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B2.-: Reguła 2 — MLX z deploy_mode = native daje MlxRequiresEmbedded.
#[test]
fn validate_mlx_native_fails() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_native_variant(
            "v",
            OsList::Single(TargetOs::Macos),
            ArchList::Single(TargetArch::Aarch64),
            GpuBackendList::Single(GpuBackend::Mlx),
        )],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::MlxRequiresEmbedded { .. })),
        "oczekiwano MlxRequiresEmbedded: {errs:?}"
    );
}

/// B3.+: Reguła 3 — CUDA na Linux przechodzi.
#[test]
fn validate_cuda_with_linux_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_docker_variant(
            "v",
            OsList::Single(TargetOs::Linux),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Cuda),
        )],
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B3.-: Reguła 3 — CUDA na macOS daje InvalidGpuOsCombo.
#[test]
fn validate_cuda_with_macos_fails() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_embedded_variant(
            "v",
            OsList::Single(TargetOs::Macos),
            ArchList::Single(TargetArch::Aarch64),
            GpuBackendList::Single(GpuBackend::Cuda),
        )],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidGpuOsCombo {
                backend: GpuBackend::Cuda,
                ..
            }
        )),
        "oczekiwano InvalidGpuOsCombo dla cuda: {errs:?}"
    );
}

/// B4.+: Reguła 4 — ROCm na Linux przechodzi.
#[test]
fn validate_rocm_with_linux_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_docker_variant(
            "v",
            OsList::Single(TargetOs::Linux),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Rocm),
        )],
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B4.-: Reguła 4 — ROCm na Windows daje InvalidGpuOsCombo.
#[test]
fn validate_rocm_with_windows_fails() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_docker_variant(
            "v",
            OsList::Single(TargetOs::Windows),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Rocm),
        )],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidGpuOsCombo {
                backend: GpuBackend::Rocm,
                ..
            }
        )),
        "oczekiwano InvalidGpuOsCombo dla rocm: {errs:?}"
    );
}

/// B5.+: Reguła 5 — XPU na Linux przechodzi.
#[test]
fn validate_xpu_with_linux_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_docker_variant(
            "v",
            OsList::Single(TargetOs::Linux),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Xpu),
        )],
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B5.-: Reguła 5 — XPU na macOS daje InvalidGpuOsCombo.
#[test]
fn validate_xpu_with_macos_fails() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_embedded_variant(
            "v",
            OsList::Single(TargetOs::Macos),
            ArchList::Single(TargetArch::Aarch64),
            GpuBackendList::Single(GpuBackend::Xpu),
        )],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidGpuOsCombo {
                backend: GpuBackend::Xpu,
                ..
            }
        )),
        "oczekiwano InvalidGpuOsCombo dla xpu: {errs:?}"
    );
}

/// B6.+: Reguła 6 — Docker na Linux przechodzi.
#[test]
fn validate_docker_with_linux_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_docker_variant(
            "v",
            OsList::Single(TargetOs::Linux),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Cpu),
        )],
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B6.-: Reguła 6 — Docker na macOS daje DockerInvalidOs.
#[test]
fn validate_docker_with_macos_fails() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![make_docker_variant(
            "v",
            OsList::Single(TargetOs::Macos),
            ArchList::Single(TargetArch::Aarch64),
            GpuBackendList::Single(GpuBackend::Cpu),
        )],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DockerInvalidOs { .. })),
        "oczekiwano DockerInvalidOs: {errs:?}"
    );
}

/// B7.+: Reguła 7 — context_path istniejacy katalog przechodzi.
#[test]
fn validate_context_path_exists_passes() {
    let tmp = TempDir::new().expect("tempdir");
    let ctx = tmp.path().join("engines").join("test");
    std::fs::create_dir_all(&ctx).expect("create_dir_all");

    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    if let Some(b) = variant.build.as_mut() {
        b.context_path = "engines/test".to_string();
    }
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    let result = validate_engine(&manifest, Some(tmp.path()));
    assert!(result.is_ok(), "oczekiwano OK, dostalem {result:?}");
}

/// B7.-: Reguła 7 — context_path nieistniejacy daje ContextPathMissing.
#[test]
fn validate_context_path_missing_fails() {
    let tmp = TempDir::new().expect("tempdir");
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    if let Some(b) = variant.build.as_mut() {
        b.context_path = "engines/nonexistent".to_string();
    }
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    let errs = validate_engine(&manifest, Some(tmp.path())).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::ContextPathMissing { .. })),
        "oczekiwano ContextPathMissing: {errs:?}"
    );
}

/// B8.+ poprawny digest: Reguła 8 — download.enabled z poprawnym sha256 przechodzi.
#[test]
fn validate_download_enabled_with_valid_digest_passes() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Pro,
        enabled: true,
    });
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B8.- niepoprawny digest: Reguła 8 — download.enabled z niepoprawnym digest daje blad.
#[test]
fn validate_download_enabled_with_invalid_digest_fails() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "not-a-digest".to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Pro,
        enabled: true,
    });
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DownloadEnabledWithoutDigest { .. })),
        "oczekiwano DownloadEnabledWithoutDigest: {errs:?}"
    );
}

/// B8.skip: Reguła 8 — gdy download.enabled = false, niepoprawny digest jest ignorowany.
#[test]
fn validate_download_disabled_with_invalid_digest_passes() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "not-a-digest".to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Pro,
        enabled: false,
    });
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B9.+: Reguła 9 — unikalne variant.id przechodzi.
#[test]
fn validate_unique_variant_ids_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![
            make_docker_variant(
                "v1",
                OsList::Single(TargetOs::Linux),
                ArchList::Single(TargetArch::X86_64),
                GpuBackendList::Single(GpuBackend::Cpu),
            ),
            make_docker_variant(
                "v2",
                OsList::Single(TargetOs::Linux),
                ArchList::Single(TargetArch::X86_64),
                GpuBackendList::Single(GpuBackend::Cpu),
            ),
        ],
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B9.-: Reguła 9 — duplikat variant.id daje DuplicateVariantId.
#[test]
fn validate_duplicate_variant_ids_fails() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        vec![
            make_docker_variant(
                "v1",
                OsList::Single(TargetOs::Linux),
                ArchList::Single(TargetArch::X86_64),
                GpuBackendList::Single(GpuBackend::Cpu),
            ),
            make_docker_variant(
                "v1",
                OsList::Single(TargetOs::Linux),
                ArchList::Single(TargetArch::X86_64),
                GpuBackendList::Single(GpuBackend::Cpu),
            ),
        ],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DuplicateVariantId { .. })),
        "oczekiwano DuplicateVariantId: {errs:?}"
    );
}

/// B-bonus: deploy_mode = embedded bez feature_flag daje EmbeddedRequiresFeatureFlag.
#[test]
fn validate_embedded_without_feature_flag_fails() {
    let mut variant = make_embedded_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.feature_flag = None;
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::EmbeddedRequiresFeatureFlag { .. })),
        "oczekiwano EmbeddedRequiresFeatureFlag: {errs:?}"
    );
}

/// B-bonus: deploy_mode = external bez detection daje ExternalRequiresDetection.
#[test]
fn validate_external_without_detection_fails() {
    let mut variant = make_external_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.detection = None;
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::ExternalRequiresDetection { .. })),
        "oczekiwano ExternalRequiresDetection: {errs:?}"
    );
}

/// B-bonus: deploy_mode = docker bez build daje BuildRequiredButMissing.
#[test]
fn validate_docker_without_build_fails() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.build = None;
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BuildRequiredButMissing { .. })),
        "oczekiwano BuildRequiredButMissing: {errs:?}"
    );
}

// =============================================================================
// GRUPA C: ManifestRegistry
// =============================================================================

/// C1: by_id istniejacego silnika zwraca Some.
#[test]
fn registry_by_id_existing_returns_some() {
    let reg = make_sample_registry();
    let m = reg.by_id("test-llm-cuda");
    assert!(m.is_some());
    assert_eq!(m.unwrap().engine.id, "test-llm-cuda");
}

/// C2: by_id nieistniejacego silnika zwraca None.
#[test]
fn registry_by_id_missing_returns_none() {
    let reg = make_sample_registry();
    assert!(reg.by_id("does-not-exist").is_none());
}

/// C3: by_category zwraca silniki tylko z danej kategorii.
#[test]
fn registry_by_category_filters() {
    let reg = make_sample_registry();
    let llms = reg.by_category(Category::Llm);
    assert_eq!(llms.len(), 1);
    assert_eq!(llms[0].engine.id, "test-llm-cuda");

    let stts = reg.by_category(Category::Stt);
    assert_eq!(stts.len(), 1);
    assert_eq!(stts[0].engine.id, "test-stt-metal");
}

/// C4: by_category uwzglednia also_serves — silnik LLM z also_serves=embeddings
///     wystepuje w obu kategoriach.
#[test]
fn registry_by_category_includes_also_serves() {
    let reg = make_sample_registry();
    let embeddings = reg.by_category(Category::Embeddings);
    let ids: Vec<&str> = embeddings.iter().map(|m| m.engine.id.as_str()).collect();
    assert!(ids.contains(&"test-llm-cuda"), "oczekiwano test-llm-cuda przez also_serves: {ids:?}");
    assert!(ids.contains(&"test-emb-rocm"), "oczekiwano test-emb-rocm: {ids:?}");
    assert_eq!(ids.len(), 2);
}

/// C5: compatible_for(linux, x86_64, cuda) zwraca silniki z odpowiednim wariantem.
#[test]
fn registry_compatible_for_linux_x64_cuda() {
    let reg = make_sample_registry();
    let comp = reg.compatible_for(TargetOs::Linux, TargetArch::X86_64, GpuBackend::Cuda);
    assert_eq!(comp.len(), 1);
    assert_eq!(comp[0].engine.id, "test-llm-cuda");
}

/// C6: compatible_for(macos, aarch64, metal) zwraca silnik macOS+metal.
#[test]
fn registry_compatible_for_macos_arm64_metal() {
    let reg = make_sample_registry();
    let comp = reg.compatible_for(TargetOs::Macos, TargetArch::Aarch64, GpuBackend::Metal);
    assert_eq!(comp.len(), 1);
    assert_eq!(comp[0].engine.id, "test-stt-metal");
}

/// C7: compatible_for nieobslugiwanej platformy (android+aarch64+cuda) → pusta lista.
#[test]
fn registry_compatible_for_no_match() {
    let reg = make_sample_registry();
    let comp = reg.compatible_for(TargetOs::Android, TargetArch::Aarch64, GpuBackend::Cuda);
    assert!(comp.is_empty(), "oczekiwano pustej listy, dostalem {} silnikow", comp.len());
}

/// C8: compatible_for z arch=any przechodzi dla dowolnej architektury.
#[test]
fn registry_compatible_for_any_arch_matches() {
    let reg = make_sample_registry();
    let comp_x64 = reg.compatible_for(TargetOs::Linux, TargetArch::X86_64, GpuBackend::Cpu);
    let comp_arm = reg.compatible_for(TargetOs::Linux, TargetArch::Aarch64, GpuBackend::Cpu);
    let id_in_x64 = comp_x64.iter().any(|m| m.engine.id == "test-tts-cpu");
    let id_in_arm = comp_arm.iter().any(|m| m.engine.id == "test-tts-cpu");
    assert!(id_in_x64, "test-tts-cpu (any arch) powinien pasowac do x86_64");
    assert!(id_in_arm, "test-tts-cpu (any arch) powinien pasowac do aarch64");
}

// =============================================================================
// GRUPA D: LicenseChecker
// =============================================================================

/// D1: Free pozwala na Free.
#[test]
fn static_free_allows_free() {
    let c = StaticLicenseChecker::free();
    assert!(c.allows(crate::license::LicenseTier::Free));
}

/// D2: Free NIE pozwala na Pro.
#[test]
fn static_free_does_not_allow_pro() {
    let c = StaticLicenseChecker::free();
    assert!(!c.allows(crate::license::LicenseTier::Pro));
}

/// D3: Free NIE pozwala na Enterprise.
#[test]
fn static_free_does_not_allow_enterprise() {
    let c = StaticLicenseChecker::free();
    assert!(!c.allows(crate::license::LicenseTier::Enterprise));
}

/// D4: Pro pozwala na Free i Pro.
#[test]
fn static_pro_allows_free_and_pro() {
    let c = StaticLicenseChecker::pro();
    assert!(c.allows(crate::license::LicenseTier::Free));
    assert!(c.allows(crate::license::LicenseTier::Pro));
}

/// D5: Pro NIE pozwala na Enterprise.
#[test]
fn static_pro_does_not_allow_enterprise() {
    let c = StaticLicenseChecker::pro();
    assert!(!c.allows(crate::license::LicenseTier::Enterprise));
}

/// D6: Enterprise pozwala na wszystkie tiery.
#[test]
fn static_enterprise_allows_all() {
    let c = StaticLicenseChecker::new(crate::license::LicenseTier::Enterprise);
    assert!(c.allows(crate::license::LicenseTier::Free));
    assert!(c.allows(crate::license::LicenseTier::Pro));
    assert!(c.allows(crate::license::LicenseTier::Enterprise));
}

/// D7: check_variant_download dla wariantu Pro z licencja Free zwraca Insufficient.
#[test]
fn check_variant_download_pro_with_free_license_fails() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Pro,
        enabled: true,
    });
    let c = StaticLicenseChecker::free();
    let result = c.check_variant_download(&variant, "test-feature");
    assert!(matches!(result, Err(LicenseError::Insufficient { .. })));
}

/// D8: check_variant_download dla wariantu Pro z licencja Pro przechodzi.
#[test]
fn check_variant_download_pro_with_pro_license_passes() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Pro,
        enabled: true,
    });
    let c = StaticLicenseChecker::pro();
    let result = c.check_variant_download(&variant, "test-feature");
    assert!(result.is_ok(), "oczekiwano Ok, dostalem {result:?}");
}

/// D9: check_variant_download dla wariantu bez sekcji download zawsze przechodzi.
#[test]
fn check_variant_download_no_download_section_passes() {
    let variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    assert!(variant.download.is_none());
    let c = StaticLicenseChecker::free();
    assert!(c.check_variant_download(&variant, "x").is_ok());
}

/// D10: check_variant_download dla wariantu Enterprise z licencja Pro zwraca blad.
#[test]
fn check_variant_download_enterprise_with_pro_license_fails() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Enterprise,
        enabled: true,
    });
    let c = StaticLicenseChecker::pro();
    let result = c.check_variant_download(&variant, "test-feature");
    assert!(matches!(result, Err(LicenseError::Insufficient { .. })));
}

// =============================================================================
// GRUPA E: Faktyczne dane z manifestu (REGISTRY)
// =============================================================================

/// E1: Globalny REGISTRY zawiera co najmniej 15 silnikow (znanych: 20).
#[test]
fn loaded_manifest_has_at_least_15_engines() {
    let reg = super::registry::registry();
    let count = reg.engines().len();
    assert!(
        count >= 15,
        "Oczekiwano min 15 silnikow w REGISTRY, znaleziono {count}"
    );
}

/// E2: Wszystkie zaladowane manifesty przechodza walidacje semantyczna
///     (bez sprawdzania context_path, bo runtime nie ma dostepu do FS containers/).
#[test]
fn all_loaded_manifests_pass_validation() {
    let reg = super::registry::registry();
    let mut failures: Vec<(String, Vec<ValidationError>)> = Vec::new();
    for manifest in reg.engines() {
        if let Err(errs) = validate_engine(manifest, None) {
            failures.push((manifest.engine.id.clone(), errs));
        }
    }
    assert!(
        failures.is_empty(),
        "Manifesty z bledami walidacji: {failures:?}"
    );
}

// =============================================================================
// GRUPA F: Walidacja engine.id (CR-011 — Reguła 10)
// =============================================================================

/// F1: Poprawne identyfikatory zwracaja true.
#[test]
fn validate_engine_id_valid_passes() {
    assert!(validate_engine_id("vllm"));
    assert!(validate_engine_id("llama-cpp"));
    assert!(validate_engine_id("stable-diffusion-cpp"));
    assert!(validate_engine_id("a"));
    assert!(validate_engine_id("0"));
    assert!(validate_engine_id("foo_bar"));
    assert!(validate_engine_id("a1b2c3"));
}

/// F2: Wielkie litery niedozwolone.
#[test]
fn validate_engine_id_invalid_uppercase_fails() {
    assert!(!validate_engine_id("vLLM"));
    assert!(!validate_engine_id("Llama-Cpp"));
    assert!(!validate_engine_id("FOO"));
}

/// F3: Path traversal odrzucony.
#[test]
fn validate_engine_id_invalid_path_traversal_fails() {
    assert!(!validate_engine_id("../../../etc/passwd"));
    assert!(!validate_engine_id("../foo"));
    assert!(!validate_engine_id("foo/bar"));
    assert!(!validate_engine_id(".."));
    assert!(!validate_engine_id("."));
}

/// F4: Znaki specjalne (separatory, kontrolne, nullbyte) odrzucone.
#[test]
fn validate_engine_id_invalid_special_chars_fails() {
    assert!(!validate_engine_id("foo;bar"));
    assert!(!validate_engine_id("foo bar"));
    assert!(!validate_engine_id("foo\0bar"));
    assert!(!validate_engine_id("foo\nbar"));
    assert!(!validate_engine_id("foo$bar"));
    assert!(!validate_engine_id("foo@bar"));
    assert!(!validate_engine_id("foo.bar"));
}

/// F5: Identyfikator dluzszy niz 64 znaki odrzucony.
#[test]
fn validate_engine_id_too_long_fails() {
    let id_64: String = "a".repeat(64);
    assert!(validate_engine_id(&id_64), "64 znaki — granica dozwolona");
    let id_65: String = "a".repeat(65);
    assert!(!validate_engine_id(&id_65));
    let id_long: String = "a".repeat(200);
    assert!(!validate_engine_id(&id_long));
}

/// F6: Pusty identyfikator odrzucony.
#[test]
fn validate_engine_id_empty_fails() {
    assert!(!validate_engine_id(""));
}

/// F7: Pierwszy znak musi byc a-z lub 0-9 — myslnik/podkreslnik na poczatku odrzucone.
#[test]
fn validate_engine_id_invalid_leading_char_fails() {
    assert!(!validate_engine_id("-foo"));
    assert!(!validate_engine_id("_foo"));
}

/// F8: validate_engine zwraca InvalidEngineId dla nieprawidlowego engine.id.
#[test]
fn validate_engine_with_invalid_id_returns_error() {
    let manifest = make_manifest(
        make_engine("Invalid Id!", Category::Llm),
        vec![make_embedded_variant(
            "v",
            OsList::Single(TargetOs::Linux),
            ArchList::Single(TargetArch::X86_64),
            GpuBackendList::Single(GpuBackend::Cpu),
        )],
    );
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidEngineId { .. })),
        "oczekiwano InvalidEngineId: {errs:?}"
    );
}

// =============================================================================
// GRUPA G: Placeholder zero-digest (CR-016)
// =============================================================================

/// G1: download.enabled = true z digest sha256:000...000 odrzucony.
#[test]
fn validate_download_enabled_with_placeholder_zero_digest_fails() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Pro,
        enabled: true,
    });
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    let errs = validate_engine(&manifest, None).expect_err("oczekiwano bledu");
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::PlaceholderDigestEnabled { .. })),
        "oczekiwano PlaceholderDigestEnabled: {errs:?}"
    );
}

/// G2: download.enabled = false z placeholder zero-digest przechodzi (gating
/// nie aktywny dopoki ktos nie wlaczy enabled).
#[test]
fn validate_download_disabled_with_placeholder_zero_digest_passes() {
    let mut variant = make_docker_variant(
        "v",
        OsList::Single(TargetOs::Linux),
        ArchList::Single(TargetArch::X86_64),
        GpuBackendList::Single(GpuBackend::Cpu),
    );
    variant.download = Some(DownloadOption {
        image: "ghcr.io/test/image".to_string(),
        digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        size_mb: None,
        license_required: RequiredLicenseTier::Pro,
        enabled: false,
    });
    let manifest = make_manifest(make_engine("e", Category::Llm), vec![variant]);
    assert!(validate_engine(&manifest, None).is_ok());
}
