// =============================================================================
// Plik: auth/mod.rs
// Opis: Modul autentykacji i autoryzacji — ACL, SSO/OIDC, rate limiting.
// =============================================================================

pub mod acl;
pub mod rate_limit;
pub mod sso;

pub use acl::UserContext;
