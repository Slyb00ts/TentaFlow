// =============================================================================
// File: bus/db.rs — schema of the per-instance `tentabus.db` (plan-app-
//       platform §1.4). Consumer group state (commit mode, pause flag) is
//       node-local by construction and was never part of
//       `sync::core_registry::CORE_SYNC_DESCRIPTORS`; this file formalizes
//       that as its own database instead of a table inside the synced
//       `tentaflow.db`.
//
//       No `instance_id` column anywhere in here: the file itself IS the
//       instance (`<instance data dir>/tentabus.db`, opened through
//       `addon::app_db`), unlike the five core bus tables that stay in the
//       main database and carry `instance_id` as a leading PK column
//       (migrations 141-145, W3).
// =============================================================================

use anyhow::Result;
use rusqlite::Connection;

use crate::addon::app_db;

use super::instance::BusInstanceId;

/// Schema steps, applied once each by `app_db::run_versioned_migrations`.
/// Append-only: a change to the content schema is a new step, never an edit.
const STEPS: &[(i64, &str)] = &[(
    1,
    "CREATE TABLE IF NOT EXISTS bus_groups (
        org_id         TEXT NOT NULL,
        group_id       TEXT NOT NULL,
        topic          TEXT NOT NULL,
        commit_mode    TEXT NOT NULL,
        paused         INTEGER NOT NULL DEFAULT 0,
        created_at_ms  INTEGER NOT NULL,
        updated_at_ms  INTEGER NOT NULL,
        PRIMARY KEY (org_id, group_id, topic)
    );
    CREATE INDEX IF NOT EXISTS idx_bus_groups_org ON bus_groups(org_id);",
)];

/// Brings the instance's content database up to date. Idempotent: the
/// versioned runner skips applied steps, so the install/enable hooks and
/// every first open of the process may call it.
pub fn migrate(conn: &Connection) -> Result<()> {
    app_db::run_versioned_migrations(conn, BusInstanceId::PACKAGE_ID, STEPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent_and_creates_bus_groups() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, STEPS.len() as i64);
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'bus_groups'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(present, 1, "bus_groups must exist in the content database");
        let index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
                 AND name = 'idx_bus_groups_org'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
    }

    /// The file itself is the instance; an `instance_id` column here would be
    /// redundant with the platform's own per-instance containment and would
    /// invite the same scoping mistake the core tables need `instance_id` to
    /// prevent.
    #[test]
    fn bus_groups_has_no_instance_id_column() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let has_column: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('bus_groups') WHERE name = 'instance_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_column, 0);
    }

    #[test]
    fn bus_groups_round_trips_a_row() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO bus_groups \
             (org_id, group_id, topic, commit_mode, paused, created_at_ms, updated_at_ms) \
             VALUES ('org-1', 'g1', 'orders', 'auto', 0, 1, 1)",
            [],
        )
        .unwrap();
        let paused: i64 = conn
            .query_row(
                "SELECT paused FROM bus_groups WHERE org_id = 'org-1' AND group_id = 'g1' \
                 AND topic = 'orders'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(paused, 0);
    }
}
