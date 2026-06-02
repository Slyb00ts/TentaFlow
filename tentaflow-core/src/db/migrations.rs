// =============================================================================
// Plik: db/migrations.rs
// Opis: Schemat bazy danych SQLite i mechanizm migracji wersjonowanych.
//       Po squashu wszystkich historycznych migracji 1..71 do jednego
//       `initial_schema (v1)` — kazda nowa instalacja dostaje czysty
//       finalny schemat w jednym kroku.
// =============================================================================

use anyhow::Result;
use rusqlite::Connection;
use tracing::info;

/// Migracje moga byc:
/// - `Sql` — zwykly batch SQL wykonany przez `execute_batch`
/// - `Rust` — funkcja, ktora dostaje `&Connection` w transakcji. Uzywana
///   gdy logika nie da sie zapisac jako pure SQL (np. row-by-row JSON
///   serializacja po stronie Rust).
pub enum MigrationStep {
    Sql(&'static str),
    Rust(fn(&Connection) -> Result<()>),
}

/// Uruchamia migracje bazy danych.
pub fn run(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS _migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
    ",
    )?;

    let current_version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM _migrations",
        [],
        |row| row.get(0),
    )?;

    for (version, name, step) in get_migrations() {
        if version > current_version {
            info!("Migracja {}: {}", version, name);
            let tx = conn.unchecked_transaction()?;
            match step {
                MigrationStep::Sql(sql) => tx.execute_batch(sql)?,
                MigrationStep::Rust(f) => f(&tx)?,
            }
            tx.execute(
                "INSERT INTO _migrations (version, name) VALUES (?1, ?2)",
                rusqlite::params![version, name],
            )?;
            tx.commit()?;
        }
    }

    Ok(())
}

fn get_migrations() -> Vec<(i64, &'static str, MigrationStep)> {
    vec![
        (1, "initial_schema", MigrationStep::Sql(INITIAL_SCHEMA)),
        (
            2,
            "flow_executions_allow_completed",
            MigrationStep::Sql(FLOW_EXECUTIONS_ALLOW_COMPLETED),
        ),
        (
            3,
            "deployments_full_columns",
            MigrationStep::Sql(DEPLOYMENTS_FULL_COLUMNS),
        ),
        (
            4,
            "flow_json_rename_edge_fields",
            MigrationStep::Sql(FLOW_JSON_RENAME_EDGE_FIELDS),
        ),
        (
            5,
            "services_progress_message",
            MigrationStep::Sql(SERVICES_PROGRESS_MESSAGE),
        ),
        (
            6,
            "flow_node_templates_params_schema",
            MigrationStep::Sql(FLOW_NODE_TEMPLATES_PARAMS_SCHEMA),
        ),
        (
            7,
            "audit_log_risk_class",
            MigrationStep::Sql(AUDIT_LOG_RISK_CLASS),
        ),
        (
            8,
            "model_alias_owners",
            MigrationStep::Sql(MODEL_ALIAS_OWNERS),
        ),
        (9, "alias_calls", MigrationStep::Sql(ALIAS_CALLS)),
        (
            10,
            "model_alias_changes",
            MigrationStep::Sql(MODEL_ALIAS_CHANGES),
        ),
        (
            11,
            "addon_migrations_applied",
            MigrationStep::Sql(ADDON_MIGRATIONS_APPLIED),
        ),
        (12, "frame_pickup_log", MigrationStep::Sql(FRAME_PICKUP_LOG)),
        (
            13,
            "teams_bot_aliases_ownership_backfill",
            MigrationStep::Sql(TEAMS_BOT_ALIASES_OWNERSHIP_BACKFILL),
        ),
        (
            14,
            "rename_alias_manage_to_read",
            MigrationStep::Sql(RENAME_ALIAS_MANAGE_TO_READ),
        ),
        (
            15,
            "model_alias_visibility",
            MigrationStep::Sql(MODEL_ALIAS_VISIBILITY),
        ),
        (
            16,
            "model_alias_consumers",
            MigrationStep::Sql(MODEL_ALIAS_CONSUMERS),
        ),
        (17, "model_visibility", MigrationStep::Sql(MODEL_VISIBILITY)),
        (18, "model_consumers", MigrationStep::Sql(MODEL_CONSUMERS)),
        (19, "addon_uses_alias", MigrationStep::Sql(ADDON_USES_ALIAS)),
        (20, "addon_uses_model", MigrationStep::Sql(ADDON_USES_MODEL)),
        (21, "cameras_table", MigrationStep::Sql(CAMERAS_TABLE)),
        (22, "recordings_table", MigrationStep::Sql(RECORDINGS_TABLE)),
        (
            23,
            "cameras_vendor_check_rtsp_onvif",
            MigrationStep::Sql(CAMERAS_VENDOR_CHECK_RTSP_ONVIF),
        ),
        (
            24,
            "frame_pickup_log_source_node_id",
            MigrationStep::Rust(frame_pickup_log_add_source_node_id),
        ),
        (
            25,
            "audit_log_merkle_chain",
            MigrationStep::Rust(audit_log_add_merkle_chain_columns),
        ),
        (
            26,
            "trusted_publishers",
            MigrationStep::Sql(TRUSTED_PUBLISHERS),
        ),
        (
            27,
            "addon_vector_namespaces",
            MigrationStep::Sql(ADDON_VECTOR_NAMESPACES),
        ),
        (28, "policy_claims", MigrationStep::Sql(POLICY_CLAIMS)),
        (29, "flow_invocations", MigrationStep::Sql(FLOW_INVOCATIONS)),
        (
            30,
            "cameras_onvif_metadata",
            MigrationStep::Rust(cameras_add_onvif_metadata_columns),
        ),
        (
            31,
            "flow_invocations_actor_user_id",
            MigrationStep::Rust(flow_invocations_add_actor_user_id),
        ),
        (
            32,
            "multi_tenant_rbac_org_isolation",
            MigrationStep::Rust(setup_multi_tenant),
        ),
        (
            33,
            "model_aliases_strategy_round_robin",
            MigrationStep::Rust(model_aliases_strategy_round_robin),
        ),
        (34, "gate_check_cache", MigrationStep::Sql(GATE_CHECK_CACHE)),
        (
            35,
            "cameras_metadata_supported",
            MigrationStep::Rust(cameras_add_metadata_supported_column),
        ),
        (
            36,
            "roles_add_camera_metadata",
            MigrationStep::Rust(roles_add_camera_metadata_permission),
        ),
        (
            37,
            "legal_documents",
            MigrationStep::Rust(create_legal_documents_table),
        ),
        (
            38,
            "backfill_admin_org_memberships",
            MigrationStep::Rust(backfill_admin_org_memberships),
        ),
        (39, "scheduled_jobs", MigrationStep::Sql(SCHEDULED_JOBS)),
        (
            40,
            "platform_locales",
            MigrationStep::Sql(PLATFORM_LOCALES_SCHEMA),
        ),
        (41, "role_catalog", MigrationStep::Sql(ROLE_CATALOG_SCHEMA)),
        (
            42,
            "sync_identity_registry",
            MigrationStep::Sql(SYNC_IDENTITY_REGISTRY),
        ),
        (
            43,
            "sync_permission_engine",
            MigrationStep::Sql(SYNC_PERMISSION_ENGINE),
        ),
        (44, "sync_policy", MigrationStep::Sql(SYNC_POLICY)),
        (
            45,
            "core_sync_captures",
            MigrationStep::Sql(CORE_SYNC_CAPTURES),
        ),
        (46, "kv_sync_captures", MigrationStep::Sql(KV_SYNC_CAPTURES)),
        (
            47,
            "blob_sync_captures",
            MigrationStep::Sql(BLOB_SYNC_CAPTURES),
        ),
        (
            48,
            "cameras_vendor_check_local_sources",
            MigrationStep::Sql(CAMERAS_VENDOR_CHECK_LOCAL_SOURCES),
        ),
        (
            49,
            "deployment_jobs_as_services",
            MigrationStep::Sql(DEPLOYMENT_JOBS_AS_SERVICES),
        ),
        (
            50,
            "compliance_core_foundation",
            MigrationStep::Rust(create_compliance_core_foundation),
        ),
        (
            51,
            "cameras_restore_org_id",
            MigrationStep::Rust(cameras_restore_org_id_column),
        ),
        (
            52,
            "services_deployed_source_hash",
            MigrationStep::Sql(SERVICES_DEPLOYED_SOURCE_HASH),
        ),
    ]
}

// The v48 `cameras` rebuild (CAMERAS_VENDOR_CHECK_LOCAL_SOURCES) recreated the
// table without the `org_id` column that v32 (setup_multi_tenant) had added,
// so every database that ran v48 lost tenant scoping on `cameras` and every
// camera_list/insert_camera query failed with "no such column: org_id". The
// v48 rebuild is fixed in place for fresh installs; this migration repairs
// databases already past v48. Idempotent: a fresh install (org_id present from
// the fixed v48) finds the column and skips the ALTER, only enforcing the
// backfill and index.
fn cameras_restore_org_id_column(conn: &Connection) -> Result<()> {
    add_org_id_column_if_missing(conn, "cameras")?;
    conn.execute(
        "UPDATE cameras SET org_id = 'org-default' WHERE org_id IS NULL",
        [],
    )?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_cameras_org_id ON cameras(org_id)")?;
    Ok(())
}

fn create_compliance_core_foundation(conn: &Connection) -> Result<()> {
    conn.execute_batch(COMPLIANCE_CORE_FOUNDATION)?;
    roles_add_permissions(
        conn,
        &["org_admin", "dpo"],
        &["compliance.read", "compliance.write"],
    )?;

    let mut stmt = conn.prepare("SELECT org_id FROM organizations WHERE status <> 'deleted'")?;
    let org_ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    for org_id in org_ids {
        crate::compliance::repository::ensure_org_defaults(conn, &org_id)?;
    }

    Ok(())
}

fn roles_add_permissions(
    conn: &Connection,
    role_names: &[&str],
    permissions: &[&str],
) -> Result<()> {
    for role_name in role_names {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT role_id, permissions_json FROM roles WHERE name = ?1",
                rusqlite::params![role_name],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        let Some((role_id, perms_json)) = row else {
            continue;
        };
        let mut existing: Vec<String> = match serde_json::from_str(&perms_json) {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!("rola '{role_name}' ma niepoprawne permissions_json");
                continue;
            }
        };
        let mut changed = false;
        for permission in permissions {
            if !existing.iter().any(|value| value == permission) {
                existing.push((*permission).to_string());
                changed = true;
            }
        }
        if changed {
            let updated = serde_json::to_string(&existing).unwrap_or(perms_json);
            conn.execute(
                "UPDATE roles SET permissions_json = ?1 WHERE role_id = ?2",
                rusqlite::params![updated, role_id],
            )?;
        }
    }
    Ok(())
}

// F2 P1.a follow-up — v32 (setup_multi_tenant) created the org_memberships
// table but did not seed entries for pre-existing admin users. As a result
// every legacy admin login resolves to `org_context=None` and every
// dispatch path requiring OrgContext (cameras, recordings, frame_url, ...)
// rejects the call. This step backfills every admin user from both legacy
// `users` (F1a auth) and `user_accounts` (F2 user mgmt) into `org-default`
// with role `role-org-admin`. Idempotent via `NOT EXISTS` guards.
fn backfill_admin_org_memberships(conn: &Connection) -> Result<()> {
    // Source 1: legacy `users` table (F1a). `role='admin'` is the only path
    // by which someone can mint a JWT pre-F2.
    conn.execute(
        "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
         SELECT 'org-default', CAST(u.id AS TEXT), 'role-org-admin', \
                strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'system' \
         FROM users u \
         WHERE u.role = 'admin' \
           AND NOT EXISTS ( \
               SELECT 1 FROM org_memberships m WHERE m.user_id = CAST(u.id AS TEXT) \
           )",
        [],
    )?;

    // Source 2: `user_accounts` table (F2 user mgmt). is_admin OR role='admin'.
    // Guarded by table-existence probe — F1a installs that never ran user
    // mgmt migrations skip this step cleanly.
    let user_accounts_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='user_accounts'",
        [],
        |r| r.get(0),
    )?;
    if user_accounts_exists > 0 {
        conn.execute(
            "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             SELECT 'org-default', CAST(u.id AS TEXT), 'role-org-admin', \
                    strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'system' \
             FROM user_accounts u \
             WHERE (u.is_admin = 1 OR u.role = 'admin') \
               AND NOT EXISTS ( \
                   SELECT 1 FROM org_memberships m WHERE m.user_id = CAST(u.id AS TEXT) \
               )",
            [],
        )?;
    }

    Ok(())
}

// F2 P8.a — RODO/GDPR document registry. Stores PDF artifacts generated per
// organization (short / standard / full variant). The `legal.read` and
// `legal.write` permission keys are already part of the v32 roles preseed
// (`org_admin`, `dpo`: read+write; `org_operator`, `org_viewer`: read only),
// so this migration adds storage only and does not touch `roles.permissions_json`.
//
// Row identity:
//   * `id`                       UUIDv4 minted by the repo (CHECK on shape)
//   * `org_id`                   tenant scope; ON DELETE RESTRICT — compliance
//                                retention forbids cascading on org delete
//                                (orgs are soft-deleted at the application layer)
//   * `variant`                  one of short|standard|full (CHECK)
//   * `generated_at`             unix epoch milliseconds
//   * `generated_by_user_id`     composite FK to org_memberships(org_id,user_id),
//                                ON DELETE RESTRICT — losing a membership must
//                                not erase the audit trail of generated documents
//   * `content_hash`             blake3 hex of the PDF bytes (64 lowercase hex, CHECK)
//   * `pdf_path`                 absolute path on disk
//   * `signed_url_ref`           HMAC ref published to the recording_url tier
//                                (NULL until the URL is minted)
//   * `revoked_at`               soft-delete timestamp; NULL = active
//
// The composite index supports the dominant query: list-by-org with newest
// rows first.
fn create_legal_documents_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS legal_documents (
            id TEXT NOT NULL PRIMARY KEY
                CHECK (length(id) = 36 AND id LIKE '________-____-____-____-____________'),
            org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE RESTRICT,
            variant TEXT NOT NULL CHECK (variant IN ('short','standard','full')),
            generated_at INTEGER NOT NULL,
            generated_by_user_id TEXT NOT NULL,
            content_hash TEXT NOT NULL
                CHECK (length(content_hash) = 64 AND content_hash GLOB '[0-9a-f]*'),
            pdf_path TEXT NOT NULL,
            signed_url_ref TEXT NULL,
            revoked_at INTEGER NULL,
            FOREIGN KEY (org_id, generated_by_user_id)
                REFERENCES org_memberships(org_id, user_id) ON DELETE RESTRICT
        );
        CREATE INDEX IF NOT EXISTS idx_legal_documents_org_id_generated_at
            ON legal_documents(org_id, generated_at DESC);
        "#,
    )?;
    Ok(())
}

// F2 P6.b — grant `camera.metadata` to operators so they can subscribe to
// ONVIF analytics streams. Admin already received this permission as part of
// the v32 seed; operator did not. Viewer is intentionally left alone — the
// subscription host fn mutates supervisor state (refcount + spawn pull task)
// and is therefore not a pure read operation.
//
// Idempotent: re-reads the permissions JSON, only appends when the entry is
// missing. Safe to run on a fresh DB (`org_operator` already exists from v32)
// and on an old DB where an admin manually edited the row.
fn roles_add_camera_metadata_permission(conn: &Connection) -> Result<()> {
    roles_add_permissions(conn, &["org_admin", "org_operator"], &["camera.metadata"])
}

