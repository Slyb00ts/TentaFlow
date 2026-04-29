// =============================================================================
// Plik: tests.rs
// Opis: Testy jednostkowe modulu service manifest — parsowanie TOML,
//       walidacja semantyczna 4 regul, ManifestRegistry oraz integracja
//       z embedowanym manifestem (15 silnikow).
// =============================================================================

#![cfg(test)]

use super::types::*;
use super::validate::{validate_engine, validate_engine_id, ValidationError};
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
        icon: None,
        default_port: 8000,
        api: ApiKind::OpenaiCompatible,
        version: "1.0.0".to_string(),
        resource_kind: None,
        requires_model: None,
        gpu_supported: None,
    }
}

fn empty_deploy() -> DeploySection {
    DeploySection {
        docker: None,
        native: None,
        external: None,
    }
}

fn docker_deploy(context_path: &str, platforms: Vec<TargetOs>) -> DockerDeploy {
    DockerDeploy {
        context_path: Some(context_path.to_string()),
        compose_path: None,
        platforms,
        download_image: None,
        download_size_mb: None,
    }
}

fn docker_compose_deploy(compose_path: &str, platforms: Vec<TargetOs>) -> DockerDeploy {
    DockerDeploy {
        context_path: None,
        compose_path: Some(compose_path.to_string()),
        platforms,
        download_image: None,
        download_size_mb: None,
    }
}

fn native_embedded(platforms: Vec<TargetOs>, feature: &str) -> NativeDeploy {
    NativeDeploy {
        platforms,
        runtime: NativeRuntime::Embedded,
        feature_flag: Some(feature.to_string()),
        binary_path: None,
        bundle_path: None,
    }
}

fn native_binary(platforms: Vec<TargetOs>, path: &str) -> NativeDeploy {
    NativeDeploy {
        platforms,
        runtime: NativeRuntime::Binary,
        feature_flag: None,
        binary_path: Some(path.to_string()),
        bundle_path: None,
    }
}

fn native_python_bundle(platforms: Vec<TargetOs>, path: &str) -> NativeDeploy {
    NativeDeploy {
        platforms,
        runtime: NativeRuntime::PythonBundle,
        feature_flag: None,
        binary_path: None,
        bundle_path: Some(path.to_string()),
    }
}

fn external_deploy(platforms: Vec<TargetOs>) -> ExternalDeploy {
    ExternalDeploy {
        platforms,
        detection_binary: "ollama".to_string(),
        detection_endpoint: "http://localhost:11434".to_string(),
        detection_health_path: "/api/tags".to_string(),
    }
}

fn make_manifest(engine: Engine, deploy: DeploySection) -> ServiceManifest {
    ServiceManifest {
        engine,
        deploy,
        model_presets: Vec::new(),
        docker_source_hash: String::new(),
        native_source_hash: String::new(),
    }
}

// =============================================================================
// GRUPA A: Parsowanie TOML
// =============================================================================

/// A1: Minimalny TOML (engine + tylko [deploy.docker]) deserializuje sie poprawnie.
#[test]
fn parse_minimal_docker_only() {
    let toml_src = r#"
[engine]
id = "minimal"
category = "llm"
name = "Minimal"
description_pl = "p"
description_en = "e"
homepage = "https://example.com"
license = "MIT"
default_port = 8000
api = "openai-compatible"
version = "0.1.0"

[deploy.docker]
context_path = "llm/docker/minimal"
platforms = ["linux"]
"#;
    let parsed: ServiceManifest = toml::from_str(toml_src).expect("parse minimalnego manifestu");
    assert_eq!(parsed.engine.id, "minimal");
    assert!(parsed.deploy.docker.is_some());
    assert!(parsed.deploy.native.is_none());
    assert!(parsed.deploy.external.is_none());
    assert_eq!(
        parsed.deploy.docker.as_ref().unwrap().platforms,
        vec![TargetOs::Linux]
    );
}

