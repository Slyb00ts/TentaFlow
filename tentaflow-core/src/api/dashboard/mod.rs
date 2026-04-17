// =============================================================================
// Plik: api/dashboard/mod.rs
// Opis: Modul dashboardu - REST API z JWT auth dla web interfejsu.
// =============================================================================

pub mod server;
pub mod auth;
pub mod api_auth;
pub mod api_services;
pub mod api_dashboard;
pub mod api_apikeys;
pub mod api_settings;
pub mod api_portainer;
pub mod api_prompts;
pub mod api_models;
pub mod api_flows;
pub mod api_pii_rules;
pub mod api_fast_path;
pub mod api_tts_rules;
pub mod static_files;
pub mod ws_metrics;
pub mod api_registries;
pub mod api_chat;
pub mod api_mesh;
pub mod api_hub;
pub mod api_addon_system;
pub mod api_clusters;
pub mod api_nim;
pub mod auto_register;
pub mod ws_deploy;
pub mod ws_binary;
pub mod api_services_manifest;

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

pub use server::DashboardServer;
pub use server::DashboardBody;
