// ============ File: oauth_cleanup.rs - periodic purge of expired OAuth pending states ============
//
// oauth_pending_states stores short-lived CSRF `state` + PKCE verifier rows with
// a TTL (default 600s set by insert_oauth_state). Stale rows accumulate if the
// user never completes the callback. This task runs once at startup and then
// hourly to keep the table clean.

use std::time::Duration;

use crate::db::{repository, DbPool};

const CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);

/// Deletes all expired rows from `oauth_pending_states`. Returns the number of
/// rows removed. Safe to call concurrently - SQLite DELETE is atomic.
pub fn cleanup_expired_oauth_states(pool: &DbPool) -> anyhow::Result<u64> {
    let n = repository::purge_expired_oauth_states(pool)?;
    Ok(n as u64)
}

/// Runs cleanup once synchronously, then spawns a background task that repeats
/// hourly. The spawned task outlives any specific request - it holds a clone
/// of the DbPool and stops naturally only when the process exits.
pub fn start_oauth_cleanup_task(pool: DbPool) {
    match cleanup_expired_oauth_states(&pool) {
        Ok(n) if n > 0 => {
            tracing::info!(
                "oauth cleanup: removed {} expired pending state(s) at startup",
                n
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("oauth cleanup at startup failed: {}", e),
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CLEANUP_INTERVAL).await;
            match cleanup_expired_oauth_states(&pool) {
                Ok(n) if n > 0 => {
                    tracing::debug!("oauth cleanup: removed {} expired pending state(s)", n);
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("oauth cleanup failed: {}", e),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cleanup_expired_oauth_states_removes_old_rows() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();

        // Insert one expired row (ttl=0 is clamped, we inject directly).
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO oauth_pending_states \
                 (state, user_id, addon_id, provider_id, mode, code_verifier, redirect_after, expires_at) \
                 VALUES (?1, NULL, 'a', 'p', 'individual', '', '', datetime('now', '-1 hour'))",
                rusqlite::params!["expired-state"],
            ).unwrap();
            conn.execute(
                "INSERT INTO oauth_pending_states \
                 (state, user_id, addon_id, provider_id, mode, code_verifier, redirect_after, expires_at) \
                 VALUES (?1, NULL, 'a', 'p', 'individual', '', '', datetime('now', '+1 hour'))",
                rusqlite::params!["fresh-state"],
            ).unwrap();
        }

        let removed = cleanup_expired_oauth_states(&db).unwrap();
        assert_eq!(removed, 1, "exactly one expired row must be removed");

        // Fresh row still present.
        let conn = db.lock().unwrap();
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM oauth_pending_states", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 1);
    }
}
