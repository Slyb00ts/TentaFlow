// =============================================================================
// Plik: api/dashboard/mod.rs
// Opis: Modul dashboardu - REST API z JWT auth dla web interfejsu.
// =============================================================================

pub mod api_addon_system;
pub mod api_apikeys;
pub mod api_auth;
pub mod api_clusters;
pub mod api_dashboard;
pub mod api_fast_path;
pub mod api_flows;
pub mod api_hub;
pub mod api_mesh;
pub mod api_models;
pub mod api_nim;
pub mod api_pii_rules;
pub mod api_prompts;
pub mod api_registries;
pub mod api_services;
pub mod api_services_manifest;
pub mod api_tts_rules;
pub mod auth;
pub mod auto_register;
pub mod handlers_addon_lifecycle;
pub mod handlers_addon_oauth;
pub mod handlers_addon_permissions;
pub mod handlers_meeting;
pub mod handlers_my_accounts;
pub mod handlers_notes;
pub mod handlers_translate;
pub mod oauth_addon_callback;
pub mod server;
pub mod static_files;
pub mod ws_binary;
pub mod ws_deploy;
pub mod ws_metrics;

#[cfg(feature = "inference-diarization")]
pub mod api_voice_profiles;

/// Escapowanie znakow specjalnych JSON w stringu
fn escape_json_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

pub use server::DashboardBody;
pub use server::DashboardServer;
