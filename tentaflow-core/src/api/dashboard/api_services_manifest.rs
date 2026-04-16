// =============================================================================
// Plik: api/dashboard/api_services_manifest.rs
// Opis: Endpointy REST dla Service Manifest — pobieranie manifestu silnikow
//       z REGISTRY, sprawdzanie tieru licencji oraz inicjowanie deploymentu
//       wariantow z gatingiem licencyjnym (Free/Pro/Enterprise).
// =============================================================================

use crate::license::{LicenseChecker, LicenseTier};
use crate::services::manifest::registry as manifest_registry;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// =============================================================================
// Modele zadan/odpowiedzi
// =============================================================================

#[derive(Serialize)]
pub struct LicenseInfoResponse {
    pub tier: LicenseTier,
    pub allows_pro: bool,
    pub allows_enterprise: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DeployMethod {
    Build,
    Download,
}

#[derive(Deserialize)]
pub struct DeployRequest {
    pub engine_id: String,
    pub variant_id: String,
    pub deploy_method: DeployMethod,
    pub node_id: String,
    #[serde(default)]
    pub config: serde_json::Value,
}

#[derive(Serialize)]
pub struct DeployResponse {
    pub status: String,
    pub deploy_id: String,
    pub engine_id: String,
    pub variant_id: String,
    pub method: DeployMethod,
    pub node_id: String,
    pub websocket_url: String,
}

#[derive(Serialize)]
pub struct ApiError {
    pub error_code: String,
    pub message: String,
}

// =============================================================================
// Helpery odpowiedzi
// =============================================================================

/// Buduje JSON dla bledu API z kodem i wiadomoscia.
fn api_error(code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&ApiError {
        error_code: code.to_string(),
        message: message.into(),
    })
    .unwrap_or_else(|_| r#"{"error_code":"INTERNAL","message":"serializacja"}"#.to_string())
}

// =============================================================================
// Handlery
// =============================================================================

/// GET /api/services/manifest — caly manifest jako JSON (lista silnikow).
pub fn handle_get_manifest() -> (u16, String) {
    let engines = manifest_registry().engines();
    match serde_json::to_string(engines) {
        Ok(body) => (200, body),
        Err(e) => (500, api_error("SERIALIZE_FAILED", e.to_string())),
    }
}

/// GET /api/services/manifest/:engine_id — pojedynczy silnik. 404 jesli brak.
pub fn handle_get_engine_manifest(engine_id: &str) -> (u16, String) {
    match manifest_registry().by_id(engine_id) {
        Some(m) => match serde_json::to_string(m) {
            Ok(body) => (200, body),
            Err(e) => (500, api_error("SERIALIZE_FAILED", e.to_string())),
        },
        None => (
            404,
            api_error(
                "ENGINE_NOT_FOUND",
                format!("Silnik '{}' nie istnieje w manifescie", engine_id),
            ),
        ),
    }
}

/// GET /api/license/info — aktualny tier licencji oraz uprawnienia.
pub fn handle_get_license_info(license: &Arc<dyn LicenseChecker>) -> (u16, String) {
    let tier = license.tier();
    let resp = LicenseInfoResponse {
        tier,
        allows_pro: license.allows(LicenseTier::Pro),
        allows_enterprise: license.allows(LicenseTier::Enterprise),
    };
    match serde_json::to_string(&resp) {
        Ok(body) => (200, body),
        Err(e) => (500, api_error("SERIALIZE_FAILED", e.to_string())),
    }
}

/// GET /api/services/deployed — lista aktywnych deploymentow (stub: pusta lista).
pub fn handle_get_deployed() -> (u16, String) {
    (200, "[]".to_string())
}

/// POST /api/services/deploy — walidacja i inicjacja deploymentu wariantu silnika.
/// Zwraca placeholder deploy_id; faktyczne uruchomienie buildu/downloadu jest
/// realizowane przez istniejacy ws_deploy/api_portainer w kolejnej iteracji.
pub fn handle_post_deploy(license: &Arc<dyn LicenseChecker>, body: &[u8]) -> (u16, String) {
    let req: DeployRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                400,
                api_error("BAD_REQUEST", format!("Nieprawidlowy JSON: {}", e)),
            );
        }
    };

    let registry = manifest_registry();

    let manifest = match registry.by_id(&req.engine_id) {
        Some(m) => m,
        None => {
            return (
                404,
                api_error(
                    "ENGINE_NOT_FOUND",
                    format!("Silnik '{}' nie istnieje w manifescie", req.engine_id),
                ),
            );
        }
    };

    let variant = match manifest.variants.iter().find(|v| v.id == req.variant_id) {
        Some(v) => v,
        None => {
            return (
                404,
                api_error(
                    "VARIANT_NOT_FOUND",
                    format!(
                        "Wariant '{}' nie istnieje dla silnika '{}'",
                        req.variant_id, req.engine_id
                    ),
                ),
            );
        }
    };

    match req.deploy_method {
        DeployMethod::Build => {
            if variant.build.is_none() {
                return (
                    400,
                    api_error(
                        "BUILD_NOT_AVAILABLE",
                        "Wariant nie posiada sekcji [variant.build]",
                    ),
                );
            }
        }
        DeployMethod::Download => {
            let download = match &variant.download {
                Some(d) => d,
                None => {
                    return (
                        400,
                        api_error(
                            "DOWNLOAD_NOT_AVAILABLE",
                            "Wariant nie posiada sekcji [variant.download]",
                        ),
                    );
                }
            };
            let feature_name = format!("{}/{}", req.engine_id, req.variant_id);
            if let Err(e) = license.check_variant_download(variant, &feature_name) {
                return (403, api_error("LICENSE_REQUIRED", e.to_string()));
            }
            if !download.enabled {
                return (
                    503,
                    api_error(
                        "DOWNLOAD_DISABLED",
                        "Download niedostepny w tej wersji, uzyj Build",
                    ),
                );
            }
        }
    }

    let deploy_id = uuid::Uuid::new_v4().to_string();
    let resp = DeployResponse {
        status: "started".to_string(),
        deploy_id: deploy_id.clone(),
        engine_id: req.engine_id,
        variant_id: req.variant_id,
        method: req.deploy_method,
        node_id: req.node_id,
        websocket_url: format!("/api/ws/deploy/{}", deploy_id),
    };

    match serde_json::to_string(&resp) {
        Ok(body) => (200, body),
        Err(e) => (500, api_error("SERIALIZE_FAILED", e.to_string())),
    }
}
