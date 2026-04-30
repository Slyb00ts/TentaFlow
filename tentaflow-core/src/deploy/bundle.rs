// =============================================================================
// Plik: deploy/bundle.rs
// Opis: Embedowany bundle kontenerow (wbudowany przez build.rs jako tar.gz).
//       extract_to(target) rozpakowuje tentaflow-containers/ oraz wspolne
//       crate'y Rust wymagane przez wybrane Dockerfile do podanego katalogu
//       — typowo tmpdir w trakcie deployu.
// =============================================================================

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// tar.gz wbudowany przez build.rs (patrz: pack_container_contexts).
const CONTAINER_BUNDLE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/container_bundle.tar.gz"));

/// Stable fingerprint of the embedded bundle bytes. Used as a marker so
/// `paths::ensure_app_dirs()` re-extracts only when the build.rs output
/// actually changed (new repo snapshot baked into the binary).
pub fn bundle_hash() -> String {
    let mut hasher = Sha256::new();
    hasher.update(CONTAINER_BUNDLE);
    hex::encode(hasher.finalize())
}

/// Returns true if build.rs successfully produced a non-empty bundle.
pub fn is_embedded() -> bool {
    !CONTAINER_BUNDLE.is_empty()
}

/// Rozpakowuje wbudowany kontekst kontenerow do podanego katalogu.
/// Po rozpakowaniu w `target` znajdziesz `tentaflow-containers/`,
/// `tentaflow-protocol/`, `tentaflow-transport/` i `tentaflow-voice/`.
/// Bezpieczne dla deploy do tmpdir.
pub fn extract_to(target: &Path) -> Result<()> {
    if CONTAINER_BUNDLE.is_empty() {
        anyhow::bail!(
            "Bundle kontenerow jest pusty — build.rs nie spakowal go (sprawdz logi cargo build)"
        );
    }
    std::fs::create_dir_all(target)
        .with_context(|| format!("nie mozna utworzyc {}", target.display()))?;

    let decoder = flate2::read::GzDecoder::new(CONTAINER_BUNDLE);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(target)
        .with_context(|| format!("rozpakowanie bundle do {}", target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_to_tmpdir_works() {
        if CONTAINER_BUNDLE.is_empty() {
            return; // build bez bundle — pomijamy
        }
        let dir = tempfile::tempdir().unwrap();
        extract_to(dir.path()).expect("extract");
        assert!(dir.path().join("tentaflow-containers").exists());
        assert!(dir.path().join("tentaflow-protocol").exists());
        assert!(dir.path().join("tentaflow-transport").exists());
        assert!(dir.path().join("tentaflow-voice").exists());
    }
}
