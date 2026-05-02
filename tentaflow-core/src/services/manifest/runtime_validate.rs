// =============================================================================
// File: services/manifest/runtime_validate.rs
// Opis: Runtime validation deploy targetu (engine_id + deploy_method)
//       wywolywana z binary handlera ServiceManifestDeployRequest.
// =============================================================================

use crate::services::manifest::registry as manifest_registry;

/// Bledy walidacji `ServiceManifestDeployRequest`.
pub enum DeployValidationError {
    EngineNotFound,
    DeployMethodNotAvailable,
    InvalidDeployMethod,
}

impl DeployValidationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::EngineNotFound => "ENGINE_NOT_FOUND",
            Self::DeployMethodNotAvailable => "DEPLOY_METHOD_NOT_AVAILABLE",
            Self::InvalidDeployMethod => "INVALID_DEPLOY_METHOD",
        }
    }
}

/// Waliduje engine_id + deploy_method na podstawie ManifestRegistry.
/// Zwraca Ok(()) gdy silnik istnieje i oferuje wybrany tryb.
pub fn validate_deploy_target(
    engine_id: &str,
    deploy_method: &str,
) -> Result<(), DeployValidationError> {
    let registry = manifest_registry();
    let manifest = registry
        .by_id(engine_id)
        .ok_or(DeployValidationError::EngineNotFound)?;

    let method_present = match deploy_method {
        "docker" => manifest.deploy.docker.is_some(),
        "native" => manifest.deploy.native.is_some(),
        "external" => manifest.deploy.external.is_some(),
        _ => return Err(DeployValidationError::InvalidDeployMethod),
    };
    if !method_present {
        return Err(DeployValidationError::DeployMethodNotAvailable);
    }
    Ok(())
}
