// =============================================================================
// Plik: api/dashboard/mod.rs
// Opis: Modul dashboardu - REST API z JWT auth dla web interfejsu.
// =============================================================================

pub mod api_addon_system;
pub mod auth;
pub mod handlers_addon_lifecycle;
pub mod handlers_addon_oauth;
pub mod handlers_addon_permissions;
pub mod handlers_browser;
pub mod handlers_meeting;
pub mod handlers_my_accounts;
pub mod handlers_notes;
pub mod handlers_translate;
pub mod handlers_vnc;
pub mod oauth_addon_callback;
pub mod server;
pub mod static_files;
pub mod vnc_tunnel;
pub mod ws_binary;
pub mod ws_metrics;

pub use server::DashboardBody;
pub use server::DashboardServer;
