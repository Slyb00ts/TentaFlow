// =============================================================================
// File: services/config_bundle/mod.rs — manual, whitelist-only config bundle
//       for cross-environment pull (ROADMAP Z12)
// =============================================================================
//
// A config bundle is NOT sync — sync only ever moves data between nodes of
// the SAME declared environment (fenced independently in `sync::ledger` and
// `net::iroh::pairing`). Moving configuration ACROSS environments (typically
// downward, Prod -> Test/Dev, but upward promotion including onto Prod is
// deliberately allowed with a server-validated confirmation gate — see
// `EnvironmentImportApplyRequest::confirm_environment_name`) is always a
// manual, admin-initiated pull of exactly this bundle, over one of two
// transports: a QUIC round trip to a trusted donor node
// (`MeshCommandType::ConfigBundleExport`) or a downloaded/uploaded file. Both
// transports carry the SAME bytes produced by `export_bundle` — there is no
// separate file format.
//
// Whitelist, never blacklist (ZADANIA.md Z12 pitfall #1): only the tables
// enumerated below ever leave the node, and `settings` additionally goes
// through `SETTINGS_ALLOWLIST` — a new setting added elsewhere in the
// codebase is excluded from the bundle by default, not included by omission.
// Clinical data and secrets are never in scope; `SETTINGS_ALLOWLIST` entries
// are STILL re-checked against `SettingsCipher::should_encrypt` at export time
// as a defense-in-depth backstop against a future accidental addition of a
// secret-shaped key to the allowlist.
//
// Not to be confused with Project Studio's `environments.rs` (test-runner
// target environments) — a different, unrelated concept.
// =============================================================================

use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use tentaflow_protocol::environment::NodeEnvironment;

use crate::db::models::{DbFlow, DbFlowModelBinding, DbModelAlias};
use crate::db::{repository, DbPool};

pub const FORMAT_VERSION: u32 = 1;

/// `settings` keys eligible for the bundle. Empty today — the product's
/// current cross-environment-portable configuration lives entirely in
/// `flows`/`flow_model_bindings`/`model_aliases`. Extend deliberately, one key
/// at a time, as genuinely portable (non-secret, non-node-identity) settings
/// are added; never widen this to "everything not explicitly excluded".
pub const SETTINGS_ALLOWLIST: &[&str] = &[];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigBundle {
    pub format_version: u32,
    pub source_node_id: String,
    pub source_environment: NodeEnvironment,
    pub exported_at: String,
    pub flows: Vec<DbFlow>,
    pub flow_model_bindings: Vec<DbFlowModelBinding>,
    pub model_aliases: Vec<DbModelAlias>,
    pub settings: Vec<(String, String)>,
}

pub struct ExportedBundle {
    pub bundle: ConfigBundle,
    pub archive_bytes: Vec<u8>,
    pub filename: String,
    pub manifest_sha256: String,
    pub table_counts: Vec<(String, u64)>,
}

/// Builds the local node's current config bundle. `is_system` flows
/// (platform-seeded, non-editable) are excluded — they are reseeded from the
/// binary itself on every node, not something a pull should ever overwrite.
pub fn export_bundle(pool: &DbPool, local_node_id: &str) -> Result<ExportedBundle> {
    let flows: Vec<DbFlow> = repository::list_flows(pool, 0, i64::MAX)?
        .into_iter()
        .filter(|f| !f.is_system)
        .collect();
    let flow_ids: HashSet<&str> = flows.iter().map(|f| f.id.as_str()).collect();
    let flow_model_bindings: Vec<DbFlowModelBinding> = repository::list_flow_model_bindings(pool)?
        .into_iter()
        .filter(|b| flow_ids.contains(b.flow_id.as_str()))
        .collect();
    let model_aliases = repository::list_model_aliases(pool)?;
    let settings = export_allowlisted_settings(pool)?;

    let bundle = ConfigBundle {
        format_version: FORMAT_VERSION,
        source_node_id: local_node_id.to_string(),
        source_environment: crate::services::environment::get_node_environment(pool),
        exported_at: chrono::Utc::now().to_rfc3339(),
        flows,
        flow_model_bindings,
        model_aliases,
        settings,
    };

    let table_counts = vec![
        ("flows".to_string(), bundle.flows.len() as u64),
        (
            "flow_model_bindings".to_string(),
            bundle.flow_model_bindings.len() as u64,
        ),
        (
            "model_aliases".to_string(),
            bundle.model_aliases.len() as u64,
        ),
        ("settings".to_string(), bundle.settings.len() as u64),
    ];

    let archive_bytes = serde_json::to_vec(&bundle)?;
    let manifest_sha256 = sha256_hex(&archive_bytes);
    let filename = format!(
        "tentaflow-config-bundle-{}-{}.json",
        bundle.source_environment.as_str(),
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );

    Ok(ExportedBundle {
        bundle,
        archive_bytes,
        filename,
        manifest_sha256,
        table_counts,
    })
}

