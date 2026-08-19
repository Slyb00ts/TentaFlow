// ===== File: code_studio/artifacts.rs — content-addressed store of a workspace =====
//
// Everything bulky a session produces — the canonical input of an operation,
// a patch hunk, a truncated command output — is stored once under the SHA-256
// of its bytes (§13.2) and referenced by digest from the timeline and the
// operation journal. Two runs that write the same file content therefore cost
// one blob, and a reference can be verified rather than trusted.
//
// Two rules make it safe rather than merely convenient.
//
// **A path is derived, never composed from input.** Every location comes from
// `paths::artifact_path`, which validates the workspace id and refuses anything
// that is not 64 hex characters. No caller passes a path in, so no caller can
// point the store at another directory.
//
// **Lifetime is a refcount plus an age, and both must agree.** `gc` removes a
// blob only when nothing holds it AND it has been unused for the retention
// window (§13.5, 30 days from `last_used_at`). The referencing tables are
// checked as well, so a refcount that drifted below the truth cannot delete a
// blob the timeline still points at.

use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::{OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use tracing::warn;

use super::paths;
use crate::db::DbPool;

/// A stored blob: what the caller writes into `artifact_ref` / `input_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    pub sha256: String,
    pub size_bytes: u64,
}

/// What one `gc` pass removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub removed: usize,
    pub bytes_freed: u64,
}

/// Stores `bytes` and returns their digest. Storing the same content twice is
/// one blob and one row — the second call only refreshes `last_used_at`.
pub fn put(pool: &DbPool, workspace_id: &str, bytes: &[u8], kind: &str) -> Result<ArtifactRef> {
    let sha256 = digest(bytes);
    let path = paths::artifact_path(workspace_id, &sha256)?;
    write_blob(&path, bytes)?;

    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "INSERT INTO artifacts (sha256, size_bytes, kind, refcount, created_at, last_used_at) \
         VALUES (?1, ?2, ?3, 0, datetime('now'), datetime('now')) \
         ON CONFLICT(sha256) DO UPDATE SET last_used_at = datetime('now')",
        rusqlite::params![sha256, bytes.len() as i64, kind],
    )?;
    Ok(ArtifactRef {
        sha256,
        size_bytes: bytes.len() as u64,
    })
}

/// Reads a blob back and verifies it against its own digest. A silent bit flip
/// or a truncated write would otherwise surface as a confusing decode error
/// somewhere far away from the store.
pub fn get(pool: &DbPool, workspace_id: &str, sha256: &str) -> Result<Vec<u8>> {
    let path = paths::artifact_path(workspace_id, sha256)?;
    let exists: bool = {
        let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
        conn.query_row(
            "SELECT 1 FROM artifacts WHERE sha256 = ?1",
            rusqlite::params![sha256],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false)
    };
    if !exists {
        return Err(anyhow!("artifact {sha256} is not in this workspace"));
    }
    let bytes = std::fs::read(&path)
        .map_err(|e| anyhow!("artifact {sha256} is registered but unreadable: {e}"))?;
    let actual = digest(&bytes);
    if actual != sha256.to_ascii_lowercase() {
        return Err(anyhow!(
            "artifact {sha256} does not match its content (found {actual})"
        ));
    }

    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE artifacts SET last_used_at = datetime('now') WHERE sha256 = ?1",
        rusqlite::params![sha256],
    )?;
    Ok(bytes)
}

/// Takes a reference. Called when a row starts pointing at the blob.
pub fn retain(pool: &DbPool, sha256: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    apply_refcount(&conn, sha256, 1)
}

/// Drops a reference. The blob survives until `gc` finds it unheld and old.
pub fn release(pool: &DbPool, sha256: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    apply_refcount(&conn, sha256, -1)
}

/// `retain` inside a transaction the caller owns — the reference and the row
/// that holds it must appear together.
pub fn retain_in_tx(tx: &Transaction<'_>, sha256: &str) -> Result<()> {
    apply_refcount(tx, sha256, 1)
}

/// `release` inside a transaction the caller owns.
pub fn release_in_tx(tx: &Transaction<'_>, sha256: &str) -> Result<()> {
    apply_refcount(tx, sha256, -1)
}

