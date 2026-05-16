// =============================================================================
// File: services/camera_ingest/credentials.rs — AES-GCM encrypt/decrypt for
// camera credentials (F1b P1.C).
// =============================================================================
//
// Master key: <tentaflow_home>/keys/cameras.key (32 raw bytes, mode 0600 on
// Unix). The path is overridable via the `TENTAFLOW_CAMERAS_KEY` env var so
// tests and CI can pin a tempfile without touching the user's HOME.
//
// Encrypted blob format: [nonce(12)][ciphertext + 16-byte tag].
// Decrypted plaintext: UTF-8 string "user:pass" (RTSP user-info portion). The
// caller (RTSP connector) overlays this string into the camera URL right
// before handing it to GStreamer; the URL persisted in `cameras.url` never
// carries credentials.
//
// Rotation: the CLI command `tentaflow-cli camera rotate-key` generates a new
// master key, re-encrypts every `cameras.credentials_encrypted` row inside a
// single SQL transaction, then atomically replaces `cameras.key` with the new
// one (old key archived as `cameras.key.<UTC-stamp>`).

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::Rng;
use thiserror::Error;

/// Override for the master-key file. When set, takes precedence over
/// `<tentaflow_home>/keys/cameras.key`.
pub const KEY_PATH_ENV: &str = "TENTAFLOW_CAMERAS_KEY";

/// Subdirectory under `tentaflow_home()` that holds AES-GCM master keys.
const KEY_SUBDIR: &str = "keys";

/// File name of the camera-credentials master key.
const KEY_FILE: &str = "cameras.key";

/// AES-GCM nonce size (12 bytes — fixed by the algorithm; do not change).
pub const NONCE_LEN: usize = 12;

/// AES-GCM authentication tag size (16 bytes — fixed; do not change).
pub const TAG_LEN: usize = 16;

/// Hard cap on credentials plaintext length. RTSP user-info `user:pass`
/// realistically tops out around 192 bytes; we allow 256 to leave headroom
/// for unusual setups but reject anything larger to prevent storage abuse.
pub const MAX_PLAINTEXT_LEN: usize = 256;

#[derive(Debug, Error)]
pub enum CredentialsError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid key file (expected 32 bytes, got {0})")]
    InvalidKeyLength(usize),
    #[error("encryption failed")]
    EncryptFailed,
    #[error("decryption failed (wrong key or tampered ciphertext)")]
    DecryptFailed,
    #[error("invalid blob (must be at least {min} bytes, got {got})", min = NONCE_LEN + TAG_LEN)]
    InvalidBlob { got: usize },
    #[error("plaintext is not valid UTF-8")]
    InvalidPlaintext,
    #[error("plaintext too long ({0} bytes, max {MAX_PLAINTEXT_LEN})")]
    PlaintextTooLong(usize),
    #[error("plaintext must contain ':' separating user and password")]
    MissingColonSeparator,
}

/// Holds a fully-initialized AES-256-GCM cipher derived from the master key.
pub struct CredentialsCipher {
    cipher: Aes256Gcm,
    /// Raw key bytes kept alongside the cipher so rotation tooling can XOR
    /// or compare without re-reading the file.
    key_bytes: [u8; 32],
}

impl CredentialsCipher {
    /// Load (or generate) the cipher from the standard `cameras.key` path.
    /// First call on a fresh install generates a 32-byte CSPRNG key and
    /// writes it with mode 0600.
    pub fn load_or_generate() -> Result<Self, CredentialsError> {
        let path = default_key_path();
        Self::load_or_generate_at(&path)
    }

    /// Same as [`load_or_generate`] but with an explicit file path. Used by
    /// the rotate-key CLI to load the *previous* key from an archived path
    /// without touching the live one.
    pub fn load_or_generate_at(path: &PathBuf) -> Result<Self, CredentialsError> {
        if !path.exists() {
            generate_key_file(path)?;
        }
        Self::from_key_file(path)
    }

    /// Load an existing key file. Errors if the file is missing or has the
    /// wrong length.
    pub fn from_key_file(path: &PathBuf) -> Result<Self, CredentialsError> {
        let bytes = std::fs::read(path)?;
        if bytes.len() != 32 {
            return Err(CredentialsError::InvalidKeyLength(bytes.len()));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Ok(Self::from_raw_key(key))
    }

    /// Construct directly from raw bytes — used by rotate-key after writing
    /// the new file and by tests.
    pub fn from_raw_key(key: [u8; 32]) -> Self {
        let cipher = Aes256Gcm::new(&key.into());
        Self {
            cipher,
            key_bytes: key,
        }
    }

    /// Encrypt UTF-8 plaintext into [nonce(12) || ciphertext+tag(>=16)].
    /// The plaintext is validated against [`MAX_PLAINTEXT_LEN`] and the
    /// `user:pass` shape so a caller cannot smuggle empty / malformed
    /// strings into the store.
    pub fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>, CredentialsError> {
        if plaintext.len() > MAX_PLAINTEXT_LEN {
            return Err(CredentialsError::PlaintextTooLong(plaintext.len()));
        }
        if !plaintext.contains(':') {
            return Err(CredentialsError::MissingColonSeparator);
        }
        self.encrypt_raw(plaintext.as_bytes())
    }

    /// Encrypt raw bytes without the `user:pass` shape check. Reserved for
    /// rotation tooling that re-encrypts an already-validated plaintext.
    pub fn encrypt_raw(&self, plaintext: &[u8]) -> Result<Vec<u8>, CredentialsError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| CredentialsError::EncryptFailed)?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a blob produced by [`encrypt`] or [`encrypt_raw`]. Returns
    /// the plaintext as a UTF-8 string; binary plaintext callers (rotation
    /// tooling) should use [`decrypt_raw`].
    pub fn decrypt(&self, blob: &[u8]) -> Result<String, CredentialsError> {
        let bytes = self.decrypt_raw(blob)?;
        String::from_utf8(bytes).map_err(|_| CredentialsError::InvalidPlaintext)
    }

