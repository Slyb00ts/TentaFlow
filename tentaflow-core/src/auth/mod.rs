// =============================================================================
// Plik: auth/mod.rs
// Opis: Modul autentykacji — SSO/OIDC i zarzadzanie uzytkownikami.
// =============================================================================

#[cfg(feature = "dashboard-api")]
pub mod sso;

pub mod rate_limit;