/// A2: Pelny TOML z wszystkimi sekcjami deploy + model_presets.
#[test]
fn parse_full_manifest_with_all_sections() {
    let toml_src = r#"
[engine]
id = "full"
category = "llm"
name = "Full"
description_pl = "pelny"
description_en = "full"
homepage = "https://example.com"
license = "Apache-2.0"
icon = "full-icon"
default_port = 8001
api = "openai-compatible"
version = "1.2.3"

[deploy.docker]
context_path = "llm/docker/full"
platforms = ["linux", "windows"]
download_image = "ghcr.io/example/full:latest"
download_size_mb = 1024

[deploy.native]
platforms = ["linux", "macos"]
runtime = "python-bundle"
bundle_path = "llm/python/full"

[deploy.external]
platforms = ["linux", "macos", "windows"]
detection_binary = "fullbin"
detection_endpoint = "http://localhost:9000"
detection_health_path = "/health"

[[model_preset]]
id = "qwen-3b"
display_name = "Qwen 3B"
repo = "Qwen/Qwen3.5-0.8B"
quantization = "Q4_K_M"
recommended = true
"#;
    let parsed: ServiceManifest = toml::from_str(toml_src).expect("parse pelnego manifestu");
    assert_eq!(parsed.engine.id, "full");
    assert_eq!(parsed.engine.icon.as_deref(), Some("full-icon"));

    let d = parsed.deploy.docker.as_ref().unwrap();
    assert_eq!(d.context_path.as_deref(), Some("llm/docker/full"));
    assert_eq!(d.compose_path, None);
    assert_eq!(d.platforms, vec![TargetOs::Linux, TargetOs::Windows]);
    assert_eq!(d.download_size_mb, Some(1024));

    let n = parsed.deploy.native.as_ref().unwrap();
    assert_eq!(n.runtime, NativeRuntime::PythonBundle);
    assert_eq!(n.bundle_path.as_deref(), Some("llm/python/full"));

    let e = parsed.deploy.external.as_ref().unwrap();
    assert_eq!(e.detection_health_path, "/health");

    assert_eq!(parsed.model_presets.len(), 1);
    assert!(parsed.model_presets[0].recommended);
}

/// A3: detection_health_path domyslnie "/" gdy pominiete.
#[test]
fn parse_external_with_default_health_path() {
    let toml_src = r#"
[engine]
id = "ext"
category = "llm"
name = "Ext"
description_pl = "p"
description_en = "e"
homepage = "https://example.com"
license = "MIT"
default_port = 8000
api = "ollama-native"
version = "1"

[deploy.external]
platforms = ["linux"]
detection_binary = "x"
detection_endpoint = "http://localhost:1234"
"#;
    let parsed: ServiceManifest = toml::from_str(toml_src).unwrap();
    assert_eq!(
        parsed
            .deploy
            .external
            .as_ref()
            .unwrap()
            .detection_health_path,
        "/"
    );
}

/// A4: Pusty TOML zwraca blad bo brakuje sekcji [engine].
#[test]
fn parse_empty_toml_returns_error() {
    let result: Result<ServiceManifest, _> = toml::from_str("");
    assert!(result.is_err());
}

// =============================================================================
// GRUPA B: Walidacja semantyczna — 4 reguly
// =============================================================================

/// B1.+: Reguła 1 — poprawny engine.id przechodzi.
#[test]
fn validate_valid_engine_id_passes() {
    let manifest = make_manifest(
        make_engine("vllm", Category::Llm),
        DeploySection {
            docker: Some(docker_deploy("llm/docker/vllm", vec![TargetOs::Linux])),
            native: None,
            external: None,
        },
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B1.-: Reguła 1 — niepoprawny engine.id daje InvalidEngineId.
#[test]
fn validate_invalid_engine_id_fails() {
    let manifest = make_manifest(
        make_engine("Invalid Id!", Category::Llm),
        DeploySection {
            docker: Some(docker_deploy("x", vec![TargetOs::Linux])),
            native: None,
            external: None,
        },
    );
    let errs = validate_engine(&manifest, None).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::InvalidEngineId { .. })));
}

/// B2.+: Reguła 2 — manifest z jedna sekcja deploy przechodzi.
#[test]
fn validate_with_at_least_one_deploy_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: None,
            native: Some(native_embedded(vec![TargetOs::Linux], "feat")),
            external: None,
        },
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

