// =============================================================================
// Plik: api/dashboard/api_services_manifest.rs
// Opis: Endpointy REST dla Service Manifest — pobieranie manifestu silnikow
//       z REGISTRY, sprawdzanie tieru licencji oraz inicjowanie deploymentu
//       wariantow z gatingiem licencyjnym (Free/Pro/Enterprise).
// =============================================================================

use crate::license::{LicenseChecker, LicenseTier};
use crate::services::manifest::{
    registry as manifest_registry, RequiredLicenseTier, ServiceManifest,
};
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
/// Pola wrazliwe (`download.image`, `download.digest`) sa redagowane jezeli
/// aktualny tier nie pozwala na pobranie wariantu (OWASP A01 — IDOR/info leak).
pub fn handle_get_manifest(license: &Arc<dyn LicenseChecker>) -> (u16, String) {
    let tier = license.tier();
    let redacted: Vec<serde_json::Value> = manifest_registry()
        .engines()
        .iter()
        .map(|m| redact_for_tier(m, tier))
        .collect();
    match serde_json::to_string(&redacted) {
        Ok(body) => (200, body),
        Err(e) => (500, api_error("SERIALIZE_FAILED", e.to_string())),
    }
}

/// GET /api/services/manifest/:engine_id — pojedynczy silnik. 404 jesli brak.
/// Stosuje to samo redagowanie pol wrazliwych co `handle_get_manifest`.
pub fn handle_get_engine_manifest(
    license: &Arc<dyn LicenseChecker>,
    engine_id: &str,
) -> (u16, String) {
    let tier = license.tier();
    match manifest_registry().by_id(engine_id) {
        Some(m) => {
            let redacted = redact_for_tier(m, tier);
            match serde_json::to_string(&redacted) {
                Ok(body) => (200, body),
                Err(e) => (500, api_error("SERIALIZE_FAILED", e.to_string())),
            }
        }
        None => (
            404,
            api_error(
                "ENGINE_NOT_FOUND",
                format!("Silnik '{}' nie istnieje w manifescie", engine_id),
            ),
        ),
    }
}

/// Mapuje wymagany przez wariant tier do tieru uzytkownika (Pro→Pro, Enterprise→Enterprise).
fn map_required_tier(req: RequiredLicenseTier) -> LicenseTier {
    match req {
        RequiredLicenseTier::Pro => LicenseTier::Pro,
        RequiredLicenseTier::Enterprise => LicenseTier::Enterprise,
    }
}

/// Buduje JSON manifestu z polami `variant.download.image` i `variant.download.digest`
/// wymazanymi dla wariantow, do ktorych aktualny tier nie ma uprawnien.
/// Pozostawia `license_required`, `enabled` i `size_mb`, zeby GUI moglo pokazac
/// "ta opcja istnieje, ale wymaga wyzszego tieru".
fn redact_for_tier(manifest: &ServiceManifest, tier: LicenseTier) -> serde_json::Value {
    let mut value = serde_json::to_value(manifest).unwrap_or(serde_json::Value::Null);

    let Some(variants) = value
        .get_mut("variants")
        .and_then(|v| v.as_array_mut())
    else {
        return value;
    };

    for variant in variants.iter_mut() {
        let Some(download) = variant
            .get_mut("download")
            .and_then(|d| if d.is_null() { None } else { Some(d) })
        else {
            continue;
        };

        // Odczytaj wymagany tier z pola `license_required` (default = pro).
        let required_tier = download
            .get("license_required")
            .and_then(|v| v.as_str())
            .and_then(|s| match s {
                "pro" => Some(RequiredLicenseTier::Pro),
                "enterprise" => Some(RequiredLicenseTier::Enterprise),
                _ => None,
            })
            .unwrap_or(RequiredLicenseTier::Pro);

        let user_required = map_required_tier(required_tier);
        let allowed = matches!(
            (tier, user_required),
            (LicenseTier::Enterprise, _)
                | (LicenseTier::Pro, LicenseTier::Pro | LicenseTier::Free)
                | (LicenseTier::Free, LicenseTier::Free)
        );

        if !allowed {
            if let Some(obj) = download.as_object_mut() {
                obj.remove("image");
                obj.remove("digest");
            }
        }
    }

    value
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
