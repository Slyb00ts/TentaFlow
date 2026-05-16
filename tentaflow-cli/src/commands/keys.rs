// =============================================================================
// File: tentaflow-cli/src/commands/keys.rs — Rotation CLI for the F1b P3.A
// persistent HMAC keys: pickup_token, frame_url, recording_url.
//
// Unlike `camera rotate-key` (P1.C) — which re-encrypts every DB blob — these
// three keys only sign in-flight credentials (one-shot tokens, signed URLs).
// Outstanding tokens minted under the old key are kept valid by the issuer's
// in-memory previous-key window until they naturally expire (max 1 h for
// recording URLs, 10 min for frame URLs, 30 s for pickup tokens).
//
// On-disk rotation flow mirrors `cameras.key`:
//
//   1.  Write new 32-byte CSPRNG key to <name>.key.staging (mode 0600).
//   2.  fsync parent dir.
//   3.  Rename .staging -> .key.new   (durable commit marker).
//   4.  Archive current live key as <name>.key.YYYYMMDD-HHMMSS.
//   5.  Rename .new     -> <name>.key (final swap).
//   6.  fsync parent dir.
//
// A crash between step 3 and step 5 leaves `.new` next to a stale `.key`;
// `PersistentKey::load_or_generate_at` recovers by promoting `.new` on
// startup. A crash between step 1 and step 3 leaves `.staging` which is
// discarded on startup (rotation never committed).
//
// Single-node only. Multi-node mesh sync of these keys is P3.B.
// =============================================================================

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;

use tentaflow_core::services::key_storage::{
    fsync_parent_dir, key_path, write_key_bytes, KEY_LEN,
};

/// Names accepted by `tentaflow-cli keys rotate <name>`. Validated at parse
/// time so the operator gets a clear error before any disk I/O runs.
const ROTATABLE: &[&str] = &["pickup_token", "frame_url", "recording_url"];

#[derive(Subcommand, Debug)]
pub enum KeysCommand {
    /// Rotate one of the persistent HMAC keys
    /// (pickup_token | frame_url | recording_url). The previous key is
    /// archived next to the live file with a UTC-timestamp suffix; the
    /// running host should be restarted after rotation so the new key is
    /// loaded — until then, outstanding tokens stay valid through the
    /// issuer's previous-key window.
    Rotate {
        /// Name of the key to rotate. One of: pickup_token, frame_url,
        /// recording_url.
        name: String,
        /// Optional explicit key file path (defaults to
        /// `<tentaflow_home>/keys/<name>.key`, or the value of
        /// `TENTAFLOW_KEY_<NAME>` when set).
        #[arg(long)]
        key_path: Option<PathBuf>,
    },
}

pub fn run(cmd: KeysCommand) -> ExitCode {
    match cmd {
        KeysCommand::Rotate { name, key_path } => match rotate(&name, key_path) {
            Ok(report) => {
                println!("{}", report);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("keys rotate failed: {e:#}");
                ExitCode::from(1)
            }
        },
    }
}

