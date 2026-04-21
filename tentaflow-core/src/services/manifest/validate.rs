// =============================================================================
// Plik: validate.rs
// Opis: Walidacja semantyczna service manifestow — implementuje 4 reguly ze
//       SCHEMA.md (sekcja "Reguly walidacji semantycznej") oraz walidacje
//       engine.id chroniaca przed path-traversal/RCE w sciezkach runtime.
// =============================================================================

use super::types::*;
use std::path::Path;

/// Waliduje `engine.id` (oraz inne identyfikatory uzywane w sciezkach FS i URL).
/// Regex: `^[a-z0-9][a-z0-9_-]{0,63}$` — kebab/snake_case, 1-64 znakow.
/// Wspoldzielona z warstwa API (`is_valid_engine_id` w server.rs MUSI byc identyczna).
pub fn validate_engine_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error(
        "Engine id = '{id}' nie spelnia wymaganego formatu \
         '^[a-z0-9][a-z0-9_-]{{0,63}}$' (1-64 znakow, kebab/snake_case)"
    )]
    InvalidEngineId { id: String },

    #[error(
        "Engine '{engine_id}': brak sekcji deploymentu — wymagana przynajmniej jedna z \
         [deploy.docker], [deploy.native], [deploy.external]"
    )]
    NoDeploySection { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.native.runtime = embedded wymaga pola feature_flag \
         (i nie moze miec binary_path/bundle_path)"
    )]
    EmbeddedRequiresFeatureFlag { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.native.runtime = binary wymaga pola binary_path \
         (i nie moze miec feature_flag/bundle_path)"
    )]
    BinaryRequiresBinaryPath { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.native.runtime = python-bundle wymaga pola bundle_path \
         (i nie moze miec feature_flag/binary_path)"
    )]
    PythonBundleRequiresBundlePath { engine_id: String },

    #[error("Engine '{engine_id}': sciezka {field} = '{path}' nie istnieje na dysku")]
    PathMissing {
        engine_id: String,
        field: &'static str,
        path: String,
    },
}

/// Waliduje pojedynczy manifest. 4 reguly ze SCHEMA.md.
///
/// Reguła 4 (sciezki istnieja na dysku) jest sprawdzana tylko gdy `containers_root`
/// jest podany — runtime moze nie miec dostepu do FS containers/.
pub fn validate_engine(
    manifest: &ServiceManifest,
    containers_root: Option<&Path>,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let eid = &manifest.engine.id;

    // Reguła 1: engine.id musi spelniac whitelist regex.
    if !validate_engine_id(eid) {
        errors.push(ValidationError::InvalidEngineId { id: eid.clone() });
    }

    // Reguła 2: minimum jedna sekcja deploy.
    let deploy = &manifest.deploy;
    if deploy.docker.is_none() && deploy.native.is_none() && deploy.external.is_none() {
        errors.push(ValidationError::NoDeploySection {
            engine_id: eid.clone(),
        });
    }

    // Reguła 3: deploy.native.runtime spojny z polami.
    if let Some(native) = &deploy.native {
        match native.runtime {
            NativeRuntime::Embedded => {
                if native.feature_flag.is_none()
                    || native.binary_path.is_some()
                    || native.bundle_path.is_some()
                {
                    errors.push(ValidationError::EmbeddedRequiresFeatureFlag {
                        engine_id: eid.clone(),
                    });
                }
            }
            NativeRuntime::Binary => {
                if native.binary_path.is_none()
                    || native.feature_flag.is_some()
                    || native.bundle_path.is_some()
                {
                    errors.push(ValidationError::BinaryRequiresBinaryPath {
                        engine_id: eid.clone(),
                    });
                }
            }
            NativeRuntime::PythonBundle => {
                if native.bundle_path.is_none()
                    || native.feature_flag.is_some()
                    || native.binary_path.is_some()
                {
                    errors.push(ValidationError::PythonBundleRequiresBundlePath {
                        engine_id: eid.clone(),
                    });
                }
            }
        }
    }

    // Reguła 4: sciezki na dysku (sprawdzana tylko build-time).
    if let Some(root) = containers_root {
        if let Some(d) = &deploy.docker {
            check_path(
                root,
                &d.context_path,
                "deploy.docker.context_path",
                eid,
                &mut errors,
            );
        }
        if let Some(n) = &deploy.native {
            if let Some(p) = &n.binary_path {
                check_path(root, p, "deploy.native.binary_path", eid, &mut errors);
            }
            if let Some(p) = &n.bundle_path {
                check_path(root, p, "deploy.native.bundle_path", eid, &mut errors);
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn check_path(
    root: &Path,
    rel: &str,
    field: &'static str,
    engine_id: &str,
    errors: &mut Vec<ValidationError>,
) {
    let full = root.join(rel);
    if !full.is_dir() {
        errors.push(ValidationError::PathMissing {
            engine_id: engine_id.to_string(),
            field,
            path: rel.to_string(),
        });
    }
}
