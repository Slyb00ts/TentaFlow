// =============================================================================
// File: services/key_storage.rs — persistent 32-byte HMAC keys on disk.
// =============================================================================
//
// Single source of truth for any non-AES key the host needs to keep around
// across restarts (F1b P3.A). Each key lives at
// `<tentaflow_home>/keys/<name>.key`, mode 0600 on Unix, holding exactly 32
// raw bytes.
//
// Layout:
//
//   <tentaflow_home>/keys/
//     pickup_token.key      <- HMAC key for PickupTokenIssuer (M1.W7)
//     frame_url.key         <- HMAC key for the frame SignedUrlIssuer (M1.W8)
//     recording_url.key     <- HMAC key for the recording SignedUrlIssuer
//     cameras.key           <- AES-GCM master key (P1.C, separate code path)
//
// Rotation contract: same atomic three-step write used by `cameras.key`
// (staging → new → live) so an interrupted `tentaflow-cli keys rotate <name>`
// is recoverable on next start. The exact rotation flow for these HMAC keys
// is in `tentaflow-cli/src/commands/keys.rs`; this module owns the storage
// primitives (load / generate / atomic write / interrupted-rotation recovery)
// and exposes them through `PersistentKey`.
//
// Override: every key's file path is overridable via
// `TENTAFLOW_KEY_<NAME>` (uppercased) so tests and CI can pin a tempfile
// without touching `$HOME`.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Subdirectory under `tentaflow_home()` that holds all 32-byte keys.
const KEY_SUBDIR: &str = "keys";

/// Standard length of every key managed by this module. Picked to match
/// HMAC-SHA256 block usage and AES-256.
pub const KEY_LEN: usize = 32;

#[derive(Debug, Error)]
pub enum KeyStorageError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid key file {path} (expected {KEY_LEN} bytes, got {got})")]
    InvalidKeyLength { path: String, got: usize },
    #[error("invalid key name {0:?} (must be lowercase ascii [a-z0-9_])")]
    InvalidKeyName(String),
}

/// Holds a 32-byte key together with the name it was loaded under. The name
/// is kept so rotation tooling can map back to the on-disk path without the
/// caller having to remember it. `Debug` redacts the bytes — the name is
/// the only safe-to-log field.
#[derive(Clone)]
pub struct PersistentKey {
    name: String,
    bytes: [u8; KEY_LEN],
}

impl PersistentKey {
    /// Load `<name>.key` from disk, generating a fresh CSPRNG key on first
    /// use. Idempotent: a second call returns the same bytes.
    ///
    /// Recovery: if `<name>.key.new` is found alongside the live file, an
    /// earlier rotation crashed between the commit marker (`.new`) and the
    /// final swap; we promote `.new` to the live path before reading.
    /// A leftover `<name>.key.staging` (rotation never committed) is
    /// discarded. A leftover `<name>.key.tmp` (aborted first-time generate)
    /// is discarded too.
    pub fn load_or_generate(name: &str) -> Result<Self, KeyStorageError> {
        validate_name(name)?;
        let path = key_path(name)?;
        Self::load_or_generate_at(name, &path)
    }

    /// Like [`load_or_generate`] but with an explicit file path. Used by
    /// tests to pin a tempdir.
    pub fn load_or_generate_at(name: &str, path: &PathBuf) -> Result<Self, KeyStorageError> {
        validate_name(name)?;
        recover_interrupted_rotation(path)?;
        if !path.exists() {
            generate_key_file(path)?;
        }
        let bytes = read_key_file(path)?;
        Ok(Self {
            name: name.to_string(),
            bytes,
        })
    }

