// =============================================================================
// Plik: validate.rs
// Opis: Walidacja semantyczna service manifestow — implementuje 10 regul ze
//       SCHEMA.md (sekcja "Reguly walidacji semantycznej") oraz walidacje
//       engine.id chroniaca przed path-traversal/RCE w sciezkach runtime.
// =============================================================================

use super::types::*;
use std::collections::HashSet;
use std::path::Path;

/// Waliduje `engine.id` (oraz inne identyfikatory uzywane w sciezkach FS i URL).
/// Regex: `^[a-z0-9][a-z0-9_-]{0,63}$` — kebab/snake_case, 1-64 znakow.
/// Wspoldzielona z warstwa API (`is_valid_engine_id` w server.rs MUSI byc identyczna).
pub fn validate_engine_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    // Pierwszy znak: a-z lub 0-9.
    let first = bytes[0];
    let first_ok = first.is_ascii_lowercase() || first.is_ascii_digit();
    if !first_ok {
        return false;
    }
    // Pozostale znaki: a-z, 0-9, _ lub -.
    bytes[1..]
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': gpu_backend = {backend:?} \
         wymaga target_os in {required:?}, ale jest {actual:?}"
    )]
    InvalidGpuOsCombo {
        engine_id: String,
        variant_id: String,
        backend: GpuBackend,
        required: Vec<TargetOs>,
        actual: Vec<TargetOs>,
    },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': gpu_backend = mlx wymaga \
         deploy_mode = embedded, jest {mode:?}"
    )]
    MlxRequiresEmbedded {
        engine_id: String,
        variant_id: String,
        mode: DeployMode,
    },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': deploy_mode = docker dziala \
         tylko na linux/windows (macOS Docker brak GPU passthrough), jest {actual:?}"
    )]
    DockerInvalidOs {
        engine_id: String,
        variant_id: String,
        actual: Vec<TargetOs>,
    },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': context_path '{path}' \
         nie istnieje na dysku"
    )]
    ContextPathMissing {
        engine_id: String,
        variant_id: String,
        path: String,
    },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': download.enabled = true wymaga \
         digest sha256:... (64 hex znakow)"
    )]
    DownloadEnabledWithoutDigest {
        engine_id: String,
        variant_id: String,
    },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': deploy_mode = embedded wymaga \
         sekcji [variant.feature_flag]"
    )]
    EmbeddedRequiresFeatureFlag {
        engine_id: String,
        variant_id: String,
    },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': deploy_mode = external wymaga \
         sekcji [variant.detection]"
    )]
    ExternalRequiresDetection {
        engine_id: String,
        variant_id: String,
    },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': deploy_mode = {mode:?} wymaga \
         sekcji [variant.build]"
    )]
    BuildRequiredButMissing {
        engine_id: String,
        variant_id: String,
        mode: DeployMode,
    },
    #[error("Engine '{engine_id}' ma duplikat variant.id = '{variant_id}'")]
    DuplicateVariantId {
        engine_id: String,
        variant_id: String,
    },
    #[error(
        "Engine id = '{id}' nie spelnia wymaganego formatu \
         '^[a-z0-9][a-z0-9_-]{{0,63}}$' (1-64 znakow, kebab/snake_case)"
    )]
    InvalidEngineId { id: String },
    #[error(
        "Engine '{engine_id}' wariant '{variant_id}': download.enabled = true z \
         placeholder digest sha256:00...00 — uzupelnij prawdziwy digest przed \
         publikacja artefaktu"
    )]
    PlaceholderDigestEnabled {
        engine_id: String,
        variant_id: String,
    },
}

