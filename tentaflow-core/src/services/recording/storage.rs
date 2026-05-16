// =============================================================================
// File: services/recording/storage.rs — filesystem layout + hashing helpers
// =============================================================================
//
// All recordings live under `<HOME>/.tentaflow/recordings/<camera_id>/<kind>/`
// where `<kind>` is `snapshots` or `segments`. Files are named
// `<recording_ref>.<ext>` and written atomically (tmp + rename).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::error::{RecordingError, Result};

/// Kind of a recording — drives subdirectory + file extension selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingKind {
    Snapshot,
    Segment,
}

impl RecordingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Segment => "segment",
        }
    }

    pub fn subdir(&self) -> &'static str {
        match self {
            Self::Snapshot => "snapshots",
            Self::Segment => "segments",
        }
    }
}

/// Opaque reference into the recordings table — `snap_<uuid>` or `clip_<uuid>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingRef(pub String);

impl RecordingRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<RecordingRef> for String {
    fn from(r: RecordingRef) -> String {
        r.0
    }
}

/// Resolve `~/.tentaflow/recordings`. Fails if `HOME` cannot be located.
pub fn recording_base_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| RecordingError::BaseDirUnavailable("HOME not set".into()))?;
    Ok(home.join(".tentaflow").join("recordings"))
}

/// `<base>/<camera_id>/<snapshots|segments>` — caller must `create_dir_all`.
pub fn camera_subdir(base: &Path, camera_id: &str, kind: RecordingKind) -> PathBuf {
    base.join(camera_id).join(kind.subdir())
}

/// Lowercase hex SHA-256 of the supplied bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Camera ids accepted by the recording API. F1a stays permissive: non-empty,
/// up to 64 chars, characters limited to alphanumerics + `_` + `-` so the id
/// is always a safe path component.
pub fn validate_camera_id(camera_id: &str) -> Result<()> {
    if camera_id.is_empty() || camera_id.len() > 64 {
        return Err(RecordingError::InvalidCameraId);
    }
    if !camera_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(RecordingError::InvalidCameraId);
    }
    Ok(())
}

/// Atomic write: stream bytes to `<file_path>.tmp` then rename to `file_path`.
pub async fn atomic_write(file_path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = tmp_path_for(file_path);
    tokio::fs::write(&tmp, bytes).await?;
    if let Err(e) = tokio::fs::rename(&tmp, file_path).await {
        // Best-effort cleanup; rename failures leave the tmp behind otherwise.
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(RecordingError::Io(e));
    }
    Ok(())
}

fn tmp_path_for(file_path: &Path) -> PathBuf {
    let mut s = file_path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

pub async fn read_recording(file_path: &Path) -> Result<Vec<u8>> {
    Ok(tokio::fs::read(file_path).await?)
}

/// Idempotent removal — missing file is not an error.
pub async fn purge_recording(file_path: &Path) -> Result<()> {
    match tokio::fs::remove_file(file_path).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(RecordingError::Io(e)),
    }
}

/// Test-only HOME mutation helper. `cargo test` runs in parallel; every test
/// that mutates `HOME` to sandbox `recording_base_dir()` must hold this lock
/// for the duration of its assertions, otherwise a peer test's `TempDir` drop
/// can yank the directory out from under the file system call.
#[cfg(test)]
pub(super) fn home_sandbox_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let lock = LOCK.get_or_init(|| std::sync::Mutex::new(()));
    lock.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_base_dir_uses_home() {
        let p = recording_base_dir().expect("home");
        assert!(p.ends_with(".tentaflow/recordings"));
    }

    #[test]
    fn test_camera_subdir_layout() {
        let base = PathBuf::from("/x");
        assert_eq!(
            camera_subdir(&base, "cam_1", RecordingKind::Snapshot),
            PathBuf::from("/x/cam_1/snapshots")
        );
        assert_eq!(
            camera_subdir(&base, "cam_1", RecordingKind::Segment),
            PathBuf::from("/x/cam_1/segments")
        );
    }

    #[test]
    fn test_validate_camera_id() {
        assert!(validate_camera_id("cam_abc-1").is_ok());
        assert!(validate_camera_id("").is_err());
        assert!(validate_camera_id("bad id").is_err());
        assert!(validate_camera_id("../etc").is_err());
        let big = "a".repeat(65);
        assert!(validate_camera_id(&big).is_err());
    }

    #[tokio::test]
    async fn test_purge_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.bin");
        purge_recording(&p).await.expect("idempotent");
        tokio::fs::write(&p, b"x").await.unwrap();
        purge_recording(&p).await.expect("removes existing");
        assert!(!p.exists());
    }

    #[tokio::test]
    async fn test_atomic_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        atomic_write(&p, b"hello").await.unwrap();
        let got = tokio::fs::read(&p).await.unwrap();
        assert_eq!(got, b"hello");
        // tmp file is gone after successful rename
        let tmp = tmp_path_for(&p);
        assert!(!tmp.exists());
    }

    #[test]
    fn test_sha256_hex_known() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