fn apply_refcount(conn: &rusqlite::Connection, sha256: &str, delta: i64) -> Result<()> {
    let changed = conn.execute(
        "UPDATE artifacts SET refcount = MAX(0, refcount + ?2), last_used_at = datetime('now') \
         WHERE sha256 = ?1",
        rusqlite::params![sha256, delta],
    )?;
    if changed == 0 {
        return Err(anyhow!("artifact {sha256} is not in this workspace"));
    }
    Ok(())
}

/// Removes unheld blobs unused for longer than `older_than`.
///
/// The row goes first and the file second: a crash between them leaves an
/// orphan file (found by the next pass over the directory) rather than a row
/// pointing at nothing, which every later `get` would trip over.
pub fn gc(pool: &DbPool, workspace_id: &str, older_than: chrono::Duration) -> Result<GcReport> {
    let cutoff_seconds = older_than.num_seconds().max(0);
    let candidates: Vec<(String, i64)> = {
        let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT sha256, size_bytes FROM artifacts \
             WHERE refcount <= 0 \
               AND COALESCE(last_used_at, created_at) < datetime('now', ?1) \
               AND NOT EXISTS (SELECT 1 FROM session_events WHERE artifact_ref = artifacts.sha256) \
               AND NOT EXISTS (SELECT 1 FROM session_operations \
                    WHERE input_ref = artifacts.sha256 OR result_ref = artifacts.sha256)",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![format!("-{cutoff_seconds} seconds")],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut report = GcReport::default();
    for (sha256, size) in candidates {
        let path = paths::artifact_path(workspace_id, &sha256)?;
        let deleted = {
            let conn = pool
                .write()
                .map_err(|e| anyhow!("workspace db write: {e}"))?;
            conn.execute(
                "DELETE FROM artifacts WHERE sha256 = ?1 AND refcount <= 0",
                rusqlite::params![sha256],
            )?
        };
        if deleted == 0 {
            // Someone took a reference between the scan and the delete. The
            // blob is held again, and its file must stay.
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(sha256 = %sha256, "cannot remove artifact file: {e}"),
        }
        report.removed += 1;
        report.bytes_freed += size.max(0) as u64;
    }
    Ok(report)
}

/// Lowercase hex SHA-256 of the bytes — the identity of a blob.
fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Writes the blob through a temporary file in the same directory, so a reader
/// never sees a half-written artifact under its final digest.
fn write_blob(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("artifact path has no parent"))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| anyhow!("create artifact shard {}: {e}", parent.display()))?;
    let temporary = parent.join(format!(
        "{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact"),
        std::process::id()
    ));
    std::fs::write(&temporary, bytes)
        .map_err(|e| anyhow!("write artifact {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        anyhow!("publish artifact {}: {e}", path.display())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::workspace_db;

    /// The storage category is process-global, so the tests that redirect it
    /// take this lock — the same convention as `session.rs`.

    struct Fixture {
        _data: tempfile::TempDir,
        pool: DbPool,
        workspace_id: String,
    }

    fn fixture(workspace_id: &str) -> Fixture {
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let root = paths::create_workspace_layout(workspace_id).expect("layout");
        let (pool, _version) = workspace_db::open_pool_at(&root).expect("open workspace.db");
        Fixture {
            _data: data,
            pool,
            workspace_id: workspace_id.to_string(),
        }
    }

    fn release_paths() {
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    #[test]
    fn identical_content_is_stored_once_and_reads_back_verbatim() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-art-dedupe");

        let first = put(
            &fx.pool,
            &fx.workspace_id,
            b"hello world",
            "operation_input",
        )
        .unwrap();
        let second = put(
            &fx.pool,
            &fx.workspace_id,
            b"hello world",
            "operation_input",
        )
        .unwrap();
        assert_eq!(first, second);

        let rows: i64 = {
            let conn = fx.pool.read().unwrap();
            conn.query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(rows, 1, "the same content produced two rows");

        assert_eq!(
            get(&fx.pool, &fx.workspace_id, &first.sha256).unwrap(),
            b"hello world"
        );
        let path = paths::artifact_path(&fx.workspace_id, &first.sha256).unwrap();
        assert!(path.starts_with(paths::artifacts_dir(&fx.workspace_id).unwrap()));
        release_paths();
    }

    #[test]
    fn a_held_artifact_survives_gc_and_an_orphan_does_not() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-art-gc");

        let held = put(&fx.pool, &fx.workspace_id, b"held blob", "patch_hunk").unwrap();
        let orphan = put(&fx.pool, &fx.workspace_id, b"orphan blob", "patch_hunk").unwrap();
        retain(&fx.pool, &held.sha256).unwrap();

        // Age both blobs beyond the retention window.
        {
            let conn = fx.pool.write().unwrap();
            conn.execute(
                "UPDATE artifacts SET last_used_at = datetime('now', '-40 days')",
                [],
            )
            .unwrap();
        }

        let report = gc(&fx.pool, &fx.workspace_id, chrono::Duration::days(30)).unwrap();
        assert_eq!(report.removed, 1, "gc removed the wrong number of blobs");
        assert_eq!(report.bytes_freed, b"orphan blob".len() as u64);
        assert!(get(&fx.pool, &fx.workspace_id, &held.sha256).is_ok());
        assert!(
            get(&fx.pool, &fx.workspace_id, &orphan.sha256).is_err(),
            "an unheld, expired blob survived"
        );
        assert!(!paths::artifact_path(&fx.workspace_id, &orphan.sha256)
            .unwrap()
            .exists());

        // Releasing the last reference makes the held blob collectable too.
        release(&fx.pool, &held.sha256).unwrap();
        {
            let conn = fx.pool.write().unwrap();
            conn.execute(
                "UPDATE artifacts SET last_used_at = datetime('now', '-40 days')",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            gc(&fx.pool, &fx.workspace_id, chrono::Duration::days(30))
                .unwrap()
                .removed,
            1
        );
        release_paths();
    }

    #[test]
    fn a_fresh_orphan_is_not_collected_before_its_time() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-art-fresh");
        let fresh = put(&fx.pool, &fx.workspace_id, b"just written", "exec_output").unwrap();

        let report = gc(&fx.pool, &fx.workspace_id, chrono::Duration::days(30)).unwrap();
        assert_eq!(
            report.removed, 0,
            "a blob written seconds ago was collected"
        );
        assert!(get(&fx.pool, &fx.workspace_id, &fresh.sha256).is_ok());
        release_paths();
    }

    #[test]
    fn an_artifact_still_named_by_the_timeline_is_never_collected() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-art-referenced");
        let blob = put(
            &fx.pool,
            &fx.workspace_id,
            b"referenced blob",
            "exec_output",
        )
        .unwrap();
        {
            let conn = fx.pool.write().unwrap();
            conn.execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('s-1', 'ws-art-referenced', 'u-1', 't', 'b', 'normal', 'f', 'v', 'idle', \
                  datetime('now'), datetime('now'))",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_events (event_id, session_id, seq, idempotency_key, \
                  schema_version, kind, payload_cbor, artifact_ref, created_at) \
                 VALUES ('e-1', 's-1', 1, 'k-1', 1, 'exec', X'00', ?1, datetime('now'))",
                rusqlite::params![blob.sha256],
            )
            .unwrap();
            // The refcount drifted below the truth — the referencing table is
            // what stops the deletion.
            conn.execute(
                "UPDATE artifacts SET refcount = 0, last_used_at = datetime('now', '-99 days')",
                [],
            )
            .unwrap();
        }

        let report = gc(&fx.pool, &fx.workspace_id, chrono::Duration::days(30)).unwrap();
        assert_eq!(report.removed, 0, "a referenced artifact was collected");
        assert!(get(&fx.pool, &fx.workspace_id, &blob.sha256).is_ok());
        release_paths();
    }

    #[test]
    fn a_reference_to_an_unknown_digest_is_refused() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let fx = fixture("ws-art-unknown");
        let unknown = "b".repeat(64);
        assert!(retain(&fx.pool, &unknown).is_err());
        assert!(get(&fx.pool, &fx.workspace_id, &unknown).is_err());
        assert!(get(&fx.pool, &fx.workspace_id, "not-a-digest").is_err());
        release_paths();
    }
}