    /// Decrypt to raw bytes without the UTF-8 check. Reserved for rotation
    /// tooling that re-encrypts an opaque payload.
    pub fn decrypt_raw(&self, blob: &[u8]) -> Result<Vec<u8>, CredentialsError> {
        if blob.len() < NONCE_LEN + TAG_LEN {
            return Err(CredentialsError::InvalidBlob { got: blob.len() });
        }
        let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ct)
            .map_err(|_| CredentialsError::DecryptFailed)
    }

    /// Raw key bytes — exposed only for the rotate-key CLI (archive +
    /// migrate). Never logged. Returned by reference to discourage copying.
    pub fn key_bytes(&self) -> &[u8; 32] {
        &self.key_bytes
    }
}

/// Default on-disk location of the master key file. Honours the
/// `TENTAFLOW_CAMERAS_KEY` env override; otherwise resolves to
/// `<tentaflow_home>/keys/cameras.key`.
pub fn default_key_path() -> PathBuf {
    if let Ok(p) = std::env::var(KEY_PATH_ENV) {
        return PathBuf::from(p);
    }
    crate::paths::tentaflow_home()
        .join(KEY_SUBDIR)
        .join(KEY_FILE)
}

/// Write a fresh random 32-byte key to `path` atomically: random bytes go
/// to `<path>.tmp` with mode 0600 first, then rename in place. Creates the
/// parent directory if missing.
fn generate_key_file(path: &PathBuf) -> Result<(), CredentialsError> {
    let parent = path
        .parent()
        .ok_or_else(|| CredentialsError::Io(io::Error::new(io::ErrorKind::Other, "no parent dir")))?;
    std::fs::create_dir_all(parent)?;

    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);

    let tmp = path.with_extension("key.tmp");
    write_key_bytes(&tmp, &key)?;
    std::fs::rename(&tmp, path)?;
    tracing::info!(
        target: "tentaflow::credentials",
        path = %path.display(),
        "generated new cameras master key — back this file up!"
    );
    Ok(())
}

/// Persist `bytes` to `path` with mode 0600 on Unix. On non-Unix the file
/// inherits the default umask (Windows enforcement happens via the parent
/// directory's ACL, which is out of scope for F1b).
pub fn write_key_bytes(path: &PathBuf, bytes: &[u8; 32]) -> Result<(), CredentialsError> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    Ok(())
}

/// Process-wide cipher singleton. Initialized lazily on first call; panics
/// only when the master-key file is unreadable AND cannot be created — at
/// which point camera credentials cannot work at all.
static CIPHER: OnceLock<Arc<CredentialsCipher>> = OnceLock::new();

/// Returns the process-wide cipher, initialising it on first access.
/// Subsequent calls return the cached instance.
pub fn credentials_cipher() -> &'static Arc<CredentialsCipher> {
    CIPHER.get_or_init(|| {
        Arc::new(
            CredentialsCipher::load_or_generate()
                .expect("cameras.key load_or_generate must succeed at first use"),
        )
    })
}

