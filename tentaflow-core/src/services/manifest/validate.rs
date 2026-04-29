// =============================================================================
// File: validate.rs
// Description: Semantic validation for service manifests. It implements the
// schema rules from SCHEMA.md and validates engine ids used in runtime paths.
// =============================================================================

use super::types::*;
use std::path::Path;

/// Validates `engine.id` and other identifiers used in filesystem paths and URLs.
/// Regex: `^[a-z0-9][a-z0-9_-]{0,63}$`.
/// Shared with the API layer and must stay identical there.
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
    #[error("Engine id = '{id}' must match '^[a-z0-9][a-z0-9_-]{{0,63}}$'")]
    InvalidEngineId { id: String },

    #[error(
        "Engine '{engine_id}': missing deployment section; define at least one of \
         [deploy.docker], [deploy.native], or [deploy.external]"
    )]
    NoDeploySection { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.docker must define exactly one of context_path or compose_path"
    )]
    DockerRequiresSingleSource { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.native.runtime = embedded must not define \
         binary_path or bundle_path (feature_flag is optional — silniki gated \
         tylko przez target_os, jak apple-tts czy tract-onnx vision, mogą go pominąć)"
    )]
    EmbeddedRequiresFeatureFlag { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.native.runtime = binary requires binary_path \
         and must not define feature_flag or bundle_path"
    )]
    BinaryRequiresBinaryPath { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.native.runtime = python-bundle requires bundle_path \
         and must not define feature_flag or binary_path"
    )]
    PythonBundleRequiresBundlePath { engine_id: String },

    #[error("Engine '{engine_id}': path {field} = '{path}' does not exist on disk")]
    PathMissing {
        engine_id: String,
        field: &'static str,
        path: String,
    },
}

/// Validates a single manifest.
///
/// The path-existence rule is only checked when `containers_root` is provided.
pub fn validate_engine(
    manifest: &ServiceManifest,
    containers_root: Option<&Path>,
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let eid = &manifest.engine.id;

    // Rule 1: validate engine id against the whitelist regex.
    if !validate_engine_id(eid) {
        errors.push(ValidationError::InvalidEngineId { id: eid.clone() });
    }

    // Rule 2: at least one deploy section must be present.
    let deploy = &manifest.deploy;
    if deploy.docker.is_none() && deploy.native.is_none() && deploy.external.is_none() {
        errors.push(ValidationError::NoDeploySection {
            engine_id: eid.clone(),
        });
    }

    if let Some(docker) = &deploy.docker {
        let has_context = docker
            .context_path
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty());
        let has_compose = docker
            .compose_path
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty());
        if has_context == has_compose {
            errors.push(ValidationError::DockerRequiresSingleSource {
                engine_id: eid.clone(),
            });
        }
    }

    // Rule 3: native runtime must be consistent with its fields.
    if let Some(native) = &deploy.native {
        match native.runtime {
            NativeRuntime::Embedded => {
                // feature_flag is optional: silniki gated wyłącznie przez target_os
                // (apple-tts via AVSpeechSynthesizer, vision/* via tract-onnx) nie
                // mają Cargo feature do wskazania. binary_path / bundle_path nigdy
                // nie pasują do embedded.
                if native.binary_path.is_some() || native.bundle_path.is_some() {
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

    // Rule 4: referenced paths must exist on disk.
    if let Some(root) = containers_root {
        if let Some(d) = &deploy.docker {
            if let Some(path) = &d.context_path {
                check_path(root, path, "deploy.docker.context_path", eid, &mut errors);
            }
            if let Some(path) = &d.compose_path {
                check_file(root, path, "deploy.docker.compose_path", eid, &mut errors);
            }
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

fn check_file(
    root: &Path,
    rel: &str,
    field: &'static str,
    engine_id: &str,
    errors: &mut Vec<ValidationError>,
) {
    let full = root.join(rel);
    if !full.is_file() {
        errors.push(ValidationError::PathMissing {
            engine_id: engine_id.to_string(),
            field,
            path: rel.to_string(),
        });
    }
}
