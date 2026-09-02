// ===== File: code_studio/db.rs — the instance content database of Code Studio =====
//
// The node-local half of Code Studio lives in `<instance data dir>/code_studio.db`
// (plan-01 §6): the vault and the provisioning saga state. Neither may travel
// through the Sync Ledger, and keeping them out of the main `tentaflow.db`
// removes them from the sync engine's reach by construction instead of by an
// entry it must remember not to add:
//
//   * `code_workspace_secrets` / `code_agent_credentials` (§5.2) — key
//     material encrypted with the PER-NODE SettingsCipher key. Replicated it
//     would be undecryptable at the far end anyway, so shipping it would only
//     widen the attack surface for no gain.
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

use crate::addon::app_db;
use crate::db::DbPool;

/// Package id of the Code Studio native app, as declared in `app-manifest.toml`.
pub const PACKAGE_ID: &str = "code-studio";

/// Schema steps, applied once each by `app_db::run_versioned_migrations`.
/// Append-only: a change to the content schema is a new step, never an edit.
const STEPS: &[(i64, &str)] = &[(
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
",
)];

/// Brings a content database up to date. Idempotent: the versioned runner
/// skips applied steps, so the install hook and every first open of the
/// process may call it.
pub fn migrate(conn: &Connection) -> Result<()> {
    app_db::run_versioned_migrations(conn, PACKAGE_ID, STEPS)
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

    #[test]
    fn migrate_is_idempotent_and_creates_the_content_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, STEPS.len() as i64);
        for table in [
            "code_workspace_saga_steps",
            "code_workspace_secrets",
            "code_agent_credentials",
        ] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "{table} must exist in the content database");
        }
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

        let fingerprint = vault::put_agent_credential(
            &local,
            &cipher,
            "org-1",
            "node-1",
            "claude-code",
            "sk-ant-example",
            "https://api.example.test",
            "u-admin",
        )
        .unwrap();
        let record = vault::get_agent_credential_record(&local, "org-1", "node-1", "claude-code")
            .unwrap()
            .expect("credential record");
        assert_eq!(record.fingerprint.as_deref(), Some(fingerprint.as_str()));
        assert_eq!(
            vault::delete_agent_credential(&local, "org-1", "node-1", "claude-code").unwrap(),
            1
        );
    }
}
