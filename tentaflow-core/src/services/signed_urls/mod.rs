// =============================================================================
// File: services/signed_urls/mod.rs — public API for generic signed URL issuer
// =============================================================================

mod issuer;

pub use issuer::{SignedUrl, SignedUrlError, SignedUrlIssuer, UrlScope};