/// Test-injectable variant of the settings whitelist filter — the production
/// path always uses `SETTINGS_ALLOWLIST`, but the "a secret never leaks even
/// if mistakenly whitelisted" guarantee is independently testable against an
/// arbitrary allowlist without needing a real secret-shaped production key.
fn export_allowlisted_settings_with(
    pool: &DbPool,
    allowlist: &[&str],
) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for key in allowlist {
        if crate::crypto::SettingsCipher::should_encrypt(key) {
            continue;
        }
        if let Some(value) = repository::get_setting(pool, key)? {
            out.push((key.to_string(), value));
        }
    }
    Ok(out)
}

fn export_allowlisted_settings(pool: &DbPool) -> Result<Vec<(String, String)>> {
    export_allowlisted_settings_with(pool, SETTINGS_ALLOWLIST)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Parses a bundle received either as a QUIC `ConfigBundleExport` response or
/// as an uploaded file — the SAME bytes, the SAME parser. Rejects a
/// `format_version` this build does not know how to interpret (P2-5) — a
/// newer producer's bundle may carry table/field shapes this parser's
/// `serde_json::from_slice` would otherwise silently truncate to defaults
/// instead of failing loudly.
pub fn parse_bundle(archive_bytes: &[u8]) -> Result<ConfigBundle> {
    let bundle: ConfigBundle = serde_json::from_slice(archive_bytes)
        .map_err(|e| anyhow::anyhow!("invalid config bundle: {e}"))?;
    if bundle.format_version != FORMAT_VERSION {
        anyhow::bail!(
            "unsupported config bundle format_version: {} (this build supports {})",
            bundle.format_version,
            FORMAT_VERSION
        );
    }
    Ok(bundle)
}

// =============================================================================
// Diff — what `ImportApply` would change, computed BEFORE anything is written.
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub table: String,
    pub resource_id: String,
    pub label: String,
}

impl DiffEntry {
    /// The `"table:resource_id"` key `ImportApply.selected_resource_keys`
    /// selects entries by.
    pub fn selection_key(&self) -> String {
        format!("{}:{}", self.table, self.resource_id)
    }
}

#[derive(Debug, Clone)]
pub struct ImportPreviewDiff {
    pub from_environment: NodeEnvironment,
    pub to_environment: NodeEnvironment,
    pub added: Vec<DiffEntry>,
    pub changed: Vec<DiffEntry>,
    pub skipped: Vec<DiffEntry>,
    pub flows_count: u32,
    pub settings_count: u32,
    pub aliases_count: u32,
}