// F2 P6.a — ONVIF metadata (Media2 + PullPoint events). The `cameras` table
// gains a boolean flag indicating whether the device exposes a metadata
// configuration that produces analytics events. Filled by the wizard when
// `GetMetadataConfigurations` returns a non-empty list; consumed by the
// upcoming event-pull supervisor to decide whether to subscribe.
fn cameras_add_metadata_supported_column(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(cameras)")?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "metadata_supported" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_col {
        conn.execute_batch(
            "ALTER TABLE cameras ADD COLUMN metadata_supported INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

// F2 P3 — reserved persistence table for gate_check decisions. The F2
// runtime keeps the cache purely in-memory (`services::policy::cache`); F3
// will gate-flip a feature flag that wires reads/writes through this table
// so cache state survives restarts. Shipping the schema now means the F3
// switch is a code change, not a migration.
const GATE_CHECK_CACHE: &str = r#"
CREATE TABLE IF NOT EXISTS gate_check_cache (
    claim_id TEXT NOT NULL,
    ctx_hash TEXT NOT NULL,
    result TEXT NOT NULL,
    cached_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (claim_id, ctx_hash)
);
CREATE INDEX IF NOT EXISTS idx_gate_cache_expires ON gate_check_cache(expires_at);
"#;

// v42 — Identity Registry dla Sync Ledger. `sync_nodes` opisuje techniczna
// tozsamosc node/device, `user_identity_keys` przechowuje kryptograficzne
// klucze uzytkownika, a `node_user_assignments` mapuje kto moze uzywac danego
// noda. `trusted_nodes` zostaje aktywnym store mesh trust; nowa tabela dostaje
// backfill jako warstwa administracyjna pod permissions/sync policy.
const SYNC_IDENTITY_REGISTRY: &str = r#"
CREATE TABLE IF NOT EXISTS sync_nodes (
    node_id TEXT PRIMARY KEY,
    public_key TEXT NOT NULL,
    public_key_type TEXT NOT NULL DEFAULT 'ed25519'
        CHECK(public_key_type IN ('ed25519','secp256k1')),
    display_name TEXT NOT NULL DEFAULT '',
    node_kind TEXT NOT NULL DEFAULT 'unknown'
        CHECK(node_kind IN ('unknown','phone','tablet','laptop','desktop','server','shared','authority')),
    trust_status TEXT NOT NULL DEFAULT 'untrusted'
        CHECK(trust_status IN ('untrusted','pending','trusted','revoked')),
    owner_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    sync_profile TEXT NOT NULL DEFAULT 'standard'
        CHECK(sync_profile IN ('standard','limited','authority','storage_only','ephemeral')),
    last_seen_at TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_sync_nodes_owner ON sync_nodes(owner_user_id);
CREATE INDEX IF NOT EXISTS idx_sync_nodes_trust ON sync_nodes(trust_status);
CREATE INDEX IF NOT EXISTS idx_sync_nodes_kind ON sync_nodes(node_kind);

CREATE TRIGGER IF NOT EXISTS sync_nodes_updated_at
AFTER UPDATE ON sync_nodes
FOR EACH ROW
BEGIN
    UPDATE sync_nodes
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE node_id = NEW.node_id;
END;

CREATE TABLE IF NOT EXISTS user_identity_keys (
    key_id TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    key_type TEXT NOT NULL CHECK(key_type IN ('ed25519','secp256k1')),
    public_key TEXT NOT NULL,
    purpose TEXT NOT NULL DEFAULT 'sync'
        CHECK(purpose IN ('auth','sync','admin','recovery')),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK(status IN ('active','revoked')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    revoked_at TEXT NULL,
    UNIQUE(user_id, key_type, public_key)
);

CREATE INDEX IF NOT EXISTS idx_user_identity_keys_user ON user_identity_keys(user_id, status);
CREATE INDEX IF NOT EXISTS idx_user_identity_keys_public ON user_identity_keys(key_type, public_key);

CREATE TABLE IF NOT EXISTS node_user_assignments (
    node_id TEXT NOT NULL REFERENCES sync_nodes(node_id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    assignment_mode TEXT NOT NULL
        CHECK(assignment_mode IN ('primary','allowed','shared_session','authority_operator')),
    valid_from TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    valid_until TEXT NULL,
    created_by INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY(node_id, user_id, assignment_mode)
);

CREATE INDEX IF NOT EXISTS idx_node_user_assignments_user ON node_user_assignments(user_id, valid_until);
CREATE INDEX IF NOT EXISTS idx_node_user_assignments_node ON node_user_assignments(node_id, valid_until);

INSERT OR IGNORE INTO sync_nodes (node_id, public_key, display_name, trust_status, created_at, updated_at)
SELECT node_id,
       public_key,
       COALESCE(NULLIF(hostname, ''), node_id),
       CASE WHEN is_active = 1 THEN 'trusted' ELSE 'untrusted' END,
       approved_at,
       approved_at
FROM trusted_nodes;
"#;

// v43 — fundament Permission Engine dla Sync Ledger. Tabele trzymaja
// minimalny, domenowo-neutralny opis zasobu i relacje dostepu, na ktorych
// pozniej opieraja sie polityki per addon oraz filtr outbox.
const SYNC_PERMISSION_ENGINE: &str = r#"
CREATE TABLE IF NOT EXISTS sync_user_org_profiles (
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    department_id TEXT NULL,
    manager_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    is_department_manager INTEGER NOT NULL DEFAULT 0 CHECK(is_department_manager IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY(org_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_sync_user_org_profiles_department
    ON sync_user_org_profiles(org_id, department_id);
CREATE INDEX IF NOT EXISTS idx_sync_user_org_profiles_manager
    ON sync_user_org_profiles(org_id, manager_user_id);

CREATE TRIGGER IF NOT EXISTS sync_user_org_profiles_updated_at
AFTER UPDATE ON sync_user_org_profiles
FOR EACH ROW
BEGIN
    UPDATE sync_user_org_profiles
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE org_id = NEW.org_id AND user_id = NEW.user_id;
END;

CREATE TABLE IF NOT EXISTS sync_resource_acl (
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    owner_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    assigned_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    department_id TEXT NULL,
    manager_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    visibility_scope TEXT NOT NULL DEFAULT 'assigned'
        CHECK(visibility_scope IN ('private','own','assigned','department','manager_subtree','explicit_share','all')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY(org_id, addon_id, resource_type, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_owner
    ON sync_resource_acl(org_id, owner_user_id);
CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_assigned
    ON sync_resource_acl(org_id, assigned_user_id);
CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_department
    ON sync_resource_acl(org_id, department_id);
CREATE INDEX IF NOT EXISTS idx_sync_resource_acl_manager
    ON sync_resource_acl(org_id, manager_user_id);

CREATE TRIGGER IF NOT EXISTS sync_resource_acl_updated_at
AFTER UPDATE ON sync_resource_acl
FOR EACH ROW
BEGIN
    UPDATE sync_resource_acl
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE org_id = NEW.org_id
      AND addon_id = NEW.addon_id
      AND resource_type = NEW.resource_type
      AND resource_id = NEW.resource_id;
END;

CREATE TABLE IF NOT EXISTS sync_explicit_shares (
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','node')),
    subject_id TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('read','write','sync_receive','admin')),
    granted_by INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    granted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    revoked_at TEXT NULL,
    PRIMARY KEY(org_id, addon_id, resource_type, resource_id, subject_type, subject_id, action)
);

CREATE INDEX IF NOT EXISTS idx_sync_explicit_shares_subject
    ON sync_explicit_shares(org_id, subject_type, subject_id, action, revoked_at);
CREATE INDEX IF NOT EXISTS idx_sync_explicit_shares_resource
    ON sync_explicit_shares(org_id, addon_id, resource_type, resource_id, revoked_at);
"#;

// v44 — Sync Policy. Polityka jest konfigurowana przez TentaFlow, nie przez
// addon. Najbardziej szczegolny wpis wygrywa: resource > resource_type > addon.
const SYNC_POLICY: &str = r#"
CREATE TABLE IF NOT EXISTS sync_policies (
    policy_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    resource_type TEXT NOT NULL DEFAULT '',
    resource_id TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL
        CHECK(mode IN ('local_only','replicated_by_permission','authority_readthrough','authority_write','sharded','ephemeral')),
    authority_node_id TEXT NULL REFERENCES sync_nodes(node_id) ON DELETE SET NULL,
    retention_days INTEGER NULL CHECK(retention_days IS NULL OR retention_days >= 0),
    is_enabled INTEGER NOT NULL DEFAULT 1 CHECK(is_enabled IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK (
        (mode IN ('authority_readthrough','authority_write') AND authority_node_id IS NOT NULL)
        OR (mode NOT IN ('authority_readthrough','authority_write'))
    ),
    CHECK (
        (resource_id = '')
        OR (resource_type <> '')
    ),
    UNIQUE(org_id, addon_id, resource_type, resource_id)
);

CREATE INDEX IF NOT EXISTS idx_sync_policies_lookup
    ON sync_policies(org_id, addon_id, resource_type, resource_id, is_enabled);
CREATE INDEX IF NOT EXISTS idx_sync_policies_authority
    ON sync_policies(authority_node_id, is_enabled);

CREATE TRIGGER IF NOT EXISTS sync_policies_updated_at
AFTER UPDATE ON sync_policies
FOR EACH ROW
BEGIN
    UPDATE sync_policies
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE policy_id = NEW.policy_id;
END;
"#;

const CORE_SYNC_CAPTURES: &str = r#"
CREATE TABLE IF NOT EXISTS __tentaflow_core_sync_captures (
    capture_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    table_name TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    primary_key TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('insert','update','delete')),
    changed_fields_blob BLOB NOT NULL,
    actor_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','ledgered','error')),
    operation_id TEXT NULL,
    error_message TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    ledgered_at_ms INTEGER NULL
);

CREATE INDEX IF NOT EXISTS idx_core_sync_captures_status
    ON __tentaflow_core_sync_captures(status, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_core_sync_captures_resource
    ON __tentaflow_core_sync_captures(org_id, resource_type, resource_id);
CREATE INDEX IF NOT EXISTS idx_core_sync_captures_operation
    ON __tentaflow_core_sync_captures(operation_id);
"#;

const KV_SYNC_CAPTURES: &str = r#"
CREATE TABLE IF NOT EXISTS __tentaflow_kv_sync_captures (
    capture_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    storage_key TEXT NOT NULL,
    action TEXT NOT NULL CHECK(action IN ('set','delete')),
    storage_value BLOB NULL,
    actor_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','ledgered','error')),
    operation_id TEXT NULL,
    error_message TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    ledgered_at_ms INTEGER NULL
);

CREATE INDEX IF NOT EXISTS idx_kv_sync_captures_status
    ON __tentaflow_kv_sync_captures(status, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_kv_sync_captures_resource
    ON __tentaflow_kv_sync_captures(org_id, addon_id, instance_id, storage_key);
CREATE INDEX IF NOT EXISTS idx_kv_sync_captures_operation
    ON __tentaflow_kv_sync_captures(operation_id);
"#;

const BLOB_SYNC_CAPTURES: &str = r#"
CREATE TABLE IF NOT EXISTS __tentaflow_blob_sync_captures (
    capture_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    blob_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    mime TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
    file_path TEXT NOT NULL,
    actor_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','ledgered','error')),
    operation_id TEXT NULL,
    error_message TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    ledgered_at_ms INTEGER NULL
);

CREATE INDEX IF NOT EXISTS idx_blob_sync_captures_status
    ON __tentaflow_blob_sync_captures(status, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_blob_sync_captures_sha
    ON __tentaflow_blob_sync_captures(org_id, sha256);
CREATE INDEX IF NOT EXISTS idx_blob_sync_captures_operation
    ON __tentaflow_blob_sync_captures(operation_id);
"#;

// F2 P2.a — formalise the legal value set for `model_aliases.strategy`.
// The initial schema declared the column without a CHECK constraint, so
// `round_robin` was already accepted by the storage layer (and the runtime
// `Strategy::from_db` parser maps the literal to `Strategy::RoundRobin`).
// This migration pins the contract: only `first_available` and `round_robin`
// are valid going forward. Any future strategy must edit both this CHECK
// and `services::catalog::Strategy::from_db`.
//
// SQLite cannot ALTER a CHECK constraint in place; we rebuild the table
// using the canonical pattern (CREATE new + INSERT SELECT + DROP old +
// RENAME + recreate index). Idempotent at three levels:
//   * If the live `model_aliases` already has a CHECK that accepts both
//     values (re-run after success), the rebuild repeats but produces an
//     equivalent schema — wasted work, never incorrect.
//   * `model_aliases_new` is dropped at the start so a half-run from a
//     previous attempt cannot collide on the staging table name.
//   * `DROP TABLE IF EXISTS` on the post-rename leftover keeps a partial
//     rerun (process killed between RENAME and index creation) recoverable.
fn model_aliases_strategy_round_robin(conn: &Connection) -> Result<()> {
    // Drop any leftover staging table from a previous interrupted run.
    conn.execute_batch("DROP TABLE IF EXISTS model_aliases_new;")?;

    // Create the rebuilt table with an explicit CHECK constraint. Column
    // order, types and defaults mirror the v1 schema (line 1174) so the
    // INSERT SELECT below is a straight column-for-column copy.
    conn.execute_batch(
        r#"
        CREATE TABLE model_aliases_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            alias TEXT UNIQUE NOT NULL,
            target_model TEXT NOT NULL,
            is_active INTEGER NOT NULL DEFAULT 1,
            fallback_targets TEXT DEFAULT NULL,
            strategy TEXT DEFAULT 'first_available'
                CHECK(strategy IN ('first_available','round_robin'))
        );
        INSERT INTO model_aliases_new (id, alias, target_model, is_active, fallback_targets, strategy)
            SELECT id, alias, target_model, is_active, fallback_targets,
                   COALESCE(strategy, 'first_available')
              FROM model_aliases;
        DROP TABLE model_aliases;
        ALTER TABLE model_aliases_new RENAME TO model_aliases;
        CREATE INDEX IF NOT EXISTS idx_model_aliases_alias ON model_aliases(alias);
        "#,
    )?;
    Ok(())
}

// F2 P1.a — multi-tenant foundation. Creates the three control tables
// (organizations, roles, org_memberships), seeds the `org-default` row + the
// five standard roles, then PRAGMA-guards eight existing tables to grow a
// nullable `org_id` column and backfills every pre-existing row to
// `org-default`. Idempotent at every step:
//   * CREATE TABLE / CREATE INDEX use IF NOT EXISTS.
//   * Seeds use INSERT OR IGNORE so a second run leaves the rows untouched.
//   * ADD COLUMN is gated by a PRAGMA table_info probe.
//   * Backfill UPDATE only touches rows where org_id IS NULL — a second run
//     finds zero such rows and is a no-op.
// Backfill is small enough (single SQLite UPDATE per table) to run inline;
// no batching is required because the migration runs inside the per-version
// transaction opened by `db::migrations::run`.
fn setup_multi_tenant(conn: &Connection) -> Result<()> {
    // Step 1: control tables.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS organizations (
            org_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            contact_email TEXT NULL,
            dpo_contact TEXT NULL,
            retention_policy_json TEXT NULL,
            status TEXT NOT NULL DEFAULT 'active'
                CHECK(status IN ('active','suspended','deleted')),
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_organizations_status ON organizations(status);
        CREATE INDEX IF NOT EXISTS idx_organizations_slug ON organizations(slug);

        CREATE TABLE IF NOT EXISTS roles (
            role_id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            permissions_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS org_memberships (
            org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
            user_id TEXT NOT NULL,
            role_id TEXT NOT NULL REFERENCES roles(role_id),
            granted_at TEXT NOT NULL,
            granted_by TEXT NOT NULL,
            PRIMARY KEY (org_id, user_id)
        );
        CREATE INDEX IF NOT EXISTS idx_org_memberships_user ON org_memberships(user_id);
        CREATE INDEX IF NOT EXISTS idx_org_memberships_role ON org_memberships(role_id);
        "#,
    )?;

    // Step 2: seed `org-default`. INSERT OR IGNORE keeps the migration
    // idempotent across re-runs and across operators who created the row by
    // hand before re-applying the migration.
    conn.execute(
        "INSERT OR IGNORE INTO organizations \
            (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status, created_at) \
         VALUES ('org-default', 'Default Organization', 'default', NULL, NULL, NULL, 'active', \
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
        [],
    )?;

    // Step 3: seed five preseed roles. Permission keys chosen to align with
    // the host-fn permission checks already in flight across the codebase
    // (camera.*, service.*, sql.*, vector.*, policy.*, gate.check, flow.invoke,
    // legal.*) plus the new RBAC-specific keys (org.*, user.*, addon.*,
    // rbac.elevate).
    let role_seeds: &[(&str, &str, &[&str])] = &[
        (
            "role-org-admin",
            "org_admin",
            &[
                "org.read",
                "org.write",
                "org.admin",
                "user.read",
                "user.write",
                "user.assign_role",
                "addon.install",
                "addon.upgrade",
                "addon.uninstall",
                "camera.read",
                "camera.write",
                "camera.discover",
                "camera.metadata",
                "service.read",
                "service.call",
                "sql.read",
                "sql.write",
                "vector.read",
                "vector.write",
                "policy.read",
                "policy.write",
                "gate.check",
                "flow.invoke",
                "legal.read",
                "legal.write",
                "rbac.elevate",
            ],
        ),
        (
            "role-org-operator",
            "org_operator",
            &[
                "org.read",
                "camera.read",
                "camera.write",
                "camera.discover",
                "service.read",
                "service.call",
                "sql.read",
                "vector.read",
                "policy.read",
                "gate.check",
                "flow.invoke",
                "legal.read",
            ],
        ),
        (
            "role-org-viewer",
            "org_viewer",
            &[
                "org.read",
                "camera.read",
                "service.read",
                "sql.read",
                "vector.read",
                "policy.read",
                "legal.read",
            ],
        ),
        (
            "role-dpo",
            "dpo",
            &[
                "org.read",
                "policy.read",
                "policy.write",
                "gate.check",
                "legal.read",
                "legal.write",
                "rbac.elevate",
            ],
        ),
        (
            "role-supervisor",
            "supervisor",
            &["org.read", "policy.read", "gate.check", "rbac.elevate"],
        ),
    ];
    for (role_id, name, perms) in role_seeds {
        let perms_json = serialize_perms_json(perms);
        conn.execute(
            "INSERT OR IGNORE INTO roles (role_id, name, permissions_json, created_at) \
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            rusqlite::params![role_id, name, perms_json],
        )?;
    }

    // Step 4 + 5 + 6: per-table ADD COLUMN (PRAGMA-guarded) + backfill +
    // index. The eight target tables hold every row that must scope to a
    // tenant from F2 onward. `addon_installations` lives under the name
    // `addons` in this schema (sole canonical addon registry — there is no
    // separate `addon_installations` table); we use the real name to avoid
    // creating a phantom column on a non-existent table.
    let tables: &[&str] = &[
        "users",
        "addons",
        "policy_claims",
        "cameras",
        "recordings",
        "addon_vector_namespaces",
        "audit_log",
        "frame_pickup_log",
    ];
    for table in tables {
        add_org_id_column_if_missing(conn, table)?;
        let sql_backfill = format!(
            "UPDATE {} SET org_id = 'org-default' WHERE org_id IS NULL",
            table
        );
        conn.execute(&sql_backfill, [])?;
        let sql_index = format!(
            "CREATE INDEX IF NOT EXISTS idx_{tbl}_org_id ON {tbl}(org_id)",
            tbl = table
        );
        conn.execute_batch(&sql_index)?;
    }

    Ok(())
}

// PRAGMA-guarded `ALTER TABLE <tbl> ADD COLUMN org_id TEXT NULL`. Idempotent:
// a second run finds the column already present and skips. The column is
// intentionally NULLABLE so the backfill UPDATE that follows is the single
// source of truth for the default-org assignment (an `NOT NULL DEFAULT
// 'org-default'` would silently mask a partial-fail backfill).
fn add_org_id_column_if_missing(conn: &Connection, table: &str) -> Result<()> {
    let pragma_sql = format!("PRAGMA table_info({})", table);
    let mut stmt = conn.prepare(&pragma_sql)?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "org_id" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_col {
        let alter_sql = format!("ALTER TABLE {} ADD COLUMN org_id TEXT NULL", table);
        conn.execute_batch(&alter_sql)?;
    }
    Ok(())
}

// Serialize a permission list to canonical JSON without pulling in
// serde_json's value tree. Permission keys never contain `"` or `\` (they
// are dot-delimited ASCII identifiers, validated implicitly by the host-fn
// permission checks elsewhere in the codebase), so a hand-rolled writer is
// safe and avoids the serde_json dependency at the migrations layer.
fn serialize_perms_json(perms: &[&str]) -> String {
    let mut out = String::from("[");
    for (i, p) in perms.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(p);
        out.push('"');
    }
    out.push(']');
    out
}

// F1c P7 — per-user audit attribution. Existing rows pre-dating this migration
// stay with NULL (system / unknown actor); WASM-driven flow invokes from this
// point forward carry `state.user_id` so DoD-9 / DoD-10 reports can join the
// row back to a concrete operator account. NULL remains valid for boot
// recovery, scheduled tasks, and mesh-originated invocations.
//
// SQLite has no `ADD COLUMN IF NOT EXISTS`. We probe PRAGMA table_info first
// so a partial reopen does not redo the ALTER.
fn flow_invocations_add_actor_user_id(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(flow_invocations)")?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "actor_user_id" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_col {
        conn.execute_batch(
            "ALTER TABLE flow_invocations ADD COLUMN actor_user_id INTEGER NULL \
                 REFERENCES users(id);",
        )?;
    }
    Ok(())
}

// F1c P6 — persist the ONVIF device-service URL and the profile token chosen
// at `camera_add_v1` time so the credentials-rotation path can re-derive the
// RTSP URI without forcing the operator to re-run discovery. Both columns
// are nullable: rows added under `vendor='rtsp'` (or pre-P6 ONVIF rows added
// by hand) keep NULL.
//
// SQLite has no `ADD COLUMN IF NOT EXISTS`. We read PRAGMA table_info first
// and skip the ALTER when the column already exists so the migration is
// idempotent across partial-run reopens.
fn cameras_add_onvif_metadata_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(cameras)")?;
    let mut rows = stmt.query([])?;
    let mut has_url = false;
    let mut has_token = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "onvif_url" {
            has_url = true;
        } else if name == "onvif_profile_token" {
            has_token = true;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_url {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN onvif_url TEXT NULL;")?;
    }
    if !has_token {
        conn.execute_batch("ALTER TABLE cameras ADD COLUMN onvif_profile_token TEXT NULL;")?;
    }
    Ok(())
}

// F1c P5 — runtime tracking of in-flight and historical flow invocations
// issued via flow_invoke_v1.
const FLOW_INVOCATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS flow_invocations (
    id TEXT PRIMARY KEY,
    addon_id TEXT NOT NULL,
    flow_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL,
    error TEXT,
    result_toml TEXT,
    operators_completed INTEGER NOT NULL DEFAULT 0,
    operators_total INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_flow_invocations_addon ON flow_invocations(addon_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_flow_invocations_running ON flow_invocations(status) WHERE status='running';
"#;

// F1c P4 — policy/claims engine tables. `policy_claims` records DPIA / FRIA
// approvals or legal grants issued by an administrator (via CLI). Each claim
// can be scoped globally (addon NULL) or narrowed to a specific addon and
// optionally a single namespace / alias id. Claims expire automatically
// (`valid_until`) and can be revoked at any time (`revoked_at` set non-NULL).
// `policy_claim_signatures` carries the multi-signer requirement — at least
// one signer per required role (typically DPO + supervisor) is enforced by
// the engine when verifying. `signature_b64` is optional: NULL means manual
// admin acknowledgment recorded via CLI; populated means a cryptographic
// Ed25519 signature exists alongside the manual approval (verified
// opportunistically — manual ack is the contract today, the signature is a
// future-proofed audit anchor).
const POLICY_CLAIMS: &str = r#"
CREATE TABLE IF NOT EXISTS policy_claims (
    claim_id TEXT PRIMARY KEY,
    claim_type TEXT NOT NULL,
    label TEXT NOT NULL,
    subject TEXT NULL,
    scope TEXT NULL,
    document_uri TEXT NULL,
    scope_addon_id TEXT NULL,
    scope_namespace TEXT NULL,
    valid_from TEXT NOT NULL,
    valid_until TEXT NOT NULL,
    revoked_at TEXT NULL,
    revoked_reason TEXT NULL,
    issued_by_user TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_policy_claims_type ON policy_claims(claim_type);
CREATE INDEX IF NOT EXISTS idx_policy_claims_scope ON policy_claims(scope_addon_id, scope_namespace);

CREATE TABLE IF NOT EXISTS policy_claim_signatures (
    claim_id TEXT NOT NULL REFERENCES policy_claims(claim_id) ON DELETE CASCADE,
    signer_role TEXT NOT NULL,
    signer_user TEXT NOT NULL,
    signed_at TEXT NOT NULL,
    signature_b64 TEXT NULL,
    PRIMARY KEY (claim_id, signer_role, signer_user)
);
CREATE INDEX IF NOT EXISTS idx_policy_claim_sig_claim ON policy_claim_signatures(claim_id);
"#;

// F1c P3 — per-addon per-namespace HNSW vector index registry. The on-disk
// HNSW file lives at `file_path` (`<HOME>/.tentaflow/addons/<addon_id>/vectors/
// <namespace>.usearch`); this table mirrors the (addon_id, namespace) pair to
// the on-disk file plus the index geometry (`dim`, `metric`) so the namespace
// can be reopened after process restart without consulting the manifest.
// `count` is a best-effort cache updated on each upsert/delete — the
// authoritative size lives inside usearch.
const ADDON_VECTOR_NAMESPACES: &str = r#"
CREATE TABLE IF NOT EXISTS addon_vector_namespaces (
    addon_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    dim INTEGER NOT NULL CHECK(dim >= 1 AND dim <= 4096),
    metric TEXT NOT NULL CHECK(metric IN ('cosine', 'euclidean', 'dot')),
    count INTEGER NOT NULL DEFAULT 0,
    file_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (addon_id, namespace)
);
CREATE INDEX IF NOT EXISTS idx_addon_vector_ns_addon ON addon_vector_namespaces(addon_id);
"#;

// F1c P2 — admin-managed allowlist of Ed25519 public keys that may sign
// addon UI bundles. `key_b64` is the canonical 44-char base64 form (32 raw
// bytes). The table is intentionally NOT seeded: an empty trust store means
// no externally-published UI addon installs until an operator explicitly
// runs `tentaflow-cli addon trust-key`. `added_by_user` is reserved for the
// future RBAC subject (F1c P7); NULL for keys added pre-RBAC or via CLI
// without an authenticated session.
const TRUSTED_PUBLISHERS: &str = r#"
CREATE TABLE IF NOT EXISTS trusted_publishers (
    key_b64 TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    added_at TEXT NOT NULL,
    added_by_user TEXT NULL,
    contact TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_trusted_publishers_label ON trusted_publishers(label);
"#;

// F1b P4 — Merkle hash chain for `audit_log` (DoD-15). Adds two BLOB columns:
//   - `prev_hash` (32 B) — copy of the previous row's `hash`, or NULL when
//     the chain has not started yet (pre-P4 legacy rows).
//   - `hash` (32 B) — `SHA256(canonical(row) || prev_hash)`.
//
// Existing F1a / pre-P4 rows keep NULL in both columns — `audit/verify.rs`
// counts them as `legacy_unchained` so a verify-after-upgrade run does not
// flag the entire pre-upgrade history as tampered. Every new row written
// through `audit_log_with_risk` after this migration MUST populate both
// columns.
//
// Idempotent via PRAGMA table_info — re-running the migration on a DB that
// already has the columns (e.g. partial earlier run that committed the
// _migrations row separately) is a no-op.
fn audit_log_add_merkle_chain_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(audit_log)")?;
    let mut rows = stmt.query([])?;
    let mut has_prev_hash = false;
    let mut has_hash = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "prev_hash" {
            has_prev_hash = true;
        } else if name == "hash" {
            has_hash = true;
        }
    }
    drop(rows);
    drop(stmt);

    if !has_prev_hash {
        conn.execute_batch("ALTER TABLE audit_log ADD COLUMN prev_hash BLOB NULL;")?;
    }
    if !has_hash {
        conn.execute_batch("ALTER TABLE audit_log ADD COLUMN hash BLOB NULL;")?;
    }
    Ok(())
}

// F1b P3.C-2 — add a nullable `source_node_id` column to `frame_pickup_log`
// so the pickup handler can record which peer's HMAC key validated the
// token (NULL when the token verified locally). The audit query
// "from which node was this frame fetched?" needs the column even though
// SQLite has no easy `ADD COLUMN IF NOT EXISTS` — we read PRAGMA
// table_info first and skip the ALTER when the column already exists so
// the migration is idempotent if a partial earlier run committed the
// _migrations row separately from the ALTER (or if an operator added
// the column out of band).
fn frame_pickup_log_add_source_node_id(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(frame_pickup_log)")?;
    let mut rows = stmt.query([])?;
    let mut has_col = false;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == "source_node_id" {
            has_col = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);
    if !has_col {
        conn.execute_batch("ALTER TABLE frame_pickup_log ADD COLUMN source_node_id TEXT NULL;")?;
    }
    Ok(())
}

// F1a M1.W8 — TentaVision recording manager registry. One row per artifact
// (snapshot PNG or segment MP4) saved by an addon via `recording_save_*_v1`
// host functions. `ref` is the public addon-facing identifier
// (`snap_<uuid>` / `clip_<uuid>`). `file_path` is the absolute on-disk
// location under `~/.tentaflow/recordings/<camera_id>/{snapshots,segments}/`.
// `hash_sha256` is content hash for integrity / dedup, `retention_class` is
// copied from `cameras.retention_class` at save time (audit chain). F1a does
// no automatic purge — `purged_at` is set by `recording_purge_v1`.
const RECORDINGS_TABLE: &str = r#"
CREATE TABLE recordings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ref TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('snapshot','segment')),
    owner_addon_id TEXT NOT NULL,
    camera_id TEXT NOT NULL,
    file_path TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    duration_ms INTEGER NULL,
    width INTEGER NULL,
    height INTEGER NULL,
    pixel_format TEXT NULL,
    hash_sha256 TEXT NOT NULL,
    retention_class TEXT NOT NULL CHECK(retention_class IN ('A','B','C','Unclassified')),
    created_at INTEGER NOT NULL,
    purged_at INTEGER NULL
);
CREATE UNIQUE INDEX idx_recordings_ref_active ON recordings(ref) WHERE purged_at IS NULL;
CREATE INDEX idx_recordings_owner ON recordings(owner_addon_id, purged_at);
CREATE INDEX idx_recordings_camera ON recordings(camera_id, purged_at);
"#;

// F1a M1.W6 — TentaVision camera ingest registry. One row per camera owned
// by an addon. F1a only supports `fake_file` vendor (mp4 loop via GStreamer
// filesrc). `credentials_encrypted` carries opaque AES-GCM blob for vendors
// that need auth (unused for fake_file). `fps_actual` + `last_frame_at`
// expose health snapshot without a separate timeseries table.
const CAMERAS_TABLE: &str = r#"
CREATE TABLE cameras (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    owner_addon_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    vendor TEXT NOT NULL CHECK(vendor IN ('fake_file')),
    url TEXT NOT NULL,
    credentials_encrypted BLOB NULL,
    profile TEXT NOT NULL DEFAULT 'default',
    target_fps INTEGER NOT NULL DEFAULT 30 CHECK(target_fps > 0 AND target_fps <= 60),
    resolution_width INTEGER NULL,
    resolution_height INTEGER NULL,
    retention_class TEXT NOT NULL DEFAULT 'C' CHECK(retention_class IN ('A','B','C','Unclassified')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK(status IN ('offline','online','error','starting','stopping')),
    status_message TEXT NULL,
    fps_actual REAL NULL,
    last_frame_at INTEGER NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed_at INTEGER NULL
);
CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);
"#;

// F1b P1.A — extend `cameras.vendor` CHECK to allow `rtsp` and `onvif` next to
// the existing `fake_file`. SQLite cannot ALTER a CHECK constraint in-place,
// so we rebuild the table: create `cameras_new` with the new CHECK, copy rows
// 1:1, drop the old table, rename, recreate indexes. Foreign keys are turned
// off during the rebuild (SQLite requirement for safe table swap) and
// re-enabled at the end. `DROP TABLE IF EXISTS cameras_new` guards against a
// partial earlier run leaving the scratch table behind.
const CAMERAS_VENDOR_CHECK_RTSP_ONVIF: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS cameras_new;

CREATE TABLE cameras_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    owner_addon_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif')),
    url TEXT NOT NULL,
    credentials_encrypted BLOB NULL,
    profile TEXT NOT NULL DEFAULT 'default',
    target_fps INTEGER NOT NULL DEFAULT 30 CHECK(target_fps > 0 AND target_fps <= 60),
    resolution_width INTEGER NULL,
    resolution_height INTEGER NULL,
    retention_class TEXT NOT NULL DEFAULT 'C' CHECK(retention_class IN ('A','B','C','Unclassified')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK(status IN ('offline','online','error','starting','stopping')),
    status_message TEXT NULL,
    fps_actual REAL NULL,
    last_frame_at INTEGER NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed_at INTEGER NULL
);

INSERT INTO cameras_new (
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at
)
SELECT
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at
FROM cameras;

DROP TABLE cameras;
ALTER TABLE cameras_new RENAME TO cameras;

CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);

PRAGMA foreign_keys = ON;
"#;

const CAMERAS_VENDOR_CHECK_LOCAL_SOURCES: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS cameras_new;

CREATE TABLE cameras_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    owner_addon_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    vendor TEXT NOT NULL CHECK(vendor IN ('fake_file', 'rtsp', 'onvif', 'local_camera', 'v4l2')),
    url TEXT NOT NULL,
    credentials_encrypted BLOB NULL,
    profile TEXT NOT NULL DEFAULT 'default',
    target_fps INTEGER NOT NULL DEFAULT 30 CHECK(target_fps > 0 AND target_fps <= 60),
    resolution_width INTEGER NULL,
    resolution_height INTEGER NULL,
    retention_class TEXT NOT NULL DEFAULT 'C' CHECK(retention_class IN ('A','B','C','Unclassified')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK(status IN ('offline','online','error','starting','stopping')),
    status_message TEXT NULL,
    fps_actual REAL NULL,
    last_frame_at INTEGER NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    removed_at INTEGER NULL,
    onvif_url TEXT NULL,
    onvif_profile_token TEXT NULL,
    metadata_supported INTEGER NOT NULL DEFAULT 0 CHECK(metadata_supported IN (0,1)),
    org_id TEXT NOT NULL DEFAULT 'org-default'
);

INSERT INTO cameras_new (
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at,
    onvif_url, onvif_profile_token, metadata_supported, org_id
)
SELECT
    id, camera_id, owner_addon_id, display_name, vendor, url,
    credentials_encrypted, profile, target_fps, resolution_width,
    resolution_height, retention_class, status, status_message,
    fps_actual, last_frame_at, created_at, updated_at, removed_at,
    onvif_url, onvif_profile_token, metadata_supported,
    COALESCE(org_id, 'org-default')
FROM cameras;

DROP TABLE cameras;
ALTER TABLE cameras_new RENAME TO cameras;

CREATE UNIQUE INDEX idx_cameras_camera_id_active ON cameras(camera_id) WHERE removed_at IS NULL;
CREATE INDEX idx_cameras_owner ON cameras(owner_addon_id, removed_at);
CREATE INDEX idx_cameras_status ON cameras(status, removed_at);
CREATE INDEX idx_cameras_org_id ON cameras(org_id);

PRAGMA foreign_keys = ON;
"#;

// F1a §6.6 v0.6.0 — readonly aliases per Chunk C decision. Permission was
// renamed from `alias.manage` (rollback removed CRUD ABI) to `alias.read`.
// Idempotent UPDATEs touch only rows whose string column literally stores
// `alias.manage`; `addon_declared_permissions.permission_type` uses the
// same string semantics as the other catalogs (manifest [[permission]].id).
const RENAME_ALIAS_MANAGE_TO_READ: &str = r#"
UPDATE addon_permissions
   SET permission_id = 'alias.read'
 WHERE permission_id = 'alias.manage';
UPDATE addon_permission_defaults
   SET permission_id = 'alias.read'
 WHERE permission_id = 'alias.manage';
UPDATE addon_permission_catalog
   SET permission_id = 'alias.read'
 WHERE permission_id = 'alias.manage';
UPDATE addon_declared_permissions
   SET permission_type = 'alias.read'
 WHERE permission_type = 'alias.manage';
"#;

// F1a §6.6 v0.6.0 Chunk C — per-alias visibility scope.
// Three levels: `private` (only owner addon may resolve), `restricted`
// (whitelist in `model_alias_consumers`), `public` (any addon may resolve).
// PK = alias_id (1:1 with model_aliases). Default `private` from manifest
// is set explicitly at install time; this CHECK has no DEFAULT so writes
// must declare visibility.
const MODEL_ALIAS_VISIBILITY: &str = r#"
CREATE TABLE model_alias_visibility (
    alias_id INTEGER PRIMARY KEY REFERENCES model_aliases(id) ON DELETE CASCADE,
    visibility TEXT NOT NULL CHECK(visibility IN ('private','restricted','public')),
    updated_at INTEGER NOT NULL,
    updated_by_user_id INTEGER NULL
);
"#;

// F1a §6.6 v0.6.0 Chunk C — explicit consumer whitelist for `restricted`
// aliases. Owner declares `allowed_consumers = [...]` in manifest; install
// writes one row per consumer with `granted_by_user_id = NULL` (auto from
// manifest). Admin can later add/remove rows via M16b. PK guarantees one
// row per (alias, consumer) pair.
const MODEL_ALIAS_CONSUMERS: &str = r#"
CREATE TABLE model_alias_consumers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias_id INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
    consumer_addon_id TEXT NOT NULL,
    granted_by_user_id INTEGER NULL,
    granted_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    UNIQUE(alias_id, consumer_addon_id)
);
CREATE INDEX idx_alias_consumers_lookup ON model_alias_consumers(consumer_addon_id, alias_id);
"#;

// F1a §6.6 v0.6.0 Chunk C — per-model visibility. Two levels only
// (`restricted` default, `public`). `model_id` is a free-form TEXT key —
// no FK because there is no `models` table in v0.6.0; the registry of
// "known model ids" lives in services + manual config. `restricted` is
// the default at the SQL layer so unknown models cannot be reached by
// addons without explicit grant.
const MODEL_VISIBILITY: &str = r#"
CREATE TABLE model_visibility (
    model_id TEXT PRIMARY KEY,
    visibility TEXT NOT NULL CHECK(visibility IN ('restricted','public')) DEFAULT 'restricted',
    updated_at INTEGER NOT NULL,
    updated_by_user_id INTEGER NULL
);
"#;

// F1a §6.6 v0.6.0 Chunk C — model consumer whitelist (symmetric to
// `model_alias_consumers`). `model_id` TEXT free-form (no FK).
const MODEL_CONSUMERS: &str = r#"
CREATE TABLE model_consumers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model_id TEXT NOT NULL,
    consumer_addon_id TEXT NOT NULL,
    granted_by_user_id INTEGER NULL,
    granted_at INTEGER NOT NULL,
    revoked_at INTEGER NULL,
    UNIQUE(model_id, consumer_addon_id)
);
CREATE INDEX idx_model_consumers_lookup ON model_consumers(consumer_addon_id, model_id);
"#;

// F1a §6.6 v0.6.0 Chunk C — consumer-side declaration `[[uses_alias]]`.
// `alias_target_name` stores the alias name (not id) because a consumer
// can declare its intent to use an alias BEFORE that alias' owner addon
// is installed; the row then stays `pending` until reconciliation runs
// at owner install time. Index `(alias_target_name, grant_status)` is
// hit by reconcile lookups; `(addon_id, grant_status)` by the resolver
// permission gate.
const ADDON_USES_ALIAS: &str = r#"
CREATE TABLE addon_uses_alias (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    alias_target_name TEXT NOT NULL,
    required INTEGER NOT NULL CHECK(required IN (0,1)),
    reason TEXT NOT NULL,
    grant_status TEXT NOT NULL CHECK(grant_status IN ('pending','granted','denied','auto_granted')),
    grant_decided_at INTEGER NULL,
    grant_decided_by_user_id INTEGER NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(addon_id, alias_target_name)
);
CREATE INDEX idx_addon_uses_alias_target ON addon_uses_alias(alias_target_name, grant_status);
CREATE INDEX idx_addon_uses_alias_addon ON addon_uses_alias(addon_id, grant_status);
"#;

// F1a §6.6 v0.6.0 Chunk C — consumer-side declaration `[[uses_model]]`.
// Same pending/reconcile pattern as `addon_uses_alias` but keyed on the
// free-form `model_id` string. `model_visibility` defaults to
// `restricted`, so unknown-model declarations stay `pending` until an
// admin explicitly grants (no auto-grant by absence of policy).
const ADDON_USES_MODEL: &str = r#"
CREATE TABLE addon_uses_model (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    model_target_name TEXT NOT NULL,
    required INTEGER NOT NULL CHECK(required IN (0,1)),
    reason TEXT NOT NULL,
    grant_status TEXT NOT NULL CHECK(grant_status IN ('pending','granted','denied','auto_granted')),
    grant_decided_at INTEGER NULL,
    grant_decided_by_user_id INTEGER NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(addon_id, model_target_name)
);
CREATE INDEX idx_addon_uses_model_target ON addon_uses_model(model_target_name, grant_status);
CREATE INDEX idx_addon_uses_model_addon ON addon_uses_model(addon_id, grant_status);
"#;

// After M1.W5: teams-bot declares aliases via [[alias]] manifest section.
// Hard-coded TEAMS_BOT_ALIASES const and activate/deactivate helpers were
// removed from addon/mod.rs. This migration backfills owner records for
// existing teams-bot aliases on already-deployed databases so the new
// owner-aware code path treats them correctly (start/stop activate/
// deactivate, uninstall preserves owner row for audit trail).
const TEAMS_BOT_ALIASES_OWNERSHIP_BACKFILL: &str = r#"
INSERT OR IGNORE INTO model_alias_owners (alias_id, owner_type, owner_id, created_at)
SELECT id, 'addon', 'teams-bot', datetime('now')
FROM model_aliases
WHERE alias IN ('teams-stt', 'teams-tts', 'teams-summary', 'teams-vision-face', 'teams-vision-emotion');
"#;

// F1a §6.5 — tabela powiazania aliasu z wlascicielem (addon lub manual).
// Pozwala odroznic aliasy stworzone automatycznie przez install addonu od
// tych wpisanych recznie przez admina (M1.W5 zacznie ja zasilac).
const MODEL_ALIAS_OWNERS: &str = r#"
CREATE TABLE model_alias_owners (
    alias_id INTEGER PRIMARY KEY REFERENCES model_aliases(id) ON DELETE CASCADE,
    owner_type TEXT NOT NULL CHECK(owner_type IN ('addon', 'manual')),
    owner_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_alias_owners_addon ON model_alias_owners(owner_type, owner_id);
"#;

// F1a §6.5 — log wywolan aliasow AI. Kazdy alias_call (M1.W6) zapisuje
// rekord z target_used, request_id, fallback_chain_position; pozwala na
// debug fallback chain w UI M16 i metryki per alias.
const ALIAS_CALLS: &str = r#"
CREATE TABLE alias_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias_id INTEGER NOT NULL REFERENCES model_aliases(id) ON DELETE CASCADE,
    alias_name TEXT NOT NULL,
    method TEXT,
    target_used TEXT NOT NULL,
    target_node_id TEXT,
    service_id TEXT,
    caller_addon_id TEXT,
    caller_user_id INTEGER,
    request_id TEXT,
    duration_ms INTEGER,
    payload_bytes INTEGER,
    response_bytes INTEGER,
    fallback_used INTEGER DEFAULT 0,
    fallback_chain_position INTEGER,
    result TEXT NOT NULL CHECK(result IN ('ok','error','no_target','timeout','permission_denied','gate_denied')),
    error_code TEXT,
    ts INTEGER NOT NULL
);
CREATE INDEX idx_alias_calls_alias_ts ON alias_calls(alias_id, ts);
CREATE INDEX idx_alias_calls_addon_ts ON alias_calls(caller_addon_id, ts);
CREATE INDEX idx_alias_calls_request_id ON alias_calls(request_id);
CREATE INDEX idx_alias_calls_fallback ON alias_calls(alias_id, fallback_used) WHERE fallback_used=1;
"#;

// F1a §6.5 — historia zmian aliasu (before/after snapshot, change_type,
// reason). UI M16 (alias detail panel) pokazuje audit trail; admin moze
// rollback przez wstawienie nowego rekordu z before_snapshot.
// Brak FK na model_aliases — alias mogl byc juz usuniety, ale historia
// musi pozostac (compliance F1a §6.2.Y).
const MODEL_ALIAS_CHANGES: &str = r#"
CREATE TABLE model_alias_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias_id INTEGER NOT NULL,
    alias_name TEXT NOT NULL,
    changed_by_user_id INTEGER,
    changed_by_addon_id TEXT,
    before_snapshot TEXT,
    after_snapshot TEXT,
    change_type TEXT NOT NULL CHECK(change_type IN
        ('create','target_change','fallback_change','strategy_change',
         'activate','deactivate','delete','suggested_default_change')),
    reason TEXT,
    ts INTEGER NOT NULL
);
CREATE INDEX idx_alias_changes_alias ON model_alias_changes(alias_id);
CREATE INDEX idx_alias_changes_user_ts ON model_alias_changes(changed_by_user_id, ts);
"#;

// F1a §6.5 — wykonanie migracji per-addon SQL storage. PRIMARY KEY
// (addon_id, migration_name) zapewnia idempotencje. Hash chroni przed
// "podmiana" tresci migracji po jej aplikacji.
const ADDON_MIGRATIONS_APPLIED: &str = r#"
CREATE TABLE addon_migrations_applied (
    addon_id TEXT NOT NULL,
    migration_name TEXT NOT NULL,
    migration_hash TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (datetime('now')),
    applied_in_addon_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('success', 'failed', 'partial')),
    error_message TEXT,
    duration_ms INTEGER,
    PRIMARY KEY (addon_id, migration_name)
);
CREATE INDEX idx_addon_migrations_status ON addon_migrations_applied(addon_id, status);
"#;

// F1a §6.5 — log pickupow surowych ramek (frame_ref) przez serwisy AI.
// Token zawarty w frame_ref ma TTL; rdzen weryfikuje go przy pickupie
// i loguje wynik (ok / token_invalid / token_expired / frame_purged /
// unauthorized). UI compliance M22 pokazuje time-to-pickup.
const FRAME_PICKUP_LOG: &str = r#"
CREATE TABLE frame_pickup_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    raw_frame_ref TEXT NOT NULL,
    service_id TEXT NOT NULL,
    caller_addon_id TEXT,
    request_id TEXT NOT NULL,
    picked_up_at INTEGER NOT NULL,
    result TEXT NOT NULL CHECK(result IN ('ok','token_invalid','token_expired','frame_purged','unauthorized'))
);
CREATE INDEX idx_frame_pickup_ref ON frame_pickup_log(raw_frame_ref);
CREATE INDEX idx_frame_pickup_request ON frame_pickup_log(request_id);
CREATE INDEX idx_frame_pickup_service_ts ON frame_pickup_log(service_id, picked_up_at);
"#;

// Rozszerzenie audit_log o pola wymagane przez F1a §6.2.Y:
// - risk_class — klasyfikacja RODO (A/B/C/unclassified); wpisy klasy B/C maja
//   indeks partial dla szybkich kwerend zgodnosciowych.
// - related_claim_id — powiazanie wpisu z claim (gate evaluation, F2).
// - request_id — korelacja wielu wpisow w obrebie jednego wywolania service_call
//   lub spans flow execution.
// SQLite nie wspiera CHECK przy ALTER TABLE — walidacja po stronie Rust w
// audit/mod.rs (RiskClass enum).
const AUDIT_LOG_RISK_CLASS: &str = r#"
ALTER TABLE audit_log ADD COLUMN risk_class TEXT NOT NULL DEFAULT 'unclassified';
ALTER TABLE audit_log ADD COLUMN related_claim_id TEXT;
ALTER TABLE audit_log ADD COLUMN request_id TEXT;
CREATE INDEX idx_audit_risk_class ON audit_log(risk_class) WHERE risk_class IN ('B','C');
CREATE INDEX idx_audit_claim ON audit_log(related_claim_id) WHERE related_claim_id IS NOT NULL;
CREATE INDEX idx_audit_request_id ON audit_log(request_id);
"#;

// params_schema: JSON-Schema-like opis pol konfiguracyjnych per node type.
// GUI flow builder rendere dynamic form z tej deklaracji (typ string z enum
// → select, number z range → slider, boolean → toggle, format=textarea →
// textarea, type=model_picker z `category` → dynamic dropdown z
// model_registry filtrowane po category). Bez tej kolumny config tab w
// builderze byl pusty bo wczytywal `template.params_schema` ktore byl
// undefined.
const FLOW_NODE_TEMPLATES_PARAMS_SCHEMA: &str = r#"
ALTER TABLE flow_node_templates ADD COLUMN params_schema TEXT;
"#;

// progress_message: krotki status text aktualizowany przez supervisor /
// detached deploy task podczas Starting (np. "warming up — alive 30s,
// waiting for /v1/models"). GUI snapshot pokazuje obok statusu, zeby
// user widzial PROGRES startu serwisu (vLLM cold start ~3 min, klient
// inaczej widzi tylko "Starting" przez kilka minut bez feedbacku).
//
// Health_last_err zostaje DEDYKOWANE dla bledow zdrowia (failed probe).
// Progress_message jest informacyjne, NULL gdy nic do powiedzenia.
const SERVICES_PROGRESS_MESSAGE: &str = r#"
ALTER TABLE services ADD COLUMN progress_message TEXT;
"#;

// deployed_source_hash: sha256 drzewa zrodel bundla (docker/native) z momentu
// deployu. build.rs liczy aktualny hash do manifestu; snapshot porownuje oba i
// wystawia flage update_available. Pusty = serwis embedded/external lub deploy
// sprzed tej kolumny (brak danych, brak falszywego alarmu o aktualizacji).
const SERVICES_DEPLOYED_SOURCE_HASH: &str = r#"
ALTER TABLE services ADD COLUMN deployed_source_hash TEXT NOT NULL DEFAULT '';
"#;

// Rename edge fieldow w flow_json: `from`/`to` -> `from_node`/`to_node`.
// GUI canvas (flows-builder/canvas.js) oczekuje `from_node`/`to_node`, seed
// historycznie pisal `from`/`to`. Bez tego edytor flow pokazuje nodes bez
// polaczen i flow zachowuje sie jakby byl pojedynczym blokiem.
// `replace()` w SQLite jest binarnie bezpieczny i podmienia substring;
// ograniczamy do flows.flow_json + flow_versions.flow_json zeby nie tknac
// settings/config z innymi `"from":` (np. mail, oauth).
const FLOW_JSON_RENAME_EDGE_FIELDS: &str = r#"
UPDATE flows
   SET flow_json = replace(replace(flow_json, '{"from":', '{"from_node":'), ',"to":', ',"to_node":')
 WHERE flow_json LIKE '%"edges"%';
UPDATE flow_versions
   SET flow_json = replace(replace(flow_json, '{"from":', '{"from_node":'), ',"to":', ',"to_node":')
 WHERE flow_json LIKE '%"edges"%';
"#;

// Squashed v1 mial uproszczona schema deployments (brak: deploy_id unique,
// node_id, phase, progress_pct, image_tag, container_name, user_id; pole
// `error_text` zamiast `error_message`). Repository i log_bus pisza do
// pelnego zestawu kolumn — bez tego startup czysci stale rows wybucha
// "no such column: error_message" i kazdy deploy progress update padl
// niewidocznie. deployments to log historii — drop+recreate akceptowalne.
const DEPLOYMENTS_FULL_COLUMNS: &str = r#"
DROP INDEX IF EXISTS idx_deployments_slug;
DROP TABLE IF EXISTS deployments;
CREATE TABLE deployments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    deploy_id TEXT NOT NULL UNIQUE,
    engine_id TEXT NOT NULL,
    deploy_method TEXT NOT NULL,
    node_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'deploying',
    phase TEXT NOT NULL DEFAULT '',
    progress_pct INTEGER NOT NULL DEFAULT 0,
    image_tag TEXT NOT NULL DEFAULT '',
    container_name TEXT NOT NULL DEFAULT '',
    config_json TEXT NOT NULL DEFAULT '{}',
    user_id INTEGER,
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TIMESTAMP,
    error_message TEXT,
    log_tail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_deployments_deploy_id ON deployments(deploy_id);
CREATE INDEX idx_deployments_engine ON deployments(engine_id);
"#;

const DEPLOYMENT_JOBS_AS_SERVICES: &str = r#"
CREATE TABLE services_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engine_id TEXT NOT NULL,
    category TEXT NOT NULL,
    display_name TEXT NOT NULL,
    deploy_method TEXT NOT NULL CHECK(deploy_method IN ('docker','native_embedded','native_binary','native_python_bundle','external')),
    transport TEXT NOT NULL CHECK(transport IN ('embedded','http_direct','sidecar_quic','external_http')),
    status TEXT NOT NULL CHECK(status IN ('deploying','starting','running','degraded','failed','stopped','interrupted')) DEFAULT 'starting',
    pinned INTEGER NOT NULL DEFAULT 0,
    paused INTEGER NOT NULL DEFAULT 0,
    runtime_pid INTEGER,
    runtime_port INTEGER,
    sidecar_quic_port INTEGER,
    endpoint_url TEXT,
    config_json TEXT NOT NULL DEFAULT '{}',
    active_deploy_id TEXT NOT NULL DEFAULT '',
    last_deploy_id TEXT NOT NULL DEFAULT '',
    deployment_progress_pct INTEGER NOT NULL DEFAULT 0,
    health_last_ok TIMESTAMP,
    health_last_err TEXT,
    progress_message TEXT,
    restart_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO services_new (
    id, engine_id, category, display_name, deploy_method, transport, status,
    pinned, paused, runtime_pid, runtime_port, sidecar_quic_port, endpoint_url,
    config_json, active_deploy_id, last_deploy_id, deployment_progress_pct,
    health_last_ok, health_last_err, progress_message,
    restart_count, created_at, updated_at
)
SELECT
    id, engine_id, category, display_name, deploy_method, transport, status,
    pinned, paused, runtime_pid, runtime_port, sidecar_quic_port, endpoint_url,
    config_json, '', '', 0, health_last_ok, health_last_err, progress_message,
    restart_count, created_at, updated_at
FROM services;

DROP TABLE services;
ALTER TABLE services_new RENAME TO services;
CREATE INDEX idx_services_status ON services(status);
CREATE INDEX idx_services_engine ON services(engine_id);
CREATE INDEX idx_services_category ON services(category);
CREATE INDEX idx_services_active_deploy ON services(active_deploy_id);

ALTER TABLE deployments ADD COLUMN target_service_id INTEGER;
ALTER TABLE deployments ADD COLUMN resume_policy TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE deployments ADD COLUMN resume_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE deployments ADD COLUMN updated_at TIMESTAMP NOT NULL DEFAULT '';

UPDATE deployments
   SET status = CASE status
       WHEN 'queued' THEN 'deploying'
       WHEN 'building' THEN 'deploying'
       WHEN 'running' THEN 'deploying'
       WHEN 'failure' THEN 'failed'
       ELSE status
   END,
       updated_at = COALESCE(finished_at, started_at, datetime('now'));

CREATE INDEX IF NOT EXISTS idx_deployments_status ON deployments(status);
CREATE INDEX IF NOT EXISTS idx_deployments_node ON deployments(node_id);
CREATE INDEX IF NOT EXISTS idx_deployments_target_service ON deployments(target_service_id);
"#;

// SQLite nie pozwala na ALTER TABLE dla CHECK constraintu — robimy klasyczne
// rebuild-via-temp-table. flow_executions to log historii, mozna stracic
// rzedy ktore i tak juz padly na CHECK (status='completed' nigdy do bazy
// nie trafil).
const FLOW_EXECUTIONS_ALLOW_COMPLETED: &str = r#"
CREATE TABLE flow_executions_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id),
    request_id TEXT,
    model TEXT,
    started_at TEXT,
    finished_at TEXT,
    status TEXT CHECK(status IN ('running','success','completed','error','cancelled')),
    execution_log TEXT,
    total_latency_ms INTEGER,
    total_tokens INTEGER
);
INSERT INTO flow_executions_new (id, flow_id, request_id, model, started_at, finished_at, status, execution_log, total_latency_ms, total_tokens)
    SELECT id, flow_id, request_id, model, started_at, finished_at, status, execution_log, total_latency_ms, total_tokens FROM flow_executions;
DROP TABLE flow_executions;
ALTER TABLE flow_executions_new RENAME TO flow_executions;
CREATE INDEX idx_flow_executions_flow ON flow_executions(flow_id);
CREATE INDEX idx_flow_executions_status ON flow_executions(status);
"#;

const INITIAL_SCHEMA: &str = r#"
CREATE TABLE api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key_hash TEXT NOT NULL UNIQUE,
    key_prefix TEXT NOT NULL,
    name TEXT NOT NULL,
    rate_limit_rps INTEGER NOT NULL DEFAULT 100,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_used_at TEXT,
    owner_user_id INTEGER
);
CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix);
CREATE INDEX idx_apikeys_owner ON api_keys(owner_user_id);

