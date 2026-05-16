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
    // Enforce mode 0600 on every load. A key file checked into git, copied
    // from a backup, or restored by an operator may carry a wider mode; we
    // narrow it here so a pre-existing 0644 key never stays world-readable
    // past the first start that touches it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                if mode != 0o600 {
                    tracing::warn!(
                        target: "tentaflow::key_storage",
                        path = %path.display(),
                        mode = format!("{:o}", mode),
                        "key file mode is not 0600 — forcing 0600"
                    );
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                }
            }
            Err(e) => return Err(KeyStorageError::Io(e)),
        }
    }
    let mut k = [0u8; KEY_LEN];
    k.copy_from_slice(&raw);
    Ok(k)
}

/// Public, read-only access to the persistent key bytes. The watcher uses
/// this to reload a rotated key off disk without going through the
/// generate-on-missing path of `load_or_generate_at` (which would mask the
/// "file vanished mid-rotation" failure mode).
pub fn read_persistent_key(path: &PathBuf) -> Result<[u8; KEY_LEN], KeyStorageError> {
    read_key_file(path)
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

/// Recovery for an interrupted rotation. The decision tree:
///
/// 1. Always discard `.staging` (rotation never committed) and `.tmp`
///    (first-generate aborted).
/// 2. If no `.new` exists, nothing to recover.
/// 3. If `.new` is present but NOT exactly 32 bytes (truncated, corrupt,
///    oversized) — discard it. We never promote a malformed key over a
///    valid live file.
/// 4. If both live and `.new` are present and both are valid 32-byte
///    files — keep live, leave `.new` for the operator/CLI to retry.
///    Live is the authoritative key that running issuers signed with;
///    silently promoting `.new` would invalidate every signature minted
///    before the crash without an explicit operator action.
/// 5. If live is missing or invalid AND `.new` is valid — promote `.new`
///    to live and force mode 0600.
fn recover_interrupted_rotation(path: &PathBuf) -> Result<(), KeyStorageError> {
    let new_path = path.with_extension("key.new");
    let staging_path = path.with_extension("key.staging");
    let tmp_path = path.with_extension("key.tmp");

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

    if !new_path.exists() {
        return Ok(());
    }

    // Validate .new BEFORE any decision to promote.
    let new_bytes = std::fs::read(&new_path)?;
    if new_bytes.len() != KEY_LEN {
        tracing::warn!(
            target: "tentaflow::key_storage",
            new = %new_path.display(),
            got = new_bytes.len(),
            "interrupted rotation: .new is not 32 bytes — discarding, live key untouched"
        );
        let _ = std::fs::remove_file(&new_path);
        return Ok(());
    }

    let live_valid = matches!(std::fs::metadata(path), Ok(_))
        && std::fs::read(path).map(|b| b.len() == KEY_LEN).unwrap_or(false);

    if live_valid {
        // Both valid: live wins. The CLI rotate can be retried by the
        // operator; promoting .new here would silently invalidate every
        // outstanding signature without an in-memory previous-key window.
        tracing::warn!(
            target: "tentaflow::key_storage",
            new = %new_path.display(),
            live = %path.display(),
            "interrupted rotation: both live and .new are valid — keeping live, leaving .new for operator retry"
        );
        return Ok(());
    }

    // Live missing/corrupt, .new validated: promote.
    tracing::warn!(
        target: "tentaflow::key_storage",
        new = %new_path.display(),
        live = %path.display(),
        "detected interrupted rotation with missing/corrupt live; promoting .new key"
    );
    std::fs::rename(&new_path, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let _ = fsync_parent_dir(path);
    Ok(())
}

/// Background file-watcher: polls a key file's mtime and invokes a callback
/// whenever the bytes on disk change. Used by the per-process issuer
/// singletons (pickup_token, frame_url, recording_url) so that
/// `tentaflow-cli keys rotate <name>` takes effect inside a running host
/// without a restart — the previous-key window then engages exactly as the
/// design intends.
///
/// Polling (every `poll_interval`, default 5 s) is preferred over SIGHUP
/// because it works uniformly on Linux/macOS/Windows, survives the case
/// where the CLI runs as a different uid than the host, and has no startup
/// race against signal handler installation. The CPU cost is one stat()
/// per file per 5 s — negligible.
pub mod watcher {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::{read_persistent_key, KEY_LEN};

    /// Spawn a watcher task on the current tokio runtime. The task lives
    /// for the lifetime of the process: it has no shutdown hook because
    /// the issuer singletons it serves are themselves `OnceLock`-static.
    /// Any panic inside the callback is caught and logged so a single bad
    /// rotate cannot kill the watcher loop.
    pub fn spawn_key_watcher<F>(
        name: &'static str,
        path: PathBuf,
        poll_interval: Duration,
        on_change: F,
    ) where
        F: Fn(&[u8; KEY_LEN], &[u8; KEY_LEN]) + Send + Sync + 'static,
    {
        if tokio::runtime::Handle::try_current().is_err() {
            // No runtime — typical for unit tests that build an issuer
            // without an async context. The host always runs under tokio.
            return;
        }
        tokio::spawn(async move {
            // Seed last_bytes from the current on-disk state so the very
            // first detected change is a real rotation, not the startup
            // load.
            let mut last_bytes: Option<[u8; KEY_LEN]> = read_persistent_key(&path).ok();
            let mut last_mtime: Option<SystemTime> = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok());

            loop {
                tokio::time::sleep(poll_interval).await;

                let meta = match std::fs::metadata(&path) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            target: "tentaflow::key_storage::watcher",
                            name = %name,
                            path = %path.display(),
                            error = %e,
                            "key file inaccessible — will retry"
                        );
                        continue;
                    }
                };

                let mtime = meta.modified().ok();
                if mtime == last_mtime {
                    continue;
                }

                match read_persistent_key(&path) {
                    Ok(new_bytes) => {
                        if let Some(ref old) = last_bytes {
                            if *old == new_bytes {
                                // mtime moved but bytes are identical
                                // (e.g. `touch`); skip the callback.
                                last_mtime = mtime;
                                continue;
                            }
                            // Catch a panicking callback so the watcher
                            // loop survives a bad rotate.
                            let old_copy = *old;
                            let result = std::panic::catch_unwind(
                                std::panic::AssertUnwindSafe(|| {
                                    on_change(&old_copy, &new_bytes);
                                }),
                            );
                            if let Err(e) = result {
                                tracing::error!(
                                    target: "tentaflow::key_storage::watcher",
                                    name = %name,
                                    "on_change callback panicked: {:?}",
                                    e
                                );
                            } else {
                                tracing::info!(
                                    target: "tentaflow::key_storage::watcher",
                                    name = %name,
                                    "key rotated on disk — reloaded into running issuer"
                                );
                            }
                        }
                        last_bytes = Some(new_bytes);
                        last_mtime = mtime;
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "tentaflow::key_storage::watcher",
                            name = %name,
                            path = %path.display(),
                            error = %e,
                            "key file changed but reload failed; keeping previous key in memory"
                        );
                        // Do NOT advance last_mtime: we want to retry on
                        // the next tick once the writer finishes.
                    }
                }
            }
        });
    }
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
    fn test_recover_keeps_live_when_both_valid() {
        // Rotation crashed AFTER `.new` was committed but BEFORE the swap
        // over the live file. Both are valid 32-byte files. Recovery must
        // keep the live key (running issuers signed with it; promoting
        // `.new` blindly would invalidate every outstanding signature).
        // The operator retries the CLI rotate.
        let td = TempDir::new().unwrap();
        let live = td.path().join("pickup_token.key");
        let new_path = td.path().join("pickup_token.key.new");

        write_key_bytes(&live, &[0xAAu8; KEY_LEN]).unwrap();
        write_key_bytes(&new_path, &[0x5Au8; KEY_LEN]).unwrap();

        let k = PersistentKey::load_or_generate_at("pickup_token", &live).unwrap();
        assert_eq!(
            k.bytes(),
            &[0xAAu8; KEY_LEN],
            "live must win when both live and .new are valid"
        );
        assert!(new_path.exists(), ".new must be left in place for operator retry");
    }

    #[test]
    fn test_recover_invalid_new_keeps_live() {
        // .new is corrupt (3 bytes). Recovery must discard .new and leave
        // the live key untouched.
        let td = TempDir::new().unwrap();
        let live = td.path().join("pickup_token.key");
        let new_path = td.path().join("pickup_token.key.new");

        write_key_bytes(&live, &[0xAAu8; KEY_LEN]).unwrap();
        std::fs::write(&new_path, &[1u8, 2, 3]).unwrap();

        let k = PersistentKey::load_or_generate_at("pickup_token", &live).unwrap();
        assert_eq!(k.bytes(), &[0xAAu8; KEY_LEN], "live untouched");
        assert!(!new_path.exists(), "corrupt .new must be discarded");
    }

    #[test]
    fn test_recover_oversized_new_keeps_live() {
        // .new oversized (64 bytes) — same as too-short: discard, keep live.
        let td = TempDir::new().unwrap();
        let live = td.path().join("pickup_token.key");
        let new_path = td.path().join("pickup_token.key.new");

        write_key_bytes(&live, &[0xAAu8; KEY_LEN]).unwrap();
        std::fs::write(&new_path, &[0x77u8; 64]).unwrap();

        let k = PersistentKey::load_or_generate_at("pickup_token", &live).unwrap();
        assert_eq!(k.bytes(), &[0xAAu8; KEY_LEN]);
        assert!(!new_path.exists());
    }

    #[test]
    fn test_recover_promotes_new_only_when_live_missing() {
        // Live is absent; .new is a valid 32-byte file. Promote.
        let td = TempDir::new().unwrap();
        let live = td.path().join("pickup_token.key");
        let new_path = td.path().join("pickup_token.key.new");

        let new_bytes = [0x5Au8; KEY_LEN];
        write_key_bytes(&new_path, &new_bytes).unwrap();

        let k = PersistentKey::load_or_generate_at("pickup_token", &live).unwrap();
        assert_eq!(k.bytes(), &new_bytes, ".new must be promoted when live is missing");
        assert!(!new_path.exists());
        assert!(live.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&live).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "promoted key must be 0600");
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_existing_key_file_chmod_forced_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let td = TempDir::new().unwrap();
        let path = td.path().join("pickup_token.key");
        // Write a valid 32-byte key with a world-readable mode.
        std::fs::write(&path, &[0x42u8; KEY_LEN]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Load — read_key_file must narrow the mode to 0600.
        let _ = PersistentKey::load_or_generate_at("pickup_token", &path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "load must force 0600, got {mode:o}");
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

    #[tokio::test]
    async fn test_pickup_token_watcher_reloads_after_disk_change() {
        use std::sync::Arc as StdArc;
        use std::sync::Mutex as StdMutex;

        let td = TempDir::new().unwrap();
        let path = td.path().join("pickup_token.key");
        write_key_bytes(&path, &[0x11u8; KEY_LEN]).unwrap();

        let captured: StdArc<StdMutex<Option<([u8; KEY_LEN], [u8; KEY_LEN])>>> =
            StdArc::new(StdMutex::new(None));
        let captured_cb = captured.clone();

        watcher::spawn_key_watcher(
            "pickup_token",
            path.clone(),
            std::time::Duration::from_millis(50),
            move |old, new| {
                *captured_cb.lock().unwrap() = Some((*old, *new));
            },
        );

        // Wait for the watcher to seed its initial state.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        // Rotate on disk.
        write_key_bytes(&path, &[0x22u8; KEY_LEN]).unwrap();

        // Wait for the watcher to detect the change.
        let mut detected: Option<([u8; KEY_LEN], [u8; KEY_LEN])> = None;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(c) = *captured.lock().unwrap() {
                detected = Some(c);
                break;
            }
        }
        let (old, new) = detected.expect("watcher must fire after on-disk rotation");
        assert_eq!(old, [0x11u8; KEY_LEN]);
        assert_eq!(new, [0x22u8; KEY_LEN]);
    }

    #[tokio::test]
    async fn test_signed_url_watcher_reloads_after_disk_change() {
        use std::sync::Arc as StdArc;
        use std::sync::Mutex as StdMutex;

        let td = TempDir::new().unwrap();
        let path = td.path().join("frame_url.key");
        write_key_bytes(&path, &[0x33u8; KEY_LEN]).unwrap();

        let last: StdArc<StdMutex<Option<[u8; KEY_LEN]>>> = StdArc::new(StdMutex::new(None));
        let last_cb = last.clone();

        watcher::spawn_key_watcher(
            "frame_url",
            path.clone(),
            std::time::Duration::from_millis(50),
            move |_old, new| {
                *last_cb.lock().unwrap() = Some(*new);
            },
        );

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        write_key_bytes(&path, &[0x44u8; KEY_LEN]).unwrap();

        let mut got = None;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(b) = *last.lock().unwrap() {
                got = Some(b);
                break;
            }
        }
        assert_eq!(got, Some([0x44u8; KEY_LEN]));
    }

    #[tokio::test]
    async fn test_watcher_ignores_touch_with_same_bytes() {
        // mtime moves but bytes don't change — callback must not fire.
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let td = TempDir::new().unwrap();
        let path = td.path().join("pickup_token.key");
        write_key_bytes(&path, &[0x55u8; KEY_LEN]).unwrap();

        let calls = StdArc::new(AtomicU32::new(0));
        let calls_cb = calls.clone();

        watcher::spawn_key_watcher(
            "pickup_token",
            path.clone(),
            std::time::Duration::from_millis(50),
            move |_old, _new| {
                calls_cb.fetch_add(1, Ordering::SeqCst);
            },
        );

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        // Rewrite identical bytes — mtime changes, content does not.
        write_key_bytes(&path, &[0x55u8; KEY_LEN]).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        assert_eq!(calls.load(Ordering::SeqCst), 0, "no-op touches must not invoke callback");
    }
}
