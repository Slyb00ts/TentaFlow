// ===== File: code_studio/db.rs — the instance content database of Code Studio =====
//
// The node-local half of Code Studio lives in `<instance data dir>/code_studio.db`
// (plan-01 §6): the vault and the provisioning saga state. Neither may travel
// through the Sync Ledger, and keeping them out of the main `tentaflow.db`
// removes them from the sync engine's reach by construction instead of by an
// entry it must remember not to add:
//
//   * `code_workspace_secrets` — key material encrypted with the PER-NODE
//     SettingsCipher key. Replicated it would be undecryptable at the far end
//     anyway, so shipping it would only widen the attack surface for no gain.
//     Provider credentials are NOT here: they belong to an account in the
//     `provider_accounts` registry, which is addressed by account id rather
//     than by (org, node, engine).
//   * `code_workspace_saga_steps` — the provisioning run state of ONE node's
//     saga. No other node can resume, retry or compensate it; the durable
//     outcome a remote UI needs (`status` + `status_detail`) travels on the
//     `code_workspaces` registry row in the main database.
//
// The database has no foreign keys outside itself: `workspace_id` values are
// handles into the registry, resolved by the callers. Deleting a workspace
// therefore removes the content rows here FIRST and the registry row second —
// a registry tombstone with live key material behind it would be a secret
// nobody can reach or revoke.
//
// The registry tables (`code_workspaces` and its satellites) stay in the main
// database; `session_assertion_jti` stays there too because the assertion
// dispatch verifies it without an app instance in hand.

use anyhow::Result;
use rusqlite::Connection;
use tracing::warn;

use crate::addon::app_db;
use crate::db::DbPool;

/// Package id of the Code Studio native app, as declared in `app-manifest.toml`.
pub const PACKAGE_ID: &str = "code-studio";

/// The rung that retires the node-local provider credentials. Named rather than
/// spelled twice because the drop rung and the "is the drop still pending?"
/// probe must agree: one of them drifting would report rows the other never
/// deletes, or delete rows nobody counted.
const CREDENTIAL_DROP_VERSION: i64 = 2;

/// Schema steps, applied once each by `app_db::run_versioned_migrations`.
/// Append-only: a change to the content schema is a new step, never an edit.
const STEPS: &[(i64, &str)] = &[
    (
        1,
        "
CREATE TABLE code_workspace_saga_steps (
    workspace_id TEXT NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','done','failed','compensated')),
    detail TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, step)
);
CREATE TABLE code_workspace_secrets (
    secret_ref TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('git_token','ssh_key')),
    material_enc BLOB NOT NULL,
    fingerprint TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    rotated_at TEXT,
    last_used_at TEXT
);
CREATE INDEX idx_code_workspace_secrets_ws
    ON code_workspace_secrets(workspace_id);
",
    ),
    // Provider credentials moved to the `provider_accounts` registry, which
    // keys them by account rather than by (org, node, engine). A node-local
    // copy would keep a second, unrevocable answer to "which key does this
    // engine run with" — dropping the table is what makes the registry the
    // only one. `IF EXISTS` because only a database created by step 1 before
    // this rung ever had it.
    (
        CREDENTIAL_DROP_VERSION,
        "DROP TABLE IF EXISTS code_agent_credentials;",
    ),
];

/// One `(node_id, engine_id)` group of the rows the drop rung retires. Two id
/// columns and a count, and nothing else: the material, its fingerprint and
/// `created_by` are precisely the fields that must not survive in a log.
struct CredentialGroup {
    node_id: String,
    engine_id: String,
    rows: i64,
}