CREATE TABLE service_aliases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias TEXT NOT NULL UNIQUE,
    target_service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE
);
CREATE INDEX idx_service_aliases_target ON service_aliases(target_service_id);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'admin' CHECK(role IN ('admin','viewer')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_login_at TEXT,
    must_change_password INTEGER NOT NULL DEFAULT 1,
    preferred_language TEXT
);

CREATE TABLE model_aliases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    alias TEXT UNIQUE NOT NULL,
    target_model TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    fallback_targets TEXT DEFAULT NULL,
    strategy TEXT DEFAULT 'first_available'
);
CREATE INDEX idx_model_aliases_alias ON model_aliases(alias);

CREATE TABLE flows (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT,
    version INTEGER DEFAULT 1,
    is_default INTEGER NOT NULL DEFAULT 0,
    service_type TEXT,
    flow_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','decoded')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    published_model_name TEXT NULL
);
CREATE INDEX idx_flows_status ON flows(status);
CREATE INDEX idx_flows_service_type ON flows(service_type);
CREATE INDEX idx_flows_default_lookup ON flows(is_default, service_type, status);
CREATE UNIQUE INDEX idx_flows_published_model_name
    ON flows(published_model_name)
    WHERE published_model_name IS NOT NULL;

CREATE TABLE flow_model_bindings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    model_pattern TEXT NOT NULL UNIQUE,
    priority INTEGER DEFAULT 0
);
CREATE INDEX idx_flow_model_bindings_flow ON flow_model_bindings(flow_id);
CREATE INDEX idx_flow_model_bindings_priority ON flow_model_bindings(flow_id, priority);