/// Waliduje pojedynczy manifest. Reguly 1-9 z SCHEMA.md.
///
/// Reguła 7 (`context_path` istnieje na dysku) jest sprawdzana tylko gdy
/// `containers_root` jest podany — runtime moze nie miec dostepu do FS.
/// Reguła 9 czesc dot. unikalnosci engine.id globalnie nalezy do build.rs/registry.
pub fn validate_engine(
    manifest: &ServiceManifest,
    containers_root: Option<&Path>,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let eid = &manifest.engine.id;

    // Reguła 10: engine.id musi spelniac whitelist regex.
    if !validate_engine_id(eid) {
        errors.push(ValidationError::InvalidEngineId { id: eid.clone() });
    }

    let mut seen_variant_ids: HashSet<String> = HashSet::new();

    for variant in &manifest.variants {
        // Reguła 9 (czesc lokalna): unikalnosc variant.id w obrebie engine.
        if !seen_variant_ids.insert(variant.id.clone()) {
            errors.push(ValidationError::DuplicateVariantId {
                engine_id: eid.clone(),
                variant_id: variant.id.clone(),
            });
        }

        let os_list = variant.target_os.as_vec();
        let backend_list = variant.gpu_backend.as_vec();

        // Reguly 1, 3, 4, 5: gpu_backend → wymagany podzbior target_os.
        for &backend in &backend_list {
            let required: Vec<TargetOs> = match backend {
                GpuBackend::Metal => vec![TargetOs::Macos, TargetOs::Ios],
                GpuBackend::Mlx => vec![TargetOs::Macos, TargetOs::Ios],
                GpuBackend::Cuda => vec![TargetOs::Linux, TargetOs::Windows],
                GpuBackend::Rocm => vec![TargetOs::Linux],
                GpuBackend::Xpu => vec![TargetOs::Linux, TargetOs::Windows],
                // cpu i vulkan dzialaja na dowolnym OS — pomijamy.
                GpuBackend::Cpu | GpuBackend::Vulkan => continue,
            };
            if !os_list.iter().all(|o| required.contains(o)) {
                errors.push(ValidationError::InvalidGpuOsCombo {
                    engine_id: eid.clone(),
                    variant_id: variant.id.clone(),
                    backend,
                    required,
                    actual: os_list.clone(),
                });
            }
        }

        // Reguła 2: mlx wymaga deploy_mode = embedded.
        if backend_list.contains(&GpuBackend::Mlx) && variant.deploy_mode != DeployMode::Embedded {
            errors.push(ValidationError::MlxRequiresEmbedded {
                engine_id: eid.clone(),
                variant_id: variant.id.clone(),
                mode: variant.deploy_mode,
            });
        }

        // Reguła 6: docker tylko na linux/windows.
        if variant.deploy_mode == DeployMode::Docker {
            let invalid = os_list
                .iter()
                .any(|os| !matches!(os, TargetOs::Linux | TargetOs::Windows));
            if invalid {
                errors.push(ValidationError::DockerInvalidOs {
                    engine_id: eid.clone(),
                    variant_id: variant.id.clone(),
                    actual: os_list.clone(),
                });
            }
        }

        // Wymagane podsekcje wedlug deploy_mode.
        match variant.deploy_mode {
            DeployMode::Docker | DeployMode::Native | DeployMode::PythonBundle => {
                if variant.build.is_none() {
                    errors.push(ValidationError::BuildRequiredButMissing {
                        engine_id: eid.clone(),
                        variant_id: variant.id.clone(),
                        mode: variant.deploy_mode,
                    });
                }
            }
            DeployMode::Embedded => {
                if variant.feature_flag.is_none() {
                    errors.push(ValidationError::EmbeddedRequiresFeatureFlag {
                        engine_id: eid.clone(),
                        variant_id: variant.id.clone(),
                    });
                }
            }
            DeployMode::External => {
                if variant.detection.is_none() {
                    errors.push(ValidationError::ExternalRequiresDetection {
                        engine_id: eid.clone(),
                        variant_id: variant.id.clone(),
                    });
                }
            }
        }

        // Reguła 7: context_path musi istniec na dysku.
        if let (Some(build), Some(root)) = (&variant.build, containers_root) {
            let full = root.join(&build.context_path);
            if !full.is_dir() {
                errors.push(ValidationError::ContextPathMissing {
                    engine_id: eid.clone(),
                    variant_id: variant.id.clone(),
                    path: build.context_path.clone(),
                });
            }
        }

        // Reguła 8: download.enabled = true wymaga poprawnego digest oraz nie
        // moze byc placeholder z samych zer (chroni przed pull-by-fake-digest
        // gdy ktos przelaczy enabled = true zapominajac zaktualizowac digest).
        if let Some(dl) = &variant.download {
            if dl.enabled {
                if !is_valid_sha256_digest(&dl.digest) {
                    errors.push(ValidationError::DownloadEnabledWithoutDigest {
                        engine_id: eid.clone(),
                        variant_id: variant.id.clone(),
                    });
                } else if is_placeholder_zero_digest(&dl.digest) {
                    errors.push(ValidationError::PlaceholderDigestEnabled {
                        engine_id: eid.clone(),
                        variant_id: variant.id.clone(),
                    });
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Sprawdza czy string ma format `sha256:<64 hex znakow>`.
fn is_valid_sha256_digest(s: &str) -> bool {
    s.starts_with("sha256:")
        && s.len() == 71
        && s[7..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Sprawdza czy digest jest placeholderem skladajacym sie z samych zer
/// (`sha256:00...00`). Wymaga, aby format byl wczesniej zwalidowany.
fn is_placeholder_zero_digest(s: &str) -> bool {
    s.len() == 71 && s.starts_with("sha256:") && s[7..].bytes().all(|b| b == b'0')
}