    /// Construct from raw bytes — used by rotation tooling after writing a
    /// fresh file and by tests that need a deterministic key.
    pub fn from_raw(name: &str, bytes: [u8; KEY_LEN]) -> Result<Self, KeyStorageError> {
        validate_name(name)?;
        Ok(Self {
            name: name.to_string(),
            bytes,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }
}

impl std::fmt::Debug for PersistentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact the raw bytes — only the name is safe to log.
        f.debug_struct("PersistentKey")
            .field("name", &self.name)
            .field("bytes", &"<redacted 32 bytes>")
            .finish()
    }
}

/// Resolve the on-disk path for `<name>.key`. Honours
/// `TENTAFLOW_KEY_<NAME>` env override; otherwise resolves to
/// `<tentaflow_home>/keys/<name>.key`.
pub fn key_path(name: &str) -> Result<PathBuf, KeyStorageError> {
    validate_name(name)?;
    let env_var = format!("TENTAFLOW_KEY_{}", name.to_ascii_uppercase());
    if let Ok(p) = std::env::var(&env_var) {
        return Ok(PathBuf::from(p));
    }
    Ok(crate::paths::tentaflow_home()
        .join(KEY_SUBDIR)
        .join(format!("{}.key", name)))
}

fn validate_name(name: &str) -> Result<(), KeyStorageError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(KeyStorageError::InvalidKeyName(name.to_string()));
    }
    Ok(())
}

fn read_key_file(path: &PathBuf) -> Result<[u8; KEY_LEN], KeyStorageError> {
    let raw = std::fs::read(path)?;
    if raw.len() != KEY_LEN {
        return Err(KeyStorageError::InvalidKeyLength {
            path: path.display().to_string(),
            got: raw.len(),
        });
    }
    let mut k = [0u8; KEY_LEN];
    k.copy_from_slice(&raw);
    Ok(k)
}

/// Atomic-write 32 fresh CSPRNG bytes to `path`. Random bytes go to
/// `<path>.tmp` with mode 0600 first, then rename in place. Creates the
/// parent directory if missing.
fn generate_key_file(path: &PathBuf) -> Result<(), KeyStorageError> {
    let parent = path.parent().ok_or_else(|| {
        KeyStorageError::Io(io::Error::new(io::ErrorKind::Other, "no parent dir"))
    })?;
    std::fs::create_dir_all(parent)?;

    let mut key = [0u8; KEY_LEN];
    getrandom::fill(&mut key).map_err(|e| {
        KeyStorageError::Io(io::Error::new(io::ErrorKind::Other, format!("getrandom: {e}")))
    })?;

    let tmp = path.with_extension("key.tmp");
    write_key_bytes(&tmp, &key)?;
    std::fs::rename(&tmp, path)?;
    fsync_parent_dir(path)?;
    tracing::info!(
        target: "tentaflow::key_storage",
        path = %path.display(),
        "generated new persistent key — back this file up!"
    );
    Ok(())
}

/// Persist `bytes` to `path` with mode 0600 on Unix. The mode is enforced
/// twice: at `open()` via `OpenOptions::mode` (covers the create case) and
/// again via `set_permissions()` after the write (covers the case where a
/// pre-existing file at the same path was created with a wider mode — e.g.
/// a stale `.new` left by an interrupted rotation — since `O_TRUNC` keeps
/// the existing inode's mode bits).
pub fn write_key_bytes(path: &PathBuf, bytes: &[u8; KEY_LEN]) -> Result<(), KeyStorageError> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    Ok(())
}

/// fsync the directory containing `path` so a subsequent `rename()` is
/// durable across crashes. No-op on non-Unix.
pub fn fsync_parent_dir(path: &PathBuf) -> Result<(), KeyStorageError> {
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        {
            let dir = std::fs::File::open(parent)?;
            dir.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let _ = parent;
        }
    }
    Ok(())
}