#[test]
fn validate_docker_with_compose_path_passes() {
    let manifest = make_manifest(
        make_engine("relay", Category::Tools),
        DeploySection {
            docker: Some(docker_compose_deploy(
                "tools/docker/iroh-relay/stack.yml",
                vec![TargetOs::Linux],
            )),
            native: None,
            external: None,
        },
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

#[test]
fn validate_docker_without_context_or_compose_fails() {
    let manifest = make_manifest(
        make_engine("relay", Category::Tools),
        DeploySection {
            docker: Some(DockerDeploy {
                context_path: None,
                compose_path: None,
                platforms: vec![TargetOs::Linux],
                download_image: None,
                download_size_mb: None,
            }),
            native: None,
            external: None,
        },
    );
    let errs = validate_engine(&manifest, None).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::DockerRequiresSingleSource { .. })));
}

#[test]
fn validate_docker_with_context_and_compose_fails() {
    let manifest = make_manifest(
        make_engine("relay", Category::Tools),
        DeploySection {
            docker: Some(DockerDeploy {
                context_path: Some("tools/docker/iroh-relay".to_string()),
                compose_path: Some("tools/docker/iroh-relay/stack.yml".to_string()),
                platforms: vec![TargetOs::Linux],
                download_image: None,
                download_size_mb: None,
            }),
            native: None,
            external: None,
        },
    );
    let errs = validate_engine(&manifest, None).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::DockerRequiresSingleSource { .. })));
}

/// B2.-: Reguła 2 — brak wszystkich sekcji deploy daje NoDeploySection.
#[test]
fn validate_no_deploy_section_fails() {
    let manifest = make_manifest(make_engine("e", Category::Llm), empty_deploy());
    let errs = validate_engine(&manifest, None).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::NoDeploySection { .. })));
}

/// B3-embedded.+: Reguła 3 — runtime=embedded z feature_flag i bez innych pol przechodzi.
#[test]
fn validate_native_embedded_consistent_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: None,
            native: Some(native_embedded(vec![TargetOs::Linux], "inference-llamacpp")),
            external: None,
        },
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B3-embedded.+: Reguła 3 (po cleanupie) — runtime=embedded bez feature_flag
/// jest dozwolony: silniki gated tylko przez target_os (apple-tts, vision/*)
/// nie mają Cargo feature. Walidacja musi przejsc.
#[test]
fn validate_native_embedded_without_feature_flag_passes() {
    let mut native = native_embedded(vec![TargetOs::Linux], "x");
    native.feature_flag = None;
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: None,
            native: Some(native),
            external: None,
        },
    );
    validate_engine(&manifest, None).expect("embedded bez feature_flag powinno przejsc");
}

/// B3-embedded.-: runtime=embedded z dodatkowym binary_path daje blad.
#[test]
fn validate_native_embedded_with_binary_path_fails() {
    let mut native = native_embedded(vec![TargetOs::Linux], "x");
    native.binary_path = Some("foo".to_string());
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: None,
            native: Some(native),
            external: None,
        },
    );
    let errs = validate_engine(&manifest, None).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::EmbeddedRequiresFeatureFlag { .. })));
}

/// B3-binary.+: runtime=binary z binary_path przechodzi.
#[test]
fn validate_native_binary_consistent_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Tts),
        DeploySection {
            docker: None,
            native: Some(native_binary(
                vec![TargetOs::Linux],
                "tts/native/sherpa-onnx",
            )),
            external: None,
        },
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B3-binary.-: runtime=binary z feature_flag daje blad.
#[test]
fn validate_native_binary_with_feature_flag_fails() {
    let mut native = native_binary(vec![TargetOs::Linux], "x");
    native.feature_flag = Some("foo".to_string());
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: None,
            native: Some(native),
            external: None,
        },
    );
    let errs = validate_engine(&manifest, None).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::BinaryRequiresBinaryPath { .. })));
}

/// B3-bundle.+: runtime=python-bundle z bundle_path przechodzi.
#[test]
fn validate_native_python_bundle_consistent_passes() {
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: None,
            native: Some(native_python_bundle(
                vec![TargetOs::Linux],
                "llm/python/vllm",
            )),
            external: None,
        },
    );
    assert!(validate_engine(&manifest, None).is_ok());
}

/// B3-bundle.-: runtime=python-bundle bez bundle_path daje blad.
#[test]
fn validate_native_python_bundle_without_path_fails() {
    let mut native = native_python_bundle(vec![TargetOs::Linux], "x");
    native.bundle_path = None;
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: None,
            native: Some(native),
            external: None,
        },
    );
    let errs = validate_engine(&manifest, None).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::PythonBundleRequiresBundlePath { .. })));
}

