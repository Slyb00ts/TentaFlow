// =============================================================================
// File: tentaflow-cli/src/commands/camera.rs — Camera-related CLI subcommands.
// Currently exposes `rotate-key`, which atomically re-encrypts every camera
// credentials blob under a freshly generated AES-GCM master key. The previous
// key is archived next to the live file so an operator can recover from a
// botched rotation if needed.
// =============================================================================

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;

use tentaflow_core::db::repository::{
    list_all_camera_credentials_blobs, replace_camera_credentials_blobs,
};
use tentaflow_core::paths;
use tentaflow_core::services::camera_ingest::credentials::{
    default_key_path, write_key_bytes, CredentialsCipher,
};

#[derive(Subcommand, Debug)]
pub enum CameraCommand {
    /// Rotate the master key in `<tentaflow_home>/keys/cameras.key`. The
    /// previous key is archived alongside the live file with a UTC
    /// timestamp suffix; every camera row that carried an encrypted
    /// credentials blob is re-encrypted under the new key inside a single
    /// SQL transaction.
    RotateKey {
        /// Explicit path to the live `cameras.key` (defaults to
        /// `<tentaflow_home>/keys/cameras.key`, or the value of
        /// `TENTAFLOW_CAMERAS_KEY` when set).
        #[arg(long)]
        key_path: Option<PathBuf>,
        /// Explicit path to the sqlite database (defaults to
        /// `<tentaflow_home>/data/router.db`).
        #[arg(long)]
        db_path: Option<PathBuf>,
    },
}

pub fn run(cmd: CameraCommand) -> ExitCode {
    match cmd {
        CameraCommand::RotateKey { key_path, db_path } => match rotate_key(key_path, db_path) {
            Ok(report) => {
                println!("{}", report);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("rotate-key failed: {e:#}");
                ExitCode::from(1)
            }
        },
    }
}

/// Drives the full rotate-key procedure. Returns a human-readable report
/// describing how many rows were re-encrypted and where the previous key was
/// archived. Errors short-circuit before any persistent change is made.
fn rotate_key(
    key_path: Option<PathBuf>,
    db_path: Option<PathBuf>,
) -> anyhow::Result<String> {
    let key_path = key_path.unwrap_or_else(default_key_path);
    let db_path = db_path.unwrap_or_else(paths::database_path);

    if !key_path.exists() {
        anyhow::bail!(
            "live cameras.key not found at {} — nothing to rotate (run the host once to generate)",
            key_path.display()
        );
    }

    // Step 1 — load old key.
    let old_cipher = CredentialsCipher::from_key_file(&key_path)
        .map_err(|e| anyhow::anyhow!("load current key: {e}"))?;

    // Step 2 — generate new 32-byte key. We do NOT write it to the live
    // path yet; that happens atomically once the DB transaction commits.
    let mut new_key = [0u8; 32];
    use rand::Rng;
    rand::rng().fill_bytes(&mut new_key);
    let new_cipher = CredentialsCipher::from_raw_key(new_key);

    // Step 3 — open DB and walk every row carrying an encrypted blob.
    let pool = tentaflow_core::db::init(&db_path)
        .map_err(|e| anyhow::anyhow!("open db {}: {e}", db_path.display()))?;
    let rows = list_all_camera_credentials_blobs(&pool)
        .map_err(|e| anyhow::anyhow!("list credentials: {e}"))?;

    let mut re_encrypted: Vec<(i64, Vec<u8>)> = Vec::with_capacity(rows.len());
    for (id, blob) in &rows {
        let plain = old_cipher
            .decrypt_raw(blob)
            .map_err(|e| anyhow::anyhow!("decrypt row id={id}: {e}"))?;
        let new_blob = new_cipher
            .encrypt_raw(&plain)
            .map_err(|e| anyhow::anyhow!("encrypt row id={id}: {e}"))?;
        re_encrypted.push((*id, new_blob));
    }

    // Step 4 — commit DB transaction first; if it fails we still have the
    // old key on disk and the DB untouched.
    let n = if re_encrypted.is_empty() {
        0
    } else {
        replace_camera_credentials_blobs(&pool, &re_encrypted)
            .map_err(|e| anyhow::anyhow!("bulk update: {e}"))?
    };

    // Step 5 — archive old key BEFORE overwriting (so a failure here still
    // leaves the operator with the live old key + matching DB blobs).
    let archive_path = archive_path_for(&key_path);
    std::fs::copy(&key_path, &archive_path).map_err(|e| {
        anyhow::anyhow!("archive old key to {}: {e}", archive_path.display())
    })?;

    // Step 6 — atomically swap in the new key (tmp + rename).
    let tmp = key_path.with_extension("key.new");
    write_key_bytes(&tmp, &new_key).map_err(|e| anyhow::anyhow!("write new key: {e}"))?;
    std::fs::rename(&tmp, &key_path)
        .map_err(|e| anyhow::anyhow!("rename new key into place: {e}"))?;

    Ok(format!(
        "rotated: {n} camera credential blobs re-encrypted\n\
         new key:   {}\n\
         archived:  {}",
        key_path.display(),
        archive_path.display()
    ))
}

/// Build the archive filename for a soon-to-be-rotated master key:
/// `cameras.key.YYYYMMDD-HHMMSS` next to the live file. Using UTC makes the
/// suffix monotonic across timezones so a `ls` listing is sorted by age.
fn archive_path_for(live: &PathBuf) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let mut name = live
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("cameras.key"));
    name.push(".");
    name.push(stamp);
    live.with_file_name(name)
}
