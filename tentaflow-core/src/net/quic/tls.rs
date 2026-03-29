// =============================================================================
// Plik: net/quic/tls.rs
// Opis: Wspolna logika TLS dla QUIC — ladowanie certyfikatow, kluczy, CA.
//       Uzywana zarowno przez QuicClient jak i QuicServer.
// =============================================================================

use crate::error::CoreError;
use anyhow::Context;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs;
use tracing::debug;

/// Wczytuje certyfikaty TLS z pliku PEM.
///
/// Parametry:
/// - `path`: Sciezka do pliku z certyfikatem (format PEM)
///
/// Zwraca: Wektor certyfikatow DER
pub fn load_certs(path: &str) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let cert_data = fs::read(path)
        .with_context(|| format!("Nie udalo sie odczytac certyfikatu: {}", path))?;

    let certs = rustls_pemfile::certs(&mut cert_data.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .context("Nie udalo sie sparsowac certyfikatu")?;

    Ok(certs)
}

/// Wczytuje klucz prywatny TLS z pliku PEM.
///
/// Parametry:
/// - `path`: Sciezka do pliku z kluczem (format PEM)
///
/// Zwraca: Klucz prywatny w formacie DER
pub fn load_private_key(path: &str) -> anyhow::Result<PrivateKeyDer<'static>> {
    let key_data = fs::read(path)
        .with_context(|| format!("Nie udalo sie odczytac klucza: {}", path))?;

    let key = rustls_pemfile::private_key(&mut key_data.as_slice())
        .context("Nie udalo sie sparsowac klucza prywatnego")?
        .ok_or_else(|| anyhow::anyhow!("Nie znaleziono klucza prywatnego w pliku"))?;

    Ok(key)
}

/// Parsuje certyfikaty TLS z bajtow PEM (bez odczytu z pliku).
pub fn parse_certs_pem(pem_data: &[u8]) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let certs = rustls_pemfile::certs(&mut &pem_data[..])
        .collect::<Result<Vec<_>, _>>()
        .context("Nie udalo sie sparsowac certyfikatu")?;
    Ok(certs)
}

/// Parsuje klucz prywatny TLS z bajtow PEM (bez odczytu z pliku).
pub fn parse_key_pem(pem_data: &[u8]) -> anyhow::Result<PrivateKeyDer<'static>> {
    let key = rustls_pemfile::private_key(&mut &pem_data[..])
        .context("Nie udalo sie sparsowac klucza prywatnego")?
        .ok_or_else(|| anyhow::anyhow!("Nie znaleziono klucza prywatnego"))?;
    Ok(key)
}

/// Laduje certyfikaty CA z pliku lub inline PEM (opcjonalne).
/// Jesli brak CA, zwraca pusty wektor — klient uzyje systemowych certyfikatow.
///
/// Parametry:
/// - `ca_value`: Sciezka do pliku PEM lub inline PEM string
///
/// Zwraca: Wektor certyfikatow CA w formacie DER
pub fn load_ca_certs(ca_value: &str) -> Result<Vec<CertificateDer<'static>>, CoreError> {
    let ca_pem = if ca_value.trim_start().starts_with("-----BEGIN") {
        debug!("CA podane jako inline PEM");
        ca_value.as_bytes().to_vec()
    } else {
        std::fs::read(ca_value)
            .context("Failed to read CA cert")
            .map_err(|e| CoreError::ConfigError {
                message: format!("Cannot read CA: {}", ca_value),
                source: e,
            })?
    };

    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &ca_pem[..])
        .map(|cert| cert.map_err(anyhow::Error::from))
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(|e| CoreError::ConfigError {
            message: "Nieprawidlowy format certyfikatu CA".to_string(),
            source: e,
        })?;

    if ca_certs.is_empty() {
        return Err(CoreError::ConfigError {
            message: "Brak certyfikatow CA w podanych danych".to_string(),
            source: anyhow::anyhow!("Empty CA data"),
        });
    }

    debug!("Zaladowano {} certyfikatow CA", ca_certs.len());
    Ok(ca_certs)
}