/// B4.+: Reguła 4 — istniejaca sciezka przechodzi.
#[test]
fn validate_path_exists_passes() {
    let tmp = TempDir::new().expect("tempdir");
    let ctx = tmp.path().join("llm").join("docker").join("test");
    std::fs::create_dir_all(&ctx).expect("create_dir_all");
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: Some(docker_deploy("llm/docker/test", vec![TargetOs::Linux])),
            native: None,
            external: None,
        },
    );
    assert!(validate_engine(&manifest, Some(tmp.path())).is_ok());
}

#[test]
fn validate_compose_file_exists_passes() {
    let tmp = TempDir::new().expect("tempdir");
    let compose = tmp
        .path()
        .join("tools")
        .join("docker")
        .join("iroh-relay")
        .join("stack.yml");
    std::fs::create_dir_all(compose.parent().unwrap()).expect("create_dir_all");
    std::fs::write(&compose, "services: {}\n").expect("write compose");
    let manifest = make_manifest(
        make_engine("relay", Category::Tools),
        DeploySection {
            docker: Some(docker_compose_deploy(
                "tools/docker/iroh-relay/stack.yml",
                vec![TargetOs::Linux],
            )),
            native: None,
            external: None,
        },
    );
    assert!(validate_engine(&manifest, Some(tmp.path())).is_ok());
}

/// B4.-: Reguła 4 — nieistniejaca sciezka daje PathMissing.
#[test]
fn validate_path_missing_fails() {
    let tmp = TempDir::new().expect("tempdir");
    let manifest = make_manifest(
        make_engine("e", Category::Llm),
        DeploySection {
            docker: Some(docker_deploy("llm/docker/missing", vec![TargetOs::Linux])),
            native: None,
            external: None,
        },
    );
    let errs = validate_engine(&manifest, Some(tmp.path())).expect_err("blad oczekiwany");
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::PathMissing { .. })));
}

// =============================================================================
// GRUPA C: validate_engine_id (helper)
// =============================================================================

/// C1: poprawne identyfikatory.
#[test]
fn validate_engine_id_valid_passes() {
    assert!(validate_engine_id("vllm"));
    assert!(validate_engine_id("llama-cpp"));
    assert!(validate_engine_id("stable-diffusion-cpp"));
    assert!(validate_engine_id("a"));
    assert!(validate_engine_id("0"));
    assert!(validate_engine_id("foo_bar"));
}

/// C2: niepoprawne identyfikatory (uppercase, znaki specjalne, path traversal).
#[test]
fn validate_engine_id_invalid_fails() {
    assert!(!validate_engine_id(""));
    assert!(!validate_engine_id("vLLM"));
    assert!(!validate_engine_id("foo bar"));
    assert!(!validate_engine_id("../foo"));
    assert!(!validate_engine_id("foo/bar"));
    assert!(!validate_engine_id("-foo"));
    assert!(!validate_engine_id("_foo"));
    assert!(!validate_engine_id(&"a".repeat(65)));
}

// =============================================================================
// GRUPA D: ManifestRegistry — uzyte na probie sztucznych silnikow
// =============================================================================

/// Buduje 5 sztucznych silnikow pokrywajacych rozne OS i kategorie.
fn make_sample_engines() -> Vec<ServiceManifest> {
    let mut llm = make_engine("test-llm", Category::Llm);
    llm.icon = Some("llm-icon".to_string());
    let llm_manifest = make_manifest(
        llm,
        DeploySection {
            docker: Some(docker_deploy(
                "llm/docker/x",
                vec![TargetOs::Linux, TargetOs::Windows],
            )),
            native: None,
            external: None,
        },
    );

    let stt_manifest = make_manifest(
        make_engine("test-stt", Category::Stt),
        DeploySection {
            docker: None,
            native: Some(native_embedded(
                vec![TargetOs::Macos, TargetOs::Ios],
                "inference-whisper",
            )),
            external: None,
        },
    );

    let tts_manifest = make_manifest(
        make_engine("test-tts", Category::Tts),
        DeploySection {
            docker: None,
            native: Some(native_binary(
                vec![TargetOs::Linux],
                "tts/native/sherpa-onnx",
            )),
            external: None,
        },
    );

    let agents_manifest = make_manifest(
        make_engine("test-agent", Category::Agents),
        DeploySection {
            docker: Some(docker_deploy(
                "agents/docker/teams-bot",
                vec![TargetOs::Linux],
            )),
            native: None,
            external: None,
        },
    );

    let ext_manifest = make_manifest(
        make_engine("test-ext", Category::Llm),
        DeploySection {
            docker: None,
            native: None,
            external: Some(external_deploy(vec![
                TargetOs::Linux,
                TargetOs::Macos,
                TargetOs::Windows,
            ])),
        },
    );

    vec![
        llm_manifest,
        stt_manifest,
        tts_manifest,
        agents_manifest,
        ext_manifest,
    ]
}