/// Computes the diff between a donor bundle and the local DB. Pure read-only —
/// callers must gate the "apply" step on `confirm_environment_name` when
/// `to_environment` outranks `from_environment` (D-Z12.8), independently of
/// what this function returns.
pub fn diff_bundle(pool: &DbPool, donor: &ConfigBundle) -> Result<ImportPreviewDiff> {
    let local_environment = crate::services::environment::get_node_environment(pool);
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut skipped = Vec::new();

    let existing_flows = repository::list_flows(pool, 0, i64::MAX)?;
    for f in &donor.flows {
        match existing_flows.iter().find(|e| e.id == f.id) {
            None => added.push(DiffEntry {
                table: "flows".to_string(),
                resource_id: f.id.clone(),
                label: f.name.clone(),
            }),
            Some(existing) if existing.flow_json != f.flow_json || existing.name != f.name => {
                changed.push(DiffEntry {
                    table: "flows".to_string(),
                    resource_id: f.id.clone(),
                    label: f.name.clone(),
                })
            }
            Some(_) => {}
        }
    }

    let existing_bindings = repository::list_flow_model_bindings(pool)?;
    for b in &donor.flow_model_bindings {
        match existing_bindings.iter().find(|e| e.id == b.id) {
            None => added.push(DiffEntry {
                table: "flow_model_bindings".to_string(),
                resource_id: b.id.clone(),
                label: b.model_pattern.clone(),
            }),
            Some(existing)
                if existing.model_pattern != b.model_pattern || existing.priority != b.priority =>
            {
                changed.push(DiffEntry {
                    table: "flow_model_bindings".to_string(),
                    resource_id: b.id.clone(),
                    label: b.model_pattern.clone(),
                })
            }
            Some(_) => {}
        }
    }

    let existing_aliases = repository::list_model_aliases(pool)?;
    for a in &donor.model_aliases {
        match existing_aliases.iter().find(|e| e.alias == a.alias) {
            None => added.push(DiffEntry {
                table: "model_aliases".to_string(),
                resource_id: a.alias.clone(),
                label: a.alias.clone(),
            }),
            Some(existing)
                if existing.target_model != a.target_model
                    || existing.fallback_targets != a.fallback_targets
                    || existing.strategy != a.strategy =>
            {
                changed.push(DiffEntry {
                    table: "model_aliases".to_string(),
                    resource_id: a.alias.clone(),
                    label: a.alias.clone(),
                })
            }
            Some(_) => {}
        }
    }

    for (key, _) in &donor.settings {
        // A setting arriving in the bundle is, by construction, already
        // allowlisted at the SOURCE. Re-checked locally so a build running an
        // older/newer allowlist fails CLOSED (skipped), never silently
        // applies a key the local build no longer considers portable.
        if SETTINGS_ALLOWLIST.contains(&key.as_str()) {
            added.push(DiffEntry {
                table: "settings".to_string(),
                resource_id: key.clone(),
                label: key.clone(),
            });
        } else {
            skipped.push(DiffEntry {
                table: "settings".to_string(),
                resource_id: key.clone(),
                label: key.clone(),
            });
        }
    }

    Ok(ImportPreviewDiff {
        from_environment: donor.source_environment,
        to_environment: local_environment,
        flows_count: donor.flows.len() as u32,
        settings_count: donor.settings.len() as u32,
        aliases_count: donor.model_aliases.len() as u32,
        added,
        changed,
        skipped,
    })
}

// =============================================================================
// Apply — the only function that writes to the local DB.
// =============================================================================

pub struct ApplyResult {
    pub imported_count: u32,
}

