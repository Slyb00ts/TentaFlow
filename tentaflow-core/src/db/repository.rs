// =============================================================================
// Plik: db/repository.rs
// Opis: Operacje CRUD na bazie danych SQLite.
// =============================================================================

use super::models::*;
use super::DbPool;
use anyhow::Result;
use rusqlite::OptionalExtension;

/// Pozyskuje polaczenie z puli (lock na Mutex)
fn acquire(pool: &DbPool) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>> {
    pool.lock().map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))
}

/// Mapowanie wiersza na DbService
fn row_to_service(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbService> {
    Ok(DbService {
        id: row.get(0)?,
        name: row.get(1)?,
        service_type: row.get(2)?,
        strategy: row.get(3)?,
        model_category: row.get(4)?,
        status: row.get(5)?,
        config_json: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        service_uuid: row.get(9)?,
        node_id: row.get(10)?,
    })
}

/// Mapowanie wiersza na DbServiceBackend
fn row_to_backend(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbServiceBackend> {
    Ok(DbServiceBackend {
        id: row.get(0)?,
        service_id: row.get(1)?,
        connection_type: row.get(2)?,
        config_json: row.get(3)?,
        max_concurrent: row.get(4)?,
        timeout_ms: row.get(5)?,
        weight: row.get(6)?,
        model_name_override: row.get(7)?,
        health_check_path: row.get(8)?,
        is_active: row.get(9)?,
    })
}

/// Mapowanie wiersza na DbPrompt
fn row_to_prompt(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbPrompt> {
    Ok(DbPrompt {
        id: row.get(0)?,
        prompt_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        content: row.get(4)?,
        prompt_type: row.get(5)?,
        default_model: row.get(6)?,
        variables: row.get(7)?,
        cache_priority: row.get(8)?,
        is_active: row.get(9)?,
        version: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// Mapowanie wiersza na DbModelEntry
fn row_to_model_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbModelEntry> {
    Ok(DbModelEntry {
        id: row.get(0)?,
        model_name: row.get(1)?,
        display_name: row.get(2)?,
        service_type: row.get(3)?,
        connection_type: row.get(4)?,
        service_id: row.get(5)?,
        flow_id: row.get(6)?,
        is_public: row.get(7)?,
        is_active: row.get(8)?,
        config_json: row.get(9)?,
        created_at: row.get(10)?,
    })
}

/// Mapowanie wiersza na DbFlow
fn row_to_flow(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbFlow> {
    Ok(DbFlow {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        version: row.get(3)?,
        is_default: row.get(4)?,
        service_type: row.get(5)?,
        flow_json: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// Mapowanie wiersza na DbPiiRule
fn row_to_pii_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbPiiRule> {
    Ok(DbPiiRule {
        id: row.get(0)?,
        name: row.get(1)?,
        category: row.get(2)?,
        pattern: row.get(3)?,
        replacement: row.get(4)?,
        is_active: row.get(5)?,
        priority: row.get(6)?,
        description: row.get(7)?,
        test_examples: row.get(8)?,
        created_at: row.get(9)?,
    })
}

/// Mapowanie wiersza na DbFastPathPattern
fn row_to_fast_path_pattern(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbFastPathPattern> {
    Ok(DbFastPathPattern {
        id: row.get(0)?,
        module: row.get(1)?,
        pattern_type: row.get(2)?,
        pattern: row.get(3)?,
        match_type: row.get(4)?,
        result_json: row.get(5)?,
        is_active: row.get(6)?,
        priority: row.get(7)?,
    })
}

/// Mapowanie wiersza na DbTtsCleaningRule
fn row_to_tts_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbTtsCleaningRule> {
    Ok(DbTtsCleaningRule {
        id: row.get(0)?,
        rule_type: row.get(1)?,
        pattern: row.get(2)?,
        replacement: row.get(3)?,
        language: row.get(4)?,
        is_active: row.get(5)?,
        priority: row.get(6)?,
    })
}

/// Mapowanie wiersza na DbFlowExecution
fn row_to_flow_execution(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbFlowExecution> {
    Ok(DbFlowExecution {
        id: row.get(0)?,
        flow_id: row.get(1)?,
        request_id: row.get(2)?,
        model: row.get(3)?,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
        status: row.get(6)?,
        execution_log: row.get(7)?,
        total_latency_ms: row.get(8)?,
        total_tokens: row.get(9)?,
    })
}

// --- Services ---

const SERVICE_COLS: &str = "id, name, service_type, strategy, model_category, status, config_json, created_at, updated_at, service_uuid, node_id";
const BACKEND_COLS: &str = "id, service_id, connection_type, config_json, max_concurrent, timeout_ms, weight, model_name_override, health_check_path, is_active";

pub fn list_services(pool: &DbPool) -> Result<Vec<DbService>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM services ORDER BY name", SERVICE_COLS
    ))?;
    let services = stmt
        .query_map([], row_to_service)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(services)
}

/// Pobiera wszystkie serwisy z backendami jednym zapytaniem JOIN (eliminacja N+1).
pub fn list_services_with_backends(pool: &DbPool) -> Result<Vec<(DbService, Vec<DbServiceBackend>)>> {
    let conn = acquire(pool)?;

    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.service_type, s.strategy, s.model_category, s.status, s.config_json, s.created_at, s.updated_at, s.service_uuid, s.node_id, \
         b.id, b.service_id, b.connection_type, b.config_json, b.max_concurrent, b.timeout_ms, b.weight, b.model_name_override, b.health_check_path, b.is_active \
         FROM services s LEFT JOIN service_backends b ON s.id = b.service_id ORDER BY s.name, b.id",
    )?;

    let mut services: Vec<(DbService, Vec<DbServiceBackend>)> = Vec::new();
    let mut last_service_id: Option<i64> = None;

    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let svc_id: i64 = row.get(0)?;

        if last_service_id != Some(svc_id) {
            let service = DbService {
                id: row.get(0)?,
                name: row.get(1)?,
                service_type: row.get(2)?,
                strategy: row.get(3)?,
                model_category: row.get(4)?,
                status: row.get(5)?,
                config_json: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                service_uuid: row.get(9)?,
                node_id: row.get(10)?,
            };
            services.push((service, Vec::new()));
            last_service_id = Some(svc_id);
        }

        let backend_id: Option<i64> = row.get(11)?;
        if backend_id.is_some() {
            let backend = DbServiceBackend {
                id: row.get(11)?,
                service_id: row.get(12)?,
                connection_type: row.get(13)?,
                config_json: row.get(14)?,
                max_concurrent: row.get(15)?,
                timeout_ms: row.get(16)?,
                weight: row.get(17)?,
                model_name_override: row.get(18)?,
                health_check_path: row.get(19)?,
                is_active: row.get(20)?,
            };
            if let Some(last) = services.last_mut() {
                last.1.push(backend);
            }
        }
    }

    Ok(services)
}

pub fn get_service(pool: &DbPool, id: i64) -> Result<Option<DbService>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM services WHERE id = ?1", SERVICE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_service)
        .optional()?;
    Ok(result)
}

pub fn create_service(
    pool: &DbPool,
    name: &str,
    service_type: &str,
    strategy: &str,
    model_category: Option<&str>,
    config_json: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO services (name, service_type, strategy, model_category, config_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![name, service_type, strategy, model_category, config_json],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_service(
    pool: &DbPool,
    id: i64,
    name: &str,
    strategy: &str,
    model_category: Option<&str>,
    status: &str,
    config_json: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE services SET name = ?2, strategy = ?3, model_category = ?4, status = ?5, config_json = ?6, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, name, strategy, model_category, status, config_json],
    )?;
    Ok(())
}

pub fn delete_service(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM services WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// Kaskadowe usuwanie serwisu po nazwie: backendy, modele, serwis
pub fn delete_service_cascade_by_name(pool: &DbPool, name: &str) -> Result<u32> {
    let conn = acquire(pool)?;
    let service_id: Option<i64> = conn
        .query_row("SELECT id FROM services WHERE name = ?1", rusqlite::params![name], |r| r.get(0))
        .optional()?;

    let Some(service_id) = service_id else {
        return Ok(0);
    };

    let mut deleted = 0u32;
    deleted += conn.execute("DELETE FROM service_backends WHERE service_id = ?1", rusqlite::params![service_id])? as u32;
    deleted += conn.execute("DELETE FROM model_registry WHERE service_id = ?1", rusqlite::params![service_id])? as u32;
    deleted += conn.execute("DELETE FROM services WHERE id = ?1", rusqlite::params![service_id])? as u32;
    Ok(deleted)
}

// --- Service Backends ---

pub fn list_backends_for_service(
    pool: &DbPool,
    service_id: i64,
) -> Result<Vec<DbServiceBackend>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM service_backends WHERE service_id = ?1", BACKEND_COLS
    ))?;
    let backends = stmt
        .query_map(rusqlite::params![service_id], row_to_backend)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(backends)
}

pub fn create_backend(pool: &DbPool, backend: &NewBackend<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO service_backends (service_id, connection_type, config_json, max_concurrent, timeout_ms, weight, model_name_override, health_check_path) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            backend.service_id,
            backend.connection_type,
            backend.config_json,
            backend.max_concurrent,
            backend.timeout_ms,
            backend.weight,
            backend.model_name_override,
            backend.health_check_path,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_backend(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM service_backends WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Usuwa wszystkie backendy nalezace do danego serwisu
pub fn delete_backends_by_service(pool: &DbPool, service_id: i64) -> Result<usize> {
    let conn = acquire(pool)?;
    let deleted = conn.execute(
        "DELETE FROM service_backends WHERE service_id = ?1",
        rusqlite::params![service_id],
    )?;
    Ok(deleted)
}

pub fn get_backend(pool: &DbPool, id: i64) -> Result<Option<DbServiceBackend>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM service_backends WHERE id = ?1", BACKEND_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_backend)
        .optional()?;
    Ok(result)
}

pub fn update_backend(
    pool: &DbPool,
    id: i64,
    connection_type: &str,
    config_json: &str,
    max_concurrent: i64,
    timeout_ms: i64,
    weight: i64,
    model_name_override: Option<&str>,
    health_check_path: Option<&str>,
    is_active: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE service_backends SET connection_type = ?1, config_json = ?2, max_concurrent = ?3, timeout_ms = ?4, weight = ?5, model_name_override = ?6, health_check_path = ?7, is_active = ?8 WHERE id = ?9",
        rusqlite::params![connection_type, config_json, max_concurrent, timeout_ms, weight, model_name_override, health_check_path, is_active, id],
    )?;
    Ok(())
}

// --- API Keys ---

const API_KEY_COLS: &str = "id, key_hash, key_prefix, name, rate_limit_rps, is_active, created_at, last_used_at";

fn row_to_api_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbApiKey> {
    Ok(DbApiKey {
        id: row.get(0)?,
        key_hash: row.get(1)?,
        key_prefix: row.get(2)?,
        name: row.get(3)?,
        rate_limit_rps: row.get(4)?,
        is_active: row.get(5)?,
        created_at: row.get(6)?,
        last_used_at: row.get(7)?,
    })
}

pub fn list_api_keys(pool: &DbPool) -> Result<Vec<DbApiKey>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, key_prefix, name, rate_limit_rps, is_active, created_at, last_used_at FROM api_keys ORDER BY name",
    )?;
    let keys = stmt
        .query_map([], |row| {
            Ok(DbApiKey {
                id: row.get(0)?,
                key_hash: String::new(),
                key_prefix: row.get(1)?,
                name: row.get(2)?,
                rate_limit_rps: row.get(3)?,
                is_active: row.get(4)?,
                created_at: row.get(5)?,
                last_used_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(keys)
}

pub fn create_api_key(
    pool: &DbPool,
    key_hash: &str,
    key_prefix: &str,
    name: &str,
    rate_limit_rps: i64,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO api_keys (key_hash, key_prefix, name, rate_limit_rps) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![key_hash, key_prefix, name, rate_limit_rps],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_api_key(pool: &DbPool, id: i64) -> Result<usize> {
    let conn = acquire(pool)?;
    let affected = conn.execute(
        "DELETE FROM api_keys WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(affected)
}

pub fn verify_api_key(pool: &DbPool, key_hash: &str) -> Result<Option<DbApiKey>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM api_keys WHERE key_hash = ?1 AND is_active = 1", API_KEY_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![key_hash], row_to_api_key)
        .optional()?;
    Ok(result)
}

// --- Service Aliases ---

pub fn list_aliases(pool: &DbPool) -> Result<Vec<DbServiceAlias>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, alias, target_service_id FROM service_aliases ORDER BY alias",
    )?;
    let aliases = stmt
        .query_map([], |row| {
            Ok(DbServiceAlias {
                id: row.get(0)?,
                alias: row.get(1)?,
                target_service_id: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(aliases)
}

pub fn create_alias(pool: &DbPool, alias: &str, target_service_id: i64) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO service_aliases (alias, target_service_id) VALUES (?1, ?2)",
        rusqlite::params![alias, target_service_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_alias(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM service_aliases WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// --- Settings ---

pub fn get_setting(pool: &DbPool, key: &str) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(result)
}

pub fn set_setting(pool: &DbPool, key: &str, value: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Usuwa ustawienie po kluczu (CR-016: jednorazowe tokeny SSO state)
pub fn delete_setting(pool: &DbPool, key: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM settings WHERE key = ?1", rusqlite::params![key])?;
    Ok(())
}

pub fn list_settings(pool: &DbPool) -> Result<Vec<DbSetting>> {
    let conn = acquire(pool)?;
    let mut stmt =
        conn.prepare("SELECT key, value, updated_at FROM settings ORDER BY key")?;
    let settings = stmt
        .query_map([], |row| {
            Ok(DbSetting {
                key: row.get(0)?,
                value: row.get(1)?,
                updated_at: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(settings)
}

// --- Users ---

pub fn get_user_by_username(pool: &DbPool, username: &str) -> Result<Option<DbUser>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, username, password_hash, role, created_at, last_login_at, must_change_password FROM users WHERE username = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![username], |row| {
            Ok(DbUser {
                id: row.get(0)?,
                username: row.get(1)?,
                password_hash: row.get(2)?,
                role: row.get(3)?,
                created_at: row.get(4)?,
                last_login_at: row.get(5)?,
                must_change_password: row.get(6)?,
            })
        })
        .optional()?;
    Ok(result)
}

pub fn create_user(
    pool: &DbPool,
    username: &str,
    password_hash: &str,
    role: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO users (username, password_hash, role) VALUES (?1, ?2, ?3)",
        rusqlite::params![username, password_hash, role],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_user_last_login(pool: &DbPool, user_id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE users SET last_login_at = datetime('now') WHERE id = ?1",
        rusqlite::params![user_id],
    )?;
    Ok(())
}

pub fn update_user_password(pool: &DbPool, user_id: i64, password_hash: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE users SET password_hash = ?1 WHERE id = ?2",
        rusqlite::params![password_hash, user_id],
    )?;
    Ok(())
}

pub fn clear_must_change_password(pool: &DbPool, user_id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE users SET must_change_password = 0 WHERE id = ?1",
        rusqlite::params![user_id],
    )?;
    // VULN-003: Wyczysc tez w tabeli user_accounts
    let _ = conn.execute(
        "UPDATE user_accounts SET must_change_password = 0 WHERE id = ?1",
        rusqlite::params![user_id],
    );
    Ok(())
}

// --- Prompts ---

const PROMPT_COLS: &str = "id, prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority, is_active, version, created_at, updated_at";

pub fn list_prompts(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbPrompt>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM prompts ORDER BY name LIMIT ?1 OFFSET ?2", PROMPT_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_prompt)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_prompt(pool: &DbPool, id: i64) -> Result<Option<DbPrompt>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM prompts WHERE id = ?1", PROMPT_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_prompt)
        .optional()?;
    Ok(result)
}

pub fn get_prompt_by_prompt_id(pool: &DbPool, prompt_id: &str) -> Result<Option<DbPrompt>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM prompts WHERE prompt_id = ?1", PROMPT_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![prompt_id], row_to_prompt)
        .optional()?;
    Ok(result)
}

pub fn create_prompt(pool: &DbPool, params: &NewPrompt<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO prompts (prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![params.prompt_id, params.name, params.description, params.content, params.prompt_type, params.default_model, params.variables, params.cache_priority],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_prompt(pool: &DbPool, params: &UpdatePrompt<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE prompts SET name = ?2, description = ?3, content = ?4, prompt_type = ?5, default_model = ?6, variables = ?7, cache_priority = ?8, is_active = ?9, version = version + 1, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![params.id, params.name, params.description, params.content, params.prompt_type, params.default_model, params.variables, params.cache_priority, params.is_active],
    )?;
    Ok(())
}

pub fn delete_prompt(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM prompts WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Model Registry ---

const MODEL_ENTRY_COLS: &str = "id, model_name, display_name, service_type, connection_type, service_id, flow_id, is_public, is_active, config_json, created_at";

pub fn list_model_entries(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbModelEntry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM model_registry ORDER BY model_name LIMIT ?1 OFFSET ?2", MODEL_ENTRY_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_model_entry)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_model_entry(pool: &DbPool, id: i64) -> Result<Option<DbModelEntry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM model_registry WHERE id = ?1", MODEL_ENTRY_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_model_entry)
        .optional()?;
    Ok(result)
}

pub fn get_model_by_name(pool: &DbPool, model_name: &str) -> Result<Option<DbModelEntry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM model_registry WHERE model_name = ?1", MODEL_ENTRY_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![model_name], row_to_model_entry)
        .optional()?;
    Ok(result)
}

pub fn create_model_entry(pool: &DbPool, params: &NewModelEntry<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO model_registry (model_name, display_name, service_type, connection_type, service_id, flow_id, is_public, config_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![params.model_name, params.display_name, params.service_type, params.connection_type, params.service_id, params.flow_id, params.is_public, params.config_json],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_model_entry(pool: &DbPool, params: &UpdateModelEntry<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE model_registry SET display_name = ?2, service_type = ?3, connection_type = ?4, service_id = ?5, flow_id = ?6, is_public = ?7, is_active = ?8, config_json = ?9 WHERE id = ?1",
        rusqlite::params![params.id, params.display_name, params.service_type, params.connection_type, params.service_id, params.flow_id, params.is_public, params.is_active, params.config_json],
    )?;
    Ok(())
}

pub fn delete_model_entry(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM model_registry WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Model Aliases ---

const MODEL_ALIAS_COLS: &str = "id, alias, target_model, is_active, fallback_targets, strategy";

fn row_to_model_alias(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbModelAlias> {
    Ok(DbModelAlias {
        id: row.get(0)?,
        alias: row.get(1)?,
        target_model: row.get(2)?,
        is_active: row.get(3)?,
        fallback_targets: row.get(4)?,
        strategy: row.get(5)?,
    })
}

pub fn list_model_aliases(pool: &DbPool) -> Result<Vec<DbModelAlias>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM model_aliases ORDER BY alias", MODEL_ALIAS_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_model_alias)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_model_alias(pool: &DbPool, id: i64) -> Result<Option<DbModelAlias>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM model_aliases WHERE id = ?1", MODEL_ALIAS_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_model_alias)
        .optional()?;
    Ok(result)
}

pub fn resolve_model_alias(pool: &DbPool, alias: &str) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT target_model FROM model_aliases WHERE alias = ?1 AND is_active = 1",
            rusqlite::params![alias],
            |row| row.get(0),
        )
        .optional()?;
    Ok(result)
}

pub fn create_model_alias(
    pool: &DbPool,
    alias: &str,
    target_model: &str,
    fallback_targets: Option<&str>,
    strategy: Option<&str>,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model, fallback_targets, strategy) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![alias, target_model, fallback_targets, strategy.unwrap_or("first_available")],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_model_alias(
    pool: &DbPool,
    id: i64,
    alias: &str,
    target_model: &str,
    is_active: bool,
    fallback_targets: Option<&str>,
    strategy: Option<&str>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE model_aliases SET alias = ?2, target_model = ?3, is_active = ?4, fallback_targets = ?5, strategy = ?6 WHERE id = ?1",
        rusqlite::params![id, alias, target_model, is_active, fallback_targets, strategy.unwrap_or("first_available")],
    )?;
    Ok(())
}

pub fn delete_model_alias(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM model_aliases WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Clusters ---

const CLUSTER_COLS: &str = "id, cluster_id, name, description, strategy, created_at, updated_at, total_vram_mb, total_ram_mb, total_cpu_cores, bottleneck_speed_mbps, interconnect_type";

fn row_to_cluster(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbCluster> {
    Ok(DbCluster {
        id: row.get(0)?,
        cluster_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        strategy: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        total_vram_mb: row.get(7)?,
        total_ram_mb: row.get(8)?,
        total_cpu_cores: row.get(9)?,
        bottleneck_speed_mbps: row.get(10)?,
        interconnect_type: row.get(11)?,
    })
}

const CLUSTER_MEMBER_COLS: &str = "id, cluster_id, node_id, role, joined_at, interface_name, interface_ip, interface_speed_mbps, interface_type";

fn row_to_cluster_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbClusterMember> {
    Ok(DbClusterMember {
        id: row.get(0)?,
        cluster_id: row.get(1)?,
        node_id: row.get(2)?,
        role: row.get(3)?,
        joined_at: row.get(4)?,
        interface_name: row.get(5)?,
        interface_ip: row.get(6)?,
        interface_speed_mbps: row.get(7)?,
        interface_type: row.get(8)?,
    })
}

pub fn create_cluster(
    pool: &DbPool,
    cluster_id: &str,
    name: &str,
    description: &str,
    strategy: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO clusters (cluster_id, name, description, strategy, total_vram_mb, total_ram_mb, total_cpu_cores, bottleneck_speed_mbps, interconnect_type) VALUES (?1, ?2, ?3, ?4, 0, 0, 0, 0, '')",
        rusqlite::params![cluster_id, name, description, strategy],
    )?;
    Ok(())
}

pub fn list_clusters(pool: &DbPool) -> Result<Vec<DbCluster>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM clusters ORDER BY name", CLUSTER_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_cluster)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_cluster(pool: &DbPool, cluster_id: &str) -> Result<Option<DbCluster>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM clusters WHERE cluster_id = ?1", CLUSTER_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![cluster_id], row_to_cluster)
        .optional()?;
    Ok(result)
}

pub fn update_cluster(
    pool: &DbPool,
    cluster_id: &str,
    name: &str,
    description: &str,
    strategy: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE clusters SET name = ?2, description = ?3, strategy = ?4, updated_at = datetime('now') WHERE cluster_id = ?1",
        rusqlite::params![cluster_id, name, description, strategy],
    )?;
    Ok(())
}

pub fn delete_cluster(pool: &DbPool, cluster_id: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM clusters WHERE cluster_id = ?1", rusqlite::params![cluster_id])?;
    Ok(())
}

/// Aktualizuje zagregowane zasoby klastra (VRAM, RAM, CPU, przepustowosc)
pub fn update_cluster_aggregates(
    pool: &DbPool,
    cluster_id: &str,
    total_vram_mb: i64,
    total_ram_mb: i64,
    total_cpu_cores: i64,
    bottleneck_speed_mbps: i64,
    interconnect_type: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE clusters SET total_vram_mb = ?2, total_ram_mb = ?3, total_cpu_cores = ?4, bottleneck_speed_mbps = ?5, interconnect_type = ?6, updated_at = datetime('now') WHERE cluster_id = ?1",
        rusqlite::params![cluster_id, total_vram_mb, total_ram_mb, total_cpu_cores, bottleneck_speed_mbps, interconnect_type],
    )?;
    Ok(())
}

pub fn add_cluster_member(
    pool: &DbPool,
    cluster_id: &str,
    node_id: &str,
    role: &str,
    interface_name: &str,
    interface_ip: &str,
    interface_speed_mbps: i64,
    interface_type: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR REPLACE INTO cluster_members (cluster_id, node_id, role, interface_name, interface_ip, interface_speed_mbps, interface_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![cluster_id, node_id, role, interface_name, interface_ip, interface_speed_mbps, interface_type],
    )?;
    Ok(())
}

pub fn remove_cluster_member(pool: &DbPool, cluster_id: &str, node_id: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM cluster_members WHERE cluster_id = ?1 AND node_id = ?2",
        rusqlite::params![cluster_id, node_id],
    )?;
    Ok(())
}

pub fn list_cluster_members(pool: &DbPool, cluster_id: &str) -> Result<Vec<DbClusterMember>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM cluster_members WHERE cluster_id = ?1 ORDER BY joined_at", CLUSTER_MEMBER_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![cluster_id], row_to_cluster_member)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// --- Flows ---

const FLOW_COLS: &str = "id, name, description, version, is_default, service_type, flow_json, status, created_at, updated_at";

pub fn list_flows(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbFlow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flows ORDER BY name LIMIT ?1 OFFSET ?2", FLOW_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_flow)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow(pool: &DbPool, id: i64) -> Result<Option<DbFlow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flows WHERE id = ?1", FLOW_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_flow)
        .optional()?;
    Ok(result)
}

pub fn get_default_flow_for_service_type(pool: &DbPool, service_type: &str) -> Result<Option<DbFlow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flows WHERE is_default = 1 AND service_type = ?1 AND status = 'active' LIMIT 1", FLOW_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![service_type], row_to_flow)
        .optional()?;
    Ok(result)
}

pub fn get_flow_for_model(pool: &DbPool, model_name: &str) -> Result<Option<DbFlow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT f.id, f.name, f.description, f.version, f.is_default, f.service_type, f.flow_json, f.status, f.created_at, f.updated_at \
         FROM flows f INNER JOIN flow_model_bindings b ON f.id = b.flow_id \
         WHERE ?1 LIKE REPLACE(b.model_pattern, '*', '%') AND f.status = 'active' ORDER BY b.priority DESC LIMIT 1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![model_name], row_to_flow)
        .optional()?;
    Ok(result)
}

