// ============ File: simulation_schema.rs — connection-local simulation metadata and trace schema ============

use anyhow::{bail, ensure, Context, Result};
use rusqlite::Connection;

pub const SIMULATION_SCHEMA_VERSION: i64 = 2;

const EXPECTED_TABLES: &[&str] = &[
    "simulation_schema_meta",
    "simulation_meta",
    "simulation_source_pins",
    "simulation_clock",
    "simulation_commands",
    "simulation_trace_steps",
];

const EXPECTED_PROCESS_TABLES: &[&str] = &[
    "organizations",
    "user_accounts",
    "org_memberships",
    "bpmn_definitions",
    "bpmn_versions",
    "bpmn_instances",
    "bpmn_scopes",
    "bpmn_tokens",
    "bpmn_events",
    "bpmn_user_tasks",
    "bpmn_timers",
    "bpmn_activity_io_witnesses",
    "bpmn_commands",
];

const SCHEMA_DDL: &str = r#"
CREATE TABLE simulation_schema_meta (
    schema_version INTEGER PRIMARY KEY CHECK(schema_version = 2)
);
CREATE TABLE simulation_meta (
    simulation_id TEXT PRIMARY KEY CHECK(length(simulation_id) = 36),
    scenario_sha256 TEXT NOT NULL CHECK(length(scenario_sha256) = 64 AND scenario_sha256 NOT GLOB '*[^0-9a-f]*'),
    profile TEXT NOT NULL CHECK(profile = 'script_user_manual'),
    org_id TEXT NOT NULL,
    actor_user_id TEXT NOT NULL,
    definition_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(typeof(version) = 'integer' AND version > 0),
    model_sha256 TEXT NOT NULL CHECK(length(model_sha256) = 64 AND model_sha256 NOT GLOB '*[^0-9a-f]*'),
    status TEXT NOT NULL CHECK(status IN ('running','completed','failed')),
    revision INTEGER NOT NULL CHECK(typeof(revision) = 'integer' AND revision > 0),
    instance_id TEXT CHECK(instance_id IS NULL OR length(instance_id) = 36),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(simulation_id, definition_id, version)
);
CREATE TABLE simulation_source_pins (
    simulation_id TEXT PRIMARY KEY REFERENCES simulation_meta(simulation_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    actor_user_id TEXT NOT NULL,
    definition_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(typeof(version) = 'integer' AND version > 0),
    model_sha256 TEXT NOT NULL CHECK(length(model_sha256) = 64 AND model_sha256 NOT GLOB '*[^0-9a-f]*'),
    model_json TEXT NOT NULL CHECK(json_valid(model_json) AND json_type(model_json) = 'object'),
    selected_process_id TEXT NOT NULL CHECK(length(selected_process_id) > 0),
    start_node_id TEXT NOT NULL CHECK(length(start_node_id) > 0),
    acl_snapshot_json TEXT NOT NULL CHECK(json_valid(acl_snapshot_json) AND json_type(acl_snapshot_json) = 'object' AND length(CAST(acl_snapshot_json AS BLOB)) <= 262144),
    captured_at_ms INTEGER NOT NULL,
    UNIQUE(simulation_id, definition_id, version)
);
CREATE TABLE simulation_clock (
    simulation_id TEXT PRIMARY KEY REFERENCES simulation_meta(simulation_id) ON DELETE CASCADE,
    start_ms INTEGER NOT NULL,
    now_ms INTEGER NOT NULL,
    horizon_ms INTEGER NOT NULL,
    tick_duration_ms INTEGER NOT NULL CHECK(typeof(tick_duration_ms) = 'integer' AND tick_duration_ms > 0),
    step_index INTEGER NOT NULL CHECK(typeof(step_index) = 'integer' AND step_index >= 0),
    revision INTEGER NOT NULL CHECK(typeof(revision) = 'integer' AND revision > 0),
    CHECK(now_ms >= start_ms AND now_ms <= horizon_ms),
    CHECK(horizon_ms >= start_ms)
);
CREATE TABLE simulation_commands (
    command_id TEXT PRIMARY KEY CHECK(length(command_id) = 36),
    simulation_id TEXT NOT NULL REFERENCES simulation_meta(simulation_id) ON DELETE CASCADE,
    actor_user_id TEXT NOT NULL,
    request_hash TEXT NOT NULL CHECK(length(request_hash) = 64 AND request_hash NOT GLOB '*[^0-9a-f]*'),
    expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision) = 'integer' AND expected_revision > 0),
    result_json TEXT NOT NULL CHECK(json_valid(result_json)),
    at_ms INTEGER NOT NULL,
    UNIQUE(simulation_id, actor_user_id, command_id)
);
CREATE TABLE simulation_trace_steps (
    trace_step_id TEXT PRIMARY KEY CHECK(length(trace_step_id) = 36),
    simulation_id TEXT NOT NULL REFERENCES simulation_meta(simulation_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(typeof(ordinal) = 'integer' AND ordinal >= 0),
    action TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    request_sha256 TEXT NOT NULL CHECK(length(request_sha256) = 64 AND request_sha256 NOT GLOB '*[^0-9a-f]*'),
    result_sha256 TEXT NOT NULL CHECK(length(result_sha256) = 64 AND result_sha256 NOT GLOB '*[^0-9a-f]*'),
    data_json TEXT NOT NULL CHECK(json_valid(data_json) AND length(CAST(data_json AS BLOB)) <= 4194304),
    UNIQUE(simulation_id, ordinal)
);
CREATE INDEX idx_simulation_commands_order
    ON simulation_commands(simulation_id, at_ms, command_id);
CREATE INDEX idx_simulation_trace_order
    ON simulation_trace_steps(simulation_id, ordinal);
CREATE TRIGGER simulation_source_pin_identity_immutable BEFORE UPDATE ON simulation_source_pins
WHEN NEW.simulation_id IS NOT OLD.simulation_id OR NEW.org_id IS NOT OLD.org_id
  OR NEW.owner_user_id IS NOT OLD.owner_user_id OR NEW.actor_user_id IS NOT OLD.actor_user_id
  OR NEW.definition_id IS NOT OLD.definition_id OR NEW.version IS NOT OLD.version
  OR NEW.model_sha256 IS NOT OLD.model_sha256 OR NEW.model_json IS NOT OLD.model_json
  OR NEW.selected_process_id IS NOT OLD.selected_process_id OR NEW.start_node_id IS NOT OLD.start_node_id
  OR NEW.acl_snapshot_json IS NOT OLD.acl_snapshot_json OR NEW.captured_at_ms IS NOT OLD.captured_at_ms
BEGIN SELECT RAISE(ABORT, 'simulation source pin is immutable'); END;
CREATE TRIGGER simulation_meta_identity_immutable BEFORE UPDATE ON simulation_meta
WHEN NEW.simulation_id IS NOT OLD.simulation_id OR NEW.scenario_sha256 IS NOT OLD.scenario_sha256
  OR NEW.profile IS NOT OLD.profile OR NEW.org_id IS NOT OLD.org_id
  OR NEW.actor_user_id IS NOT OLD.actor_user_id OR NEW.definition_id IS NOT OLD.definition_id
  OR NEW.version IS NOT OLD.version OR NEW.model_sha256 IS NOT OLD.model_sha256
  OR (OLD.instance_id IS NOT NULL AND NEW.instance_id IS NOT OLD.instance_id)
  OR NEW.created_at_ms IS NOT OLD.created_at_ms
BEGIN SELECT RAISE(ABORT, 'simulation identity is immutable'); END;
CREATE TRIGGER simulation_clock_monotonic BEFORE UPDATE ON simulation_clock
WHEN NEW.simulation_id IS NOT OLD.simulation_id OR NEW.start_ms IS NOT OLD.start_ms
  OR NEW.horizon_ms IS NOT OLD.horizon_ms OR NEW.tick_duration_ms IS NOT OLD.tick_duration_ms
  OR NEW.now_ms < OLD.now_ms OR NEW.now_ms > NEW.horizon_ms
  OR NEW.step_index <> OLD.step_index + 1 OR NEW.revision <> OLD.revision + 1
  OR (OLD.step_index = 0 AND NEW.now_ms <> OLD.now_ms)
  OR (OLD.step_index > 0 AND NEW.now_ms <> OLD.now_ms + OLD.tick_duration_ms)
BEGIN SELECT RAISE(ABORT, 'simulation clock must advance monotonically'); END;
"#;

pub(crate) fn initialize(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    verify_process_schema(conn)?;
    let marker_exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='simulation_schema_meta')",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1;
    if marker_exists {
        let version = conn.query_row(
            "SELECT schema_version FROM simulation_schema_meta",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        ensure!(
            version == SIMULATION_SCHEMA_VERSION,
            "unsupported simulation schema version {version}"
        );
        verify_schema(conn)?;
        return Ok(());
    }

    let tx = conn
        .unchecked_transaction()
        .context("begin simulation metadata schema transaction")?;
    tx.execute_batch(SCHEMA_DDL)
        .context("create simulation metadata schema")?;
    tx.execute(
        "INSERT INTO simulation_schema_meta(schema_version) VALUES (?1)",
        [SIMULATION_SCHEMA_VERSION],
    )?;
    tx.commit().context("commit simulation metadata schema")?;
    verify_schema(conn)
}

fn verify_process_schema(conn: &Connection) -> Result<()> {
    for table in EXPECTED_PROCESS_TABLES {
        let exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get::<_, i64>(0),
        )? == 1;
        ensure!(
            exists,
            "private simulation database is missing production table {table}"
        );
    }
    Ok(())
}

fn verify_schema(conn: &Connection) -> Result<()> {
    ensure!(
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))? == 1,
        "simulation metadata schema requires foreign keys"
    );
    for table in EXPECTED_TABLES {
        let exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get::<_, i64>(0),
        )? == 1;
        ensure!(
            exists,
            "simulation metadata schema is missing table {table}"
        );
    }
    let mut foreign_keys = conn.prepare("PRAGMA foreign_key_check")?;
    let violations = foreign_keys
        .query_map([], |row| {
            Ok(format!(
                "{} rowid={} parent={} fk={}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !violations.is_empty() {
        bail!(
            "simulation metadata schema foreign key violations: {}",
            violations.join("; ")
        );
    }
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(
        integrity == "ok",
        "simulation metadata schema integrity check: {integrity}"
    );
    Ok(())
}