CREATE TABLE flow_node_templates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_type TEXT NOT NULL,
    category TEXT NOT NULL CHECK(category IN ('trigger','service','transform','logic','output')),
    label TEXT NOT NULL,
    description TEXT,
    default_config TEXT NOT NULL DEFAULT '{}',
    icon TEXT
);
CREATE INDEX idx_flow_node_templates_category ON flow_node_templates(category);
CREATE UNIQUE INDEX idx_flow_node_templates_type_unique ON flow_node_templates(node_type);

CREATE TABLE pii_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    category TEXT NOT NULL,
    pattern TEXT NOT NULL,
    replacement TEXT NOT NULL DEFAULT '[UKRYTY]',
    is_active INTEGER NOT NULL DEFAULT 1,
    priority INTEGER DEFAULT 0,
    description TEXT,
    test_examples TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_pii_rules_active ON pii_rules(is_active, priority);
CREATE UNIQUE INDEX idx_pii_rules_name_unique ON pii_rules(name);

CREATE TABLE fast_path_patterns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    module TEXT NOT NULL,
    pattern_type TEXT NOT NULL,
    pattern TEXT NOT NULL,
    match_type TEXT NOT NULL DEFAULT 'exact' CHECK(match_type IN ('exact','starts_with','contains','regex','length')),
    result_json TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    priority INTEGER DEFAULT 0
);
CREATE INDEX idx_fast_path_module ON fast_path_patterns(module, pattern_type);
CREATE INDEX idx_fast_path_active_module ON fast_path_patterns(module, is_active, priority);
CREATE UNIQUE INDEX idx_fast_path_module_pattern_unique ON fast_path_patterns(module, pattern_type, pattern);