pub fn create_flow(pool: &DbPool, params: &FlowParams<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO flows (name, description, is_default, service_type, flow_json, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![params.name, params.description, params.is_default, params.service_type, params.flow_json, params.status],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_flow(pool: &DbPool, id: i64, expected_version: i64, params: &FlowParams<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    let rows_affected = conn.execute(
        "UPDATE flows SET name = ?2, description = ?3, is_default = ?4, service_type = ?5, flow_json = ?6, status = ?7, version = version + 1, updated_at = datetime('now') WHERE id = ?1 AND version = ?8",
        rusqlite::params![id, params.name, params.description, params.is_default, params.service_type, params.flow_json, params.status, expected_version],
    )?;
    if rows_affected == 0 {
        return Err(anyhow::anyhow!("CONFLICT"));
    }
    Ok(())
}

pub fn delete_flow(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM flows WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Flow Model Bindings ---

const FLOW_BINDING_COLS: &str = "id, flow_id, model_pattern, priority";

fn row_to_flow_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbFlowModelBinding> {
    Ok(DbFlowModelBinding {
        id: row.get(0)?,
        flow_id: row.get(1)?,
        model_pattern: row.get(2)?,
        priority: row.get(3)?,
    })
}

pub fn list_flow_model_bindings(pool: &DbPool) -> Result<Vec<DbFlowModelBinding>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flow_model_bindings ORDER BY priority DESC", FLOW_BINDING_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_flow_binding)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow_model_binding(pool: &DbPool, id: i64) -> Result<Option<DbFlowModelBinding>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flow_model_bindings WHERE id = ?1", FLOW_BINDING_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_flow_binding)
        .optional()?;
    Ok(result)
}

pub fn create_flow_model_binding(
    pool: &DbPool,
    flow_id: i64,
    model_pattern: &str,
    priority: i64,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO flow_model_bindings (flow_id, model_pattern, priority) VALUES (?1, ?2, ?3)",
        rusqlite::params![flow_id, model_pattern, priority],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_flow_model_binding(
    pool: &DbPool,
    id: i64,
    flow_id: i64,
    model_pattern: &str,
    priority: i64,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE flow_model_bindings SET flow_id = ?2, model_pattern = ?3, priority = ?4 WHERE id = ?1",
        rusqlite::params![id, flow_id, model_pattern, priority],
    )?;
    Ok(())
}

pub fn delete_flow_model_binding(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM flow_model_bindings WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Flow Node Templates ---

const NODE_TEMPLATE_COLS: &str = "id, node_type, category, label, description, default_config, icon";

fn row_to_node_template(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbFlowNodeTemplate> {
    Ok(DbFlowNodeTemplate {
        id: row.get(0)?,
        node_type: row.get(1)?,
        category: row.get(2)?,
        label: row.get(3)?,
        description: row.get(4)?,
        default_config: row.get(5)?,
        icon: row.get(6)?,
    })
}

pub fn list_flow_node_templates(pool: &DbPool) -> Result<Vec<DbFlowNodeTemplate>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flow_node_templates ORDER BY category, label", NODE_TEMPLATE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_node_template)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow_node_template(pool: &DbPool, id: i64) -> Result<Option<DbFlowNodeTemplate>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flow_node_templates WHERE id = ?1", NODE_TEMPLATE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_node_template)
        .optional()?;
    Ok(result)
}

pub fn create_flow_node_template(pool: &DbPool, params: &FlowNodeTemplateParams<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO flow_node_templates (node_type, category, label, description, default_config, icon) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![params.node_type, params.category, params.label, params.description, params.default_config, params.icon],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_flow_node_template(pool: &DbPool, id: i64, params: &FlowNodeTemplateParams<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE flow_node_templates SET node_type = ?2, category = ?3, label = ?4, description = ?5, default_config = ?6, icon = ?7 WHERE id = ?1",
        rusqlite::params![id, params.node_type, params.category, params.label, params.description, params.default_config, params.icon],
    )?;
    Ok(())
}

pub fn delete_flow_node_template(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM flow_node_templates WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- PII Rules ---

const PII_RULE_COLS: &str = "id, name, category, pattern, replacement, is_active, priority, description, test_examples, created_at";

pub fn list_pii_rules(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbPiiRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM pii_rules ORDER BY priority DESC LIMIT ?1 OFFSET ?2", PII_RULE_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_pii_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_pii_rules_active(pool: &DbPool) -> Result<Vec<DbPiiRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM pii_rules WHERE is_active = 1 ORDER BY priority DESC", PII_RULE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_pii_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_pii_rule(pool: &DbPool, id: i64) -> Result<Option<DbPiiRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM pii_rules WHERE id = ?1", PII_RULE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_pii_rule)
        .optional()?;
    Ok(result)
}

pub fn create_pii_rule(pool: &DbPool, params: &NewPiiRule<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO pii_rules (name, category, pattern, replacement, priority, description, test_examples) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![params.name, params.category, params.pattern, params.replacement, params.priority, params.description, params.test_examples],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_pii_rule(pool: &DbPool, params: &UpdatePiiRule<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE pii_rules SET name = ?2, category = ?3, pattern = ?4, replacement = ?5, is_active = ?6, priority = ?7, description = ?8, test_examples = ?9 WHERE id = ?1",
        rusqlite::params![params.id, params.name, params.category, params.pattern, params.replacement, params.is_active, params.priority, params.description, params.test_examples],
    )?;
    Ok(())
}

pub fn delete_pii_rule(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM pii_rules WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Fast Path Patterns ---

const FAST_PATH_COLS: &str = "id, module, pattern_type, pattern, match_type, result_json, is_active, priority";

pub fn list_fast_path_patterns(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbFastPathPattern>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM fast_path_patterns ORDER BY module, priority DESC LIMIT ?1 OFFSET ?2", FAST_PATH_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_fast_path_pattern)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_fast_path_patterns_by_module(pool: &DbPool, module: &str) -> Result<Vec<DbFastPathPattern>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM fast_path_patterns WHERE module = ?1 AND is_active = 1 ORDER BY priority DESC", FAST_PATH_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![module], row_to_fast_path_pattern)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_fast_path_pattern(pool: &DbPool, id: i64) -> Result<Option<DbFastPathPattern>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM fast_path_patterns WHERE id = ?1", FAST_PATH_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_fast_path_pattern)
        .optional()?;
    Ok(result)
}

pub fn create_fast_path_pattern(
    pool: &DbPool,
    module: &str,
    pattern_type: &str,
    pattern: &str,
    match_type: &str,
    result_json: &str,
    priority: i64,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO fast_path_patterns (module, pattern_type, pattern, match_type, result_json, priority) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![module, pattern_type, pattern, match_type, result_json, priority],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_fast_path_pattern(pool: &DbPool, params: &UpdateFastPathPattern<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE fast_path_patterns SET module = ?2, pattern_type = ?3, pattern = ?4, match_type = ?5, result_json = ?6, is_active = ?7, priority = ?8 WHERE id = ?1",
        rusqlite::params![params.id, params.module, params.pattern_type, params.pattern, params.match_type, params.result_json, params.is_active, params.priority],
    )?;
    Ok(())
}

pub fn delete_fast_path_pattern(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM fast_path_patterns WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- TTS Cleaning Rules ---

const TTS_RULE_COLS: &str = "id, rule_type, pattern, replacement, language, is_active, priority";

pub fn list_tts_cleaning_rules(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbTtsCleaningRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM tts_cleaning_rules ORDER BY priority LIMIT ?1 OFFSET ?2", TTS_RULE_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_tts_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_tts_cleaning_rules_active(pool: &DbPool) -> Result<Vec<DbTtsCleaningRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM tts_cleaning_rules WHERE is_active = 1 ORDER BY priority", TTS_RULE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_tts_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_tts_cleaning_rule(pool: &DbPool, id: i64) -> Result<Option<DbTtsCleaningRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM tts_cleaning_rules WHERE id = ?1", TTS_RULE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_tts_rule)
        .optional()?;
    Ok(result)
}

pub fn create_tts_cleaning_rule(
    pool: &DbPool,
    rule_type: &str,
    pattern: &str,
    replacement: Option<&str>,
    language: &str,
    priority: i64,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO tts_cleaning_rules (rule_type, pattern, replacement, language, priority) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![rule_type, pattern, replacement, language, priority],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_tts_cleaning_rule(pool: &DbPool, params: &UpdateTtsCleaningRule<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE tts_cleaning_rules SET rule_type = ?2, pattern = ?3, replacement = ?4, language = ?5, is_active = ?6, priority = ?7 WHERE id = ?1",
        rusqlite::params![params.id, params.rule_type, params.pattern, params.replacement, params.language, params.is_active, params.priority],
    )?;
    Ok(())
}

pub fn delete_tts_cleaning_rule(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM tts_cleaning_rules WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Flow Executions ---

const FLOW_EXEC_COLS: &str = "id, flow_id, request_id, model, started_at, finished_at, status, execution_log, total_latency_ms, total_tokens";

pub fn list_flow_executions(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbFlowExecution>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flow_executions ORDER BY id DESC LIMIT ?1 OFFSET ?2", FLOW_EXEC_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_flow_execution)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_flow_executions_for_flow(pool: &DbPool, flow_id: i64, limit: i64) -> Result<Vec<DbFlowExecution>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flow_executions WHERE flow_id = ?1 ORDER BY id DESC LIMIT ?2", FLOW_EXEC_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![flow_id, limit], row_to_flow_execution)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow_execution(pool: &DbPool, id: i64) -> Result<Option<DbFlowExecution>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM flow_executions WHERE id = ?1", FLOW_EXEC_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_flow_execution)
        .optional()?;
    Ok(result)
}

pub fn create_flow_execution(
    pool: &DbPool,
    flow_id: i64,
    request_id: Option<&str>,
    model: Option<&str>,
    status: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO flow_executions (flow_id, request_id, model, started_at, status) VALUES (?1, ?2, ?3, datetime('now'), ?4)",
        rusqlite::params![flow_id, request_id, model, status],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_flow_execution(
    pool: &DbPool,
    id: i64,
    status: &str,
    execution_log: Option<&str>,
    total_latency_ms: Option<i64>,
    total_tokens: Option<i64>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE flow_executions SET finished_at = datetime('now'), status = ?2, execution_log = ?3, total_latency_ms = ?4, total_tokens = ?5 WHERE id = ?1",
        rusqlite::params![id, status, execution_log, total_latency_ms, total_tokens],
    )?;
    Ok(())
}

pub fn delete_flow_execution(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM flow_executions WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Portainer Instances ---

const PORTAINER_INSTANCE_COLS: &str = "id, name, url, api_key, created_at, updated_at, username, password";

fn row_to_portainer_instance(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbPortainerInstance> {
    Ok(DbPortainerInstance {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        api_key: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        username: row.get(6)?,
        password: row.get(7)?,
    })
}

pub fn list_portainer_instances(pool: &DbPool) -> Result<Vec<DbPortainerInstance>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM portainer_instances ORDER BY name", PORTAINER_INSTANCE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_portainer_instance)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_portainer_instance(pool: &DbPool, id: i64) -> Result<Option<DbPortainerInstance>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM portainer_instances WHERE id = ?1", PORTAINER_INSTANCE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_portainer_instance)
        .optional()?;
    Ok(result)
}

pub fn create_portainer_instance(pool: &DbPool, name: &str, url: &str, api_key: &str, username: &str, password: &str) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO portainer_instances (name, url, api_key, username, password) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![name, url, api_key, username, password],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_portainer_instance(pool: &DbPool, id: i64, name: &str, url: &str, api_key: &str, username: &str, password: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE portainer_instances SET name = ?2, url = ?3, api_key = ?4, username = ?5, password = ?6, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, name, url, api_key, username, password],
    )?;
    Ok(())
}

pub fn delete_portainer_instance(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute("DELETE FROM portainer_instances WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

// --- Docker Registries ---

const DOCKER_REGISTRY_COLS: &str = "id, name, registry_type, url, username, password_encrypted, is_active, skip_tls_verify, created_at, updated_at";

fn row_to_docker_registry(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbDockerRegistry> {
    Ok(DbDockerRegistry {
        id: row.get(0)?,
        name: row.get(1)?,
        registry_type: row.get(2)?,
        url: row.get(3)?,
        username: row.get(4)?,
        password_encrypted: row.get(5)?,
        is_active: row.get(6)?,
        skip_tls_verify: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub fn list_registries(pool: &DbPool) -> Result<Vec<DbDockerRegistry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM registries ORDER BY name", DOCKER_REGISTRY_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_docker_registry)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_registry(pool: &DbPool, id: i64) -> Result<Option<DbDockerRegistry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM registries WHERE id = ?1", DOCKER_REGISTRY_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_docker_registry)
        .optional()?;
    Ok(result)
}

pub fn create_registry(
    pool: &DbPool,
    name: &str,
    registry_type: &str,
    url: &str,
    username: &str,
    password_encrypted: &str,
    skip_tls_verify: bool,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO registries (name, registry_type, url, username, password_encrypted, skip_tls_verify) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![name, registry_type, url, username, password_encrypted, skip_tls_verify],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_registry(
    pool: &DbPool,
    id: i64,
    name: &str,
    registry_type: &str,
    url: &str,
    username: &str,
    password_encrypted: &str,
    skip_tls_verify: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE registries SET name = ?2, registry_type = ?3, url = ?4, username = ?5, password_encrypted = ?6, skip_tls_verify = ?7, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, name, registry_type, url, username, password_encrypted, skip_tls_verify],
    )?;
    Ok(())
}

pub fn delete_registry(pool: &DbPool, id: i64) -> Result<usize> {
    let conn = acquire(pool)?;
    let affected = conn.execute(
        "DELETE FROM registries WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(affected)
}

// =============================================================================
// User Accounts (tabela user_accounts — migracja 14)
// =============================================================================

/// Mapowanie wiersza na UserAccount
fn row_to_user_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserAccount> {
    Ok(UserAccount {
        id: row.get(0)?,
        username: row.get(1)?,
        password_hash: row.get(2)?,
        display_name: row.get(3)?,
        email: row.get(4)?,
        is_active: row.get(5)?,
        is_admin: row.get(6)?,
        must_change_password: row.get(7)?,
        sso_provider: row.get(8)?,
        sso_subject: row.get(9)?,
        last_login_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

const USER_ACCOUNT_COLS: &str =
    "id, username, password_hash, display_name, email, is_active, is_admin, must_change_password, \
     sso_provider, sso_subject, last_login_at, created_at, updated_at";

/// Tworzy nowego uzytkownika w tabeli user_accounts.
/// Zwraca ID nowo utworzonego wiersza.
pub fn create_user_account(
    pool: &DbPool,
    username: &str,
    password_hash: &str,
    display_name: &str,
    email: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO user_accounts (username, password_hash, display_name, email) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![username, password_hash, display_name, email],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Pobiera uzytkownika po nazwie z tabeli user_accounts.
pub fn get_user_account_by_username(pool: &DbPool, username: &str) -> Result<Option<UserAccount>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM user_accounts WHERE username = ?1",
        USER_ACCOUNT_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![username], row_to_user_account)
        .optional()?;
    Ok(result)
}

/// Pobiera uzytkownika po ID z tabeli user_accounts.
pub fn get_user_account_by_id(pool: &DbPool, id: i64) -> Result<Option<UserAccount>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM user_accounts WHERE id = ?1",
        USER_ACCOUNT_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_user_account)
        .optional()?;
    Ok(result)
}

/// Lista wszystkich uzytkownikow z tabeli user_accounts.
pub fn list_user_accounts(pool: &DbPool) -> Result<Vec<UserAccount>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM user_accounts ORDER BY id",
        USER_ACCOUNT_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_user_account)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Aktualizuje hash hasla uzytkownika w tabeli user_accounts.
pub fn update_user_account_password(pool: &DbPool, id: i64, new_password_hash: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE user_accounts SET password_hash = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![new_password_hash, id],
    )?;
    Ok(())
}

/// Aktualizuje dane uzytkownika (display_name, email, is_active).
pub fn update_user_account(
    pool: &DbPool,
    id: i64,
    display_name: &str,
    email: &str,
    is_active: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE user_accounts SET display_name = ?1, email = ?2, is_active = ?3, \
         updated_at = datetime('now') WHERE id = ?4",
        rusqlite::params![display_name, email, is_active, id],
    )?;
    Ok(())
}

/// Usuwa uzytkownika z tabeli user_accounts (kaskadowo czlonkostwa w grupach).
pub fn delete_user_account(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM user_accounts WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Weryfikuje haslo uzytkownika. Zwraca UserAccount jesli login i haslo poprawne.
pub fn verify_user_account_password(
    pool: &DbPool,
    username: &str,
    password: &str,
) -> Result<Option<UserAccount>> {
    let user = get_user_account_by_username(pool, username)?;
    match user {
        Some(u) if !u.is_active => Ok(None),
        Some(u) => {
            if crate::crypto::verify_password(password, &u.password_hash) {
                // Aktualizuj last_login_at
                let conn = acquire(pool)?;
                let _ = conn.execute(
                    "UPDATE user_accounts SET last_login_at = datetime('now') WHERE id = ?1",
                    rusqlite::params![u.id],
                );
                Ok(Some(u))
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

// =============================================================================
// User Groups (tabela user_groups, group_members — migracja 14)
// =============================================================================

/// Tworzy nowa grupe uzytkownikow. Zwraca ID.
pub fn create_group(pool: &DbPool, name: &str, description: &str) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO user_groups (name, description) VALUES (?1, ?2)",
        rusqlite::params![name, description],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Lista wszystkich grup uzytkownikow.
pub fn list_groups(pool: &DbPool) -> Result<Vec<UserGroup>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, description, created_at FROM user_groups ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(UserGroup {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Dodaje uzytkownika do grupy.
pub fn add_user_to_group(pool: &DbPool, group_id: i64, user_id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?1, ?2)",
        rusqlite::params![group_id, user_id],
    )?;
    Ok(())
}

/// Usuwa uzytkownika z grupy.
pub fn remove_user_from_group(pool: &DbPool, group_id: i64, user_id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params![group_id, user_id],
    )?;
    Ok(())
}

/// Pobiera grupy do ktorych nalezy uzytkownik.
pub fn get_user_groups(pool: &DbPool, user_id: i64) -> Result<Vec<UserGroup>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, g.description, g.created_at \
         FROM user_groups g \
         JOIN group_members gm ON g.id = gm.group_id \
         WHERE gm.user_id = ?1 ORDER BY g.id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![user_id], |row| {
            Ok(UserGroup {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Usuwa grupe uzytkownikow (kaskadowo czlonkostwa).
pub fn delete_group(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM user_groups WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// =============================================================================
// Addon Permissions (tabela addon_permissions — migracja 14)
// =============================================================================

/// Ustawia (INSERT OR REPLACE) uprawnienie addonu (boolean: przyznane/nieprzyznane).
pub fn set_addon_permission(
    pool: &DbPool,
    addon_id: &str,
    subject_type: &str,
    subject_id: i64,
    permission_id: &str,
    granted: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_permissions (addon_id, subject_type, subject_id, permission_id, granted) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(addon_id, subject_type, subject_id, permission_id) \
         DO UPDATE SET granted = excluded.granted",
        rusqlite::params![addon_id, subject_type, subject_id, permission_id, granted as i32],
    )?;
    Ok(())
}

/// Pobiera wszystkie uprawnienia danego addonu.
pub fn get_addon_permissions(pool: &DbPool, addon_id: &str) -> Result<Vec<AddonPermission>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, addon_id, subject_type, subject_id, permission_id, granted, created_at \
         FROM addon_permissions WHERE addon_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok(AddonPermission {
                id: row.get(0)?,
                addon_id: row.get(1)?,
                subject_type: row.get(2)?,
                subject_id: row.get(3)?,
                permission_id: row.get(4)?,
                granted: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Sprawdza czy uzytkownik (bezposrednio lub przez grupe) ma przyznane uprawnienie addonu.
/// Uprawnienia sa boolean — granted=1 oznacza przyznane.
pub fn check_permission(
    pool: &DbPool,
    addon_id: &str,
    user_id: i64,
    permission_id: &str,
) -> Result<bool> {
    let conn = acquire(pool)?;

    // Sprawdz czy istnieje granted=1 dla uzytkownika lub jego grup
    let has_grant: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0
            FROM addon_permissions
            WHERE addon_id = ?1
              AND permission_id = ?2
              AND granted = 1
              AND (
                  (subject_type = 'user' AND subject_id = ?3)
                  OR (subject_type = 'group' AND subject_id IN (
                      SELECT group_id FROM group_members WHERE user_id = ?3
                  ))
              )
            LIMIT 1",
            rusqlite::params![addon_id, permission_id, user_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    Ok(has_grant)
}

/// Pobiera wszystkie uprawnienia (bezposrednie i przez grupy) dla danego uzytkownika.
pub fn get_user_permissions(pool: &DbPool, user_id: i64) -> Result<Vec<AddonPermission>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, addon_id, subject_type, subject_id, permission_id, granted, created_at \
         FROM addon_permissions \
         WHERE (subject_type = 'user' AND subject_id = ?1) \
            OR (subject_type = 'group' AND subject_id IN ( \
                SELECT group_id FROM group_members WHERE user_id = ?1 \
            )) \
         ORDER BY addon_id, permission_id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![user_id], |row| {
            Ok(AddonPermission {
                id: row.get(0)?,
                addon_id: row.get(1)?,
                subject_type: row.get(2)?,
                subject_id: row.get(3)?,
                permission_id: row.get(4)?,
                granted: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// =============================================================================
// Audit Log (tabela audit_log — migracja 14)
// =============================================================================

/// Zapisuje wpis logu audytowego.
pub fn log_audit(
    pool: &DbPool,
    user_id: Option<i64>,
    addon_id: Option<&str>,
    action: &str,
    resource: Option<&str>,
    details: Option<&str>,
    ip_address: Option<&str>,
    node_id: Option<&str>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO audit_log (user_id, addon_id, action, resource, details, ip_address, node_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![user_id, addon_id, action, resource, details, ip_address, node_id],
    )?;
    Ok(())
}

/// Lista wpisow logu audytowego z filtrami i paginacja.
pub fn list_audit_logs(
    pool: &DbPool,
    filters: &AuditLogFilters,
    offset: i64,
    limit: i64,
) -> Result<Vec<AuditLogEntry>> {
    let conn = acquire(pool)?;

    let mut sql = String::from(
        "SELECT id, timestamp, user_id, addon_id, action, resource, details, ip_address, node_id \
         FROM audit_log WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    if let Some(uid) = filters.user_id {
        sql.push_str(&format!(" AND user_id = ?{}", idx));
        params.push(Box::new(uid));
        idx += 1;
    }
    if let Some(ref aid) = filters.addon_id {
        sql.push_str(&format!(" AND addon_id = ?{}", idx));
        params.push(Box::new(aid.clone()));
        idx += 1;
    }
    if let Some(ref act) = filters.action {
        sql.push_str(&format!(" AND action = ?{}", idx));
        params.push(Box::new(act.clone()));
        idx += 1;
    }
    if let Some(ref from) = filters.from_date {
        sql.push_str(&format!(" AND timestamp >= ?{}", idx));
        params.push(Box::new(from.clone()));
        idx += 1;
    }
    if let Some(ref to) = filters.to_date {
        sql.push_str(&format!(" AND timestamp <= ?{}", idx));
        params.push(Box::new(to.clone()));
        idx += 1;
    }

    sql.push_str(&format!(
        " ORDER BY id DESC LIMIT ?{} OFFSET ?{}",
        idx,
        idx + 1
    ));
    params.push(Box::new(limit));
    params.push(Box::new(offset));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(AuditLogEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                user_id: row.get(2)?,
                addon_id: row.get(3)?,
                action: row.get(4)?,
                resource: row.get(5)?,
                details: row.get(6)?,
                ip_address: row.get(7)?,
                node_id: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Usuwa wpisy logu audytowego starsze niz podana liczba dni.
/// Zwraca liczbe usunietych wierszy.
pub fn cleanup_audit_logs(pool: &DbPool, days_to_keep: i64) -> Result<u64> {
    let conn = acquire(pool)?;
    let affected = conn.execute(
        "DELETE FROM audit_log WHERE timestamp < datetime('now', ?1)",
        rusqlite::params![format!("-{} days", days_to_keep)],
    )?;
    Ok(affected as u64)
}

// =============================================================================
// Addons (tabela addons — migracja 14)
// =============================================================================

/// Rejestruje nowy addon. Zwraca ID.
pub fn register_addon(
    pool: &DbPool,
    addon_id: &str,
    name: &str,
    version: &str,
    manifest_json: &str,
    platforms: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addons (addon_id, name, version, manifest_json, platforms) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![addon_id, name, version, manifest_json, platforms],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Lista wszystkich addonow.
pub fn list_addons(pool: &DbPool) -> Result<Vec<Addon>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, addon_id, name, version, description, author, platforms, \
         manifest_json, is_enabled, is_system, installed_at, updated_at \
         FROM addons ORDER BY name",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Addon {
                id: row.get(0)?,
                addon_id: row.get(1)?,
                name: row.get(2)?,
                version: row.get(3)?,
                description: row.get(4)?,
                author: row.get(5)?,
                platforms: row.get(6)?,
                manifest_json: row.get(7)?,
                is_enabled: row.get(8)?,
                is_system: row.get(9)?,
                installed_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pobiera addon po addon_id.
pub fn get_addon(pool: &DbPool, addon_id: &str) -> Result<Option<Addon>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, addon_id, name, version, description, author, platforms, \
         manifest_json, is_enabled, is_system, installed_at, updated_at \
         FROM addons WHERE addon_id = ?1",
    )?;
    let result = stmt
        .query_row(rusqlite::params![addon_id], |row| {
            Ok(Addon {
                id: row.get(0)?,
                addon_id: row.get(1)?,
                name: row.get(2)?,
                version: row.get(3)?,
                description: row.get(4)?,
                author: row.get(5)?,
                platforms: row.get(6)?,
                manifest_json: row.get(7)?,
                is_enabled: row.get(8)?,
                is_system: row.get(9)?,
                installed_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })
        .optional()?;
    Ok(result)
}

/// Aktualizuje wersje i manifest addonu.
pub fn update_addon(pool: &DbPool, addon_id: &str, version: &str, manifest_json: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE addons SET version = ?1, manifest_json = ?2, updated_at = datetime('now') \
         WHERE addon_id = ?3",
        rusqlite::params![version, manifest_json, addon_id],
    )?;
    Ok(())
}

/// Usuwa addon z rejestru.
pub fn delete_addon(pool: &DbPool, addon_id: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
    )?;
    Ok(())
}

// =============================================================================
// Addon Secrets (tabela addon_secrets — migracja 14)
// =============================================================================

/// Ustawia (INSERT OR REPLACE) zaszyfrowany sekret addonu.
pub fn set_addon_secret(
    pool: &DbPool,
    addon_id: &str,
    user_id: Option<i64>,
    key: &str,
    encrypted_value: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_secrets (addon_id, user_id, key, value_encrypted) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(addon_id, user_id, key) \
         DO UPDATE SET value_encrypted = excluded.value_encrypted, updated_at = datetime('now')",
        rusqlite::params![addon_id, user_id, key, encrypted_value],
    )?;
    Ok(())
}

/// Pobiera zaszyfrowana wartosc sekretu addonu.
pub fn get_addon_secret(
    pool: &DbPool,
    addon_id: &str,
    user_id: Option<i64>,
    key: &str,
) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let result: Option<String> = conn
        .query_row(
            "SELECT value_encrypted FROM addon_secrets \
             WHERE addon_id = ?1 AND user_id IS ?2 AND key = ?3",
            rusqlite::params![addon_id, user_id, key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(result)
}

/// Usuwa sekret addonu.
pub fn delete_addon_secret(
    pool: &DbPool,
    addon_id: &str,
    user_id: Option<i64>,
    key: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM addon_secrets WHERE addon_id = ?1 AND user_id IS ?2 AND key = ?3",
        rusqlite::params![addon_id, user_id, key],
    )?;
    Ok(())
}

// =============================================================================
// SSO Providers (tabela sso_providers — migracja 14)
// =============================================================================

/// Tworzy nowego SSO providera. Zwraca ID.
pub fn create_sso_provider(
    pool: &DbPool,
    name: &str,
    provider_type: &str,
    client_id: &str,
    client_secret_encrypted: &str,
    discovery_url: &str,
    auto_create_users: bool,
    default_group_id: Option<i64>,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO sso_providers (name, provider_type, client_id, client_secret_encrypted, \
         discovery_url, auto_create_users, default_group_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            name,
            provider_type,
            client_id,
            client_secret_encrypted,
            discovery_url,
            auto_create_users,
            default_group_id
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Lista wszystkich SSO providerow.
pub fn list_sso_providers(pool: &DbPool) -> Result<Vec<SsoProvider>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, provider_type, client_id, client_secret_encrypted, \
         discovery_url, enabled, auto_create_users, default_group_id, created_at \
         FROM sso_providers ORDER BY name",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SsoProvider {
                id: row.get(0)?,
                name: row.get(1)?,
                provider_type: row.get(2)?,
                client_id: row.get(3)?,
                client_secret_encrypted: row.get(4)?,
                discovery_url: row.get(5)?,
                enabled: row.get(6)?,
                auto_create_users: row.get(7)?,
                default_group_id: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pobiera SSO providera po ID.
pub fn get_sso_provider(pool: &DbPool, id: i64) -> Result<Option<SsoProvider>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT id, name, provider_type, client_id, client_secret_encrypted, \
             discovery_url, enabled, auto_create_users, default_group_id, created_at \
             FROM sso_providers WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok(SsoProvider {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider_type: row.get(2)?,
                    client_id: row.get(3)?,
                    client_secret_encrypted: row.get(4)?,
                    discovery_url: row.get(5)?,
                    enabled: row.get(6)?,
                    auto_create_users: row.get(7)?,
                    default_group_id: row.get(8)?,
                    created_at: row.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(result)
}

/// Pobiera SSO providera po nazwie.
pub fn get_sso_provider_by_name(pool: &DbPool, name: &str) -> Result<Option<SsoProvider>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT id, name, provider_type, client_id, client_secret_encrypted, \
             discovery_url, enabled, auto_create_users, default_group_id, created_at \
             FROM sso_providers WHERE name = ?1",
            rusqlite::params![name],
            |row| {
                Ok(SsoProvider {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider_type: row.get(2)?,
                    client_id: row.get(3)?,
                    client_secret_encrypted: row.get(4)?,
                    discovery_url: row.get(5)?,
                    enabled: row.get(6)?,
                    auto_create_users: row.get(7)?,
                    default_group_id: row.get(8)?,
                    created_at: row.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(result)
}

/// Usuwa SSO providera.
pub fn delete_sso_provider(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM sso_providers WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Upsert SSO providera po nazwie (uzywany przez CRDT sync).
pub fn upsert_sso_provider(
    pool: &DbPool,
    name: &str,
    provider_type: &str,
    client_id: &str,
    client_secret_encrypted: &str,
    discovery_url: &str,
    enabled: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO sso_providers (name, provider_type, client_id, client_secret_encrypted, \
         discovery_url, enabled) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(name) DO UPDATE SET \
         provider_type = excluded.provider_type, \
         client_id = excluded.client_id, \
         client_secret_encrypted = excluded.client_secret_encrypted, \
         discovery_url = excluded.discovery_url, \
         enabled = excluded.enabled",
        rusqlite::params![name, provider_type, client_id, client_secret_encrypted, discovery_url, enabled],
    )?;
    Ok(())
}

/// Usuwa SSO providera po nazwie (uzywany przez CRDT sync).
pub fn delete_sso_provider_by_name(pool: &DbPool, name: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM sso_providers WHERE name = ?1",
        rusqlite::params![name],
    )?;
    Ok(())
}

// =============================================================================
// SSO Users — tworzenie i wyszukiwanie uzytkownikow SSO
// =============================================================================

/// Tworzy uzytkownika z logowaniem SSO (bez hasla).
pub fn create_user_account_sso(
    pool: &DbPool,
    username: &str,
    display_name: &str,
    email: &str,
    sso_provider: &str,
    sso_subject: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    // Haslo = losowy hash (uzytkownik SSO nie loguje sie haslem)
    let random_hash = format!("$sso${}${}", sso_provider, uuid::Uuid::new_v4());
    conn.execute(
        "INSERT INTO user_accounts (username, password_hash, display_name, email, \
         sso_provider, sso_subject, is_active) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
        rusqlite::params![username, random_hash, display_name, email, sso_provider, sso_subject],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Wyszukuje uzytkownika po SSO provider + subject.
pub fn get_user_account_by_sso(
    pool: &DbPool,
    sso_provider: &str,
    sso_subject: &str,
) -> Result<Option<UserAccount>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM user_accounts WHERE sso_provider = ?1 AND sso_subject = ?2",
        USER_ACCOUNT_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![sso_provider, sso_subject], row_to_user_account)
        .optional()?;
    Ok(result)
}

/// Aktualizuje last_login_at uzytkownika.
pub fn update_user_account_last_login(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE user_accounts SET last_login_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// =============================================================================
// CRDT Sync Helpers — upsert po nazwie (nie po ID)
// =============================================================================

/// Upsert uzytkownika po username (uzywany przez CRDT sync).
pub fn upsert_user_account_by_username(
    pool: &DbPool,
    username: &str,
    password_hash: &str,
    display_name: &str,
    email: &str,
    is_active: bool,
    is_admin: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
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
        rusqlite::params![username, password_hash, display_name, email, is_active, is_admin],
    )?;
    Ok(())
}

/// Usuwa uzytkownika po username (uzywany przez CRDT sync).
pub fn delete_user_account_by_username(pool: &DbPool, username: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM user_accounts WHERE username = ?1",
        rusqlite::params![username],
    )?;
    Ok(())
}

/// Upsert grupy po nazwie (uzywany przez CRDT sync).
pub fn upsert_group_by_name(pool: &DbPool, name: &str, description: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO user_groups (name, description) VALUES (?1, ?2) \
         ON CONFLICT(name) DO UPDATE SET description = excluded.description",
        rusqlite::params![name, description],
    )?;
    Ok(())
}

/// Usuwa grupe po nazwie (uzywany przez CRDT sync).
pub fn delete_group_by_name(pool: &DbPool, name: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM user_groups WHERE name = ?1",
        rusqlite::params![name],
    )?;
    Ok(())
}

/// Dodaje uzytkownika do grupy po nazwach (uzywany przez CRDT sync).
pub fn add_user_to_group_by_names(
    pool: &DbPool,
    group_name: &str,
    username: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR IGNORE INTO group_members (group_id, user_id) \
         SELECT g.id, u.id FROM user_groups g, user_accounts u \
         WHERE g.name = ?1 AND u.username = ?2",
        rusqlite::params![group_name, username],
    )?;
    Ok(())
}

/// Usuwa uzytkownika z grupy po nazwach (uzywany przez CRDT sync).
pub fn remove_user_from_group_by_names(
    pool: &DbPool,
    group_name: &str,
    username: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM group_members WHERE group_id IN (SELECT id FROM user_groups WHERE name = ?1) \
         AND user_id IN (SELECT id FROM user_accounts WHERE username = ?2)",
        rusqlite::params![group_name, username],
    )?;
    Ok(())
}

/// Upsert uprawnienia per nazwy (uzywany przez CRDT sync).
pub fn upsert_permission_by_names(
    pool: &DbPool,
    addon_id: &str,
    subject_type: &str,
    subject_name: &str,
    permission_id: &str,
    granted: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
    // Rozwiaz subject_name na subject_id
    let subject_id: Option<i64> = match subject_type {
        "user" => conn
            .query_row(
                "SELECT id FROM user_accounts WHERE username = ?1",
                rusqlite::params![subject_name],
                |row| row.get(0),
            )
            .optional()?,
        "group" => conn
            .query_row(
                "SELECT id FROM user_groups WHERE name = ?1",
                rusqlite::params![subject_name],
                |row| row.get(0),
            )
            .optional()?,
        _ => None,
    };

    if let Some(sid) = subject_id {
        conn.execute(
            "INSERT INTO addon_permissions (addon_id, subject_type, subject_id, permission_id, granted) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(addon_id, subject_type, subject_id, permission_id) \
             DO UPDATE SET granted = excluded.granted",
            rusqlite::params![addon_id, subject_type, sid, permission_id, granted as i32],
        )?;
    }
    Ok(())
}

/// Usuwa uprawnienie per nazwy (uzywany przez CRDT sync).
pub fn delete_permission_by_names(
    pool: &DbPool,
    addon_id: &str,
    subject_type: &str,
    subject_name: &str,
    permission_id: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    let subject_id: Option<i64> = match subject_type {
        "user" => conn
            .query_row(
                "SELECT id FROM user_accounts WHERE username = ?1",
                rusqlite::params![subject_name],
                |row| row.get(0),
            )
            .optional()?,
        "group" => conn
            .query_row(
                "SELECT id FROM user_groups WHERE name = ?1",
                rusqlite::params![subject_name],
                |row| row.get(0),
            )
            .optional()?,
        _ => None,
    };

    if let Some(sid) = subject_id {
        conn.execute(
            "DELETE FROM addon_permissions \
             WHERE addon_id = ?1 AND subject_type = ?2 AND subject_id = ?3 AND permission_id = ?4",
            rusqlite::params![addon_id, subject_type, sid, permission_id],
        )?;
    }
    Ok(())
}

/// Upsert addon z synchronizacji CRDT (po addon_id).
pub fn upsert_addon_sync(
    pool: &DbPool,
    addon_id: &str,
    name: &str,
    version: &str,
    manifest_json: &str,
    platforms: &str,
    wasm_hash: &str,
) -> Result<bool> {
    let conn = acquire(pool)?;

    // Sprawdz czy hash WASM sie zmienil — jesli tak, trzeba pobrac plik
    let current_hash: Option<String> = conn
        .query_row(
            "SELECT manifest_json FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;

    // Sprawdz czy hash sie zmienil (uproszczone — porownujemy manifest_json ktory zawiera hash)
    let wasm_changed = current_hash.as_deref() != Some(manifest_json);

    conn.execute(
        "INSERT INTO addons (addon_id, name, version, manifest_json, platforms) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(addon_id) DO UPDATE SET \
         name = excluded.name, \
         version = excluded.version, \
         manifest_json = excluded.manifest_json, \
         platforms = excluded.platforms, \
         updated_at = datetime('now')",
        rusqlite::params![addon_id, name, version, manifest_json, platforms],
    )?;

    // Zanotuj wasm_hash w ustawieniach addonu (do porownywania przy sync)
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value, updated_at) \
         VALUES (?1, ?2, datetime('now'))",
        rusqlite::params![format!("addon_wasm_hash:{addon_id}"), wasm_hash],
    )?;

    Ok(wasm_changed)
}

/// Upsert sekretu addonu per nazwy (uzywany przez CRDT sync).
pub fn upsert_addon_secret_sync(
    pool: &DbPool,
    addon_id: &str,
    username: Option<&str>,
    key: &str,
    encrypted_value: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    let user_id: Option<i64> = if let Some(uname) = username {
        conn.query_row(
            "SELECT id FROM user_accounts WHERE username = ?1",
            rusqlite::params![uname],
            |row| row.get(0),
        )
        .optional()?
    } else {
        None
    };

    conn.execute(
        "INSERT INTO addon_secrets (addon_id, user_id, key, value_encrypted) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(addon_id, user_id, key) \
         DO UPDATE SET value_encrypted = excluded.value_encrypted, updated_at = datetime('now')",
        rusqlite::params![addon_id, user_id, key, encrypted_value],
    )?;
    Ok(())
}

/// Usuwa sekret addonu per nazwy (uzywany przez CRDT sync).
pub fn delete_addon_secret_sync(
    pool: &DbPool,
    addon_id: &str,
    username: Option<&str>,
    key: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    let user_id: Option<i64> = if let Some(uname) = username {
        conn.query_row(
            "SELECT id FROM user_accounts WHERE username = ?1",
            rusqlite::params![uname],
            |row| row.get(0),
        )
        .optional()?
    } else {
        None
    };

    conn.execute(
        "DELETE FROM addon_secrets WHERE addon_id = ?1 AND user_id IS ?2 AND key = ?3",
        rusqlite::params![addon_id, user_id, key],
    )?;
    Ok(())
}

/// Upsert sync exclusion (uzywany przez CRDT sync).
pub fn upsert_sync_exclusion(
    pool: &DbPool,
    group_name: &str,
    resource_type: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR IGNORE INTO sync_exclusions (group_id, resource_type) \
         SELECT id, ?2 FROM user_groups WHERE name = ?1",
        rusqlite::params![group_name, resource_type],
    )?;
    Ok(())
}

/// Usuwa sync exclusion (uzywany przez CRDT sync).
pub fn delete_sync_exclusion(
    pool: &DbPool,
    group_name: &str,
    resource_type: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM sync_exclusions WHERE group_id IN \
         (SELECT id FROM user_groups WHERE name = ?1) AND resource_type = ?2",
        rusqlite::params![group_name, resource_type],
    )?;
    Ok(())
}

/// Sprawdza czy dany resource_type jest wykluczony z synchronizacji
/// dla jakiejkolwiek grupy lokalnego noda.
pub fn is_sync_excluded(pool: &DbPool, resource_type: &str) -> Result<bool> {
    let conn = acquire(pool)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_exclusions WHERE resource_type = ?1",
        rusqlite::params![resource_type],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Upsert konfiguracji addonu (uzywany przez CRDT sync).
pub fn set_addon_config(pool: &DbPool, addon_id: &str, key: &str, value: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value, updated_at) \
         VALUES (?1, ?2, datetime('now'))",
        rusqlite::params![format!("addon_config:{addon_id}:{key}"), value],
    )?;
    Ok(())
}

/// Pobiera wszystkie ustawienia konfiguracji addonu jako HashMap
pub fn get_addon_config_values(pool: &DbPool, addon_id: &str) -> Result<std::collections::HashMap<String, String>> {
    let conn = acquire(pool)?;
    let prefix = format!("addon_config:{}:", addon_id);
    let mut stmt = conn.prepare(
        "SELECT key, value FROM settings WHERE key LIKE ?1"
    )?;
    let rows = stmt.query_map(rusqlite::params![format!("{}%", prefix)], |row| {
        let full_key: String = row.get(0)?;
        let value: String = row.get(1)?;
        let short_key = full_key.strip_prefix(&prefix).unwrap_or(&full_key).to_string();
        Ok((short_key, value))
    })?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (k, v) = row?;
        map.insert(k, v);
    }
    Ok(map)
}

// =============================================================================
// Trusted Nodes — zaufane nody mesh
// =============================================================================

/// Dodaje zaufany node do bazy
pub fn add_trusted_node(
    pool: &DbPool,
    node_id: &str,
    public_key_hex: &str,
    hostname: &str,
    approved_by: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR REPLACE INTO trusted_nodes (node_id, public_key, hostname, approved_by, approved_at, is_active) \
         VALUES (?1, ?2, ?3, ?4, datetime('now'), 1)",
        rusqlite::params![node_id, public_key_hex, hostname, approved_by],
    )?;
    Ok(())
}

/// Pobiera liste zaufanych nodow
pub fn list_trusted_nodes(pool: &DbPool) -> Result<Vec<TrustedNode>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, node_id, public_key, hostname, approved_by, approved_at, is_active, last_addresses \
         FROM trusted_nodes WHERE is_active = 1 ORDER BY approved_at DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(TrustedNode {
                id: row.get(0)?,
                node_id: row.get(1)?,
                public_key: row.get(2)?,
                hostname: row.get(3)?,
                approved_by: row.get(4)?,
                approved_at: row.get(5)?,
                is_active: row.get(6)?,
                last_addresses: row.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Aktualizuje ostatnie znane adresy trusted noda
pub fn update_trusted_node_addresses(pool: &DbPool, node_id: &str, addresses: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE trusted_nodes SET last_addresses = ?2 WHERE node_id = ?1 AND is_active = 1",
        rusqlite::params![node_id, addresses],
    )?;
    Ok(())
}

/// Usuwa zaufany node z bazy
pub fn remove_trusted_node(pool: &DbPool, node_id: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM trusted_nodes WHERE node_id = ?1",
        rusqlite::params![node_id],
    )?;
    Ok(())
}

/// Sprawdza czy node jest zaufany
pub fn is_node_trusted(pool: &DbPool, node_id: &str) -> Result<bool> {
    let conn = acquire(pool)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM trusted_nodes WHERE node_id = ?1 AND is_active = 1",
        rusqlite::params![node_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Pobiera klucz publiczny zaufanego noda
pub fn get_trusted_node_public_key(pool: &DbPool, node_id: &str) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT public_key FROM trusted_nodes WHERE node_id = ?1 AND is_active = 1",
            rusqlite::params![node_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(result)
}

// =============================================================================
// Pending Pairings — oczekujace parowania
// =============================================================================

/// Tworzy nowe oczekujace parowanie
pub fn create_pending_pairing(
    pool: &DbPool,
    remote_node_id: &str,
    pin: &str,
    direction: &str,
    expires_at: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    // Usun poprzednie oczekujace parowania z tym nodem
    conn.execute(
        "DELETE FROM pending_pairings WHERE remote_node_id = ?1",
        rusqlite::params![remote_node_id],
    )?;
    conn.execute(
        "INSERT INTO pending_pairings (remote_node_id, pin_code, direction, expires_at) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![remote_node_id, pin, direction, expires_at],
    )?;
    Ok(())
}

/// Pobiera oczekujace parowanie z nodem
pub fn get_pending_pairing(pool: &DbPool, remote_node_id: &str) -> Result<Option<PendingPairing>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT id, remote_node_id, pin_code, direction, expires_at \
             FROM pending_pairings WHERE remote_node_id = ?1",
            rusqlite::params![remote_node_id],
            |row| {
                Ok(PendingPairing {
                    id: row.get(0)?,
                    remote_node_id: row.get(1)?,
                    pin_code: row.get(2)?,
                    direction: row.get(3)?,
                    expires_at: row.get(4)?,
                })
            },
        )
        .optional()?;
    Ok(result)
}

/// Pobiera wszystkie oczekujace parowania
pub fn list_pending_pairings(pool: &DbPool) -> Result<Vec<PendingPairing>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare(
        "SELECT id, remote_node_id, pin_code, direction, expires_at \
         FROM pending_pairings ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(PendingPairing {
                id: row.get(0)?,
                remote_node_id: row.get(1)?,
                pin_code: row.get(2)?,
                direction: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Usuwa oczekujace parowanie z nodem
pub fn delete_pending_pairing(pool: &DbPool, remote_node_id: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM pending_pairings WHERE remote_node_id = ?1",
        rusqlite::params![remote_node_id],
    )?;
    Ok(())
}

/// Usuwa wygasle parowania
pub fn cleanup_expired_pairings(pool: &DbPool) -> Result<u64> {
    let conn = acquire(pool)?;
    let deleted = conn.execute(
        "DELETE FROM pending_pairings WHERE expires_at < datetime('now')",
        [],
    )?;
    Ok(deleted as u64)
}

// =============================================================================
// Addon Resource Limits — limity zasobow addonow (migracja 16)
// =============================================================================

/// Struktura limitow zasobow addonu. 0 = bez limitu (unlimited).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AddonResourceLimits {
    pub addon_id: String,
    pub max_instances: i64,
    pub cpu_limit_ms_per_min: i64,
    pub ram_limit_mb: i64,
    pub gpu_enabled: bool,
    pub vram_limit_mb: i64,
    pub storage_limit_mb: i64,
    pub http_requests_per_min: i64,
    pub llm_tokens_per_min: i64,
    /// Limit paliwa WASM per wywolanie (0 = domyslny 10M instrukcji)
    pub fuel_limit: i64,
}

/// Pobiera limity zasobow addonu. Zwraca domyslne (0 = bez limitu) jesli brak wpisu.
pub fn get_addon_resource_limits(pool: &DbPool, addon_id: &str) -> Result<AddonResourceLimits> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, \
             gpu_enabled, vram_limit_mb, storage_limit_mb, http_requests_per_min, \
             llm_tokens_per_min, fuel_limit \
             FROM addon_resource_limits WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| {
                Ok(AddonResourceLimits {
                    addon_id: row.get(0)?,
                    max_instances: row.get(1)?,
                    cpu_limit_ms_per_min: row.get(2)?,
                    ram_limit_mb: row.get(3)?,
                    gpu_enabled: row.get::<_, i64>(4)? != 0,
                    vram_limit_mb: row.get(5)?,
                    storage_limit_mb: row.get(6)?,
                    http_requests_per_min: row.get(7)?,
                    llm_tokens_per_min: row.get(8)?,
                    fuel_limit: row.get(9)?,
                })
            },
        )
        .optional()?;

    // Jesli brak wpisu — zwroc domyslne (0 = bez limitu)
    Ok(result.unwrap_or(AddonResourceLimits {
        addon_id: addon_id.to_string(),
        max_instances: 0,
        cpu_limit_ms_per_min: 0,
        ram_limit_mb: 0,
        gpu_enabled: true,
        vram_limit_mb: 0,
        storage_limit_mb: 0,
        http_requests_per_min: 0,
        llm_tokens_per_min: 0,
        fuel_limit: 0,
    }))
}

/// Ustawia (INSERT OR REPLACE) limity zasobow addonu.
pub fn set_addon_resource_limits(pool: &DbPool, limits: &AddonResourceLimits) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_resource_limits \
         (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
          vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min, \
          fuel_limit, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, datetime('now')) \
         ON CONFLICT(addon_id) DO UPDATE SET \
         max_instances = excluded.max_instances, \
         cpu_limit_ms_per_min = excluded.cpu_limit_ms_per_min, \
         ram_limit_mb = excluded.ram_limit_mb, \
         gpu_enabled = excluded.gpu_enabled, \
         vram_limit_mb = excluded.vram_limit_mb, \
         storage_limit_mb = excluded.storage_limit_mb, \
         http_requests_per_min = excluded.http_requests_per_min, \
         llm_tokens_per_min = excluded.llm_tokens_per_min, \
         fuel_limit = excluded.fuel_limit, \
         updated_at = datetime('now')",
        rusqlite::params![
            &limits.addon_id,
            limits.max_instances,
            limits.cpu_limit_ms_per_min,
            limits.ram_limit_mb,
            limits.gpu_enabled as i64,
            limits.vram_limit_mb,
            limits.storage_limit_mb,
            limits.http_requests_per_min,
            limits.llm_tokens_per_min,
            limits.fuel_limit,
        ],
    )?;
    Ok(())
}

/// Tworzy domyslne limity zasobow addonu (INSERT OR IGNORE — nie nadpisuje istniejacych).
pub fn create_default_addon_resource_limits(pool: &DbPool, addon_id: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR IGNORE INTO addon_resource_limits \
         (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
          vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
         VALUES (?1, 0, 0, 0, 1, 0, 0, 0, 0)",
        rusqlite::params![addon_id],
    )?;
    Ok(())
}

// =============================================================================
// Revoked nodes — cofniete zaufanie z persistencja
// =============================================================================

/// Dodaje node do listy revoked
pub fn add_revoked_node(pool: &DbPool, node_id: &str, revoked_by: Option<&str>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR IGNORE INTO revoked_nodes (node_id, revoked_by) VALUES (?1, ?2)",
        rusqlite::params![node_id, revoked_by],
    )?;
    Ok(())
}

/// Sprawdza czy node jest revoked
pub fn is_node_revoked(pool: &DbPool, node_id: &str) -> Result<bool> {
    let conn = acquire(pool)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM revoked_nodes WHERE node_id = ?1",
        rusqlite::params![node_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Usuwa node z listy revoked (admin re-trust)
pub fn remove_revoked_node(pool: &DbPool, node_id: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM revoked_nodes WHERE node_id = ?1",
        rusqlite::params![node_id],
    )?;
    Ok(())
}

/// Lista wszystkich revoked nodow
pub fn list_revoked_nodes(pool: &DbPool) -> Result<Vec<String>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare("SELECT node_id FROM revoked_nodes ORDER BY revoked_at DESC")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
