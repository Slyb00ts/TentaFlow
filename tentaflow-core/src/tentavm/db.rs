// ===== File: tentavm/db.rs — the instance content database of TentaVM =====
//
// `<instance data dir>/tentavm.db` is the NODE-LOCAL half of an environment
// (plan §4.2). Nothing in this file may ever be added to
// `sync/core_registry.rs`, and keeping it out of the main `tentaflow.db`
// removes it from the sync engine's reach by construction instead of by a rule
// somebody has to remember.
//
// This file is the SCHEMA only: no row of these tables is written or read yet.
// The list below is therefore the CONTRACT each table is created for — what
// phase 1 will put in it and why it may not travel — not a description of code
// that exists:
//
//   * `vm_connector_secrets` — credentials of an external hypervisor, sealed
//     with the PER-NODE SettingsCipher key. Replicated they would be
//     undecryptable at the far end, so shipping them would only widen the
//     attack surface. The one thing that DOES travel is the sealed envelope in
//     `vm_connector_secret_grants` (main DB), addressed to one node's key.
//   * `vm_connector_recipients` — the owner node's narrowing of who may be
//     handed such an envelope. A decision about this node's secrets, taken on
//     the node that holds them.
//   * `vm_provisioning_inputs` — passwords and SSH material a machine needs
//     exactly once, deleted after its first successful boot.
//   * `vm_saga_steps` — the run state of ONE node's job. No other node can
//     resume, retry or compensate it; what a remote UI needs (`state`,
//     `phase`, `progress_pct`, `error`) lives on the `vm_jobs` registry row.
//   * `vm_probe_cache`, `vm_runtime_map`, `vm_host_settings` — what this node
//     measured about itself and how it addresses its own hypervisor.
//   * `vm_guest_events`, `vm_job_logs` — append-only histories kept off the
//     ledger deliberately (plan §4.1): they are read by forwarding the request
//     to the owner node, not by replicating every line to every node.
//
// The database has no foreign keys outside itself: `guest_id`, `job_id` and
// `connector_id` are handles into the registry in the main database, resolved
// by the callers.
//
// Console secrets (VNC passwords, Proxmox tickets) are absent by design — plan
// §4.2 keeps them in RAM only, and no schema step here may introduce them.

use anyhow::Result;
use rusqlite::Connection;

use crate::addon::app_db;

/// Schema steps, applied once each by `app_db::run_versioned_migrations`.
/// Append-only: a change to the content schema is a NEW step, never an edit of
/// a released one.
const MIGRATIONS: &[(i64, &str)] = &[(
    1,
    "
CREATE TABLE vm_connector_secrets (
    connector_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    material_enc TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('owner','envelope')),
    kid TEXT,
    fingerprint TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_used_at TEXT
);

CREATE TABLE vm_connector_recipients (
    connector_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    granted_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (connector_id, node_id)
) WITHOUT ROWID;

CREATE TABLE vm_provisioning_inputs (
    id TEXT PRIMARY KEY,
    guest_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload_enc TEXT NOT NULL,
    created_at TEXT NOT NULL,
    consumed_at TEXT
);
CREATE INDEX idx_vm_provisioning_inputs_guest ON vm_provisioning_inputs(guest_id);

CREATE TABLE vm_saga_steps (
    job_id TEXT NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','done','failed','compensated')),
    detail TEXT,
    resume_after_restart INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (job_id, step)
) WITHOUT ROWID;

CREATE TABLE vm_probe_cache (
    probe_key TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    probed_at TEXT NOT NULL,
    expires_at TEXT
);

CREATE TABLE vm_runtime_map (
    guest_id TEXT PRIMARY KEY,
    engine TEXT NOT NULL,
    runtime_ref TEXT NOT NULL,
    observed_state TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE vm_host_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE vm_guest_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guest_id TEXT NOT NULL,
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    actor_user_id TEXT,
    actor_node_id TEXT
);
CREATE INDEX idx_vm_guest_events_guest ON vm_guest_events(guest_id, at DESC);

CREATE TABLE vm_job_logs (
    job_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    at TEXT NOT NULL,
    level TEXT NOT NULL,
    line TEXT NOT NULL,
    PRIMARY KEY (job_id, seq)
) WITHOUT ROWID;
",
)];

/// Brings a content database up to date. Idempotent: the versioned runner
/// skips applied steps, so the init hook and every first open of the process
/// may call it.
pub fn migrate(conn: &Connection) -> Result<()> {
    app_db::run_versioned_migrations(conn, super::PACKAGE_ID, MIGRATIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        names
    }

    #[test]
    fn migrate_is_idempotent_and_creates_the_content_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, MIGRATIONS.len() as i64);

        let tables = table_names(&conn);
        for table in [
            "vm_connector_secrets",
            "vm_connector_recipients",
            "vm_provisioning_inputs",
            "vm_saga_steps",
            "vm_probe_cache",
            "vm_runtime_map",
            "vm_host_settings",
            "vm_guest_events",
            "vm_job_logs",
        ] {
            assert!(
                tables.iter().any(|t| t == table),
                "{table} must exist in the content database"
            );
        }
    }

    /// The registry lives in the main database and replicates from there; a
    /// copy here would be a second source of truth for the same machine.
    #[test]
    fn the_content_schema_holds_no_registry_table() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let tables = table_names(&conn);
        for table in [
            "vm_hosts",
            "vm_host_grants",
            "vm_connectors",
            "vm_connector_secret_grants",
            "vm_guests",
            "vm_guest_members",
            "vm_jobs",
            "vm_instance_settings",
        ] {
            assert!(
                !tables.iter().any(|t| t == table),
                "{table} belongs to the main database"
            );
        }
    }

    /// Plan §4.2 keeps VNC passwords and Proxmox tickets in RAM only. This is
    /// a NAMING guard on the schema — it catches a column created for such a
    /// secret, not a secret smuggled into `material_enc`; the real check (§4.2:
    /// "no SQLite table contains a VNC password or a ticket") needs the console
    /// path and belongs to phase 1.
    #[test]
    fn no_column_is_named_after_a_console_secret() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for table in table_names(&conn) {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let columns = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            for name in columns {
                let lowered = name.to_ascii_lowercase();
                assert!(
                    !lowered.contains("vnc") && !lowered.contains("ticket"),
                    "{table}.{name} looks like a console secret, which lives in RAM only"
                );
            }
        }
    }
}