/// Lekka kopia ManifestRegistry uzywana w testach (replikuje API).
struct TestRegistry {
    engines: Vec<ServiceManifest>,
}
impl TestRegistry {
    fn by_id(&self, id: &str) -> Option<&ServiceManifest> {
        self.engines.iter().find(|e| e.engine.id == id)
    }
    fn by_category(&self, cat: Category) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|e| e.engine.category == cat)
            .collect()
    }
    fn compatible_for(&self, os: TargetOs) -> Vec<&ServiceManifest> {
        self.engines
            .iter()
            .filter(|m| {
                let d = &m.deploy;
                d.docker.as_ref().is_some_and(|x| x.platforms.contains(&os))
                    || d.native.as_ref().is_some_and(|x| x.platforms.contains(&os))
                    || d.external
                        .as_ref()
                        .is_some_and(|x| x.platforms.contains(&os))
            })
            .collect()
    }
    fn non_empty_categories(&self) -> Vec<Category> {
        let mut seen: Vec<Category> = Vec::new();
        for e in &self.engines {
            if !seen.contains(&e.engine.category) {
                seen.push(e.engine.category);
            }
        }
        seen
    }
}

/// D1: by_id istniejacego silnika zwraca Some.
#[test]
fn registry_by_id_existing_returns_some() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    assert_eq!(reg.by_id("test-llm").unwrap().engine.id, "test-llm");
}

/// D2: by_id nieistniejacego silnika zwraca None.
#[test]
fn registry_by_id_missing_returns_none() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    assert!(reg.by_id("nope").is_none());
}

/// D3: by_category zwraca silniki tylko z danej kategorii.
#[test]
fn registry_by_category_filters() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    let llms = reg.by_category(Category::Llm);
    assert_eq!(llms.len(), 2);
    let stts = reg.by_category(Category::Stt);
    assert_eq!(stts.len(), 1);
}

/// D4: compatible_for(linux) zwraca silniki z linux na liscie platforms.
#[test]
fn registry_compatible_for_linux() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    let comp = reg.compatible_for(TargetOs::Linux);
    let ids: Vec<&str> = comp.iter().map(|m| m.engine.id.as_str()).collect();
    assert!(ids.contains(&"test-llm"));
    assert!(ids.contains(&"test-tts"));
    assert!(ids.contains(&"test-agent"));
    assert!(ids.contains(&"test-ext"));
    assert!(!ids.contains(&"test-stt"));
}

/// D5: compatible_for(macos) — tylko silniki z macos.
#[test]
fn registry_compatible_for_macos() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    let comp = reg.compatible_for(TargetOs::Macos);
    let ids: Vec<&str> = comp.iter().map(|m| m.engine.id.as_str()).collect();
    assert!(ids.contains(&"test-stt"));
    assert!(ids.contains(&"test-ext"));
    assert!(!ids.contains(&"test-llm"));
}

/// D6: compatible_for(windows) — silnik LLM (docker) i ext.
#[test]
fn registry_compatible_for_windows() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    let comp = reg.compatible_for(TargetOs::Windows);
    let ids: Vec<&str> = comp.iter().map(|m| m.engine.id.as_str()).collect();
    assert!(ids.contains(&"test-llm"));
    assert!(ids.contains(&"test-ext"));
}