CREATE TABLE tts_cleaning_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    rule_type TEXT NOT NULL CHECK(rule_type IN ('abbreviation','phonetic','emoji_range','regex_remove')),
    pattern TEXT NOT NULL,
    replacement TEXT,
    language TEXT NOT NULL DEFAULT 'pl',
    is_active INTEGER NOT NULL DEFAULT 1,
    priority INTEGER DEFAULT 0
);
CREATE INDEX idx_tts_rules_active ON tts_cleaning_rules(is_active, priority);
CREATE UNIQUE INDEX idx_tts_rules_type_pattern_unique ON tts_cleaning_rules(rule_type, pattern);

CREATE TABLE flow_executions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id),
    request_id TEXT,
    model TEXT,
    started_at TEXT,
    finished_at TEXT,
    status TEXT CHECK(status IN ('running','success','completed','error','cancelled')),
    execution_log TEXT,
    total_latency_ms INTEGER,
    total_tokens INTEGER
);
CREATE INDEX idx_flow_executions_flow ON flow_executions(flow_id);
CREATE INDEX idx_flow_executions_status ON flow_executions(status);

CREATE TABLE registries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    registry_type TEXT NOT NULL DEFAULT 'custom',
    url TEXT NOT NULL,
    username TEXT NOT NULL DEFAULT '',
    password_encrypted TEXT NOT NULL DEFAULT '',
    is_active INTEGER NOT NULL DEFAULT 1,
    skip_tls_verify INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_registries_name ON registries(name);

CREATE TABLE user_accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    email TEXT DEFAULT '',
    is_active INTEGER NOT NULL DEFAULT 1,
    is_admin INTEGER NOT NULL DEFAULT 0,
    sso_provider TEXT DEFAULT NULL,
    sso_subject TEXT DEFAULT NULL,
    last_login_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    must_change_password INTEGER NOT NULL DEFAULT 0,
    role TEXT NOT NULL DEFAULT 'user'
);

