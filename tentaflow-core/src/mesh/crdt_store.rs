// =============================================================================
// Plik: mesh/crdt_store.rs
// Opis: Warstwa persystencji CRDT w SQLite — zapis/odczyt operacji, aplikacja
//       zmian do tabel biznesowych, version vector i kompaktowanie logu.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use tracing::warn;

use super::crdt::{CrdtOperation, LamportClock};
use crate::crypto::SettingsCipher;

/// Persystencja operacji CRDT w bazie SQLite
pub struct CrdtStore {
    conn: Arc<Mutex<Connection>>,
    settings_cipher: Option<Arc<SettingsCipher>>,
}

impl CrdtStore {
    /// Tworzy nowy CrdtStore z istniejacym polaczeniem
    pub fn new(conn: Arc<Mutex<Connection>>) -> Result<Self> {
        Ok(Self {
            conn,
            settings_cipher: None,
        })
    }

    /// Tworzy CrdtStore z cipherem do szyfrowania/deszyfrowania sekretow
    pub fn with_cipher(conn: Arc<Mutex<Connection>>, cipher: Arc<SettingsCipher>) -> Result<Self> {
        Ok(Self {
            conn,
            settings_cipher: Some(cipher),
        })
    }

    /// Zapisuje operacje CRDT do tabeli crdt_operations.
    /// Dla UpsertSetting z kluczem wrazliwym — deszyfruje wartosc przed zapisem,
    /// tak aby peer z innym master key mogl odczytac plaintext.
    pub fn save_operation(&self, op: &CrdtOperation, clock: &LamportClock) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad locka: {e}"))?;

        let op_to_save = self.decrypt_setting_for_sync(op);
        let (op_type, op_key) = Self::operation_type_and_key(&op_to_save);
        let op_data = serde_json::to_string(&op_to_save).context("Serializacja operacji CRDT")?;