/// Promote any `.key.new` next to the live path (rotation committed but
/// crashed before final swap), and delete any stale `.key.staging` /
/// `.key.tmp` that an aborted run left behind.
fn recover_interrupted_rotation(path: &PathBuf) -> Result<(), KeyStorageError> {
    let new_path = path.with_extension("key.new");
    let staging_path = path.with_extension("key.staging");
    let tmp_path = path.with_extension("key.tmp");
    if new_path.exists() {
        tracing::warn!(
            target: "tentaflow::key_storage",
            new = %new_path.display(),
            live = %path.display(),
            "detected interrupted rotation; promoting .new key to live"
        );
        std::fs::rename(&new_path, path)?;
        let _ = fsync_parent_dir(path);
    }
    if staging_path.exists() {
        tracing::warn!(
            target: "tentaflow::key_storage",
            staging = %staging_path.display(),
            "discarding stale .staging key (rotation never committed)"
        );
        let _ = std::fs::remove_file(&staging_path);
    }
    if tmp_path.exists() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_or_generate_creates_key_file() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("pickup_token.key");
        let k = PersistentKey::load_or_generate_at("pickup_token", &path).unwrap();
        assert_eq!(k.bytes().len(), KEY_LEN);
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_load_or_generate_creates_key_file_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let td = TempDir::new().unwrap();
        let path = td.path().join("frame_url.key");
        let _ = PersistentKey::load_or_generate_at("frame_url", &path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must be mode 0600, got {mode:o}");
    }

    #[test]
    fn test_load_or_generate_idempotent_returns_same_key() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("recording_url.key");
        let k1 = PersistentKey::load_or_generate_at("recording_url", &path).unwrap();
        let k2 = PersistentKey::load_or_generate_at("recording_url", &path).unwrap();
        assert_eq!(k1.bytes(), k2.bytes());
    }

    #[test]
    fn test_load_or_generate_recovers_interrupted_rotation() {
        // Simulate: rotation crashed AFTER renaming staging -> .new but
        // BEFORE swapping .new over the live file. The .new bytes are the
        // authoritative new key; load_or_generate must promote them.
        let td = TempDir::new().unwrap();
        let live = td.path().join("pickup_token.key");
        let new_path = td.path().join("pickup_token.key.new");

        // Pre-existing (stale) live key.
        write_key_bytes(&live, &[0xAAu8; KEY_LEN]).unwrap();
        // Authoritative new key sitting in `.new`.
        let new_bytes = [0x5Au8; KEY_LEN];
        write_key_bytes(&new_path, &new_bytes).unwrap();

        let k = PersistentKey::load_or_generate_at("pickup_token", &live).unwrap();
        assert_eq!(
            k.bytes(),
            &new_bytes,
            ".new must be promoted as the live key on recovery"
        );
        assert!(!new_path.exists(), ".new must be consumed by recovery");
    }

    #[test]
    fn test_key_file_too_short_rejected() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("pickup_token.key");
        // Write a too-short file: load must reject with InvalidKeyLength.
        std::fs::write(&path, &[1, 2, 3]).unwrap();
        let err = PersistentKey::load_or_generate_at("pickup_token", &path).unwrap_err();
        assert!(matches!(
            err,
            KeyStorageError::InvalidKeyLength { got: 3, .. }
        ));
    }

    #[test]
    fn invalid_name_rejected() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("x.key");
        let err = PersistentKey::load_or_generate_at("Bad-Name!", &path).unwrap_err();
        assert!(matches!(err, KeyStorageError::InvalidKeyName(_)));
    }

    #[test]
    fn staging_leftover_discarded() {
        let td = TempDir::new().unwrap();
        let live = td.path().join("frame_url.key");
        let staging = td.path().join("frame_url.key.staging");
        write_key_bytes(&live, &[0x11u8; KEY_LEN]).unwrap();
        write_key_bytes(&staging, &[0x22u8; KEY_LEN]).unwrap();
        let k = PersistentKey::load_or_generate_at("frame_url", &live).unwrap();
        assert_eq!(k.bytes(), &[0x11u8; KEY_LEN], "live key wins over staging");
        assert!(!staging.exists(), "staging must be cleaned up");
    }
}
