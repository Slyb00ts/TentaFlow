// ===== File: project_studio/mod.rs — native Project Studio ("Projekty") core module =====
//
// Project Studio is a native core module (not a WASM addon). The registry
// (projects, members, grants, chats) lives in its own `<data>/projects.db`;
// per-project content (sources, files, jobs, activity, settings, tags) lives
// in `<dir_path>/project.db` behind a bounded pool cache. Identity is always
// referenced at the application level from the request `HandlerContext` —
// never via SQL foreign keys into `tentaflow.db`.

pub mod activity;
pub mod db;
pub mod generation;
pub mod ingest;
pub mod knowledge;
pub mod models;
pub mod notifications;
pub mod project_db;
pub mod reports;
pub mod repository;
pub mod runs;
pub mod tasks;
pub mod tests;

use anyhow::Result;

use crate::db::DbPool;

/// Initialises Project Studio: opens `<data>/projects.db`, runs its
/// migrations, repairs `dir_path` rows left stale by a data-directory
/// migration, publishes the pool and starts the idle sweeper for cached
/// per-project pools. Call once at startup, next to `ml_studio::init`,
/// from within the tokio runtime (the sweeper spawns a task).
pub fn init() -> Result<DbPool> {
    let pool = db::init(&crate::paths::data_dir().join("projects.db"))?;
    heal_project_dir_paths(&pool);
    project_db::spawn_idle_sweeper();
    Ok(pool)
}

/// Repairs `projects.dir_path` after a Data-category storage migration. The
/// data directory moves at boot (`paths::apply_pending_boot_migrations`,
/// BEFORE this module initialises) and takes `projects/<id>/` along, but the
/// registry rows still hold absolute paths under the old root — a row is
/// rewritten only when its stored directory is gone AND the canonical
/// location exists, so intentionally custom paths are never clobbered.
/// Per-project pools are frozen for the duration (same contract as the addon
/// storage freeze) so nothing opens a project.db under a stale path
/// mid-rewrite.
fn heal_project_dir_paths(pool: &DbPool) {
    project_db::set_frozen(true);
    let result = (|| -> Result<u32> {
        let conn = pool
            .write()
            .map_err(|e| anyhow::anyhow!("projects registry write: {e}"))?;
        let rows: Vec<(String, String)> = {
            let mut stmt = conn.prepare("SELECT project_id, dir_path FROM projects")?;
            let mapped = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            mapped.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut healed = 0u32;
        for (project_id, dir_path) in rows {
            let expected = project_dir(&project_id);
            if std::path::Path::new(&dir_path).is_dir() || !expected.is_dir() {
                continue;
            }
            conn.execute(
                "UPDATE projects SET dir_path = ?1 WHERE project_id = ?2",
                rusqlite::params![expected.to_string_lossy(), project_id],
            )?;
            healed += 1;
        }
        Ok(healed)
    })();
    project_db::set_frozen(false);
    match result {
        Ok(0) => {}
        Ok(n) => {
            tracing::info!(healed = n, "rewrote project dir_path after data-dir move")
        }
        Err(e) => tracing::warn!("project dir_path heal failed: {e}"),
    }
}

/// Directory holding all of a project's data:
/// `<data>/projects/<project_id>/{project.db,files/,vectors/}`. Computed only
/// at CREATE time — afterwards the persisted `projects.dir_path` is the
/// single source of truth.
pub fn project_dir(project_id: &str) -> std::path::PathBuf {
    crate::paths::data_dir().join("projects").join(project_id)
}