/// Applies the SELECTED entries of a donor bundle. `selected_resource_keys`
/// are `DiffEntry::selection_key()` values — anything not selected is
/// skipped, there is no "select all by default" semantics (matches the wire
/// contract, `EnvironmentImportApplyRequest::selected_resource_keys`).
///
/// Flow import goes through the SAME structural validation (R1-R8) a manual
/// flow save does (`flow_engine::validation::validate`) before it ever
/// reaches SQLite. `registry` being unavailable is a hard error, never a
/// silent skip: a blind `INSERT` of donor flow JSON with no R1-R8 check would
/// let a malformed or malicious DAG land in `flows` unvalidated.
pub fn apply_bundle(
    pool: &DbPool,
    donor: &ConfigBundle,
    selected_resource_keys: &[String],
    registry: Option<&crate::flow_engine::node_adapter::AdapterRegistry>,
) -> Result<ApplyResult> {
    let selected: HashSet<&str> = selected_resource_keys.iter().map(|s| s.as_str()).collect();
    let mut imported = 0u32;

    let mut conn = pool
        .write()
        .map_err(|e| anyhow::anyhow!("db write lock: {e}"))?;
    let tx = conn.transaction()?;

    for f in &donor.flows {
        let key = format!("flows:{}", f.id);
        if !selected.contains(key.as_str()) {
            continue;
        }
        let reg = registry.ok_or_else(|| {
            anyhow::anyhow!(
                "flow '{}' cannot be imported: adapter registry unavailable, refusing an \
                 unvalidated INSERT",
                f.id
            )
        })?;
        let parsed: crate::flow_engine::types::FlowDefinition = serde_json::from_str(&f.flow_json)
            .map_err(|e| anyhow::anyhow!("flow '{}' has invalid flow_json: {e}", f.id))?;
        crate::flow_engine::validation::validate(&parsed, reg)
            .map_err(|e| anyhow::anyhow!("flow '{}' failed structural validation: {e}", f.id))?;
        let existed = tx
            .query_row(
                "SELECT 1 FROM flows WHERE id = ?1",
                rusqlite::params![f.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        tx.execute(
            "INSERT INTO flows (id, name, description, version, is_default, service_type, flow_json, status, published_model_name) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, NULL) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, description = excluded.description, \
                 version = excluded.version, service_type = excluded.service_type, \
                 flow_json = excluded.flow_json, status = excluded.status, updated_at = datetime('now')",
            rusqlite::params![
                f.id,
                f.name,
                f.description,
                f.version,
                f.service_type,
                f.flow_json,
                f.status,
            ],
        )?;
        let mut changed_fields = BTreeMap::new();
        changed_fields.insert(
            "flow_json".to_string(),
            crate::sync::ledger::FieldValue::String(f.flow_json.clone()),
        );
        repository::record_core_capture_for_org_tx(
            &tx,
            crate::sync::core_registry::CoreSyncResourceKind::Flow,
            crate::services::org::DEFAULT_ORG_ID,
            f.id.clone(),
            if existed {
                crate::sync::runtime::SqlWriteAction::Update
            } else {
                crate::sync::runtime::SqlWriteAction::Insert
            },
            changed_fields,
            None,
        )?;
        imported += 1;
    }

    for b in &donor.flow_model_bindings {
        let key = format!("flow_model_bindings:{}", b.id);
        if !selected.contains(key.as_str()) {
            continue;
        }
        tx.execute(
            "INSERT INTO flow_model_bindings (id, flow_id, model_pattern, priority) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET flow_id = excluded.flow_id, \
                 model_pattern = excluded.model_pattern, priority = excluded.priority",
            rusqlite::params![b.id, b.flow_id, b.model_pattern, b.priority],
        )?;
        imported += 1;
    }

    for a in &donor.model_aliases {
        let key = format!("model_aliases:{}", a.alias);
        if !selected.contains(key.as_str()) {
            continue;
        }
        let existing_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM model_aliases WHERE alias = ?1",
                rusqlite::params![a.alias],
                |row| row.get(0),
            )
            .optional()?;
        match existing_id {
            Some(id) => {
                repository::update_model_alias_with_chain_check_tx(
                    &tx,
                    id,
                    &a.alias,
                    &a.target_model,
                    a.is_active,
                    a.fallback_targets.as_deref(),
                    a.strategy.as_deref(),
                )?;
            }
            None => {
                repository::create_model_alias_with_chain_check_tx(
                    &tx,
                    &a.alias,
                    &a.target_model,
                    a.fallback_targets.as_deref(),
                    a.strategy.as_deref(),
                )?;
            }
        }
        imported += 1;
    }

    for (setting_key, value) in &donor.settings {
        let key = format!("settings:{}", setting_key);
        if !selected.contains(key.as_str()) || !SETTINGS_ALLOWLIST.contains(&setting_key.as_str()) {
            continue;
        }
        // Goes through the same chokepoint as every other settings write
        // (`repository::set_setting`) rather than a raw `tx.execute` — a raw
        // write here is a no-op difference TODAY (`SETTINGS_ALLOWLIST` is
        // empty) but silently skips shared-setting fleet replication
        // (`is_shared_setting_key`/`record_core_capture_tx`) the moment a key
        // is added to the allowlist.
        repository::set_setting_tx(&tx, setting_key, value)?;
        imported += 1;
    }

    tx.commit()?;
    Ok(ApplyResult {
        imported_count: imported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_pool() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn seed_flow(pool: &DbPool, id: &str, name: &str, flow_json: &str) {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, ?2, ?3, 'active')",
            rusqlite::params![id, name, flow_json],
        )
        .unwrap();
    }

    /// Minimal structurally-valid flow (R5: exactly one entry node) — `apply_
    /// bundle` runs every imported flow through R1-R8 validation, so a
    /// bare-`{}` flow (valid only for pre-Z12 diff-only tests) can no longer
    /// stand in for an APPLIED flow.
    const VALID_FLOW_JSON: &str =
        r#"{"nodes":[{"id":"t1","type":"trigger","config":{}}],"edges":[]}"#;

    fn test_adapter_registry() -> crate::flow_engine::node_adapter::AdapterRegistry {
        let mut registry = crate::flow_engine::node_adapter::AdapterRegistry::new();
        registry.register(Arc::new(
            crate::flow_engine::node_adapters::TriggerNodeAdapter::new(),
        ));
        registry
    }

    #[test]
    fn export_excludes_system_flows() {
        let pool = test_pool();
        seed_flow(&pool, "f1", "user flow", "{}");
        {
            let conn = pool.write().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status, is_system) VALUES ('sys1', 'system flow', '{}', 'active', 1)",
                [],
            )
            .unwrap();
        }
        let exported = export_bundle(&pool, "node-a").unwrap();
        assert_eq!(exported.bundle.flows.len(), 1);
        assert_eq!(exported.bundle.flows[0].id, "f1");
    }

    /// Even a key mistakenly added to the allowlist must never leave the
    /// node if it matches `SettingsCipher::should_encrypt`'s secret-name
    /// heuristic — whitelist is not a bypass for the secret filter.
    #[test]
    fn secret_shaped_key_never_exported_even_if_whitelisted() {
        let pool = test_pool();
        repository::set_setting(&pool, "hf_token", "super-secret-value").unwrap();
        repository::set_setting(&pool, "ui_theme", "dark").unwrap();
        let misconfigured_allowlist = &["hf_token", "ui_theme"];
        let exported = export_allowlisted_settings_with(&pool, misconfigured_allowlist).unwrap();
        assert!(
            exported.iter().all(|(k, _)| k != "hf_token"),
            "a secret-shaped key must never be exported even if whitelisted"
        );
        assert!(exported.iter().any(|(k, _)| k == "ui_theme"));
    }

    /// `manifest_sha256` must be a pure function of the archive bytes — the
    /// SAME content re-serialized always hashes identically, independent of
    /// when it happened to be exported. `export_bundle` itself stamps a live
    /// `exported_at`, so this constructs two byte-identical bundles directly
    /// rather than calling it twice (which would legitimately differ by
    /// timestamp).
    #[test]
    fn manifest_sha256_is_a_pure_function_of_archive_bytes() {
        let pool = test_pool();
        seed_flow(&pool, "f1", "flow one", "{}");
        let exported = export_bundle(&pool, "node-a").unwrap();
        let recomputed = sha256_hex(&exported.archive_bytes);
        assert_eq!(exported.manifest_sha256, recomputed);

        // Re-parsing and re-serializing the SAME bundle must reproduce the
        // exact same bytes/hash — the format is deterministic.
        let reparsed = parse_bundle(&exported.archive_bytes).unwrap();
        let re_bytes = serde_json::to_vec(&reparsed).unwrap();
        assert_eq!(sha256_hex(&re_bytes), exported.manifest_sha256);
    }

    #[test]
    fn diff_reports_new_flow_as_added() {
        let donor_pool = test_pool();
        seed_flow(
            &donor_pool,
            "f1",
            "donor flow",
            "{\"nodes\":[],\"edges\":[]}",
        );
        let donor = export_bundle(&donor_pool, "donor-node").unwrap().bundle;

        let receiver_pool = test_pool();
        let diff = diff_bundle(&receiver_pool, &donor).unwrap();
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].table, "flows");
        assert_eq!(diff.added[0].resource_id, "f1");
        assert!(diff.changed.is_empty());
    }

    #[test]
    fn diff_reports_existing_flow_with_different_json_as_changed() {
        let donor_pool = test_pool();
        seed_flow(
            &donor_pool,
            "f1",
            "donor flow",
            "{\"nodes\":[1],\"edges\":[]}",
        );
        let donor = export_bundle(&donor_pool, "donor-node").unwrap().bundle;

        let receiver_pool = test_pool();
        seed_flow(
            &receiver_pool,
            "f1",
            "donor flow",
            "{\"nodes\":[],\"edges\":[]}",
        );
        let diff = diff_bundle(&receiver_pool, &donor).unwrap();
        assert!(diff.added.is_empty());
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].resource_id, "f1");
    }

    #[test]
    fn apply_writes_only_selected_entries() {
        let donor_pool = test_pool();
        seed_flow(&donor_pool, "f1", "flow one", VALID_FLOW_JSON);
        seed_flow(&donor_pool, "f2", "flow two", VALID_FLOW_JSON);
        let donor = export_bundle(&donor_pool, "donor-node").unwrap().bundle;

        let receiver_pool = test_pool();
        let registry = test_adapter_registry();
        let result = apply_bundle(
            &receiver_pool,
            &donor,
            &["flows:f1".to_string()],
            Some(&registry),
        )
        .unwrap();
        assert_eq!(result.imported_count, 1);

        let conn = receiver_pool.read().unwrap();
        let f1: Option<String> = conn
            .query_row("SELECT id FROM flows WHERE id = 'f1'", [], |r| r.get(0))
            .optional()
            .unwrap();
        let f2: Option<String> = conn
            .query_row("SELECT id FROM flows WHERE id = 'f2'", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert!(f1.is_some());
        assert!(f2.is_none(), "unselected entries must never be applied");
    }

    #[test]
    fn apply_is_idempotent_on_repeated_pulls() {
        let donor_pool = test_pool();
        seed_flow(&donor_pool, "f1", "flow one", VALID_FLOW_JSON);
        let donor = export_bundle(&donor_pool, "donor-node").unwrap().bundle;

        let receiver_pool = test_pool();
        let registry = test_adapter_registry();
        apply_bundle(
            &receiver_pool,
            &donor,
            &["flows:f1".to_string()],
            Some(&registry),
        )
        .unwrap();
        apply_bundle(
            &receiver_pool,
            &donor,
            &["flows:f1".to_string()],
            Some(&registry),
        )
        .unwrap();

        let conn = receiver_pool.read().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM flows WHERE id = 'f1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 1,
            "re-importing the same donor must not duplicate rows"
        );
    }

    /// A selected flow entry with no adapter registry available must be a
    /// hard error, never a silent unvalidated `INSERT` (P3).
    #[test]
    fn apply_selected_flow_without_registry_is_a_hard_error() {
        let donor_pool = test_pool();
        seed_flow(&donor_pool, "f1", "flow one", VALID_FLOW_JSON);
        let donor = export_bundle(&donor_pool, "donor-node").unwrap().bundle;

        let receiver_pool = test_pool();
        let result = apply_bundle(&receiver_pool, &donor, &["flows:f1".to_string()], None);
        assert!(
            result.is_err(),
            "importing a selected flow without a registry must refuse, not silently insert"
        );

        let conn = receiver_pool.read().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM flows WHERE id = 'f1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "the transaction must roll back, not partially apply"
        );
    }

    /// A `format_version` newer than this build understands must be rejected
    /// outright rather than silently defaulting missing/renamed fields (P2-5).
    #[test]
    fn parse_bundle_rejects_unknown_format_version() {
        let donor_pool = test_pool();
        seed_flow(&donor_pool, "f1", "flow one", VALID_FLOW_JSON);
        let mut bundle = export_bundle(&donor_pool, "donor-node").unwrap().bundle;
        bundle.format_version = FORMAT_VERSION + 1;
        let bytes = serde_json::to_vec(&bundle).unwrap();
        assert!(parse_bundle(&bytes).is_err());
    }

    #[test]
    fn round_trip_through_bytes() {
        let donor_pool = test_pool();
        seed_flow(&donor_pool, "f1", "flow one", "{}");
        let exported = export_bundle(&donor_pool, "donor-node").unwrap();
        let parsed = parse_bundle(&exported.archive_bytes).unwrap();
        assert_eq!(parsed.flows.len(), 1);
        assert_eq!(parsed.source_node_id, "donor-node");
    }
}