CREATE TABLE user_groups (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    description TEXT DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE group_members (
    group_id INTEGER NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);

CREATE TABLE sso_providers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    provider_type TEXT NOT NULL CHECK(provider_type IN ('oidc','azure_ad','google','adfs','authentik')),
    client_id TEXT NOT NULL,
    client_secret_encrypted TEXT NOT NULL,
    discovery_url TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    auto_create_users INTEGER NOT NULL DEFAULT 0,
    default_group_id INTEGER REFERENCES user_groups(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE addons (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    description TEXT DEFAULT '',
    author TEXT DEFAULT '',
    platforms TEXT NOT NULL DEFAULT 'all',
    manifest_json TEXT NOT NULL DEFAULT '{}',
    is_enabled INTEGER NOT NULL DEFAULT 1,
    is_system INTEGER NOT NULL DEFAULT 0,
    installed_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    skill_md TEXT,
    keywords_json TEXT NOT NULL DEFAULT '[]',
    category TEXT NOT NULL DEFAULT '',
    disambiguation_json TEXT NOT NULL DEFAULT '[]',
    admin_only INTEGER NOT NULL DEFAULT 0,
    icon TEXT,
    runtime TEXT NOT NULL DEFAULT 'wasmtime',
    wasm_size_bytes INTEGER NOT NULL DEFAULT 0,
    license TEXT NOT NULL DEFAULT '',
    show_in_catalog INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE addon_secrets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    user_id INTEGER,
    key TEXT NOT NULL,
    value_encrypted TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, user_id, key)
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    user_id INTEGER,
    addon_id TEXT,
    action TEXT NOT NULL,
    resource TEXT,
    details TEXT,
    ip_address TEXT,
    node_id TEXT,
    instance_id TEXT,
    resource_type TEXT,
    resource_id TEXT,
    result TEXT,
    error_message TEXT,
    action_hash INTEGER,
    severity TEXT NOT NULL DEFAULT 'info'
);
CREATE INDEX idx_audit_log_timestamp ON audit_log(timestamp);
CREATE INDEX idx_audit_log_user ON audit_log(user_id);
CREATE INDEX idx_audit_log_addon ON audit_log(addon_id);
CREATE INDEX idx_audit_log_severity ON audit_log(severity);

CREATE TABLE sync_exclusions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id INTEGER REFERENCES user_groups(id) ON DELETE CASCADE,
    resource_type TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(group_id, resource_type)
);

CREATE TABLE trusted_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id TEXT NOT NULL UNIQUE,
    public_key TEXT NOT NULL,
    hostname TEXT DEFAULT '',
    approved_by TEXT DEFAULT '',
    approved_at TEXT NOT NULL DEFAULT (datetime('now')),
    is_active INTEGER NOT NULL DEFAULT 1,
    last_addresses TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_trusted_nodes_node_id ON trusted_nodes(node_id);

CREATE TABLE pending_pairings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    remote_node_id TEXT NOT NULL,
    pin_code TEXT NOT NULL,
    direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_pending_pairings_node ON pending_pairings(remote_node_id);

CREATE TABLE addon_resource_limits (
    addon_id TEXT NOT NULL UNIQUE,
    max_instances INTEGER NOT NULL DEFAULT 0,
    cpu_limit_ms_per_min INTEGER NOT NULL DEFAULT 0,
    ram_limit_mb INTEGER NOT NULL DEFAULT 0,
    gpu_enabled INTEGER NOT NULL DEFAULT 1,
    vram_limit_mb INTEGER NOT NULL DEFAULT 0,
    storage_limit_mb INTEGER NOT NULL DEFAULT 0,
    http_requests_per_min INTEGER NOT NULL DEFAULT 0,
    llm_tokens_per_min INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    fuel_limit INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_addon_resource_limits_addon ON addon_resource_limits(addon_id);

CREATE TABLE addon_config (
    addon_id TEXT NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    is_secret INTEGER NOT NULL DEFAULT 0,
    updated_by INTEGER,
    PRIMARY KEY (addon_id, key)
);

CREATE TABLE addon_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','group')),
    subject_id INTEGER NOT NULL,
    permission_id TEXT NOT NULL,
    granted INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    grant_mode TEXT NOT NULL DEFAULT 'inherit'
        CHECK(grant_mode IN ('allow','deny','inherit')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    UNIQUE(addon_id, subject_type, subject_id, permission_id)
);

CREATE TABLE addon_storage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    storage_key TEXT NOT NULL,
    storage_value BLOB,
    value_size_bytes INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, instance_id, storage_key)
);
CREATE INDEX idx_addon_storage_addon ON addon_storage(addon_id);

CREATE TABLE addon_instances (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    instance_id TEXT NOT NULL UNIQUE,
    instance_name TEXT,
    status TEXT NOT NULL DEFAULT 'stopped',
    created_by INTEGER,
    started_at TEXT,
    stopped_at TEXT
);
CREATE INDEX idx_addon_instances_addon ON addon_instances(addon_id);

CREATE TABLE addon_wasm (
    addon_id TEXT NOT NULL UNIQUE,
    wasm_bytes BLOB NOT NULL
);

CREATE TABLE addon_tools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    description TEXT DEFAULT '',
    parameters_schema_json TEXT DEFAULT '{}',
    return_schema_json TEXT,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    keywords_json TEXT NOT NULL DEFAULT '[]',
    UNIQUE(addon_id, tool_name)
);
CREATE INDEX idx_addon_tools_addon ON addon_tools(addon_id);

CREATE TABLE addon_declared_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    permission_type TEXT NOT NULL,
    UNIQUE(addon_id, permission_type)
);
CREATE INDEX idx_addon_declared_perms_addon ON addon_declared_permissions(addon_id);

CREATE TABLE addon_network_rules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    protocol TEXT NOT NULL CHECK(protocol IN ('tcp','udp')),
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    description TEXT DEFAULT '',
    required INTEGER NOT NULL DEFAULT 0,
    approved INTEGER NOT NULL DEFAULT 0,
    approved_by INTEGER,
    approved_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(addon_id, rule_id)
);
CREATE INDEX idx_addon_network_rules_addon ON addon_network_rules(addon_id);

CREATE TABLE clusters (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cluster_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT DEFAULT '',
    strategy TEXT NOT NULL DEFAULT 'distributed',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    total_vram_mb INTEGER DEFAULT 0,
    total_ram_mb INTEGER DEFAULT 0,
    total_cpu_cores INTEGER DEFAULT 0,
    bottleneck_speed_mbps INTEGER DEFAULT 0,
    interconnect_type TEXT DEFAULT '',
    failover_enabled INTEGER NOT NULL DEFAULT 0,
    failover_target TEXT,
    health_check_interval_ms INTEGER NOT NULL DEFAULT 5000,
    timeout_ms INTEGER NOT NULL DEFAULT 10000
);
CREATE INDEX idx_clusters_cluster_id ON clusters(cluster_id);

CREATE TABLE cluster_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cluster_id TEXT NOT NULL REFERENCES clusters(cluster_id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'worker',
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    interface_name TEXT DEFAULT '',
    interface_ip TEXT DEFAULT '',
    interface_speed_mbps INTEGER DEFAULT 0,
    interface_type TEXT DEFAULT '',
    UNIQUE(cluster_id, node_id)
);
CREATE INDEX idx_cluster_members_cluster ON cluster_members(cluster_id);
CREATE INDEX idx_cluster_members_node ON cluster_members(node_id);

CREATE TABLE revoked_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id TEXT NOT NULL UNIQUE,
    revoked_at TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_by TEXT
);
CREATE INDEX idx_revoked_nodes_node_id ON revoked_nodes(node_id);

CREATE TABLE voice_profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    centroid BLOB NOT NULL,
    sample_count INTEGER NOT NULL DEFAULT 0,
    reliability_score REAL NOT NULL DEFAULT 0.0,
    source TEXT NOT NULL DEFAULT 'manual',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    enrolled_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen_at TEXT,
    total_utterances INTEGER NOT NULL DEFAULT 0,
    first_name TEXT NOT NULL DEFAULT '',
    last_name TEXT,
    nickname TEXT
);
CREATE INDEX idx_voice_profiles_name ON voice_profiles(name);
CREATE INDEX idx_voice_profiles_last_seen ON voice_profiles(last_seen_at);
CREATE INDEX idx_voice_profiles_first_last ON voice_profiles(first_name, last_name);
CREATE INDEX idx_voice_profiles_nickname ON voice_profiles(nickname);

CREATE TABLE voice_profile_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id INTEGER NOT NULL REFERENCES voice_profiles(id) ON DELETE CASCADE,
    embedding BLOB NOT NULL,
    duration_ms INTEGER NOT NULL,
    snr_db REAL NOT NULL DEFAULT 0.0,
    intra_similarity REAL NOT NULL DEFAULT 0.0,
    meeting_id TEXT,
    source TEXT NOT NULL DEFAULT 'enrollment',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_voice_samples_profile ON voice_profile_samples(profile_id);
CREATE INDEX idx_voice_samples_created ON voice_profile_samples(created_at);

CREATE TABLE voice_temp_speakers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id TEXT NOT NULL,
    temp_label TEXT NOT NULL,
    embeddings_blob BLOB NOT NULL,
    sample_count INTEGER NOT NULL DEFAULT 0,
    total_duration_ms INTEGER NOT NULL DEFAULT 0,
    assigned_profile_id INTEGER REFERENCES voice_profiles(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(meeting_id, temp_label)
);
CREATE INDEX idx_voice_temp_meeting ON voice_temp_speakers(meeting_id);
CREATE INDEX idx_voice_temp_assigned ON voice_temp_speakers(assigned_profile_id);

CREATE TABLE meeting_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_key TEXT NOT NULL UNIQUE,
    meeting_url TEXT,
    title TEXT,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
    status TEXT NOT NULL DEFAULT 'ended',
    ended_at TEXT,
    container_id TEXT,
    container_name TEXT,
    quic_port INTEGER,
    vnc_port INTEGER,
    novnc_port INTEGER,
    bot_endpoint_id TEXT,
    bot_secret_key_hex TEXT,
    platform TEXT,
    owner_user_id INTEGER,
    lifecycle_stage TEXT DEFAULT 'idle',
    lifecycle_details TEXT,
    lifecycle_updated_at TEXT,
    backend_stt_model TEXT,
    backend_tts_model TEXT,
    backend_summarization_model TEXT,
    backend_diarization_model TEXT,
    backend_streaming_latency_ms INTEGER,
    backend_enrolled_speakers INTEGER,
    backend_total_participants INTEGER
);
CREATE INDEX idx_meeting_sessions_started ON meeting_sessions(started_at DESC);
CREATE INDEX idx_meeting_sessions_last_activity ON meeting_sessions(last_activity_at DESC);
CREATE INDEX idx_meeting_sessions_status ON meeting_sessions(status);
CREATE INDEX idx_meeting_sessions_owner ON meeting_sessions(owner_user_id);

CREATE TABLE meeting_transcripts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    timestamp_ms INTEGER NOT NULL,
    speaker TEXT NOT NULL,
    profile_id INTEGER,
    confidence REAL,
    is_enrolled INTEGER NOT NULL DEFAULT 0,
    text TEXT NOT NULL,
    model TEXT NOT NULL
);
CREATE INDEX idx_meeting_transcripts_session ON meeting_transcripts(session_id, timestamp_ms);

CREATE TABLE flow_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    version_num INTEGER NOT NULL,
    flow_json TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    status TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    created_by TEXT,
    UNIQUE(flow_id, version_num)
);
CREATE INDEX idx_flow_versions_flow_id ON flow_versions(flow_id, version_num DESC);

CREATE TABLE addon_permission_defaults (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    permission_id TEXT NOT NULL,
    grant_mode TEXT NOT NULL CHECK(grant_mode IN ('allow','deny')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    UNIQUE(addon_id, permission_id)
);
CREATE INDEX idx_addon_perm_defaults_addon ON addon_permission_defaults(addon_id);

CREATE TABLE addon_visibility (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    group_id INTEGER NOT NULL REFERENCES user_groups(id) ON DELETE CASCADE,
    visible INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    UNIQUE(addon_id, group_id)
);
CREATE INDEX idx_addon_visibility_addon ON addon_visibility(addon_id);
CREATE INDEX idx_addon_visibility_group ON addon_visibility(group_id);

CREATE TABLE addon_permission_catalog (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    permission_id TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    risk TEXT NOT NULL DEFAULT 'low' CHECK(risk IN ('low','medium','high','critical')),
    sort_order INTEGER NOT NULL DEFAULT 0,
    UNIQUE(addon_id, permission_id)
);
CREATE INDEX idx_addon_perm_catalog_addon ON addon_permission_catalog(addon_id);

CREATE TABLE addon_oauth_providers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    authorize_url TEXT NOT NULL,
    token_url TEXT NOT NULL,
    revoke_url TEXT,
    scopes TEXT NOT NULL DEFAULT '',
    mode TEXT NOT NULL DEFAULT 'individual'
        CHECK(mode IN ('global','individual','none')),
    pkce INTEGER NOT NULL DEFAULT 1,
    UNIQUE(addon_id, provider_id)
);
CREATE INDEX idx_addon_oauth_providers_addon ON addon_oauth_providers(addon_id);

CREATE TABLE addon_oauth_config (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    addon_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    client_id TEXT NOT NULL DEFAULT '',
    client_secret_encrypted BLOB,
    redirect_uri TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER REFERENCES user_accounts(id) ON DELETE SET NULL,
    oauth_mode TEXT NOT NULL DEFAULT 'individual'
        CHECK(oauth_mode IN ('global','individual','none')),
    UNIQUE(addon_id, provider_id)
);
CREATE INDEX idx_addon_oauth_config_addon ON addon_oauth_config(addon_id);

CREATE TABLE oauth_pending_states (
    state TEXT PRIMARY KEY,
    user_id INTEGER REFERENCES user_accounts(id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('global','individual')),
    code_verifier TEXT NOT NULL DEFAULT '',
    redirect_after TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL
);
CREATE INDEX idx_oauth_pending_states_expires ON oauth_pending_states(expires_at);

CREATE TABLE addon_network_config (
    addon_id TEXT NOT NULL PRIMARY KEY,
    allowed_hosts TEXT NOT NULL DEFAULT '[]',
    blocked_hosts TEXT NOT NULL DEFAULT '[]',
    mode TEXT NOT NULL DEFAULT 'strict' CHECK(mode IN ('strict','permissive')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by INTEGER
);

CREATE TABLE user_oauth_accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER REFERENCES user_accounts(id) ON DELETE CASCADE,
    addon_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    external_account_id TEXT NOT NULL DEFAULT '',
    display_name TEXT NOT NULL DEFAULT '',
    access_token_encrypted BLOB,
    refresh_token_encrypted BLOB,
    token_type TEXT NOT NULL DEFAULT 'Bearer',
    scopes TEXT NOT NULL DEFAULT '',
    expires_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_used_at TEXT,
    revoked INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX uq_user_oauth_individual
    ON user_oauth_accounts(user_id, addon_id, provider_id)
    WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX uq_user_oauth_global
    ON user_oauth_accounts(addon_id, provider_id)
    WHERE user_id IS NULL;
CREATE INDEX idx_user_oauth_accounts_user ON user_oauth_accounts(user_id);
CREATE INDEX idx_user_oauth_accounts_addon ON user_oauth_accounts(addon_id);
CREATE INDEX idx_user_oauth_accounts_addon_provider ON user_oauth_accounts(addon_id, provider_id);

CREATE TABLE notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    title TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT '',
    pinned INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_notes_user ON notes(user_id);
CREATE INDEX idx_notes_user_updated ON notes(user_id, updated_at DESC);

CREATE TABLE meeting_port_allocations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    port INTEGER NOT NULL,
    kind TEXT NOT NULL,
    session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    allocated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(port, kind)
);
CREATE INDEX idx_meeting_port_allocations_session ON meeting_port_allocations(session_id);