/// Overlay `user:pass` into an RTSP URL that lacks credentials. Returns the
/// new URL. Errors if the URL already carries credentials (we refuse to
/// silently overwrite operator-provided credentials with stored ones) or if
/// the scheme is not `rtsp://` / `rtsps://`.
pub fn overlay_credentials(url: &str, creds: &str) -> Result<String, &'static str> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("rtsp://") {
        ("rtsp", r)
    } else if let Some(r) = url.strip_prefix("rtsps://") {
        ("rtsps", r)
    } else {
        return Err("unsupported scheme for credentials overlay");
    };
    // `@` may legitimately appear inside the path component, so only treat
    // it as a credentials separator if it precedes the first `/`.
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host_part = &rest[..host_end];
    if host_part.contains('@') {
        return Err("url already carries credentials; refusing to overlay");
    }
    if !creds.contains(':') {
        return Err("creds must be user:pass");
    }
    Ok(format!("{scheme}://{creds}@{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_cipher(td: &TempDir) -> CredentialsCipher {
        let path = td.path().join("cameras.key");
        CredentialsCipher::load_or_generate_at(&path).expect("fresh cipher")
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let td = TempDir::new().unwrap();
        let c = fresh_cipher(&td);
        let pt = "user:s3cr3t";
        let blob = c.encrypt(pt).unwrap();
        // Nonce is at the front; blob must be longer than nonce + tag.
        assert!(blob.len() > NONCE_LEN + TAG_LEN);
        let got = c.decrypt(&blob).unwrap();
        assert_eq!(got, pt);
    }

    #[test]
    fn decrypt_wrong_key_fails() {
        let td1 = TempDir::new().unwrap();
        let td2 = TempDir::new().unwrap();
        let c1 = fresh_cipher(&td1);
        let c2 = fresh_cipher(&td2);
        let blob = c1.encrypt("admin:hunter2").unwrap();
        // Two independently-generated keys must not cross-decrypt.
        let err = c2.decrypt(&blob).unwrap_err();
        assert!(matches!(err, CredentialsError::DecryptFailed));
    }

    #[test]
    fn decrypt_tampered_ciphertext_fails() {
        let td = TempDir::new().unwrap();
        let c = fresh_cipher(&td);
        let mut blob = c.encrypt("u:p").unwrap();
        // Flip a byte well inside the ciphertext body — AES-GCM auth tag
        // must catch this.
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        let err = c.decrypt(&blob).unwrap_err();
        assert!(matches!(err, CredentialsError::DecryptFailed));
    }

    #[test]
    fn invalid_blob_too_short_rejected() {
        let td = TempDir::new().unwrap();
        let c = fresh_cipher(&td);
        let err = c.decrypt(&[]).unwrap_err();
        assert!(matches!(err, CredentialsError::InvalidBlob { got: 0 }));
        let err = c.decrypt(&[0u8; NONCE_LEN + TAG_LEN - 1]).unwrap_err();
        assert!(matches!(err, CredentialsError::InvalidBlob { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn key_file_generation_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let td = TempDir::new().unwrap();
        let path = td.path().join("cameras.key");
        let _ = CredentialsCipher::load_or_generate_at(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "cameras.key must be mode 0600, got {mode:o}");
    }

    #[test]
    fn load_existing_key_is_idempotent() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("cameras.key");
        let c1 = CredentialsCipher::load_or_generate_at(&path).unwrap();
        let blob = c1.encrypt("u:p").unwrap();
        // Drop and reload — second load must read the existing file so the
        // blob produced by c1 decrypts under c2.
        drop(c1);
        let c2 = CredentialsCipher::load_or_generate_at(&path).unwrap();
        let got = c2.decrypt(&blob).unwrap();
        assert_eq!(got, "u:p");
    }

    #[test]
    fn encrypt_rejects_oversize_plaintext() {
        let td = TempDir::new().unwrap();
        let c = fresh_cipher(&td);
        let pt = "u:".to_string() + &"x".repeat(MAX_PLAINTEXT_LEN);
        let err = c.encrypt(&pt).unwrap_err();
        assert!(matches!(err, CredentialsError::PlaintextTooLong(_)));
    }

    #[test]
    fn encrypt_rejects_missing_colon() {
        let td = TempDir::new().unwrap();
        let c = fresh_cipher(&td);
        let err = c.encrypt("noseparator").unwrap_err();
        assert!(matches!(err, CredentialsError::MissingColonSeparator));
    }

    #[test]
    fn overlay_credentials_basic() {
        assert_eq!(
            overlay_credentials("rtsp://cam.local:554/stream", "alice:secret").unwrap(),
            "rtsp://alice:secret@cam.local:554/stream"
        );
        assert_eq!(
            overlay_credentials("rtsps://10.0.0.5/h264", "bob:p").unwrap(),
            "rtsps://bob:p@10.0.0.5/h264"
        );
    }

    #[test]
    fn overlay_refuses_when_url_has_credentials() {
        let err = overlay_credentials("rtsp://u:p@cam/s", "a:b").unwrap_err();
        assert_eq!(err, "url already carries credentials; refusing to overlay");
    }

    #[test]
    fn overlay_refuses_non_rtsp_scheme() {
        let err = overlay_credentials("http://cam/s", "a:b").unwrap_err();
        assert_eq!(err, "unsupported scheme for credentials overlay");
    }

    #[test]
    fn overlay_refuses_creds_without_colon() {
        let err = overlay_credentials("rtsp://cam/s", "userpass").unwrap_err();
        assert_eq!(err, "creds must be user:pass");
    }

    #[test]
    fn overlay_ignores_at_in_path() {
        // `@` after the first `/` is part of the path, not credentials —
        // overlay must still succeed.
        let out = overlay_credentials("rtsp://cam/stream@channel1", "a:b").unwrap();
        assert_eq!(out, "rtsp://a:b@cam/stream@channel1");
    }
}
