// =============================================================================
// Plik: identity.rs
// Opis: Ladowanie lub generowanie Ed25519 SecretKey dla iroh endpointa sidecara.
//       Klucz zapisywany na volumenie (`/data/endpoint-key.bin`) zeby sidecar
//       po restarcie mial ten sam `EndpointId` — router nie musi odnawiac
//       rejestracji.
// =============================================================================

use anyhow::{Context, Result};
use iroh::SecretKey;
use std::path::Path;

/// Laduje `SecretKey` z pliku, albo generuje nowy i zapisuje pod wskazana sciezke.
/// Jesli `path` jest `None`, generuje ephemeral key (po restarcie zmienia sie).
pub fn load_or_generate(path: Option<&str>) -> Result<SecretKey> {
    let Some(path) = path else {
        tracing::warn!("Brak secret_key_path — generuje ephemeral key (restart = nowy EndpointId)");
        return Ok(SecretKey::generate());
    };

    let path = Path::new(path);
    if path.exists() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("nie moge wczytac klucza z {}", path.display()))?;
        if bytes.len() != 32 {
            anyhow::bail!(
                "plik klucza {} ma {} bajtow, wymagane 32",
                path.display(),
                bytes.len()
            );
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        tracing::info!(path = %path.display(), "wczytano istniejacy Ed25519 secret key");
        return Ok(SecretKey::from_bytes(&arr));
    }

    let key = SecretKey::generate();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    std::fs::write(path, key.to_bytes())
        .with_context(|| format!("zapis klucza do {}", path.display()))?;
    tracing::info!(path = %path.display(), "wygenerowano i zapisano nowy Ed25519 secret key");
    Ok(key)
}