CREATE TABLE meeting_settings (
    user_id INTEGER NOT NULL REFERENCES user_accounts(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, key)
);

CREATE TABLE mesh_topology (
    node_id TEXT PRIMARY KEY,
    hostname TEXT NOT NULL DEFAULT '',
    platform TEXT NOT NULL DEFAULT '',
    os_info TEXT NOT NULL DEFAULT '',
    connected_to TEXT NOT NULL DEFAULT '[]',
    direct_addrs TEXT NOT NULL DEFAULT '[]',
    port INTEGER NOT NULL DEFAULT 0,
    services_json TEXT NOT NULL DEFAULT '[]',
    models_json TEXT NOT NULL DEFAULT '[]',
    last_epoch INTEGER NOT NULL DEFAULT 0,
    last_seen_ms INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_mesh_topology_last_seen ON mesh_topology(last_seen_ms DESC);

CREATE TABLE resource_permissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id INTEGER NOT NULL,
    access_level TEXT NOT NULL CHECK(access_level IN ('allow','deny')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(resource_type, resource_id, subject_type, subject_id)
);
CREATE INDEX idx_resperm_subject ON resource_permissions(subject_type, subject_id);
CREATE INDEX idx_resperm_resource ON resource_permissions(resource_type, resource_id);

CREATE TABLE prompts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    prompt_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    content TEXT NOT NULL,
    prompt_type TEXT NOT NULL CHECK(prompt_type IN ('system','suffix','template','user')),
    default_model TEXT,
    variables TEXT,
    cache_priority INTEGER DEFAULT 50,
    is_active INTEGER NOT NULL DEFAULT 1,
    version INTEGER DEFAULT 1,
    language TEXT NOT NULL DEFAULT 'pl',
    is_system INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(prompt_id, language)
);
CREATE INDEX idx_prompts_prompt_id ON prompts(prompt_id);
CREATE INDEX idx_prompts_language ON prompts(language);

CREATE TABLE meeting_summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    decisions_text TEXT NOT NULL DEFAULT '',
    summary_text TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    FOREIGN KEY (session_id) REFERENCES meeting_sessions(id) ON DELETE CASCADE
);
CREATE INDEX idx_meeting_summaries_session ON meeting_summaries(session_id, created_at DESC);

CREATE TABLE meeting_action_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL,
    owner TEXT NOT NULL,
    task TEXT NOT NULL,
    deadline TEXT,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK(status IN ('pending','done','cancelled')),
    content_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (session_id) REFERENCES meeting_sessions(id) ON DELETE CASCADE,
    UNIQUE(session_id, content_hash)
);
CREATE INDEX idx_meeting_action_items_session ON meeting_action_items(session_id, status, created_at DESC);