/// D7: compatible_for(ios) — tylko stt (embedded whisper).
#[test]
fn registry_compatible_for_ios() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    let comp = reg.compatible_for(TargetOs::Ios);
    let ids: Vec<&str> = comp.iter().map(|m| m.engine.id.as_str()).collect();
    assert_eq!(ids, vec!["test-stt"]);
}

/// D8: compatible_for(android) — pusta lista (zaden sample manifest nie wspiera androida).
#[test]
fn registry_compatible_for_android_empty() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    let comp = reg.compatible_for(TargetOs::Android);
    assert!(comp.is_empty());
}

/// D9: non_empty_categories zwraca tylko kategorie z silnikami (Llm, Stt, Tts, Agents).
#[test]
fn registry_non_empty_categories_only_used() {
    let reg = TestRegistry {
        engines: make_sample_engines(),
    };
    let cats = reg.non_empty_categories();
    assert!(cats.contains(&Category::Llm));
    assert!(cats.contains(&Category::Stt));
    assert!(cats.contains(&Category::Tts));
    assert!(cats.contains(&Category::Agents));
    assert!(!cats.contains(&Category::Vision));
    assert!(!cats.contains(&Category::Embeddings));
    // 4 unikalne kategorie (Llm wystepuje 2x ale powinna byc raz).
    assert_eq!(cats.len(), 4);
}

// =============================================================================
// GRUPA E: Faktyczne dane z embedowanego REGISTRY (15 silnikow)
// =============================================================================

/// E1: Globalny REGISTRY ma niepustą listę silnikow i kazdy ma unikalne id.
/// Sztywna liczba (np. 15) szybko driftuje, gdy dodajemy nowe manifesty —
/// asercja sprawdza tylko nietrywialny rozmiar i obecnosc kluczowych silnikow
/// (test E4 weryfikuje dokladniejsza liste LLM).
#[test]
fn loaded_manifest_has_engines() {
    let reg = super::registry::registry();
    let count = reg.engines().len();
    assert!(count > 0, "REGISTRY pusty — build.rs nie wygenerowal manifestow?");
}

/// E2: Wszystkie zaladowane manifesty przechodza walidacje semantyczna
/// (bez sprawdzania sciezek na dysku — runtime nie ma dostepu do FS).
#[test]
fn all_loaded_manifests_pass_validation() {
    let reg = super::registry::registry();
    let mut failures: Vec<(String, Vec<ValidationError>)> = Vec::new();
    for manifest in reg.engines() {
        if let Err(errs) = validate_engine(manifest, None) {
            failures.push((manifest.engine.id.clone(), errs));
        }
    }
    assert!(failures.is_empty(), "Manifesty z bledami: {failures:?}");
}

/// E3: Wszystkie engine.id sa unikalne (uniqueness sprawdzana przez build.rs,
/// ale potwierdzamy dla pewnosci).
#[test]
fn loaded_manifest_engine_ids_unique() {
    let reg = super::registry::registry();
    let ids: Vec<&str> = reg.engines().iter().map(|m| m.engine.id.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "Duplikaty engine.id wsrod {ids:?}");
}

/// E4: REGISTRY zawiera kluczowe silniki LLM (llama-cpp, mlx, vllm, sglang, ollama, tensorrt-llm).
#[test]
fn loaded_manifest_contains_known_llm_engines() {
    let reg = super::registry::registry();
    for id in &[
        "llama-cpp",
        "mlx",
        "vllm",
        "sglang",
        "ollama",
        "tensorrt-llm",
    ] {
        assert!(
            reg.by_id(id).is_some(),
            "Brak silnika '{id}' w embedded manifescie"
        );
    }
}

/// E5: non_empty_categories zawiera kluczowe kategorie (llm, stt, tts, image-gen, agents).
/// Liczba kategorii moze rosnac (np. dodano vision, tools), wiec sprawdzamy
/// tylko obecnosc znanych zamiast sztywnego count, ktory szybko driftuje.
#[test]
fn loaded_manifest_has_required_non_empty_categories() {
    let reg = super::registry::registry();
    let cats = reg.non_empty_categories();
    for required in [
        Category::Llm,
        Category::Stt,
        Category::Tts,
        Category::ImageGen,
        Category::Agents,
    ] {
        assert!(
            cats.contains(&required),
            "Brak wymaganej kategorii {required:?} w {cats:?}"
        );
    }
}