/// Whether this database still holds the credentials the drop rung retires.
/// The probe has to answer BEFORE the runner, which is the only moment both
/// facts are still true — and it is what keeps a database that already went
/// through the rung from reporting a second time.
fn credential_drop_is_pending(conn: &Connection) -> Result<bool> {
    let holds_credentials: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name = 'code_agent_credentials'",
        [],
        |row| row.get(0),
    )?;
    if holds_credentials == 0 {
        return Ok(false);
    }
    // On a fresh file the runner has not created the version table yet, which
    // means no rung has been applied and every one of them is pending.
    let versioned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'table' AND name = 'app_schema_version'",
        [],
        |row| row.get(0),
    )?;
    if versioned == 0 {
        return Ok(true);
    }
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM app_schema_version",
        [],
        |row| row.get(0),
    )?;
    Ok(current < CREDENTIAL_DROP_VERSION)
}

/// How many credential rows each node and engine is about to lose. Read on
/// purpose: nothing adopts these rows into `provider_accounts`, so this count is
/// the only record of what the upgrade cost the operator.
fn credential_rows_about_to_drop(conn: &Connection) -> Result<Vec<CredentialGroup>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, engine_id, COUNT(*) FROM code_agent_credentials \
         GROUP BY node_id, engine_id ORDER BY node_id, engine_id",
    )?;
    let groups = stmt
        .query_map([], |row| {
            Ok(CredentialGroup {
                node_id: row.get(0)?,
                engine_id: row.get(1)?,
                rows: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(groups)
}

/// The sentences the drop rung writes, rendered separately from the logging
/// call so a test can read exactly what an operator would see — counts and the
/// two id columns, never any part of a credential.
fn dropped_credential_lines(groups: &[CredentialGroup]) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = groups
        .iter()
        .map(|group| {
            format!(
                "code-studio: dropping {} provider credential row(s) for node '{}', engine '{}' — \
                 this account is not migrated and needs a sign-in after the upgrade",
                group.rows, group.node_id, group.engine_id
            )
        })
        .collect();
    lines.push(format!(
        "code-studio: {} provider credential row(s) dropped in total; every adopted account \
         starts as 'requires sign-in'",
        groups.iter().map(|group| group.rows).sum::<i64>()
    ));
    lines
}

fn report_dropped_credentials(groups: &[CredentialGroup]) {
    for line in dropped_credential_lines(groups) {
        warn!("{line}");
    }
}

/// Brings a content database up to date. Idempotent: the versioned runner
/// skips applied steps, so the install hook and every first open of the
/// process may call it.
pub fn migrate(conn: &Connection) -> Result<()> {
    // Counted before the runner: the rung this reports on is a DROP, so
    // afterwards there is nothing left to count. Nothing is logged when the
    // runner fails, because then nothing was dropped either.
    let doomed = if credential_drop_is_pending(conn)? {
        credential_rows_about_to_drop(conn)?
    } else {
        Vec::new()
    };
    app_db::run_versioned_migrations(conn, PACKAGE_ID, STEPS)?;
    report_dropped_credentials(&doomed);
    Ok(())
}

/// Pool of the installed Code Studio instance's content database, opened on
/// first use. Code Studio is a singleton app of the default organization, so
/// the instance is resolved by package rather than carried through every
/// caller that never went through the app gate (provisioning threads, the
/// delegation adapter, the git broker).
pub fn pool(main_db: &DbPool) -> Result<DbPool> {
    let (_addon_id, pool) = app_db::open_for_package(
        main_db,
        crate::services::org::DEFAULT_ORG_ID,
        PACKAGE_ID,
        migrate,
    )?;
    Ok(pool)
}

/// An in-memory content database for tests of the code that writes to it:
/// the same schema as the instance file, without an installed instance.
#[cfg(test)]
pub(crate) fn test_pool() -> DbPool {
    let conn = Connection::open_in_memory().expect("in-memory content db");
    migrate(&conn).expect("content db schema");
    std::sync::Arc::new(crate::db::Db::from_connection(conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step 1 as it looked while it still created the credential table. The
    /// live rung no longer does, so the shape a pre-upgrade database actually
    /// has would otherwise be unreproducible — and the report would be tested
    /// against a table nothing ever produced. Split in two because the last
    /// test re-creates the credential table alone, on a database whose other
    /// rungs have already run.
    const FROZEN_STEP_1_SAGA_TABLE: &str = "
CREATE TABLE code_workspace_saga_steps (
    workspace_id TEXT NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','done','failed','compensated')),
    detail TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, step)
);
";

    const FROZEN_CREDENTIAL_TABLE: &str = "
CREATE TABLE code_agent_credentials (
    org_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    engine_id TEXT NOT NULL,
    material_enc BLOB NOT NULL,
    provider_base_url TEXT NOT NULL,
    fingerprint TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    rotated_at TEXT,
    last_used_at TEXT,
    PRIMARY KEY (org_id, node_id, engine_id)
);
";

    /// A content database that was created by the old step 1 and has not seen
    /// the drop rung: version 1 applied, credentials present.
    fn seed_pre_drop_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_schema_version (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO app_schema_version (version) VALUES (1);",
        )
        .unwrap();
        conn.execute_batch(FROZEN_STEP_1_SAGA_TABLE).unwrap();
        conn.execute_batch(FROZEN_CREDENTIAL_TABLE).unwrap();
        conn
    }

    /// The old table was keyed `(org_id, node_id, engine_id)`, so a single
    /// `(node, engine)` group reaches a count above one only across orgs — the
    /// org is not one of the two columns the report groups by.
    fn insert_credential(conn: &Connection, org: &str, node: &str, engine: &str, material: &str) {
        conn.execute(
            "INSERT INTO code_agent_credentials \
             (org_id, node_id, engine_id, material_enc, provider_base_url, fingerprint, \
              created_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'https://api.example', 'fp', 'u-owner', '2026-01-01')",
            rusqlite::params![org, node, engine, material.as_bytes()],
        )
        .unwrap();
    }

    fn groups(conn: &Connection) -> Vec<(String, String, i64)> {
        credential_rows_about_to_drop(conn)
            .unwrap()
            .into_iter()
            .map(|g| (g.node_id, g.engine_id, g.rows))
            .collect()
    }

    #[test]
    fn migrate_is_idempotent_and_creates_the_content_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, STEPS.len() as i64);
        for table in ["code_workspace_saga_steps", "code_workspace_secrets"] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "{table} must exist in the content database");
        }
        // Step 2 drops the node-local credential table. A database that went
        // through step 1 while it still created it is the only one that has
        // the table; a fresh install never does.
        let dropped: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'code_agent_credentials'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dropped, 0, "provider credentials belong to the registry");
        let index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
                 AND name = 'idx_code_workspace_secrets_ws'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn the_content_schema_holds_no_registry_table() {
        // The registry stays in the main database and is synchronised from
        // there; a copy here would be a second source of truth for the same
        // workspace.
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for table in [
            "code_workspaces",
            "code_workspace_members",
            "code_workspace_creator_grants",
            "code_workspace_project_links",
            "code_workspace_allowlist",
            "session_assertion_jti",
        ] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(present, 0, "{table} belongs to the main database");
        }
    }

    #[test]
    fn saga_steps_and_secrets_round_trip_through_the_moved_functions() {
        use crate::code_studio::models::SagaStepStatus;
        use crate::code_studio::repository;
        use crate::code_studio::vault::{self, SecretKind};

        let local = test_pool();
        repository::record_saga_step(&local, "ws-1", "repository", SagaStepStatus::Done, None)
            .unwrap();
        repository::record_saga_step(
            &local,
            "ws-1",
            "index",
            SagaStepStatus::Failed,
            Some("indexer unavailable"),
        )
        .unwrap();
        let steps = repository::list_saga_steps(&local, "ws-1").unwrap();
        assert_eq!(steps.len(), 2);
        assert!(repository::step_is_done(&local, "ws-1", "repository").unwrap());
        assert!(!repository::step_is_done(&local, "ws-1", "index").unwrap());

        let cipher = crate::crypto::SettingsCipher::new(&[5_u8; 32]);
        let stored = vault::put_workspace_secret(
            &local,
            &cipher,
            "ws-1",
            SecretKind::GitToken,
            "ghp_example",
            "u-owner",
        )
        .unwrap();
        let material = vault::get_workspace_secret(&local, &cipher, &stored.secret_ref).unwrap();
        assert_eq!(material.expose(), "ghp_example");
        assert_eq!(material.kind(), SecretKind::GitToken);
    }

    // -----------------------------------------------------------------------
    // The credential drop rung reports what it costs before it costs it
    // -----------------------------------------------------------------------

    #[test]
    fn the_drop_counts_every_row_it_retires_per_node_and_engine() {
        let conn = seed_pre_drop_database();
        insert_credential(&conn, "org-1", "node-a", "claude", "secret-alpha");
        insert_credential(&conn, "org-2", "node-a", "claude", "secret-beta");
        insert_credential(&conn, "org-1", "node-b", "codex", "secret-gamma");

        assert!(credential_drop_is_pending(&conn).unwrap());
        assert_eq!(
            groups(&conn),
            vec![
                ("node-a".to_string(), "claude".to_string(), 2),
                ("node-b".to_string(), "codex".to_string(), 1),
            ]
        );

        migrate(&conn).unwrap();

        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'code_agent_credentials'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(present, 0, "the rung must still drop the table");
        let current: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM app_schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current, CREDENTIAL_DROP_VERSION);
    }

    #[test]
    fn the_report_names_counts_and_ids_and_never_a_credential() {
        let conn = seed_pre_drop_database();
        insert_credential(&conn, "org-1", "node-a", "claude", "secret-alpha");
        insert_credential(&conn, "org-2", "node-a", "claude", "secret-beta");
        insert_credential(&conn, "org-1", "node-b", "codex", "secret-gamma");

        let lines = dropped_credential_lines(&credential_rows_about_to_drop(&conn).unwrap());
        let report = lines.join("\n");

        assert!(report
            .contains("dropping 2 provider credential row(s) for node 'node-a', engine 'claude'"));
        assert!(report
            .contains("dropping 1 provider credential row(s) for node 'node-b', engine 'codex'"));
        assert!(report.contains("3 provider credential row(s) dropped in total"));
        // The whole point of keeping the secret out: none of the three columns
        // a log must never carry appears in the message.
        for forbidden in [
            "secret-alpha",
            "secret-beta",
            "secret-gamma",
            "fp",
            "u-owner",
            "material_enc",
        ] {
            assert!(!report.contains(forbidden), "the report leaked {forbidden}");
        }
    }

    #[test]
    fn a_fresh_database_has_nothing_to_count_and_says_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        // The version table does not exist yet either — the probe must answer
        // without one rather than fail the whole first open.
        assert!(!credential_drop_is_pending(&conn).unwrap());
        migrate(&conn).unwrap();
        assert!(!credential_drop_is_pending(&conn).unwrap());
        assert!(dropped_credential_lines(&[]).is_empty());
    }

    #[test]
    fn an_already_migrated_database_does_not_report_a_second_time() {
        let conn = seed_pre_drop_database();
        insert_credential(&conn, "org-1", "node-a", "claude", "secret-alpha");
        migrate(&conn).unwrap();
        assert!(!credential_drop_is_pending(&conn).unwrap());

        // Every later open of the process calls `migrate` again; the rows are
        // long gone, so a second report would invent a loss that never happened.
        migrate(&conn).unwrap();
        assert!(!credential_drop_is_pending(&conn).unwrap());
    }

    #[test]
    fn a_credential_table_that_outlived_its_rung_is_neither_counted_nor_dropped() {
        let conn = seed_pre_drop_database();
        migrate(&conn).unwrap();
        // An operator who re-created the table by hand must not get a start-up
        // line about rows being dropped when the rung has already run and will
        // not run again.
        conn.execute_batch(FROZEN_CREDENTIAL_TABLE).unwrap();
        insert_credential(&conn, "org-1", "node-a", "claude", "secret-alpha");

        assert!(!credential_drop_is_pending(&conn).unwrap());
        migrate(&conn).unwrap();
        assert_eq!(
            groups(&conn),
            vec![("node-a".to_string(), "claude".to_string(), 1)]
        );
    }
}