CREATE TABLE teams_bot_wake_words (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    word TEXT NOT NULL UNIQUE COLLATE NOCASE,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE services (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    engine_id TEXT NOT NULL,
    category TEXT NOT NULL,
    display_name TEXT NOT NULL,
    deploy_method TEXT NOT NULL CHECK(deploy_method IN ('docker','native_embedded','native_binary','native_python_bundle','external')),
    transport TEXT NOT NULL CHECK(transport IN ('embedded','http_direct','sidecar_quic','external_http')),
    status TEXT NOT NULL CHECK(status IN ('deploying','starting','running','degraded','failed','stopped','interrupted')) DEFAULT 'starting',
    pinned INTEGER NOT NULL DEFAULT 0,
    paused INTEGER NOT NULL DEFAULT 0,
    runtime_pid INTEGER,
    runtime_port INTEGER,
    sidecar_quic_port INTEGER,
    endpoint_url TEXT,
    config_json TEXT NOT NULL DEFAULT '{}',
    active_deploy_id TEXT NOT NULL DEFAULT '',
    last_deploy_id TEXT NOT NULL DEFAULT '',
    deployment_progress_pct INTEGER NOT NULL DEFAULT 0,
    health_last_ok TIMESTAMP,
    health_last_err TEXT,
    -- progress_message dodawany przez migration 5 (services_progress_message).
    -- Nie dodajemy tu zeby ALTER TABLE w migracji nie zwalil "duplicate column"
    -- na fresh DB.
    restart_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_services_status ON services(status);
CREATE INDEX idx_services_engine ON services(engine_id);
CREATE INDEX idx_services_category ON services(category);

CREATE TABLE model_registry (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    service_id INTEGER NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    model_name TEXT NOT NULL,
    display_name TEXT,
    capabilities TEXT NOT NULL DEFAULT '[]',
    context_length INTEGER,
    quantization TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(service_id, model_name)
);
CREATE INDEX idx_models_service ON model_registry(service_id);
CREATE INDEX idx_models_name ON model_registry(model_name);

CREATE TABLE deployments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    deploy_id TEXT NOT NULL UNIQUE,
    engine_id TEXT NOT NULL,
    deploy_method TEXT NOT NULL,
    node_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'deploying',
    phase TEXT NOT NULL DEFAULT '',
    progress_pct INTEGER NOT NULL DEFAULT 0,
    image_tag TEXT NOT NULL DEFAULT '',
    container_name TEXT NOT NULL DEFAULT '',
    config_json TEXT NOT NULL DEFAULT '{}',
    user_id INTEGER,
    started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at TIMESTAMP,
    error_message TEXT,
    log_tail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_deployments_deploy_id ON deployments(deploy_id);
CREATE INDEX idx_deployments_engine ON deployments(engine_id);

CREATE TABLE peer_persisted (
    node_id        BLOB PRIMARY KEY,
    pubkey         BLOB NOT NULL,
    trust_state    INTEGER NOT NULL DEFAULT 0,
    hostname       TEXT,
    platform       TEXT,
    role           INTEGER NOT NULL DEFAULT 0,
    last_seen_ms   INTEGER NOT NULL DEFAULT 0,
    persisted_ver  INTEGER NOT NULL DEFAULT 0,
    updated_at_ms  INTEGER NOT NULL
);

CREATE TABLE peer_hints (
    node_id     BLOB NOT NULL,
    hint_kind   INTEGER NOT NULL,
    payload     TEXT NOT NULL,
    last_ok_ms  INTEGER,
    fail_count  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, hint_kind, payload),
    FOREIGN KEY (node_id) REFERENCES peer_persisted(node_id) ON DELETE CASCADE
);
CREATE INDEX idx_peer_hints_node ON peer_hints(node_id);

INSERT INTO user_groups (id, name, description) VALUES (1, 'admins', 'Administratorzy systemu');

INSERT INTO settings(key, value) VALUES
    ('mesh.bind_mode', 'auto'),
    ('mesh.bind_ipv4', ''),
    ('mesh.advertise_hide_docker', '1'),
    ('mesh.advertise_hide_link_local', '1'),
    ('mesh.advertise_hide_loopback', '1'),
    ('mesh.advertise_hide_cgnat', '0'),
    ('mesh.advertise_prefer_same_subnet', '1'),
    ('mesh.iroh_relay_url', 'https://relay.nextapp.pl');
"#;

const SCHEDULED_JOBS: &str = r#"
CREATE TABLE IF NOT EXISTS scheduled_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    target_type TEXT NOT NULL,
    target_addon_id TEXT NOT NULL DEFAULT '',
    target_action_id TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '{}',
    schedule_kind TEXT NOT NULL,
    schedule_expr TEXT NOT NULL,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    next_run_at TEXT,
    max_runtime_seconds INTEGER NOT NULL DEFAULT 1800,
    retry_policy_json TEXT NOT NULL DEFAULT '{"max_attempts":1,"backoff_seconds":60}',
    concurrency_policy TEXT NOT NULL DEFAULT 'skip',
    created_by_user_id INTEGER,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_due
    ON scheduled_jobs(enabled, next_run_at);
CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_target
    ON scheduled_jobs(target_type, target_addon_id, target_action_id);

CREATE TABLE IF NOT EXISTS scheduled_runs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES scheduled_jobs(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    scheduled_for TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    result_json TEXT,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_scheduled_runs_job
    ON scheduled_runs(job_id, scheduled_for DESC);
CREATE INDEX IF NOT EXISTS idx_scheduled_runs_status
    ON scheduled_runs(status);
"#;

const COMPLIANCE_CORE_FOUNDATION: &str = r#"
CREATE TABLE IF NOT EXISTS compliance_data_categories (
    category_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name_translations TEXT NOT NULL DEFAULT '{}',
    description_translations TEXT NOT NULL DEFAULT '{}',
    personal_data INTEGER NOT NULL DEFAULT 1 CHECK(personal_data IN (0,1)),
    sensitive_data INTEGER NOT NULL DEFAULT 0 CHECK(sensitive_data IN (0,1)),
    risk_class TEXT NOT NULL DEFAULT 'standard' CHECK(risk_class IN ('low','standard','high','critical')),
    source_scope TEXT NOT NULL DEFAULT 'core' CHECK(source_scope IN ('core','addon','external')),
    addon_id TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, slug),
    CHECK(json_valid(name_translations)),
    CHECK(json_valid(description_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_data_categories_org
    ON compliance_data_categories(org_id, risk_class, personal_data);

CREATE TRIGGER IF NOT EXISTS compliance_data_categories_updated_at
AFTER UPDATE ON compliance_data_categories
FOR EACH ROW
BEGIN
    UPDATE compliance_data_categories
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE category_id = NEW.category_id;
END;

CREATE TABLE IF NOT EXISTS compliance_processing_activities (
    activity_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name_translations TEXT NOT NULL DEFAULT '{}',
    purpose_translations TEXT NOT NULL DEFAULT '{}',
    controller_role TEXT NOT NULL DEFAULT 'controller'
        CHECK(controller_role IN ('controller','processor','joint_controller')),
    owner_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    system_scope TEXT NOT NULL DEFAULT 'core' CHECK(system_scope IN ('core','addon','external')),
    addon_id TEXT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('draft','active','retired')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, slug),
    CHECK(json_valid(name_translations)),
    CHECK(json_valid(purpose_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_processing_activities_org
    ON compliance_processing_activities(org_id, status, system_scope);

CREATE TRIGGER IF NOT EXISTS compliance_processing_activities_updated_at
AFTER UPDATE ON compliance_processing_activities
FOR EACH ROW
BEGIN
    UPDATE compliance_processing_activities
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE activity_id = NEW.activity_id;
END;

CREATE TABLE IF NOT EXISTS compliance_activity_categories (
    activity_id TEXT NOT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    category_id TEXT NOT NULL REFERENCES compliance_data_categories(category_id) ON DELETE CASCADE,
    PRIMARY KEY(activity_id, category_id)
);
CREATE INDEX IF NOT EXISTS idx_compliance_activity_categories_category
    ON compliance_activity_categories(category_id);

CREATE TABLE IF NOT EXISTS compliance_legal_basis (
    legal_basis_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    activity_id TEXT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE CASCADE,
    basis_kind TEXT NOT NULL
        CHECK(basis_kind IN ('consent','contract','legal_obligation','vital_interests','public_task','legitimate_interest')),
    basis_reference TEXT NOT NULL DEFAULT '',
    description_translations TEXT NOT NULL DEFAULT '{}',
    is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(activity_id IS NOT NULL OR category_id IS NOT NULL),
    CHECK(json_valid(description_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_legal_basis_lookup
    ON compliance_legal_basis(org_id, activity_id, category_id, is_active);

CREATE TRIGGER IF NOT EXISTS compliance_legal_basis_updated_at
AFTER UPDATE ON compliance_legal_basis
FOR EACH ROW
BEGIN
    UPDATE compliance_legal_basis
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE legal_basis_id = NEW.legal_basis_id;
END;

CREATE TABLE IF NOT EXISTS compliance_retention_policies (
    retention_policy_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    name_translations TEXT NOT NULL DEFAULT '{}',
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('audit','ai_audit','data_category','document','dsar','breach','general')),
    category_id TEXT NULL REFERENCES compliance_data_categories(category_id) ON DELETE SET NULL,
    retention_days INTEGER NOT NULL,
    minimum_days INTEGER NOT NULL DEFAULT 0,
    action_after_retention TEXT NOT NULL DEFAULT 'delete'
        CHECK(action_after_retention IN ('delete','anonymize','archive')),
    is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)),
    is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(retention_days >= minimum_days),
    UNIQUE(org_id, slug),
    CHECK(json_valid(name_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_retention_policies_lookup
    ON compliance_retention_policies(org_id, scope_kind, category_id, is_active);
CREATE UNIQUE INDEX IF NOT EXISTS idx_compliance_retention_one_default
    ON compliance_retention_policies(org_id, scope_kind)
    WHERE is_default = 1 AND category_id IS NULL AND is_active = 1;

CREATE TRIGGER IF NOT EXISTS compliance_retention_policies_updated_at
AFTER UPDATE ON compliance_retention_policies
FOR EACH ROW
BEGIN
    UPDATE compliance_retention_policies
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE retention_policy_id = NEW.retention_policy_id;
END;

CREATE TABLE IF NOT EXISTS compliance_legal_holds (
    legal_hold_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    scope_kind TEXT NOT NULL CHECK(scope_kind IN ('audit','ai_audit','data_category','document','dsar','breach','resource','general')),
    scope_id TEXT NOT NULL DEFAULT '',
    reason TEXT NOT NULL,
    created_by_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    released_at TEXT NULL,
    released_by_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    release_reason TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_compliance_legal_holds_active
    ON compliance_legal_holds(org_id, scope_kind, scope_id, released_at);

CREATE TABLE IF NOT EXISTS compliance_documents (
    compliance_document_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    source_legal_document_id TEXT NULL REFERENCES legal_documents(id) ON DELETE SET NULL,
    document_type TEXT NOT NULL,
    title_translations TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','active','archived')),
    version INTEGER NOT NULL DEFAULT 1,
    artifact_path TEXT NOT NULL DEFAULT '',
    artifact_hash TEXT NOT NULL DEFAULT '',
    created_by_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, document_type, version),
    CHECK(json_valid(title_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_documents_org
    ON compliance_documents(org_id, document_type, status);

CREATE TRIGGER IF NOT EXISTS compliance_documents_updated_at
AFTER UPDATE ON compliance_documents
FOR EACH ROW
BEGIN
    UPDATE compliance_documents
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE compliance_document_id = NEW.compliance_document_id;
END;

CREATE TABLE IF NOT EXISTS compliance_ai_events (
    event_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    node_id TEXT NOT NULL DEFAULT '',
    addon_id TEXT NULL,
    instance_id TEXT NULL,
    flow_id INTEGER NULL REFERENCES flows(id) ON DELETE SET NULL,
    flow_node_id TEXT NULL,
    request_id TEXT NOT NULL,
    model_id TEXT NOT NULL DEFAULT '',
    backend TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    finished_at TEXT NULL,
    status TEXT NOT NULL CHECK(status IN ('running','success','failed','cancelled')),
    risk_class TEXT NOT NULL DEFAULT 'standard' CHECK(risk_class IN ('low','standard','high','critical')),
    legal_basis_id TEXT NULL REFERENCES compliance_legal_basis(legal_basis_id) ON DELETE SET NULL,
    retention_policy_id TEXT NOT NULL REFERENCES compliance_retention_policies(retention_policy_id) ON DELETE RESTRICT,
    prompt_hash TEXT NOT NULL DEFAULT '',
    response_hash TEXT NOT NULL DEFAULT '',
    audit_log_id INTEGER NULL REFERENCES audit_log(id) ON DELETE SET NULL,
    error_message TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, request_id)
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_org_started
    ON compliance_ai_events(org_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_user
    ON compliance_ai_events(org_id, user_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_addon
    ON compliance_ai_events(org_id, addon_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_events_status
    ON compliance_ai_events(org_id, status, started_at DESC);

CREATE TRIGGER IF NOT EXISTS compliance_ai_events_updated_at
AFTER UPDATE ON compliance_ai_events
FOR EACH ROW
BEGIN
    UPDATE compliance_ai_events
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE event_id = NEW.event_id;
END;

CREATE TABLE IF NOT EXISTS compliance_ai_payloads (
    payload_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    payload_kind TEXT NOT NULL CHECK(payload_kind IN ('prompt','response','system','tool_input','tool_output')),
    content_hash TEXT NOT NULL,
    content_text TEXT NOT NULL,
    content_redacted INTEGER NOT NULL DEFAULT 0 CHECK(content_redacted IN (0,1)),
    token_count INTEGER NULL CHECK(token_count IS NULL OR token_count >= 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_payloads_event
    ON compliance_ai_payloads(event_id, payload_kind);

CREATE TABLE IF NOT EXISTS compliance_ai_sources (
    source_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('rag','file','url','database','addon','memory','vector','other')),
    source_ref TEXT NOT NULL,
    source_hash TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    excerpt_hash TEXT NOT NULL DEFAULT '',
    excerpt_text TEXT NOT NULL DEFAULT '',
    score REAL NULL,
    metadata_cbor BLOB NOT NULL DEFAULT X'',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_sources_event
    ON compliance_ai_sources(event_id, source_kind);

CREATE TABLE IF NOT EXISTS compliance_ai_tool_calls (
    tool_call_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    addon_id TEXT NULL,
    tool_name TEXT NOT NULL,
    input_hash TEXT NOT NULL DEFAULT '',
    output_hash TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK(status IN ('running','success','failed')),
    started_at TEXT NOT NULL,
    finished_at TEXT NULL,
    error_message TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_tool_calls_event
    ON compliance_ai_tool_calls(event_id, status);

CREATE TABLE IF NOT EXISTS compliance_ai_policy_decisions (
    decision_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES compliance_ai_events(event_id) ON DELETE CASCADE,
    decision_kind TEXT NOT NULL CHECK(decision_kind IN ('allow','deny','redact','retain','legal_hold','risk_class')),
    decision_value TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_ai_policy_decisions_event
    ON compliance_ai_policy_decisions(event_id, decision_kind);

CREATE TABLE IF NOT EXISTS compliance_data_subjects (
    subject_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    subject_type TEXT NOT NULL CHECK(subject_type IN ('user','contact','customer','employee','external_person','unknown')),
    display_name_translations TEXT NOT NULL DEFAULT '{}',
    email_hash TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(json_valid(display_name_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_data_subjects_org
    ON compliance_data_subjects(org_id, subject_type, email_hash);

CREATE TRIGGER IF NOT EXISTS compliance_data_subjects_updated_at
AFTER UPDATE ON compliance_data_subjects
FOR EACH ROW
BEGIN
    UPDATE compliance_data_subjects
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE subject_id = NEW.subject_id;
END;

CREATE TABLE IF NOT EXISTS compliance_data_subject_links (
    link_id TEXT PRIMARY KEY,
    subject_id TEXT NOT NULL REFERENCES compliance_data_subjects(subject_id) ON DELETE CASCADE,
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    addon_id TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_compliance_data_subject_links_unique
    ON compliance_data_subject_links(subject_id, resource_kind, resource_id, COALESCE(addon_id, ''));
CREATE INDEX IF NOT EXISTS idx_compliance_data_subject_links_resource
    ON compliance_data_subject_links(resource_kind, resource_id, addon_id);

CREATE TABLE IF NOT EXISTS compliance_dsar_requests (
    dsar_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    subject_id TEXT NOT NULL REFERENCES compliance_data_subjects(subject_id) ON DELETE CASCADE,
    request_type TEXT NOT NULL CHECK(request_type IN ('access','rectification','erasure','restriction','portability','objection')),
    status TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open','in_progress','completed','rejected','cancelled')),
    requested_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    due_at TEXT NOT NULL,
    completed_at TEXT NULL,
    handled_by_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    notes TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_compliance_dsar_requests_org
    ON compliance_dsar_requests(org_id, status, due_at);

CREATE TABLE IF NOT EXISTS compliance_dsar_exports (
    export_id TEXT PRIMARY KEY,
    dsar_id TEXT NOT NULL REFERENCES compliance_dsar_requests(dsar_id) ON DELETE CASCADE,
    artifact_path TEXT NOT NULL,
    artifact_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE IF NOT EXISTS compliance_consent_records (
    consent_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    subject_id TEXT NOT NULL REFERENCES compliance_data_subjects(subject_id) ON DELETE CASCADE,
    activity_id TEXT NOT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    consent_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('granted','withdrawn','expired')),
    granted_at TEXT NULL,
    withdrawn_at TEXT NULL,
    evidence_hash TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_consent_records_subject
    ON compliance_consent_records(org_id, subject_id, activity_id, status);

CREATE TABLE IF NOT EXISTS compliance_dpia_records (
    dpia_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    activity_id TEXT NOT NULL REFERENCES compliance_processing_activities(activity_id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'draft' CHECK(status IN ('draft','review','approved','rejected','retired')),
    risk_class TEXT NOT NULL DEFAULT 'standard' CHECK(risk_class IN ('low','standard','high','critical')),
    owner_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    summary TEXT NOT NULL DEFAULT '',
    reviewed_at TEXT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_compliance_dpia_records_org
    ON compliance_dpia_records(org_id, status, risk_class);

CREATE TRIGGER IF NOT EXISTS compliance_dpia_records_updated_at
AFTER UPDATE ON compliance_dpia_records
FOR EACH ROW
BEGIN
    UPDATE compliance_dpia_records
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE dpia_id = NEW.dpia_id;
END;

CREATE TABLE IF NOT EXISTS compliance_breach_incidents (
    breach_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    detected_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK(status IN ('open','investigating','notified','closed')),
    severity TEXT NOT NULL DEFAULT 'medium' CHECK(severity IN ('low','medium','high','critical')),
    title_translations TEXT NOT NULL DEFAULT '{}',
    summary_translations TEXT NOT NULL DEFAULT '{}',
    dpa_notified_at TEXT NULL,
    subjects_notified_at TEXT NULL,
    created_by_user_id INTEGER NULL REFERENCES user_accounts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CHECK(json_valid(title_translations)),
    CHECK(json_valid(summary_translations))
);
CREATE INDEX IF NOT EXISTS idx_compliance_breach_incidents_org
    ON compliance_breach_incidents(org_id, status, detected_at DESC);

CREATE TRIGGER IF NOT EXISTS compliance_breach_incidents_updated_at
AFTER UPDATE ON compliance_breach_incidents
FOR EACH ROW
BEGIN
    UPDATE compliance_breach_incidents
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE breach_id = NEW.breach_id;
END;

CREATE TABLE IF NOT EXISTS compliance_processors (
    processor_id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('processor','subprocessor','joint_controller')),
    country TEXT NOT NULL DEFAULT '',
    transfer_mechanism TEXT NOT NULL DEFAULT '',
    dpa_reference TEXT NOT NULL DEFAULT '',
    is_active INTEGER NOT NULL DEFAULT 1 CHECK(is_active IN (0,1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(org_id, name)
);
CREATE INDEX IF NOT EXISTS idx_compliance_processors_org
    ON compliance_processors(org_id, is_active);

CREATE TRIGGER IF NOT EXISTS compliance_processors_updated_at
AFTER UPDATE ON compliance_processors
FOR EACH ROW
BEGIN
    UPDATE compliance_processors
    SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    WHERE processor_id = NEW.processor_id;
END;
"#;

// v40 — `platform_locales`: katalog jezykow interfejsu per organizacja.
// Tabela trzyma kody ISO 639-1 dostepne dla danej organizacji oraz wskazuje
// jezyk domyslny. Dane sluza warstwie aplikacyjnej do walidacji kompletnosci
// tlumaczen w `role_catalog.name_translations` i pozniej w innych tabelach
// platformy (stanowiska, etykiety addonow). SQLite nie wymusza obecnosci
// kluczy JSON dla wszystkich locale — kontrolka jest po stronie Rust
// (services/role_catalog), tu seedujemy startowy zestaw pl + en.
//
// Czesciowy unikalny indeks `idx_platform_locales_one_default_per_org`
// gwarantuje ze w danej organizacji jest dokladnie jeden jezyk z
// `is_default = 1` — bez tego UI mialby ambiwalentny fallback.
const PLATFORM_LOCALES_SCHEMA: &str = r#"
CREATE TABLE platform_locales (
  id            TEXT PRIMARY KEY,
  org_id        TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
  code          TEXT NOT NULL,
  display_name  TEXT NOT NULL,
  is_default    INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
  is_active     INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
  created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  UNIQUE (org_id, code)
);

CREATE INDEX idx_platform_locales_org ON platform_locales(org_id);
CREATE UNIQUE INDEX idx_platform_locales_one_default_per_org
  ON platform_locales(org_id) WHERE is_default = 1;

INSERT INTO platform_locales (id, org_id, code, display_name, is_default, is_active)
VALUES
  (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
   'org-default', 'pl', 'Polski', 1, 1),
  (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
   'org-default', 'en', 'English', 0, 1);
"#;

// v41 — `role_catalog`: globalny, administrowalny katalog rol funkcjonalnych
// w organizacji. Rola opisuje *funkcje* osoby (np. handlowiec, PM techniczny,
// architekt) niezaleznie od stanowiska w drzewie organizacyjnym. Stanowiska
// i tabele typu `responsible_persons` w CRM bedaca referencja do wpisu z tego
// katalogu.
//
// `name_translations` i `description_translations` trzymane jako JSON
// (`{"pl":"...","en":"..."}`) — SQLite nie ma JSONB, walidujemy `json_valid`,
// pelna walidacja kompletnosci kluczy wzgledem `platform_locales` zyje w
// `services/role_catalog/repo.rs`. Seed zapewnia komplet pl + en dla wszystkich
// 14 rol z dokumentu `00-platform-roles-catalog.md`.
//
// `default_visibility_scope` dziedziczy sie do reguly P2 (permissions) jako
// startowa propozycja — admin moze zmienic w UI bez modyfikacji katalogu.
// `is_manager` jest uzywane przez O1 do layoutu drzewa stanowisk; nie wplywa
// bezposrednio na uprawnienia (te zyja w P2 jako reguly `flag:is_manager`).
const ROLE_CATALOG_SCHEMA: &str = r#"
CREATE TABLE role_catalog (
  id                          TEXT PRIMARY KEY,
  org_id                      TEXT NOT NULL REFERENCES organizations(org_id) ON DELETE CASCADE,
  slug                        TEXT NOT NULL CHECK (slug GLOB '[a-z][a-z0-9_]*' AND length(slug) <= 50),
  kind                        TEXT NOT NULL CHECK (kind IN ('sales','technical','management','external','other')),
  name_translations           TEXT NOT NULL DEFAULT '{}',
  description_translations    TEXT NOT NULL DEFAULT '{}',
  icon                        TEXT,
  color_hint                  TEXT,
  is_manager                  INTEGER NOT NULL DEFAULT 0 CHECK (is_manager IN (0,1)),
  default_visibility_scope    TEXT NOT NULL DEFAULT 'assigned'
    CHECK (default_visibility_scope IN ('assigned','own','section','department','all')),
  is_active                   INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0,1)),
  created_at                  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  updated_at                  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
  created_by                  TEXT,
  UNIQUE (org_id, slug),
  CHECK (json_valid(name_translations)),
  CHECK (json_valid(description_translations))
);

CREATE INDEX idx_role_catalog_org_active ON role_catalog(org_id, is_active);
CREATE INDEX idx_role_catalog_org_kind   ON role_catalog(org_id, kind);

CREATE TRIGGER role_catalog_updated_at
AFTER UPDATE ON role_catalog
FOR EACH ROW
BEGIN
  UPDATE role_catalog SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = NEW.id;
END;

INSERT INTO role_catalog (id, org_id, slug, kind, name_translations, description_translations, icon, is_manager, default_visibility_scope) VALUES
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','handlowiec_l1','sales',
 json_object('pl','Handlowiec L1','en','Sales Rep L1'),
 json_object('pl','Junior — podstawowa rola sprzedazowa, prowadzi wlasne dealy','en','Junior — entry-level sales role, owns their own deals'),
 'i-briefcase', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','handlowiec_l2','sales',
 json_object('pl','Handlowiec L2','en','Sales Rep L2'),
 json_object('pl','Mid — samodzielnie prowadzi dealy oraz wspiera juniorow','en','Mid-level — runs deals independently and supports juniors'),
 'i-briefcase', 0, 'own'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','sales_lead','sales',
 json_object('pl','Lider sprzedazy','en','Sales Lead'),
 json_object('pl','Kierownik sekcji sprzedazowej, prowadzi zespol handlowcow','en','Manages a sales section and a team of sales reps'),
 'i-users', 1, 'section'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','pm_technical','technical',
 json_object('pl','PM techniczny','en','Technical PM'),
 json_object('pl','Project Manager po stronie technicznej dealu/projektu','en','Project Manager on the technical side of a deal or project'),
 'i-clipboard', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','architect_senior','technical',
 json_object('pl','Architekt senior','en','Senior Architect'),
 json_object('pl','Architekt rozwiazan, odpowiada za design techniczny','en','Solutions architect responsible for technical design'),
 'i-cube', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','consultant_technical','technical',
 json_object('pl','Konsultant techniczny','en','Technical Consultant'),
 json_object('pl','Konsultant doradzajacy klientowi w obszarze technicznym','en','Consultant advising the client on technical matters'),
 'i-headset', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','developer','technical',
 json_object('pl','Programista','en','Developer'),
 json_object('pl','Programista realizujacy implementacje na projekcie','en','Developer implementing project work'),
 'i-code', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','qa','technical',
 json_object('pl','Tester QA','en','QA Engineer'),
 json_object('pl','Odpowiada za testy i jakosc dostarczanych rozwiazan','en','Responsible for testing and quality of delivered solutions'),
 'i-bug', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','section_director','management',
 json_object('pl','Dyrektor sekcji','en','Section Director'),
 json_object('pl','Dyrektor odpowiedzialny za sekcje organizacyjna','en','Director responsible for an organizational section'),
 'i-building', 1, 'section'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','sales_director','management',
 json_object('pl','Dyrektor sprzedazy','en','Sales Director'),
 json_object('pl','Dyrektor pionu sprzedazy, nadzoruje wiele sekcji','en','Sales department director, oversees multiple sections'),
 'i-chart', 1, 'department'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','ceo','management',
 json_object('pl','Prezes','en','CEO'),
 json_object('pl','Prezes zarzadu, najwyzszy poziom decyzyjny','en','Chief Executive Officer, top decision-making level'),
 'i-crown', 1, 'all'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','decision_maker','external',
 json_object('pl','Decydent klienta','en','Client Decision Maker'),
 json_object('pl','Osoba po stronie klienta podejmujaca decyzje zakupowe','en','Person on the client side making purchasing decisions'),
 'i-user-check', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','influencer','external',
 json_object('pl','Influencer po stronie klienta','en','Client Influencer'),
 json_object('pl','Osoba wplywajaca na decyzje klienta, bez bezposredniej decyzyjnosci','en','Person influencing client decisions without direct authority'),
 'i-user', 0, 'assigned'),
(lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))),2) || '-' || substr('89ab',abs(random()) % 4 + 1, 1) || substr(lower(hex(randomblob(2))),2) || '-' || lower(hex(randomblob(6))),
 'org-default','power_user_sponsor','external',
 json_object('pl','Sponsor wewnetrzny u klienta','en','Internal Sponsor at Client'),
 json_object('pl','Kluczowy uzytkownik i sponsor wdrozenia po stronie klienta','en','Key user and rollout sponsor on the client side'),
 'i-user-cog', 0, 'assigned');
"#;