/// Drives the on-disk rotation. Returns a human-readable report.
fn rotate(name: &str, explicit_path: Option<PathBuf>) -> anyhow::Result<String> {
    if !ROTATABLE.contains(&name) {
        anyhow::bail!(
            "unknown key name {:?}; allowed: {}",
            name,
            ROTATABLE.join(", ")
        );
    }

    let live_path = match explicit_path {
        Some(p) => p,
        None => key_path(name)
            .map_err(|e| anyhow::anyhow!("resolve key path for {name}: {e}"))?,
    };

    if !live_path.exists() {
        anyhow::bail!(
            "live key not found at {} — nothing to rotate (run the host once to generate)",
            live_path.display()
        );
    }

    // Step 1 — generate new 32-byte CSPRNG key.
    let mut new_key = [0u8; KEY_LEN];
    use rand::Rng;
    rand::rng().fill_bytes(&mut new_key);

    // Step 2 — write the new key bytes to `*.key.staging`.
    let staging_path = live_path.with_extension("key.staging");
    let new_path = live_path.with_extension("key.new");
    write_key_bytes(&staging_path, &new_key)
        .map_err(|e| anyhow::anyhow!("write new key (.staging): {e}"))?;
    fsync_parent_dir(&staging_path)
        .map_err(|e| anyhow::anyhow!("fsync parent dir after .staging: {e}"))?;

    // Step 3 — promote `.staging → .new` (durable commit marker).
    std::fs::rename(&staging_path, &new_path)
        .map_err(|e| anyhow::anyhow!("promote .staging to .new: {e}"))?;
    fsync_parent_dir(&new_path)
        .map_err(|e| anyhow::anyhow!("fsync parent dir after .new: {e}"))?;

    // Step 4 — archive the current live key (best-effort; the `.new` marker
    // is already durable so even if the archive copy fails the next process
    // start will promote `.new` cleanly).
    let archive_path = archive_path_for(&live_path);
    if let Err(e) = std::fs::copy(&live_path, &archive_path) {
        eprintln!(
            "warning: archive old key to {} failed (non-fatal): {e}",
            archive_path.display()
        );
    }

    // Step 5 — atomically swap `.new` into the live path.
    std::fs::rename(&new_path, &live_path)
        .map_err(|e| anyhow::anyhow!("rename new key into place: {e}"))?;
    fsync_parent_dir(&live_path)
        .map_err(|e| anyhow::anyhow!("fsync parent dir after swap: {e}"))?;

    Ok(format!(
        "rotated key {name}\n  new key:  {}\n  archived: {}\n\n\
         note: running host instances keep the previous key in memory as a\n\
         verify-only secondary until their natural TTL window closes (30 s\n\
         for pickup_token, 10 min for frame_url, 1 h for recording_url).\n\
         Restart the host to load the new key for signing.",
        live_path.display(),
        archive_path.display()
    ))
}

/// Build the archive filename for a rotated key:
/// `<name>.key.YYYYMMDD-HHMMSS` next to the live file. UTC keeps the
/// suffix monotonic across timezones so a `ls` listing sorts by age.
fn archive_path_for(live: &PathBuf) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let mut name = live
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("key"));
    name.push(".");
    name.push(stamp);
    live.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rotate_frame_url_writes_new_key_and_archives_old() {
        let td = TempDir::new().unwrap();
        let live = td.path().join("frame_url.key");
        let old_bytes = [0x33u8; KEY_LEN];
        write_key_bytes(&live, &old_bytes).unwrap();

        let report = rotate("frame_url", Some(live.clone())).expect("rotate ok");
        assert!(report.contains("rotated key frame_url"));

        // Live key file is now different from the original.
        let new_disk = std::fs::read(&live).unwrap();
        assert_eq!(new_disk.len(), KEY_LEN);
        assert_ne!(new_disk.as_slice(), &old_bytes[..], "key bytes must change");

        // Archive file holds the old bytes.
        let entries: Vec<_> = std::fs::read_dir(td.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| {
                let s = n.to_string_lossy();
                s.starts_with("frame_url.key.") && s != "frame_url.key.new"
            })
            .collect();
        assert_eq!(entries.len(), 1, "exactly one archive expected");
        let archived = std::fs::read(td.path().join(&entries[0])).unwrap();
        assert_eq!(archived.as_slice(), &old_bytes[..], "archive holds old key");

        // No staging / .new leftovers.
        assert!(!td.path().join("frame_url.key.staging").exists());
        assert!(!td.path().join("frame_url.key.new").exists());
    }

    #[test]
    fn rotate_rejects_unknown_name() {
        let td = TempDir::new().unwrap();
        let live = td.path().join("foo.key");
        write_key_bytes(&live, &[0u8; KEY_LEN]).unwrap();
        let err = rotate("not_a_key", Some(live)).unwrap_err();
        assert!(err.to_string().contains("unknown key name"));
    }

    #[test]
    fn rotate_rejects_missing_live_file() {
        let td = TempDir::new().unwrap();
        let live = td.path().join("pickup_token.key"); // does not exist
        let err = rotate("pickup_token", Some(live)).unwrap_err();
        assert!(err.to_string().contains("nothing to rotate"));
    }
}
