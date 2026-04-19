// =============================================================================
// Plik: api/tls_pem.rs
// Opis: Parsowanie certyfikatow i kluczy PEM dla HTTPS dashboard API (axum +
//       tokio-rustls). Iroh ma wlasny mechanizm TLS — te helpery obsluguja tylko
//       warstwe HTTPS.
// =============================================================================

use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Parsuje certyfikaty TLS z bajtow PEM.
pub fn parse_certs_pem(pem_data: &[u8]) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let certs = rustls_pemfile::certs(&mut &pem_data[..])
        .collect::<Result<Vec<_>, _>>()
        .context("Nie udalo sie sparsowac certyfikatu")?;
    Ok(certs)
}

/// Parsuje klucz prywatny TLS z bajtow PEM.
pub fn parse_key_pem(pem_data: &[u8]) -> anyhow::Result<PrivateKeyDer<'static>> {
    let key = rustls_pemfile::private_key(&mut &pem_data[..])
        .context("Nie udalo sie sparsowac klucza prywatnego")?
        .ok_or_else(|| anyhow::anyhow!("Nie znaleziono klucza prywatnego"))?;
    Ok(key)
}
