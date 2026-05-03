// ============ File: validate.rs — semantic validation for service manifests ============

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
        "Engine '{engine_id}': [deploy.docker] is missing required `transport` field — \
         set it to \"sidecar-quic\" or \"direct-http\""
    )]
    DockerRequiresTransport { engine_id: String },

    #[error(
        "Engine '{engine_id}': deploy.native.runtime = embedded must not define \
         binary_path or bundle_path (feature_flag is optional — engines gated \
         purely by target_os, e.g. apple-tts or tract-onnx vision, may omit it)"
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

    /// `service_surfaces` / `input_modalities` / `output_modalities` got
    /// a value the runtime does not know how to map. Caught at build
    /// time so a typo (`"chats"` instead of `"chat"`) cannot quietly
    /// produce an entry that resolver / catalog ignores.
    #[error(
        "Engine '{engine_id}': {field} contains unknown value '{value}' \
         (allowed: {allowed:?})"
    )]
    UnknownEnumValue {
        engine_id: String,
        field: &'static str,
        value: String,
        allowed: Vec<&'static str>,
    },

    /// Explicit empty list (`service_surfaces = []`) is rejected — it
    /// is ambiguous between "no advertised surface" and "fall back to
    /// category default". Manifests must omit the field for the
    /// fallback or list every value they intend to advertise.
    #[error(
        "Engine '{engine_id}': {field} is an explicit empty list — omit \
         the field to fall back to category defaults"
    )]
    EmptyEnumList {
        engine_id: String,
        field: &'static str,
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
        // Phase 6: every docker manifest must declare a runtime transport.
        if docker.transport.is_none() {
            errors.push(ValidationError::DockerRequiresTransport {
                engine_id: eid.clone(),
            });
        }
    }

    // Rule 3: native runtime must be consistent with its fields.
    if let Some(native) = &deploy.native {
        match native.runtime {
            NativeRuntime::Embedded => {
                // feature_flag is optional: engines gated purely by target_os
                // (apple-tts via AVSpeechSynthesizer, vision/* via tract-onnx)
                // have no Cargo feature to point at. binary_path / bundle_path
                // never match embedded.
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

    // Rule 5: capability vocabulary checks. Values flow into the public
    // catalog; an unknown surface or modality silently makes the entry
    // invisible to resolvers, so we reject typos at build time.
    validate_enum_list(
        eid,
        "engine.service_surfaces",
        manifest.engine.service_surfaces.as_deref(),
        super::types::VALID_SERVICE_SURFACES,
        &mut errors,
    );
    validate_enum_list(
        eid,
        "engine.input_modalities",
        manifest.engine.input_modalities.as_deref(),
        super::types::VALID_INPUT_MODALITIES,
        &mut errors,
    );
    validate_enum_list(
        eid,
        "engine.output_modalities",
        manifest.engine.output_modalities.as_deref(),
        super::types::VALID_OUTPUT_MODALITIES,
        &mut errors,
    );
    for preset in &manifest.model_presets {
        validate_enum_list(
            eid,
            "model_preset.service_surfaces",
            preset.service_surfaces.as_deref(),
            super::types::VALID_SERVICE_SURFACES,
            &mut errors,
        );
        validate_enum_list(
            eid,
            "model_preset.input_modalities",
            preset.input_modalities.as_deref(),
            super::types::VALID_INPUT_MODALITIES,
            &mut errors,
        );
        validate_enum_list(
            eid,
            "model_preset.output_modalities",
            preset.output_modalities.as_deref(),
            super::types::VALID_OUTPUT_MODALITIES,
            &mut errors,
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_enum_list(
    engine_id: &str,
    field: &'static str,
    list: Option<&[String]>,
    allowed: &'static [&'static str],
    errors: &mut Vec<ValidationError>,
) {
    let Some(values) = list else { return };
    if values.is_empty() {
        errors.push(ValidationError::EmptyEnumList {
            engine_id: engine_id.to_string(),
            field,
        });
        return;
    }
    for value in values {
        if !allowed.contains(&value.as_str()) {
            errors.push(ValidationError::UnknownEnumValue {
                engine_id: engine_id.to_string(),
                field,
                value: value.clone(),
                allowed: allowed.to_vec(),
            });
        }
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
