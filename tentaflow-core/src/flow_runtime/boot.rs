// =============================================================================
// File: flow_runtime/boot.rs — startup-time reconciliation of flow_invocations
// =============================================================================
//
// Process crashes (or a clean restart) leave `flow_invocations` rows in
// `status='running'` because no scheduler is alive to finalize them. On
// boot we mark those rows as failed with a synthetic reason so the addon
// surface and audit history reflect the true outcome instead of advertising
// invocations that nobody is executing.

use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;

/// Marks every `status='running'` row as failed with `error='core_restart'`
/// and `finished_at = now`. Idempotent: a second call after a clean boot
/// affects 0 rows. Returns the number of rows updated.
pub fn mark_orphaned_invocations(conn: &Connection) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let n = conn.execute(
        "UPDATE flow_invocations \
         SET status = 'failed', error = 'core_restart', finished_at = ?1 \
         WHERE status = 'running'",
        rusqlite::params![now],
    )?;
    Ok(n)
}