        conn.execute(
            "INSERT INTO crdt_operations (clock_time, clock_node_hash, op_type, op_key, op_data) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                clock.time as i64,
                clock.node_id_hash as i64,
                op_type,
                op_key,
                op_data,
            ],
        )?;

        Ok(())
    }

    /// Pobiera operacje nowsze niz podany czas (do delta sync)
    pub fn get_operations_since(
        &self,
        since_time: u64,
    ) -> Result<Vec<(LamportClock, CrdtOperation)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad locka: {e}"))?;

        let mut stmt = conn.prepare(
            "SELECT clock_time, clock_node_hash, op_data FROM crdt_operations \
             WHERE clock_time > ?1 ORDER BY clock_time ASC, id ASC",
        )?;

        let rows = stmt.query_map(params![since_time as i64], |row| {
            let time: i64 = row.get(0)?;
            let node_hash: i64 = row.get(1)?;
            let data: String = row.get(2)?;
            Ok((time as u64, node_hash as u64, data))
        })?;

        let mut result = Vec::new();
        for row in rows {
            let (time, node_id_hash, data) = row?;
            let clock = LamportClock { time, node_id_hash };
            let op: CrdtOperation =
                serde_json::from_str(&data).context("Deserializacja operacji CRDT z bazy")?;
            result.push((clock, op));
        }

        Ok(result)
    }

    /// Aplikuje operacje CRDT do tabel biznesowych
    pub fn apply_to_db(&self, op: &CrdtOperation) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad locka: {e}"))?;

        match op {
            CrdtOperation::UpsertService {
                id,
                name,
                data_json,
                ..
            } => {
                // Parsuj config z data_json, uzyj domyslnych wartosci dla brakujacych pol
                let data: serde_json::Value =
                    serde_json::from_str(data_json).unwrap_or_else(|_| serde_json::json!({}));

                let service_type = data["service_type"].as_str().unwrap_or("llm");
                let strategy = data["strategy"].as_str().unwrap_or("single");
                let model_category = data["model_category"].as_str();
                let status = data["status"].as_str().unwrap_or("active");
                let config_json = data
                    .get("config_json")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());

                conn.execute(
                    "INSERT OR REPLACE INTO services (id, name, service_type, strategy, model_category, status, config_json, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
                    params![id, name, service_type, strategy, model_category, status, config_json],
                )?;
            }

            CrdtOperation::DeleteService { id, .. } => {
                conn.execute("DELETE FROM services WHERE id = ?1", params![id])?;
            }

            CrdtOperation::UpsertModel {
                id,
                name,
                data_json,
                ..
            } => {
                let data: serde_json::Value =
                    serde_json::from_str(data_json).unwrap_or_else(|_| serde_json::json!({}));

                let display_name = data["display_name"].as_str();
                let service_type = data["service_type"].as_str().unwrap_or("llm");
                let connection_type = data["connection_type"].as_str().unwrap_or("quic");
                let service_id = data["service_id"].as_i64();
                let flow_id = data["flow_id"].as_i64();
                let is_public = data["is_public"].as_i64().unwrap_or(1);
                let is_active = data["is_active"].as_i64().unwrap_or(1);
                let config_json = data
                    .get("config_json")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());

                conn.execute(
                    "INSERT OR REPLACE INTO model_registry \
                     (id, model_name, display_name, service_type, connection_type, service_id, flow_id, is_public, is_active, config_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![id, name, display_name, service_type, connection_type, service_id, flow_id, is_public, is_active, config_json],
                )?;
            }

            CrdtOperation::DeleteModel { id, .. } => {
                conn.execute("DELETE FROM model_registry WHERE id = ?1", params![id])?;
            }

            CrdtOperation::UpsertAlias { alias, target, .. } => {
                conn.execute(
                    "INSERT OR REPLACE INTO model_aliases (alias, target_model, is_active) \
                     VALUES (?1, ?2, 1)",
                    params![alias, target],
                )?;
            }

            CrdtOperation::DeleteAlias { alias, .. } => {
                conn.execute("DELETE FROM model_aliases WHERE alias = ?1", params![alias])?;
            }

            CrdtOperation::UpsertFlow { id, data_json, .. } => {
                let data: serde_json::Value =
                    serde_json::from_str(data_json).unwrap_or_else(|_| serde_json::json!({}));

                let name = data["name"].as_str().unwrap_or("unnamed");
                let description = data["description"].as_str();
                let version = data["version"].as_i64().unwrap_or(1);
                let is_default = data["is_default"].as_i64().unwrap_or(0);
                let service_type = data["service_type"].as_str();
                let flow_json = data
                    .get("flow_json")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let status = data["status"].as_str().unwrap_or("draft");

                conn.execute(
                    "INSERT OR REPLACE INTO flows \
                     (id, name, description, version, is_default, service_type, flow_json, status, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))",
                    params![id, name, description, version, is_default, service_type, flow_json, status],
                )?;
            }

            CrdtOperation::UpsertPrompt {
                prompt_id,
                data_json,
                ..
            } => {
                let data: serde_json::Value =
                    serde_json::from_str(data_json).unwrap_or_else(|_| serde_json::json!({}));

                let name = data["name"].as_str().unwrap_or("unnamed");
                let description = data["description"].as_str();
                let content = data["content"].as_str().unwrap_or("");
                let prompt_type = data["prompt_type"].as_str().unwrap_or("system");
                let default_model = data["default_model"].as_str();
                let variables = data["variables"].as_str();
                let cache_priority = data["cache_priority"].as_i64().unwrap_or(50);
                let is_active = data["is_active"].as_i64().unwrap_or(1);
                let version = data["version"].as_i64().unwrap_or(1);

                conn.execute(
                    "INSERT INTO prompts \
                     (prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority, is_active, version, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, datetime('now')) \
                     ON CONFLICT(prompt_id) DO UPDATE SET \
                     name=excluded.name, description=excluded.description, content=excluded.content, \
                     prompt_type=excluded.prompt_type, default_model=excluded.default_model, \
                     variables=excluded.variables, cache_priority=excluded.cache_priority, \
                     is_active=excluded.is_active, version=excluded.version, updated_at=datetime('now')",
                    params![prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority, is_active, version],
                )?;
            }

            CrdtOperation::UpsertApiKey { id, data_json, .. } => {
                let data: serde_json::Value =
                    serde_json::from_str(data_json).unwrap_or_else(|_| serde_json::json!({}));

                let key_hash = data["key_hash"].as_str().unwrap_or("");
                let key_prefix = data["key_prefix"].as_str().unwrap_or("");
                let name = data["name"].as_str().unwrap_or("unnamed");
                let rate_limit_rps = data["rate_limit_rps"].as_i64().unwrap_or(100);
                let is_active = data["is_active"].as_i64().unwrap_or(1);

                conn.execute(
                    "INSERT OR REPLACE INTO api_keys (id, key_hash, key_prefix, name, rate_limit_rps, is_active) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, key_hash, key_prefix, name, rate_limit_rps, is_active],
                )?;
            }

            CrdtOperation::UpsertSetting { key, value, .. } => {
                // Retired feature flag — older peers may still propagate it.
                // Drop silently instead of writing dead state into settings.
                if key == "flow_engine_enabled" {
                    return Ok(());
                }
                // Re-szyfruj wartosc lokalnym master key jesli klucz jest wrazliwy
                let store_value = self.encrypt_setting_for_storage(key, value);
                conn.execute(
                    "INSERT OR REPLACE INTO settings (key, value, updated_at) \
                     VALUES (?1, ?2, datetime('now'))",
                    params![key, store_value],
                )?;
            }

            // --- Nowe operacje: Users ---
            CrdtOperation::UpsertUser {
                username,
                password_hash,
                display_name,
                email,
                is_active,
                is_admin,
                ..
            } => {
                // Sprawdz sync exclusions
                if Self::check_sync_exclusion(&conn, "users")? {
                    return Ok(());
                }
                conn.execute(
                    "INSERT INTO user_accounts (username, password_hash, display_name, email, is_active, is_admin) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT(username) DO UPDATE SET \
                     password_hash = excluded.password_hash, \
                     display_name = excluded.display_name, \
                     email = excluded.email, \
                     is_active = excluded.is_active, \
                     is_admin = excluded.is_admin, \
                     updated_at = datetime('now')",
                    params![username, password_hash, display_name, email, is_active, is_admin],
                )?;
            }

            CrdtOperation::DeleteUser { username, .. } => {
                if Self::check_sync_exclusion(&conn, "users")? {
                    return Ok(());
                }
                conn.execute(
                    "DELETE FROM user_accounts WHERE username = ?1",
                    params![username],
                )?;
            }

            // --- Nowe operacje: Groups ---
            CrdtOperation::UpsertGroup {
                name, description, ..
            } => {
                if Self::check_sync_exclusion(&conn, "groups")? {
                    return Ok(());
                }
                conn.execute(
                    "INSERT INTO user_groups (name, description) VALUES (?1, ?2) \
                     ON CONFLICT(name) DO UPDATE SET description = excluded.description",
                    params![name, description],
                )?;
            }

            CrdtOperation::DeleteGroup { name, .. } => {
                if Self::check_sync_exclusion(&conn, "groups")? {
                    return Ok(());
                }
                conn.execute("DELETE FROM user_groups WHERE name = ?1", params![name])?;
            }

            CrdtOperation::AddGroupMember {
                group_name,
                username,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "groups")? {
                    return Ok(());
                }
                conn.execute(
                    "INSERT OR IGNORE INTO group_members (group_id, user_id) \
                     SELECT g.id, u.id FROM user_groups g, user_accounts u \
                     WHERE g.name = ?1 AND u.username = ?2",
                    params![group_name, username],
                )?;
            }

            CrdtOperation::RemoveGroupMember {
                group_name,
                username,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "groups")? {
                    return Ok(());
                }
                conn.execute(
                    "DELETE FROM group_members WHERE group_id IN \
                     (SELECT id FROM user_groups WHERE name = ?1) \
                     AND user_id IN (SELECT id FROM user_accounts WHERE username = ?2)",
                    params![group_name, username],
                )?;
            }

            // --- Nowe operacje: Permissions ---
            CrdtOperation::SetPermission {
                addon_id,
                subject_type,
                subject_name,
                resource,
                access_level,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "permissions")? {
                    return Ok(());
                }
                // Rozwiaz subject_name na subject_id
                let subject_id: Option<i64> = match subject_type.as_str() {
                    "user" => conn
                        .query_row(
                            "SELECT id FROM user_accounts WHERE username = ?1",
                            params![subject_name],
                            |row| row.get(0),
                        )
                        .ok(),
                    "group" => conn
                        .query_row(
                            "SELECT id FROM user_groups WHERE name = ?1",
                            params![subject_name],
                            |row| row.get(0),
                        )
                        .ok(),
                    _ => None,
                };
                if let Some(sid) = subject_id {
                    conn.execute(
                        "INSERT INTO addon_permissions (addon_id, subject_type, subject_id, resource, access_level) \
                         VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT(addon_id, subject_type, subject_id, resource) \
                         DO UPDATE SET access_level = excluded.access_level",
                        params![addon_id, subject_type, sid, resource, access_level],
                    )?;
                }
            }

            CrdtOperation::DeletePermission {
                addon_id,
                subject_type,
                subject_name,
                resource,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "permissions")? {
                    return Ok(());
                }
                let subject_id: Option<i64> = match subject_type.as_str() {
                    "user" => conn
                        .query_row(
                            "SELECT id FROM user_accounts WHERE username = ?1",
                            params![subject_name],
                            |row| row.get(0),
                        )
                        .ok(),
                    "group" => conn
                        .query_row(
                            "SELECT id FROM user_groups WHERE name = ?1",
                            params![subject_name],
                            |row| row.get(0),
                        )
                        .ok(),
                    _ => None,
                };
                if let Some(sid) = subject_id {
                    conn.execute(
                        "DELETE FROM addon_permissions \
                         WHERE addon_id = ?1 AND subject_type = ?2 AND subject_id = ?3 AND resource = ?4",
                        params![addon_id, subject_type, sid, resource],
                    )?;
                }
            }

            // --- Nowe operacje: Addons ---
            CrdtOperation::SyncAddon {
                addon_id,
                name,
                version,
                manifest_json,
                platforms,
                wasm_hash,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "addons")? {
                    return Ok(());
                }
                conn.execute(
                    "INSERT INTO addons (addon_id, name, version, manifest_json, platforms) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(addon_id) DO UPDATE SET \
                     name = excluded.name, version = excluded.version, \
                     manifest_json = excluded.manifest_json, platforms = excluded.platforms, \
                     updated_at = datetime('now')",
                    params![addon_id, name, version, manifest_json, platforms],
                )?;
                // Zapisz wasm_hash do porownywania przy nastepnym sync
                conn.execute(
                    "INSERT OR REPLACE INTO settings (key, value, updated_at) \
                     VALUES (?1, ?2, datetime('now'))",
                    params![format!("addon_wasm_hash:{addon_id}"), wasm_hash],
                )?;
            }

            CrdtOperation::DeleteAddon { addon_id, .. } => {
                if Self::check_sync_exclusion(&conn, "addons")? {
                    return Ok(());
                }
                conn.execute("DELETE FROM addons WHERE addon_id = ?1", params![addon_id])?;
            }

            // --- Nowe operacje: Addon configs ---
            CrdtOperation::SetAddonConfig {
                addon_id,
                key,
                value,
                ..
            } => {
                conn.execute(
                    "INSERT OR REPLACE INTO settings (key, value, updated_at) \
                     VALUES (?1, ?2, datetime('now'))",
                    params![format!("addon_config:{addon_id}:{key}"), value],
                )?;
            }

            // --- Nowe operacje: Secrets ---
            CrdtOperation::SetSecret {
                addon_id,
                username,
                key,
                encrypted_value,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "secrets")? {
                    return Ok(());
                }
                let user_id: Option<i64> = username.as_ref().and_then(|uname| {
                    conn.query_row(
                        "SELECT id FROM user_accounts WHERE username = ?1",
                        params![uname],
                        |row| row.get(0),
                    )
                    .ok()
                });
                conn.execute(
                    "INSERT INTO addon_secrets (addon_id, user_id, key, value_encrypted) \
                     VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(addon_id, user_id, key) \
                     DO UPDATE SET value_encrypted = excluded.value_encrypted, updated_at = datetime('now')",
                    params![addon_id, user_id, key, encrypted_value],
                )?;
            }

            CrdtOperation::DeleteSecret {
                addon_id,
                username,
                key,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "secrets")? {
                    return Ok(());
                }
                let user_id: Option<i64> = username.as_ref().and_then(|uname| {
                    conn.query_row(
                        "SELECT id FROM user_accounts WHERE username = ?1",
                        params![uname],
                        |row| row.get(0),
                    )
                    .ok()
                });
                conn.execute(
                    "DELETE FROM addon_secrets WHERE addon_id = ?1 AND user_id IS ?2 AND key = ?3",
                    params![addon_id, user_id, key],
                )?;
            }

            // --- Nowe operacje: SSO Providers ---
            CrdtOperation::UpsertSsoProvider {
                name,
                provider_type,
                client_id,
                client_secret_encrypted,
                discovery_url,
                enabled,
                ..
            } => {
                if Self::check_sync_exclusion(&conn, "sso")? {
                    return Ok(());
                }
                conn.execute(
                    "INSERT INTO sso_providers (name, provider_type, client_id, client_secret_encrypted, \
                     discovery_url, enabled) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT(name) DO UPDATE SET \
                     provider_type = excluded.provider_type, \
                     client_id = excluded.client_id, \
                     client_secret_encrypted = excluded.client_secret_encrypted, \
                     discovery_url = excluded.discovery_url, \
                     enabled = excluded.enabled",
                    params![name, provider_type, client_id, client_secret_encrypted, discovery_url, enabled],
                )?;
            }

            CrdtOperation::DeleteSsoProvider { name, .. } => {
                if Self::check_sync_exclusion(&conn, "sso")? {
                    return Ok(());
                }
                conn.execute("DELETE FROM sso_providers WHERE name = ?1", params![name])?;
            }

            // --- Nowe operacje: Sync Exclusions ---
            CrdtOperation::SetSyncExclusion {
                group_name,
                resource_type,
                ..
            } => {
                conn.execute(
                    "INSERT OR IGNORE INTO sync_exclusions (group_id, resource_type) \
                     SELECT id, ?2 FROM user_groups WHERE name = ?1",
                    params![group_name, resource_type],
                )?;
            }

            CrdtOperation::DeleteSyncExclusion {
                group_name,
                resource_type,
                ..
            } => {
                conn.execute(
                    "DELETE FROM sync_exclusions WHERE group_id IN \
                     (SELECT id FROM user_groups WHERE name = ?1) AND resource_type = ?2",
                    params![group_name, resource_type],
                )?;
            }

            // --- Operacje na zaufanych nodach mesh ---
            CrdtOperation::AddTrustedNode {
                node_id,
                public_key_hex,
                hostname,
                ..
            } => {
                conn.execute(
                    "INSERT OR REPLACE INTO trusted_nodes (node_id, public_key, hostname, approved_by, approved_at, is_active) \
                     VALUES (?1, ?2, ?3, 'crdt-sync', datetime('now'), 1)",
                    params![node_id, public_key_hex, hostname],
                )?;
            }

            CrdtOperation::RemoveTrustedNode { node_id, .. } => {
                conn.execute(
                    "DELETE FROM trusted_nodes WHERE node_id = ?1",
                    params![node_id],
                )?;
            }

            CrdtOperation::RevokeTrustedNode { node_id, .. } => {
                conn.execute(
                    "DELETE FROM trusted_nodes WHERE node_id = ?1",
                    params![node_id],
                )?;
                conn.execute(
                    "INSERT OR IGNORE INTO revoked_nodes (node_id, revoked_by) VALUES (?1, ?2)",
                    params![node_id, "crdt-sync"],
                )?;
            }
        }

        Ok(())
    }

    /// Sprawdza czy dany typ zasobu jest wykluczony z synchronizacji.
    fn check_sync_exclusion(conn: &Connection, resource_type: &str) -> Result<bool> {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_exclusions WHERE resource_type = ?1",
                params![resource_type],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count > 0)
    }

    /// Pobiera version vector z bazy
    pub fn load_version_vector(&self) -> Result<HashMap<u64, u64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad locka: {e}"))?;

        let mut stmt = conn.prepare("SELECT node_hash, last_time FROM crdt_version_vector")?;

        let rows = stmt.query_map([], |row| {
            let node_hash: i64 = row.get(0)?;
            let last_time: i64 = row.get(1)?;
            Ok((node_hash as u64, last_time as u64))
        })?;

        let mut vv = HashMap::new();
        for row in rows {
            let (node_hash, last_time) = row?;
            vv.insert(node_hash, last_time);
        }

        Ok(vv)
    }

    /// Zapisuje version vector do bazy
    pub fn save_version_vector(&self, vv: &HashMap<u64, u64>) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad locka: {e}"))?;

        for (&node_hash, &last_time) in vv {
            conn.execute(
                "INSERT OR REPLACE INTO crdt_version_vector (node_hash, last_time, updated_at) \
                 VALUES (?1, ?2, datetime('now'))",
                params![node_hash as i64, last_time as i64],
            )?;
        }

        Ok(())
    }

    /// Kompaktuje stare operacje — zachowuje tylko najnowsza per klucz.
    /// Zwraca liczbe usunietych wierszy.
    pub fn compact(&self, keep_recent: usize) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad locka: {e}"))?;

        // Pobierz calkowita liczbe operacji
        let total: i64 =
            conn.query_row("SELECT COUNT(*) FROM crdt_operations", [], |row| row.get(0))?;

        if (total as usize) <= keep_recent {
            return Ok(0);
        }

        // Dla kazdego klucza zachowaj najnowsza operacje + ostatnie keep_recent wierszy
        // Usun wiersze ktore nie sa najnowsze per klucz i nie naleza do ostatnich keep_recent
        let deleted = conn.execute(
            "DELETE FROM crdt_operations WHERE id NOT IN (
                SELECT id FROM (
                    SELECT id, ROW_NUMBER() OVER (PARTITION BY op_key ORDER BY clock_time DESC, id DESC) AS rn
                    FROM crdt_operations
                ) WHERE rn = 1
                UNION
                SELECT id FROM (
                    SELECT id FROM crdt_operations ORDER BY id DESC LIMIT ?1
                )
            )",
            params![keep_recent as i64],
        )?;

        Ok(deleted)
    }

    /// Zwraca typ i klucz operacji (do zapisu w bazie)
    fn operation_type_and_key(op: &CrdtOperation) -> (&'static str, String) {
        match op {
            CrdtOperation::UpsertService { id, .. } => ("upsert_service", format!("service:{id}")),
            CrdtOperation::DeleteService { id, .. } => ("delete_service", format!("service:{id}")),
            CrdtOperation::UpsertModel { id, .. } => ("upsert_model", format!("model:{id}")),
            CrdtOperation::DeleteModel { id, .. } => ("delete_model", format!("model:{id}")),
            CrdtOperation::UpsertAlias { alias, .. } => ("upsert_alias", format!("alias:{alias}")),
            CrdtOperation::DeleteAlias { alias, .. } => ("delete_alias", format!("alias:{alias}")),
            CrdtOperation::UpsertFlow { id, .. } => ("upsert_flow", format!("flow:{id}")),
            CrdtOperation::UpsertPrompt { prompt_id, .. } => {
                ("upsert_prompt", format!("prompt:{prompt_id}"))
            }
            CrdtOperation::UpsertApiKey { id, .. } => ("upsert_apikey", format!("apikey:{id}")),
            CrdtOperation::UpsertSetting { key, .. } => {
                ("upsert_setting", format!("setting:{key}"))
            }

            // Nowe typy operacji
            CrdtOperation::UpsertUser { username, .. } => {
                ("upsert_user", format!("user:{username}"))
            }
            CrdtOperation::DeleteUser { username, .. } => {
                ("delete_user", format!("user:{username}"))
            }
            CrdtOperation::UpsertGroup { name, .. } => ("upsert_group", format!("group:{name}")),
            CrdtOperation::DeleteGroup { name, .. } => ("delete_group", format!("group:{name}")),
            CrdtOperation::AddGroupMember {
                group_name,
                username,
                ..
            } => (
                "add_group_member",
                format!("group_member:{group_name}:{username}"),
            ),
            CrdtOperation::RemoveGroupMember {
                group_name,
                username,
                ..
            } => (
                "remove_group_member",
                format!("group_member:{group_name}:{username}"),
            ),
            CrdtOperation::SetPermission {
                addon_id,
                subject_type,
                subject_name,
                resource,
                ..
            } => (
                "set_permission",
                format!("perm:{addon_id}:{subject_type}:{subject_name}:{resource}"),
            ),
            CrdtOperation::DeletePermission {
                addon_id,
                subject_type,
                subject_name,
                resource,
                ..
            } => (
                "delete_permission",
                format!("perm:{addon_id}:{subject_type}:{subject_name}:{resource}"),
            ),
            CrdtOperation::SyncAddon { addon_id, .. } => {
                ("sync_addon", format!("addon:{addon_id}"))
            }
            CrdtOperation::DeleteAddon { addon_id, .. } => {
                ("delete_addon", format!("addon:{addon_id}"))
            }
            CrdtOperation::SetAddonConfig { addon_id, key, .. } => {
                ("set_addon_config", format!("addon_config:{addon_id}:{key}"))
            }
            CrdtOperation::SetSecret {
                addon_id,
                username,
                key,
                ..
            } => {
                let uname = username.as_deref().unwrap_or("_global_");
                ("set_secret", format!("secret:{addon_id}:{uname}:{key}"))
            }
            CrdtOperation::DeleteSecret {
                addon_id,
                username,
                key,
                ..
            } => {
                let uname = username.as_deref().unwrap_or("_global_");
                ("delete_secret", format!("secret:{addon_id}:{uname}:{key}"))
            }
            CrdtOperation::UpsertSsoProvider { name, .. } => ("upsert_sso", format!("sso:{name}")),
            CrdtOperation::DeleteSsoProvider { name, .. } => ("delete_sso", format!("sso:{name}")),
            CrdtOperation::SetSyncExclusion {
                group_name,
                resource_type,
                ..
            } => (
                "set_sync_exclusion",
                format!("sync_excl:{group_name}:{resource_type}"),
            ),
            CrdtOperation::DeleteSyncExclusion {
                group_name,
                resource_type,
                ..
            } => (
                "delete_sync_exclusion",
                format!("sync_excl:{group_name}:{resource_type}"),
            ),
            CrdtOperation::AddTrustedNode { node_id, .. } => {
                ("add_trusted_node", format!("trusted_node:{node_id}"))
            }
            CrdtOperation::RemoveTrustedNode { node_id, .. } => {
                ("remove_trusted_node", format!("trusted_node:{node_id}"))
            }
            CrdtOperation::RevokeTrustedNode { node_id, .. } => {
                ("revoke_trusted_node", format!("trusted_node:{node_id}"))
            }
        }
    }

    /// Deszyfruje wartosc UpsertSetting przed wyslaniem do peerow.
    /// Transport mesh jest szyfrowany (TLS + ChaCha20), wiec plaintext jest bezpieczny.
    fn decrypt_setting_for_sync(&self, op: &CrdtOperation) -> CrdtOperation {
        if let CrdtOperation::UpsertSetting { key, value, clock } = op {
            if SettingsCipher::should_encrypt(key) {
                if let Some(ref cipher) = self.settings_cipher {
                    match cipher.decrypt(value) {
                        Ok(plain) => {
                            return CrdtOperation::UpsertSetting {
                                key: key.clone(),
                                value: plain,
                                clock: *clock,
                            };
                        }
                        Err(e) => {
                            warn!(key = %key, "Nie udalo sie deszyfrowac setting przed sync: {e}");
                        }
                    }
                }
            }
        }
        op.clone()
    }

    /// Re-szyfruje wartosc wrazliwego klucza lokalnym master key przed zapisem do DB.
    fn encrypt_setting_for_storage(&self, key: &str, value: &str) -> String {
        if SettingsCipher::should_encrypt(key) {
            if let Some(ref cipher) = self.settings_cipher {
                // Jesli wartosc juz jest zaszyfrowana — nie szyfruj ponownie
                if value.starts_with("enc:") {
                    return value.to_string();
                }
                match cipher.encrypt(value) {
                    Ok(encrypted) => return encrypted,
                    Err(e) => {
                        warn!(key = %key, "Nie udalo sie zaszyfrowac setting po sync: {e}");
                    }
                }
            }
        }
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::crdt::LamportClock;

    /// Tworzy baze in-memory z minimalnymi tabelami do testow
    fn setup_test_db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS crdt_operations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                clock_time INTEGER NOT NULL,
                clock_node_hash INTEGER NOT NULL,
                op_type TEXT NOT NULL,
                op_key TEXT NOT NULL,
                op_data TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_crdt_ops_time ON crdt_operations(clock_time);
            CREATE INDEX IF NOT EXISTS idx_crdt_ops_key ON crdt_operations(op_key);

            CREATE TABLE IF NOT EXISTS crdt_version_vector (
                node_hash INTEGER PRIMARY KEY,
                last_time INTEGER NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS model_aliases (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                alias TEXT UNIQUE NOT NULL,
                target_model TEXT NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 1
            );
            ",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn save_and_load_operations() {
        let pool = setup_test_db();
        let store = CrdtStore::new(pool).unwrap();

        let mut clock = LamportClock::new("test-node");
        let t1 = clock.tick();

        let op = CrdtOperation::UpsertSetting {
            key: "klucz-testowy".into(),
            value: "wartosc-testowa".into(),
            clock: t1,
        };

        store.save_operation(&op, &t1).unwrap();

        let ops = store.get_operations_since(0).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].0.time, 1);
    }

    #[test]
    fn get_operations_since_filters_correctly() {
        let pool = setup_test_db();
        let store = CrdtStore::new(pool).unwrap();

        let mut clock = LamportClock::new("test-node");

        // Zapisz 3 operacje z rosnacym czasem
        for i in 1..=3 {
            let t = clock.tick();
            let op = CrdtOperation::UpsertSetting {
                key: format!("key-{i}"),
                value: format!("val-{i}"),
                clock: t,
            };
            store.save_operation(&op, &t).unwrap();
        }

        // Pobierz operacje nowsze niz czas 1
        let ops = store.get_operations_since(1).unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].0.time, 2);
        assert_eq!(ops[1].0.time, 3);
    }

    #[test]
    fn apply_upsert_setting() {
        let pool = setup_test_db();
        let store = CrdtStore::new(pool.clone()).unwrap();

        let mut clock = LamportClock::new("test-node");
        let t1 = clock.tick();

        let op = CrdtOperation::UpsertSetting {
            key: "test-key".into(),
            value: "test-value".into(),
            clock: t1,
        };

        store.apply_to_db(&op).unwrap();

        let conn = pool.lock().unwrap();
        let val: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params!["test-key"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(val, "test-value");
    }

    #[test]
    fn apply_upsert_and_delete_alias() {
        let pool = setup_test_db();
        let store = CrdtStore::new(pool.clone()).unwrap();

        let mut clock = LamportClock::new("test-node");

        // Dodaj alias
        let t1 = clock.tick();
        let op_add = CrdtOperation::UpsertAlias {
            alias: "gpt4".into(),
            target: "openai/gpt-4".into(),
            clock: t1,
        };
        store.apply_to_db(&op_add).unwrap();

        {
            let conn = pool.lock().unwrap();
            let target: String = conn
                .query_row(
                    "SELECT target_model FROM model_aliases WHERE alias = 'gpt4'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(target, "openai/gpt-4");
        }

        // Usun alias
        let t2 = clock.tick();
        let op_del = CrdtOperation::DeleteAlias {
            alias: "gpt4".into(),
            clock: t2,
        };
        store.apply_to_db(&op_del).unwrap();

        let conn = pool.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_aliases WHERE alias = 'gpt4'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn version_vector_save_and_load() {
        let pool = setup_test_db();
        let store = CrdtStore::new(pool).unwrap();

        let mut vv = HashMap::new();
        vv.insert(111_u64, 5_u64);
        vv.insert(222_u64, 10_u64);

        store.save_version_vector(&vv).unwrap();

        let loaded = store.load_version_vector().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[&111], 5);
        assert_eq!(loaded[&222], 10);
    }

    #[test]
    fn compact_removes_old_duplicates() {
        let pool = setup_test_db();
        let store = CrdtStore::new(pool.clone()).unwrap();

        let mut clock = LamportClock::new("test-node");

        // 3 updaty tego samego klucza
        for i in 1..=3 {
            let t = clock.tick();
            let op = CrdtOperation::UpsertSetting {
                key: "same-key".into(),
                value: format!("val-{i}"),
                clock: t,
            };
            store.save_operation(&op, &t).unwrap();
        }

        // Plus jeden inny klucz
        let t = clock.tick();
        let op = CrdtOperation::UpsertSetting {
            key: "other-key".into(),
            value: "other-val".into(),
            clock: t,
        };
        store.save_operation(&op, &t).unwrap();

        // Kompaktuj, zachowaj 0 ostatnich (tylko najnowsze per klucz)
        let deleted = store.compact(0).unwrap();
        assert_eq!(deleted, 2);

        let conn = pool.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM crdt_operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn save_operation_decrypts_sensitive_setting() {
        let pool = setup_test_db();
        let cipher = Arc::new(SettingsCipher::new(&[42u8; 32]));
        let store = CrdtStore::with_cipher(pool.clone(), cipher.clone()).unwrap();

        let mut clock = LamportClock::new("test-node");
        let t1 = clock.tick();

        // Zaszyfruj wartosc jak robi to set_setting_secure
        let encrypted = cipher.encrypt("super-tajny-klucz").unwrap();
        assert!(encrypted.starts_with("enc:"));

        let op = CrdtOperation::UpsertSetting {
            key: "ngc_api_key".into(),
            value: encrypted,
            clock: t1,
        };

        store.save_operation(&op, &t1).unwrap();

        // Sprawdz ze w crdt_operations wartosc jest PLAINTEXT (deszyfrowana)
        let ops = store.get_operations_since(0).unwrap();
        assert_eq!(ops.len(), 1);
        if let CrdtOperation::UpsertSetting { value, .. } = &ops[0].1 {
            assert_eq!(value, "super-tajny-klucz");
            assert!(!value.starts_with("enc:"));
        } else {
            panic!("Oczekiwano UpsertSetting");
        }
    }

    #[test]
    fn apply_to_db_reencrypts_sensitive_setting() {
        let pool = setup_test_db();
        let cipher = Arc::new(SettingsCipher::new(&[99u8; 32]));
        let store = CrdtStore::with_cipher(pool.clone(), cipher.clone()).unwrap();

        let mut clock = LamportClock::new("test-node");
        let t1 = clock.tick();

        // Symuluj odbiorcze: wartosc przychodzi plaintext od peera
        let op = CrdtOperation::UpsertSetting {
            key: "ngc_api_key".into(),
            value: "plaintext-od-peera".into(),
            clock: t1,
        };

        store.apply_to_db(&op).unwrap();

        // Sprawdz ze w tabeli settings wartosc jest ZASZYFROWANA
        let conn = pool.lock().unwrap();
        let stored: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params!["ngc_api_key"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            stored.starts_with("enc:"),
            "Wartosc powinna byc zaszyfrowana, ale: {stored}"
        );

        // Deszyfruj i sprawdz poprawnosc
        let decrypted = cipher.decrypt(&stored).unwrap();
        assert_eq!(decrypted, "plaintext-od-peera");
    }

    #[test]
    fn nonsensitive_setting_passes_through() {
        let pool = setup_test_db();
        let cipher = Arc::new(SettingsCipher::new(&[1u8; 32]));
        let store = CrdtStore::with_cipher(pool.clone(), cipher).unwrap();

        let mut clock = LamportClock::new("test-node");
        let t1 = clock.tick();

        let op = CrdtOperation::UpsertSetting {
            key: "flow_debug_mode".into(),
            value: "true".into(),
            clock: t1,
        };

        // save_operation nie powinno zmieniac wartosci
        store.save_operation(&op, &t1).unwrap();
        let ops = store.get_operations_since(0).unwrap();
        if let CrdtOperation::UpsertSetting { value, .. } = &ops[0].1 {
            assert_eq!(value, "true");
        }

        // apply_to_db nie powinno szyfrowac
        store.apply_to_db(&op).unwrap();
        let conn = pool.lock().unwrap();
        let stored: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params!["flow_debug_mode"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "true");
    }
}
