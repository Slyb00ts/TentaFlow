// =============================================================================
// Plik: validate.rs
// Opis: Walidacja semantyczna service manifestow — implementuje 9 regul ze
//       SCHEMA.md (sekcja "Reguly walidacji semantycznej").
// =============================================================================

use super::types::*;
use std::collections::HashSet;
use std::path::Path;

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

        // Reguła 8: download.enabled = true wymaga poprawnego digest.
        if let Some(dl) = &variant.download {
            if dl.enabled && !is_valid_sha256_digest(&dl.digest) {
                errors.push(ValidationError::DownloadEnabledWithoutDigest {
                    engine_id: eid.clone(),
                    variant_id: variant.id.clone(),
                });
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
