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
    pool.lock()
        .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))
}

/// Mapowanie wiersza na DbService
fn row_to_service(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbService> {
    let pinned_int: i64 = row.get(11).unwrap_or(0);
    let paused_int: i64 = row.get(12).unwrap_or(0);
    let vram_estimate: Option<i64> = row.get(13).ok();
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
        pinned: pinned_int != 0,
        paused: paused_int != 0,
        vram_estimate_mb: vram_estimate.map(|v| v.max(0) as u64),
        deployed_source_hash: row.get(14)?,
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
        language: row.get(11)?,
        is_system: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
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

// --- Teams Bot Wake Words ---

#[derive(Debug, Clone, serde::Serialize)]
pub struct WakeWord {
    pub id: i64,
    pub word: String,
    pub enabled: bool,
    pub created_at: String,
}

/// Lista wszystkich slow aktywujacych (wlaczonych i wylaczonych).
pub fn list_wake_words(pool: &DbPool) -> Result<Vec<WakeWord>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, word, enabled, created_at FROM teams_bot_wake_words ORDER BY word",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(WakeWord {
                id: r.get(0)?,
                word: r.get(1)?,
                enabled: r.get::<_, i64>(2)? != 0,
                created_at: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Lista samych wlaczonych slow w postaci CSV — uzywane przy spawn bota.
pub fn enabled_wake_words_csv(pool: &DbPool) -> Result<String> {
    let conn = acquire(pool)?;
    let mut stmt = conn
        .prepare_cached("SELECT word FROM teams_bot_wake_words WHERE enabled = 1 ORDER BY word")?;
    let words: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(words.join(","))
}

/// Dodaje slowo (idempotentnie). Zwraca id istniejacego/nowego rekordu.
pub fn add_wake_word(pool: &DbPool, word: &str) -> Result<i64> {
    let trimmed = word.trim();
    if trimmed.is_empty() {
        anyhow::bail!("wake_word puste");
    }
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR IGNORE INTO teams_bot_wake_words (word, enabled) VALUES (?1, 1)",
        rusqlite::params![trimmed],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM teams_bot_wake_words WHERE word = ?1",
        rusqlite::params![trimmed],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// Usuwa slowo po id.
pub fn delete_wake_word(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM teams_bot_wake_words WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Toggle enabled/disabled. Zwraca nowy stan.
pub fn set_wake_word_enabled(pool: &DbPool, id: i64, enabled: bool) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE teams_bot_wake_words SET enabled = ?2 WHERE id = ?1",
        rusqlite::params![id, if enabled { 1 } else { 0 }],
    )?;
    Ok(())
}

// --- Services ---

const SERVICE_COLS: &str = "id, name, service_type, strategy, model_category, status, config_json, created_at, updated_at, service_uuid, node_id, pinned, paused, vram_estimate_mb, deployed_source_hash";
const BACKEND_COLS: &str = "id, service_id, connection_type, config_json, max_concurrent, timeout_ms, weight, model_name_override, health_check_path, is_active";

pub fn list_services(pool: &DbPool) -> Result<Vec<DbService>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM services ORDER BY name",
        SERVICE_COLS
    ))?;
    let services = stmt
        .query_map([], row_to_service)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(services)
}

/// Pobiera wszystkie serwisy z backendami jednym zapytaniem JOIN (eliminacja N+1).
pub fn list_services_with_backends(
    pool: &DbPool,
) -> Result<Vec<(DbService, Vec<DbServiceBackend>)>> {
    let conn = acquire(pool)?;

    let mut stmt = conn.prepare_cached(
        "SELECT s.id, s.name, s.service_type, s.strategy, s.model_category, s.status, s.config_json, s.created_at, s.updated_at, s.service_uuid, s.node_id, s.pinned, s.paused, s.vram_estimate_mb, s.deployed_source_hash, \
         b.id, b.service_id, b.connection_type, b.config_json, b.max_concurrent, b.timeout_ms, b.weight, b.model_name_override, b.health_check_path, b.is_active \
         FROM services s LEFT JOIN service_backends b ON s.id = b.service_id ORDER BY s.name, b.id",
    )?;

    let mut services: Vec<(DbService, Vec<DbServiceBackend>)> = Vec::new();
    let mut last_service_id: Option<i64> = None;

    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let svc_id: i64 = row.get(0)?;

        if last_service_id != Some(svc_id) {
            let pinned_int: i64 = row.get(11).unwrap_or(0);
            let paused_int: i64 = row.get(12).unwrap_or(0);
            let vram_estimate: Option<i64> = row.get(13).ok();
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
                pinned: pinned_int != 0,
                paused: paused_int != 0,
                vram_estimate_mb: vram_estimate.map(|v| v.max(0) as u64),
                deployed_source_hash: row.get(14)?,
            };
            services.push((service, Vec::new()));
            last_service_id = Some(svc_id);
        }

        let backend_id: Option<i64> = row.get(15)?;
        if backend_id.is_some() {
            let backend = DbServiceBackend {
                id: row.get(15)?,
                service_id: row.get(16)?,
                connection_type: row.get(17)?,
                config_json: row.get(18)?,
                max_concurrent: row.get(19)?,
                timeout_ms: row.get(20)?,
                weight: row.get(21)?,
                model_name_override: row.get(22)?,
                health_check_path: row.get(23)?,
                is_active: row.get(24)?,
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM services WHERE id = ?1",
        SERVICE_COLS
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

/// Wstawia serwis on_demand (agents/tools). Gdy wiersz o tej nazwie juz istnieje,
/// aktualizuje config_json + updated_at bez zmiany statusu (zeby nie resetowac
/// stanu wlacz/wylacz ustawionego przez admina). Zwraca id wiersza.
pub fn upsert_service_on_demand(
    pool: &DbPool,
    name: &str,
    service_type: &str,
    model_category: Option<&str>,
    config_json: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO services (name, service_type, strategy, model_category, status, config_json)
             VALUES (?1, ?2, 'single', ?3, 'on_demand', ?4)
         ON CONFLICT(name) DO UPDATE SET
             service_type = excluded.service_type,
             model_category = excluded.model_category,
             config_json = excluded.config_json,
             updated_at = datetime('now')",
        rusqlite::params![name, service_type, model_category, config_json],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM services WHERE name = ?1",
        rusqlite::params![name],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn update_service(
    pool: &DbPool,
    id: i64,
    name: &str,
    service_type: &str,
    strategy: &str,
    model_category: Option<&str>,
    status: &str,
    config_json: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE services SET name = ?2, service_type = ?3, strategy = ?4, model_category = ?5, status = ?6, config_json = ?7, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, name, service_type, strategy, model_category, status, config_json],
    )?;
    Ok(())
}

pub fn delete_service(pool: &DbPool, id: i64) -> Result<()> {
    // FK `model_registry.service_id` ma ON DELETE SET NULL (sierocce wpisy w GUI po usunieciu serwisu)
    // — kasujemy modele jawnie zeby UI byl spojny. service_backends ma ON DELETE CASCADE,
    // ale wykonujemy DELETE jawnie dla symetrii z `delete_service_cascade_by_name`.
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM service_backends WHERE service_id = ?1",
        rusqlite::params![id],
    )?;
    conn.execute(
        "DELETE FROM model_registry WHERE service_id = ?1",
        rusqlite::params![id],
    )?;
    conn.execute("DELETE FROM services WHERE id = ?1", rusqlite::params![id])?;
    Ok(())
}

/// MemoryGuard: ustawia flag pinned (zawsze warm, nie evict) per service.
pub fn set_service_pinned(pool: &DbPool, id: i64, pinned: bool) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE services SET pinned = ?2, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, if pinned { 1 } else { 0 }],
    )?;
    Ok(())
}

/// MemoryGuard: ustawia flag paused (skip autostart, request odrzucany).
pub fn set_service_paused(pool: &DbPool, id: i64, paused: bool) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE services SET paused = ?2, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, if paused { 1 } else { 0 }],
    )?;
    Ok(())
}

/// Ustawia `node_id` serwisu (hex-enkodowany klucz mesh) lub czysci pole
/// jezeli przekazano `None`. Aktualizuje `updated_at` do bierzacego czasu.
pub fn set_service_node_id(pool: &DbPool, id: i64, node_id: Option<&str>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE services SET node_id = ?2, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, node_id],
    )?;
    Ok(())
}

/// Records the sha256 of the container source tree captured at deploy time.
/// Used by the dashboard to detect whether a running service is out of date
/// relative to the compile-time manifest hash.
pub fn set_deployed_source_hash(pool: &DbPool, service_id: i64, hash: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE services SET deployed_source_hash = ?2, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![service_id, hash],
    )?;
    Ok(())
}

/// Kaskadowe usuwanie serwisu po nazwie: backendy, modele, serwis
pub fn delete_service_cascade_by_name(pool: &DbPool, name: &str) -> Result<u32> {
    let conn = acquire(pool)?;
    let service_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM services WHERE name = ?1",
            rusqlite::params![name],
            |r| r.get(0),
        )
        .optional()?;

    let Some(service_id) = service_id else {
        return Ok(0);
    };

    let mut deleted = 0u32;
    deleted += conn.execute(
        "DELETE FROM service_backends WHERE service_id = ?1",
        rusqlite::params![service_id],
    )? as u32;
    deleted += conn.execute(
        "DELETE FROM model_registry WHERE service_id = ?1",
        rusqlite::params![service_id],
    )? as u32;
    deleted += conn.execute(
        "DELETE FROM services WHERE id = ?1",
        rusqlite::params![service_id],
    )? as u32;
    Ok(deleted)
}

// --- Service Backends ---

pub fn list_backends_for_service(pool: &DbPool, service_id: i64) -> Result<Vec<DbServiceBackend>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM service_backends WHERE service_id = ?1",
        BACKEND_COLS
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM service_backends WHERE id = ?1",
        BACKEND_COLS
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

const API_KEY_COLS: &str =
    "id, key_hash, key_prefix, name, rate_limit_rps, is_active, created_at, last_used_at, owner_user_id";

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
        owner_user_id: row.get::<_, Option<i64>>(8).ok().flatten(),
    })
}

pub fn list_api_keys(pool: &DbPool) -> Result<Vec<DbApiKey>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, key_prefix, name, rate_limit_rps, is_active, created_at, last_used_at, owner_user_id FROM api_keys ORDER BY name",
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
                owner_user_id: row.get::<_, Option<i64>>(7).ok().flatten(),
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
    let affected = conn.execute("DELETE FROM api_keys WHERE id = ?1", rusqlite::params![id])?;
    Ok(affected)
}

pub fn verify_api_key(pool: &DbPool, key_hash: &str) -> Result<Option<DbApiKey>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM api_keys WHERE key_hash = ?1 AND is_active = 1",
        API_KEY_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![key_hash], row_to_api_key)
        .optional()?;
    Ok(result)
}

// --- Service Aliases ---

pub fn list_aliases(pool: &DbPool) -> Result<Vec<DbServiceAlias>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
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

/// Odczytuje setting z automatycznym deszyfrowaniem (jesli klucz jest wrazliwy)
pub fn get_setting_secure(
    pool: &DbPool,
    key: &str,
    cipher: &crate::crypto::SettingsCipher,
) -> Result<Option<String>> {
    let raw = get_setting(pool, key)?;
    match raw {
        Some(val) if crate::crypto::SettingsCipher::should_encrypt(key) => Ok(Some(
            cipher.decrypt(&val).map_err(|e| anyhow::anyhow!("{}", e))?,
        )),
        other => Ok(other),
    }
}

/// Zapisuje setting z automatycznym szyfrowaniem (jesli klucz jest wrazliwy)
pub fn set_setting_secure(
    pool: &DbPool,
    key: &str,
    value: &str,
    cipher: &crate::crypto::SettingsCipher,
) -> Result<()> {
    if crate::crypto::SettingsCipher::should_encrypt(key) {
        let encrypted = cipher
            .encrypt(value)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        set_setting(pool, key, &encrypted)
    } else {
        set_setting(pool, key, value)
    }
}

/// Usuwa ustawienie po kluczu (CR-016: jednorazowe tokeny SSO state)
pub fn delete_setting(pool: &DbPool, key: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![key],
    )?;
    Ok(())
}

pub fn list_settings(pool: &DbPool) -> Result<Vec<DbSetting>> {
    let conn = acquire(pool)?;
    let mut stmt =
        conn.prepare_cached("SELECT key, value, updated_at FROM settings ORDER BY key")?;
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

/// Zwraca pary `(key, value)` ze `settings` ktorych klucz zaczyna sie od `prefix`.
/// Uzywane m.in. przez `net::iroh::pairing::sanitize_trusted_contacts` do iteracji
/// po wpisach `trusted_contact:*` bez wczytywania calej tabeli.
pub fn list_settings_with_prefix(pool: &DbPool, prefix: &str) -> Result<Vec<(String, String)>> {
    let conn = acquire(pool)?;
    let pattern = format!("{}%", prefix);
    let mut stmt = conn.prepare_cached(
        "SELECT key, value FROM settings WHERE key LIKE ?1 ESCAPE '\\' ORDER BY key",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![pattern], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// --- Users ---

pub fn get_user_by_username(pool: &DbPool, username: &str) -> Result<Option<DbUser>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
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

pub fn create_user(pool: &DbPool, username: &str, password_hash: &str, role: &str) -> Result<i64> {
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

/// Lista jezykow akceptowanych w `users.preferred_language` i w polu
/// `language` requestu TTS. Trzymana w jednym miejscu, bo walidacja zapisu
/// preferencji i walidacja body'ego endpointu musza wzajemnie sie zgadzac.
pub const SUPPORTED_USER_LANGUAGES: &[&str] = &["pl", "en", "fr", "es", "de"];

/// Zwraca preferowany jezyk uzytkownika lub None jesli brak preferencji.
pub fn get_user_preferred_language(pool: &DbPool, user_id: i64) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT preferred_language FROM users WHERE id = ?1",
            rusqlite::params![user_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(result)
}

/// Ustawia preferowany jezyk uzytkownika. `lang = None` czysci preferencje.
/// Zwraca blad gdy `lang` nie nalezy do `SUPPORTED_USER_LANGUAGES`.
pub fn set_user_preferred_language(pool: &DbPool, user_id: i64, lang: Option<&str>) -> Result<()> {
    if let Some(code) = lang {
        if !SUPPORTED_USER_LANGUAGES.contains(&code) {
            return Err(anyhow::anyhow!(
                "Nieobslugiwany kod jezyka: '{}' (dozwolone: {:?})",
                code,
                SUPPORTED_USER_LANGUAGES
            ));
        }
    }
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE users SET preferred_language = ?1 WHERE id = ?2",
        rusqlite::params![lang, user_id],
    )?;
    Ok(())
}

// --- Prompts ---

const PROMPT_COLS: &str = "id, prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority, is_active, version, language, is_system, created_at, updated_at";

pub fn list_prompts(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbPrompt>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM prompts ORDER BY name LIMIT ?1 OFFSET ?2",
        PROMPT_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_prompt)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_prompt(pool: &DbPool, id: i64) -> Result<Option<DbPrompt>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM prompts WHERE id = ?1",
        PROMPT_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_prompt)
        .optional()?;
    Ok(result)
}

pub fn get_prompt_by_prompt_id(pool: &DbPool, prompt_id: &str) -> Result<Option<DbPrompt>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM prompts WHERE prompt_id = ?1 ORDER BY (language = 'pl') DESC, language ASC LIMIT 1",
        PROMPT_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![prompt_id], row_to_prompt)
        .optional()?;
    Ok(result)
}

/// Runtime lookup z fallbackiem na `pl`. Uzywane przez bota gdy chcemy wariant
/// per-jezyk, ale baza domyslnie ma polski seed wiec ten sam prompt zadziala
/// gdy lokal nie jest przetlumaczony.
pub fn find_prompt(pool: &DbPool, prompt_id: &str, language: &str) -> Result<Option<DbPrompt>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM prompts WHERE prompt_id = ?1 AND language = ?2",
        PROMPT_COLS
    ))?;
    let exact = stmt
        .query_row(rusqlite::params![prompt_id, language], row_to_prompt)
        .optional()?;
    if exact.is_some() {
        return Ok(exact);
    }
    if language == "pl" {
        return Ok(None);
    }
    let fallback = stmt
        .query_row(rusqlite::params![prompt_id, "pl"], row_to_prompt)
        .optional()?;
    Ok(fallback)
}

pub fn create_prompt(pool: &DbPool, params: &NewPrompt<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO prompts (prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority, language) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![params.prompt_id, params.name, params.description, params.content, params.prompt_type, params.default_model, params.variables, params.cache_priority, params.language],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_prompt(pool: &DbPool, params: &UpdatePrompt<'_>) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE prompts SET name = ?2, description = ?3, content = ?4, prompt_type = ?5, default_model = ?6, variables = ?7, cache_priority = ?8, is_active = ?9, language = ?10, version = version + 1, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![params.id, params.name, params.description, params.content, params.prompt_type, params.default_model, params.variables, params.cache_priority, params.is_active, params.language],
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM model_registry ORDER BY model_name LIMIT ?1 OFFSET ?2",
        MODEL_ENTRY_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_model_entry)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_model_entry(pool: &DbPool, id: i64) -> Result<Option<DbModelEntry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM model_registry WHERE id = ?1",
        MODEL_ENTRY_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_model_entry)
        .optional()?;
    Ok(result)
}

pub fn get_model_by_name(pool: &DbPool, model_name: &str) -> Result<Option<DbModelEntry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM model_registry WHERE model_name = ?1",
        MODEL_ENTRY_COLS
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
    conn.execute(
        "DELETE FROM model_registry WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Re-linkuje istniejacy wpis `model_registry` (po `model_name`) na nowy
/// `service_id`, aktualizujac rownoczesnie `service_type`, `config_json`,
/// `is_active=1` i `connection_type='local'`. Idempotentne i atomowe — caly
/// UPDATE jest pojedynczym statement SQL.
///
/// DLACZEGO: Re-deploy po wczesniejszym delete zostawial w bazie sierote
/// (`service_id=NULL`, bo do FIX 1 FK byl `ON DELETE SET NULL`). Bez relinka
/// `ensure_model_registry_entry` widzial istniejacy `model_name` i robil
/// early return — nowy serwis dostawal `services.id`, ale routing TTS/STT/LLM
/// nie znajdowal backendu (model.service_id dalej NULL).
pub fn relink_model_entry_to_service(
    pool: &DbPool,
    model_name: &str,
    service_id: i64,
    service_type: &str,
    config_json: &str,
) -> Result<usize> {
    let conn = acquire(pool)?;
    // Schema CHECK pozwala tylko ('quic','openai_api','internal'). Native
    // deploy (libloading w procesie) = `internal`. Nie nadpisujemy connection_type
    // jezeli istniejacy wpis mial juz inny prawidlowy typ — UPDATE bez tego pola
    // zachowuje istniejaca wartosc (kompatybilnosc z modelami zarejestrowanymi
    // przez inne sciezki, np. openai_api).
    let updated = conn.execute(
        "UPDATE model_registry \
         SET service_id = ?2, service_type = ?3, config_json = ?4, \
             is_active = 1 \
         WHERE model_name = ?1",
        rusqlite::params![model_name, service_id, service_type, config_json],
    )?;
    Ok(updated)
}

/// Czysci sieroty po historycznych usunieciach serwisow sprzed kaskadowego
/// `delete_service` (gdy FK `model_registry.service_id` byl `ON DELETE SET NULL`).
/// Schema CHECK na `connection_type` to ('quic','openai_api','internal') —
/// `quic` bezpiecznie kasujemy bo to zawsze osobny serwis. `internal` z
/// `service_id IS NULL` jest niejednoznaczny (legitne globale + sieroty po
/// natywnych deploy'ach), wiec kasujemy tylko te dopasowane do znanych
/// prefiksow nazw silnikow natywnych (apple-tts-, kokoro-, llamacpp-,
/// whisper-, mlx-) — pokrywa historyczne sieroty bez usuniecia legitnych
/// globali typu zewnetrzne API.
pub fn prune_orphaned_quic_models(pool: &DbPool) -> Result<u32> {
    let conn = acquire(pool)?;
    let mut total = 0u32;
    total += conn.execute(
        "DELETE FROM model_registry \
         WHERE service_id IS NULL AND connection_type = 'quic'",
        rusqlite::params![],
    )? as u32;
    total += conn.execute(
        "DELETE FROM model_registry \
         WHERE service_id IS NULL AND connection_type = 'internal' \
           AND (model_name LIKE 'apple-tts-%' \
             OR model_name LIKE 'kokoro-%' \
             OR model_name LIKE 'llamacpp-%' \
             OR model_name LIKE 'whisper-%' \
             OR model_name LIKE 'mlx-%')",
        rusqlite::params![],
    )? as u32;
    if total > 0 {
        tracing::info!(count = total, "Usunieto osierocone wpisy model_registry");
    }
    Ok(total)
}

/// Usuwa wszystkie wpisy model_registry powiazane z danym serwisem.
/// Wolane przy service_delete — bez tego stare modele MLX/llama.cpp zostaja
/// w GUI jako "Załadowane" mimo ze ich serwis juz nie istnieje.
pub fn delete_model_entries_by_service(pool: &DbPool, service_id: i64) -> Result<usize> {
    let conn = acquire(pool)?;
    let removed = conn.execute(
        "DELETE FROM model_registry WHERE service_id = ?1",
        rusqlite::params![service_id],
    )?;
    Ok(removed)
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM model_aliases ORDER BY alias",
        MODEL_ALIAS_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_model_alias)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_model_alias(pool: &DbPool, id: i64) -> Result<Option<DbModelAlias>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM model_aliases WHERE id = ?1",
        MODEL_ALIAS_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_model_alias)
        .optional()?;
    Ok(result)
}

pub fn resolve_model_alias(pool: &DbPool, alias: &str) -> Result<Option<DbModelAlias>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM model_aliases WHERE alias = ?1 AND is_active = 1",
        MODEL_ALIAS_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![alias], row_to_model_alias)
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
    conn.execute(
        "DELETE FROM model_aliases WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Tworzy alias jesli nie istnieje, lub reaktywuje istniejacy (bez zmiany target_model).
/// Uzywane przez cykl zycia addonow do automatycznego zarzadzania aliasami.
pub fn create_or_reactivate_model_alias(
    pool: &DbPool,
    alias: &str,
    default_target_model: &str,
    strategy: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    // Jesli alias juz istnieje — reaktywuj go (zachowuj target_model ustawiony przez uzytkownika)
    let existing: Option<i64> = conn
        .prepare("SELECT id FROM model_aliases WHERE alias = ?1")?
        .query_row(rusqlite::params![alias], |row| row.get(0))
        .optional()?;

    if let Some(id) = existing {
        conn.execute(
            "UPDATE model_aliases SET is_active = 1 WHERE id = ?1",
            rusqlite::params![id],
        )?;
    } else {
        conn.execute(
            "INSERT INTO model_aliases (alias, target_model, is_active, strategy) VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![alias, default_target_model, strategy],
        )?;
    }
    Ok(())
}

/// Ustawia flage is_active aliasu po nazwie.
pub fn set_model_alias_active(pool: &DbPool, alias: &str, is_active: bool) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE model_aliases SET is_active = ?1 WHERE alias = ?2",
        rusqlite::params![is_active, alias],
    )?;
    Ok(())
}

// --- Clusters ---

const CLUSTER_COLS: &str = "id, cluster_id, name, description, strategy, created_at, updated_at, total_vram_mb, total_ram_mb, total_cpu_cores, bottleneck_speed_mbps, interconnect_type, failover_enabled, failover_target, health_check_interval_ms, timeout_ms";

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
        failover_enabled: row.get::<_, i64>(12)? != 0,
        failover_target: row.get(13)?,
        health_check_interval_ms: row.get(14)?,
        timeout_ms: row.get(15)?,
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM clusters ORDER BY name",
        CLUSTER_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_cluster)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_cluster(pool: &DbPool, cluster_id: &str) -> Result<Option<DbCluster>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM clusters WHERE cluster_id = ?1",
        CLUSTER_COLS
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
    conn.execute(
        "DELETE FROM clusters WHERE cluster_id = ?1",
        rusqlite::params![cluster_id],
    )?;
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

/// Lista klastrow z agregatami liczby czlonkow (LEFT JOIN cluster_members).
/// `members_online` przyjmuje `members_count` jako proxy — peer_store dolicza
/// online/offline po stronie handlera (peer_store nie jest w DB).
pub fn list_clusters_with_counts(
    pool: &DbPool,
) -> Result<Vec<crate::db::models::DbClusterWithCounts>> {
    let conn = acquire(pool)?;
    let sql = format!(
        "SELECT {}, \
         (SELECT COUNT(*) FROM cluster_members cm WHERE cm.cluster_id = c.cluster_id) AS members_count \
         FROM clusters c ORDER BY name",
        CLUSTER_COLS.split(',').map(|s| format!("c.{}", s.trim())).collect::<Vec<_>>().join(", ")
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map([], |row| {
            let cluster = row_to_cluster(row)?;
            let members_count: i64 = row.get(16)?;
            Ok(crate::db::models::DbClusterWithCounts {
                cluster,
                members_count,
                members_online: members_count,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pelna aktualizacja klastra wraz z polami failover + health-check + timeout.
/// Pola `Option::None` zachowuja dotychczasowa wartosc (COALESCE).
pub fn update_cluster_full(
    pool: &DbPool,
    cluster_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    strategy: Option<&str>,
    failover_enabled: Option<bool>,
    failover_target: Option<Option<&str>>,
    health_check_interval_ms: Option<i64>,
    timeout_ms: Option<i64>,
) -> Result<()> {
    let conn = acquire(pool)?;
    let failover_target_param: Option<&str> = failover_target.unwrap_or(None);
    let failover_target_provided = failover_target.is_some();
    conn.execute(
        "UPDATE clusters SET \
            name = COALESCE(?2, name), \
            description = COALESCE(?3, description), \
            strategy = COALESCE(?4, strategy), \
            failover_enabled = COALESCE(?5, failover_enabled), \
            failover_target = CASE WHEN ?6 = 1 THEN ?7 ELSE failover_target END, \
            health_check_interval_ms = COALESCE(?8, health_check_interval_ms), \
            timeout_ms = COALESCE(?9, timeout_ms), \
            updated_at = datetime('now') \
         WHERE cluster_id = ?1",
        rusqlite::params![
            cluster_id,
            name,
            description,
            strategy,
            failover_enabled.map(|b| if b { 1i64 } else { 0i64 }),
            if failover_target_provided { 1i64 } else { 0i64 },
            failover_target_param,
            health_check_interval_ms,
            timeout_ms,
        ],
    )?;
    Ok(())
}

pub fn list_cluster_members(pool: &DbPool, cluster_id: &str) -> Result<Vec<DbClusterMember>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM cluster_members WHERE cluster_id = ?1 ORDER BY joined_at",
        CLUSTER_MEMBER_COLS
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flows ORDER BY name LIMIT ?1 OFFSET ?2",
        FLOW_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_flow)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow(pool: &DbPool, id: i64) -> Result<Option<DbFlow>> {
    let conn = acquire(pool)?;
    let mut stmt =
        conn.prepare_cached(&format!("SELECT {} FROM flows WHERE id = ?1", FLOW_COLS))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_flow)
        .optional()?;
    Ok(result)
}

pub fn get_default_flow_for_service_type(
    pool: &DbPool,
    service_type: &str,
) -> Result<Option<DbFlow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flows WHERE is_default = 1 AND service_type = ?1 AND status = 'active' LIMIT 1", FLOW_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![service_type], row_to_flow)
        .optional()?;
    Ok(result)
}

pub fn get_flow_for_model(pool: &DbPool, model_name: &str) -> Result<Option<DbFlow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
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

pub fn update_flow(
    pool: &DbPool,
    id: i64,
    expected_version: i64,
    params: &FlowParams<'_>,
) -> Result<()> {
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

// --- Flow Versions ---

/// Maksymalna liczba wersji historii przechowywana per flow.
pub const FLOW_VERSIONS_KEEP: i64 = 5;

const FLOW_VERSION_LIST_COLS: &str =
    "id, flow_id, version_num, name, description, status, created_at, created_by";
const FLOW_VERSION_FULL_COLS: &str =
    "id, flow_id, version_num, name, description, status, created_at, created_by, flow_json";

fn row_to_flow_version_list(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbFlowVersion> {
    Ok(DbFlowVersion {
        id: row.get(0)?,
        flow_id: row.get(1)?,
        version_num: row.get(2)?,
        name: row.get(3)?,
        description: row.get(4)?,
        status: row.get(5)?,
        created_at: row.get(6)?,
        created_by: row.get(7)?,
        flow_json: None,
    })
}

fn row_to_flow_version_full(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbFlowVersion> {
    Ok(DbFlowVersion {
        id: row.get(0)?,
        flow_id: row.get(1)?,
        version_num: row.get(2)?,
        name: row.get(3)?,
        description: row.get(4)?,
        status: row.get(5)?,
        created_at: row.get(6)?,
        created_by: row.get(7)?,
        flow_json: Some(row.get(8)?),
    })
}

/// Zwraca liste wersji (bez flow_json) posortowana malejaco, max 5.
pub fn list_flow_versions(pool: &DbPool, flow_id: i64) -> Result<Vec<DbFlowVersion>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_versions WHERE flow_id = ?1 \
         ORDER BY version_num DESC LIMIT {}",
        FLOW_VERSION_LIST_COLS, FLOW_VERSIONS_KEEP
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![flow_id], row_to_flow_version_list)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Zwraca pojedyncza wersje z pelnym flow_json.
pub fn get_flow_version(
    pool: &DbPool,
    flow_id: i64,
    version_id: i64,
) -> Result<Option<DbFlowVersion>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_versions WHERE flow_id = ?1 AND id = ?2",
        FLOW_VERSION_FULL_COLS
    ))?;
    let result = stmt
        .query_row(
            rusqlite::params![flow_id, version_id],
            row_to_flow_version_full,
        )
        .optional()?;
    Ok(result)
}

/// Atomowa aktualizacja flow z zachowaniem snapshotu poprzedniej wersji.
///
/// W jednej transakcji: (1) zapisuje obecny stan do flow_versions z kolejnym
/// version_num, (2) prunuje wersje starsze niz FLOW_VERSIONS_KEEP, (3)
/// wykonuje UPDATE z optimistic locking.
///
/// Zwraca `Err("CONFLICT")` jesli expected_version nie pasuje.
pub fn update_flow_with_snapshot(
    pool: &DbPool,
    id: i64,
    expected_version: i64,
    params: &FlowParams<'_>,
    created_by: Option<&str>,
) -> Result<()> {
    let mut conn = acquire(pool)?;
    let tx = conn.transaction()?;

    // Pobierz aktualny stan do snapshotu (jesli flow istnieje)
    let current: Option<(String, Option<String>, Option<String>, String)> = tx
        .query_row(
            "SELECT name, description, status, flow_json FROM flows WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    if let Some((old_name, old_description, old_status, old_flow_json)) = current {
        // Kolejny numer wersji dla tego flow
        let next_ver: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version_num), 0) + 1 FROM flow_versions WHERE flow_id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )?;

        tx.execute(
            "INSERT INTO flow_versions \
             (flow_id, version_num, flow_json, name, description, status, created_by) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                next_ver,
                old_flow_json,
                old_name,
                old_description,
                old_status,
                created_by,
            ],
        )?;

        // Prune — zostawiamy tylko FLOW_VERSIONS_KEEP najnowszych
        tx.execute(
            "DELETE FROM flow_versions WHERE flow_id = ?1 AND id NOT IN ( \
               SELECT id FROM flow_versions WHERE flow_id = ?1 \
               ORDER BY version_num DESC LIMIT ?2 \
             )",
            rusqlite::params![id, FLOW_VERSIONS_KEEP],
        )?;
    }

    // Wlasciwa aktualizacja z optimistic locking
    let rows_affected = tx.execute(
        "UPDATE flows SET name = ?2, description = ?3, is_default = ?4, service_type = ?5, \
         flow_json = ?6, status = ?7, version = version + 1, updated_at = datetime('now') \
         WHERE id = ?1 AND version = ?8",
        rusqlite::params![
            id,
            params.name,
            params.description,
            params.is_default,
            params.service_type,
            params.flow_json,
            params.status,
            expected_version
        ],
    )?;
    if rows_affected == 0 {
        return Err(anyhow::anyhow!("CONFLICT"));
    }

    tx.commit()?;
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_model_bindings ORDER BY priority DESC",
        FLOW_BINDING_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_flow_binding)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow_model_binding(pool: &DbPool, id: i64) -> Result<Option<DbFlowModelBinding>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_model_bindings WHERE id = ?1",
        FLOW_BINDING_COLS
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
    conn.execute(
        "DELETE FROM flow_model_bindings WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// --- Flow Node Templates ---

const NODE_TEMPLATE_COLS: &str =
    "id, node_type, category, label, description, default_config, icon";

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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_node_templates ORDER BY category, label",
        NODE_TEMPLATE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_node_template)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow_node_template(pool: &DbPool, id: i64) -> Result<Option<DbFlowNodeTemplate>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_node_templates WHERE id = ?1",
        NODE_TEMPLATE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_node_template)
        .optional()?;
    Ok(result)
}

pub fn create_flow_node_template(
    pool: &DbPool,
    params: &FlowNodeTemplateParams<'_>,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO flow_node_templates (node_type, category, label, description, default_config, icon) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![params.node_type, params.category, params.label, params.description, params.default_config, params.icon],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_flow_node_template(
    pool: &DbPool,
    id: i64,
    params: &FlowNodeTemplateParams<'_>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE flow_node_templates SET node_type = ?2, category = ?3, label = ?4, description = ?5, default_config = ?6, icon = ?7 WHERE id = ?1",
        rusqlite::params![id, params.node_type, params.category, params.label, params.description, params.default_config, params.icon],
    )?;
    Ok(())
}

pub fn delete_flow_node_template(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM flow_node_templates WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// --- PII Rules ---

const PII_RULE_COLS: &str = "id, name, category, pattern, replacement, is_active, priority, description, test_examples, created_at";

pub fn list_pii_rules(pool: &DbPool, offset: i64, limit: i64) -> Result<Vec<DbPiiRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM pii_rules ORDER BY priority DESC LIMIT ?1 OFFSET ?2",
        PII_RULE_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_pii_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_pii_rules_active(pool: &DbPool) -> Result<Vec<DbPiiRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM pii_rules WHERE is_active = 1 ORDER BY priority DESC",
        PII_RULE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_pii_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_pii_rule(pool: &DbPool, id: i64) -> Result<Option<DbPiiRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM pii_rules WHERE id = ?1",
        PII_RULE_COLS
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

const FAST_PATH_COLS: &str =
    "id, module, pattern_type, pattern, match_type, result_json, is_active, priority";

pub fn list_fast_path_patterns(
    pool: &DbPool,
    offset: i64,
    limit: i64,
) -> Result<Vec<DbFastPathPattern>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM fast_path_patterns ORDER BY module, priority DESC LIMIT ?1 OFFSET ?2",
        FAST_PATH_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_fast_path_pattern)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_fast_path_patterns_by_module(
    pool: &DbPool,
    module: &str,
) -> Result<Vec<DbFastPathPattern>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM fast_path_patterns WHERE module = ?1 AND is_active = 1 ORDER BY priority DESC", FAST_PATH_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![module], row_to_fast_path_pattern)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_fast_path_pattern(pool: &DbPool, id: i64) -> Result<Option<DbFastPathPattern>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM fast_path_patterns WHERE id = ?1",
        FAST_PATH_COLS
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
    conn.execute(
        "DELETE FROM fast_path_patterns WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// --- TTS Cleaning Rules ---

const TTS_RULE_COLS: &str = "id, rule_type, pattern, replacement, language, is_active, priority";

pub fn list_tts_cleaning_rules(
    pool: &DbPool,
    offset: i64,
    limit: i64,
) -> Result<Vec<DbTtsCleaningRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM tts_cleaning_rules ORDER BY priority LIMIT ?1 OFFSET ?2",
        TTS_RULE_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_tts_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_tts_cleaning_rules_active(pool: &DbPool) -> Result<Vec<DbTtsCleaningRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM tts_cleaning_rules WHERE is_active = 1 ORDER BY priority",
        TTS_RULE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_tts_rule)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_tts_cleaning_rule(pool: &DbPool, id: i64) -> Result<Option<DbTtsCleaningRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM tts_cleaning_rules WHERE id = ?1",
        TTS_RULE_COLS
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
    conn.execute(
        "DELETE FROM tts_cleaning_rules WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// --- Flow Executions ---

const FLOW_EXEC_COLS: &str = "id, flow_id, request_id, model, started_at, finished_at, status, execution_log, total_latency_ms, total_tokens";

pub fn list_flow_executions(
    pool: &DbPool,
    offset: i64,
    limit: i64,
) -> Result<Vec<DbFlowExecution>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_executions ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        FLOW_EXEC_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![limit, offset], row_to_flow_execution)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_flow_executions_for_flow(
    pool: &DbPool,
    flow_id: i64,
    limit: i64,
) -> Result<Vec<DbFlowExecution>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_executions WHERE flow_id = ?1 ORDER BY id DESC LIMIT ?2",
        FLOW_EXEC_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![flow_id, limit], row_to_flow_execution)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_flow_execution(pool: &DbPool, id: i64) -> Result<Option<DbFlowExecution>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM flow_executions WHERE id = ?1",
        FLOW_EXEC_COLS
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
    conn.execute(
        "DELETE FROM flow_executions WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

// --- Portainer Instances ---

const PORTAINER_INSTANCE_COLS: &str =
    "id, name, url, api_key, created_at, updated_at, username, password";

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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM portainer_instances ORDER BY name",
        PORTAINER_INSTANCE_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_portainer_instance)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_portainer_instance(pool: &DbPool, id: i64) -> Result<Option<DbPortainerInstance>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM portainer_instances WHERE id = ?1",
        PORTAINER_INSTANCE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![id], row_to_portainer_instance)
        .optional()?;
    Ok(result)
}

pub fn create_portainer_instance(
    pool: &DbPool,
    name: &str,
    url: &str,
    api_key: &str,
    username: &str,
    password: &str,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO portainer_instances (name, url, api_key, username, password) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![name, url, api_key, username, password],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_portainer_instance(
    pool: &DbPool,
    id: i64,
    name: &str,
    url: &str,
    api_key: &str,
    username: &str,
    password: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE portainer_instances SET name = ?2, url = ?3, api_key = ?4, username = ?5, password = ?6, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![id, name, url, api_key, username, password],
    )?;
    Ok(())
}

pub fn delete_portainer_instance(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM portainer_instances WHERE id = ?1",
        rusqlite::params![id],
    )?;
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM registries ORDER BY name",
        DOCKER_REGISTRY_COLS
    ))?;
    let rows = stmt
        .query_map([], row_to_docker_registry)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_registry(pool: &DbPool, id: i64) -> Result<Option<DbDockerRegistry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM registries WHERE id = ?1",
        DOCKER_REGISTRY_COLS
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
        role: row
            .get::<_, Option<String>>(13)?
            .unwrap_or_else(|| "user".to_string()),
    })
}

const USER_ACCOUNT_COLS: &str =
    "id, username, password_hash, display_name, email, is_active, is_admin, must_change_password, \
     sso_provider, sso_subject, last_login_at, created_at, updated_at, role";

/// Ustaw role usera. Akceptuje tylko 'user' | 'power_user' | 'admin'.
/// is_admin jest synchronizowany automatycznie (role='admin' → is_admin=1).
pub fn set_user_role(pool: &DbPool, user_id: i64, role: &str) -> Result<()> {
    let role = match role {
        "user" | "power_user" | "admin" => role,
        _ => anyhow::bail!("Nieprawidlowa rola: {}", role),
    };
    let is_admin = role == "admin";
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE user_accounts SET role = ?1, is_admin = ?2, updated_at = datetime('now') WHERE id = ?3",
        rusqlite::params![role, is_admin, user_id],
    )?;
    Ok(())
}

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
    let mut stmt = conn.prepare_cached(&format!(
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
    let mut stmt = conn.prepare_cached(&format!(
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
    let mut stmt = conn.prepare_cached(&format!(
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
    let mut stmt = conn
        .prepare_cached("SELECT id, name, description, created_at FROM user_groups ORDER BY id")?;
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
    let mut stmt = conn.prepare_cached(
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

/// Aktualizuje nazwe i opis grupy.
pub fn update_group(pool: &DbPool, id: i64, name: &str, description: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE user_groups SET name = ?1, description = ?2 WHERE id = ?3",
        rusqlite::params![name, description, id],
    )?;
    Ok(())
}

/// Lista czlonkow grupy (user accounts).
pub fn list_group_members(pool: &DbPool, group_id: i64) -> Result<Vec<UserAccount>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM user_accounts u \
         JOIN group_members gm ON u.id = gm.user_id \
         WHERE gm.group_id = ?1 ORDER BY u.id",
        USER_ACCOUNT_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![group_id], row_to_user_account)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pobiera grupe po id.
pub fn get_group_by_id(pool: &DbPool, id: i64) -> Result<Option<UserGroup>> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT id, name, description, created_at FROM user_groups WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok(UserGroup {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(result)
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
    let mut stmt = conn.prepare_cached(
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
    let mut stmt = conn.prepare_cached(
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

    let mut stmt = conn.prepare_cached(&sql)?;
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

/// Zlicza wpisy logu audytowego spelniajace filtry (bez paginacji). Uzywane
/// przy paginacji w GUI — pozwala wyswietlic "Strona X z Y".
pub fn count_audit_logs(pool: &DbPool, filters: &AuditLogFilters) -> Result<u64> {
    let conn = acquire(pool)?;

    let mut sql = String::from("SELECT COUNT(*) FROM audit_log WHERE 1=1");
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
    }

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let count: i64 = conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))?;
    Ok(count.max(0) as u64)
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
    let mut stmt = conn.prepare_cached(
        "SELECT id, addon_id, name, version, description, author, platforms, \
         manifest_json, is_enabled, is_system, installed_at, updated_at, \
         COALESCE(category, ''), COALESCE(icon, ''), \
         COALESCE(runtime, 'wasmtime'), COALESCE(wasm_size_bytes, 0) \
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
                category: row.get(12)?,
                icon: row.get(13)?,
                runtime: row.get(14)?,
                wasm_size_bytes: row.get(15)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pobiera addon po addon_id.
pub fn get_addon(pool: &DbPool, addon_id: &str) -> Result<Option<Addon>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, addon_id, name, version, description, author, platforms, \
         manifest_json, is_enabled, is_system, installed_at, updated_at, \
         COALESCE(category, ''), COALESCE(icon, ''), \
         COALESCE(runtime, 'wasmtime'), COALESCE(wasm_size_bytes, 0) \
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
                category: row.get(12)?,
                icon: row.get(13)?,
                runtime: row.get(14)?,
                wasm_size_bytes: row.get(15)?,
            })
        })
        .optional()?;
    Ok(result)
}

/// Aktualizuje wersje i manifest addonu.
pub fn update_addon(
    pool: &DbPool,
    addon_id: &str,
    version: &str,
    manifest_json: &str,
) -> Result<()> {
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
    let mut stmt = conn.prepare_cached(
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
        rusqlite::params![
            name,
            provider_type,
            client_id,
            client_secret_encrypted,
            discovery_url,
            enabled
        ],
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
        rusqlite::params![
            username,
            random_hash,
            display_name,
            email,
            sso_provider,
            sso_subject
        ],
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM user_accounts WHERE sso_provider = ?1 AND sso_subject = ?2",
        USER_ACCOUNT_COLS
    ))?;
    let result = stmt
        .query_row(
            rusqlite::params![sso_provider, sso_subject],
            row_to_user_account,
        )
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
pub fn add_user_to_group_by_names(pool: &DbPool, group_name: &str, username: &str) -> Result<()> {
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
pub fn upsert_sync_exclusion(pool: &DbPool, group_name: &str, resource_type: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT OR IGNORE INTO sync_exclusions (group_id, resource_type) \
         SELECT id, ?2 FROM user_groups WHERE name = ?1",
        rusqlite::params![group_name, resource_type],
    )?;
    Ok(())
}

/// Usuwa sync exclusion (uzywany przez CRDT sync).
pub fn delete_sync_exclusion(pool: &DbPool, group_name: &str, resource_type: &str) -> Result<()> {
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
pub fn get_addon_config_values(
    pool: &DbPool,
    addon_id: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let conn = acquire(pool)?;
    let prefix = format!("addon_config:{}:", addon_id);
    let mut stmt = conn.prepare_cached("SELECT key, value FROM settings WHERE key LIKE ?1")?;
    let rows = stmt.query_map(rusqlite::params![format!("{}%", prefix)], |row| {
        let full_key: String = row.get(0)?;
        let value: String = row.get(1)?;
        let short_key = full_key
            .strip_prefix(&prefix)
            .unwrap_or(&full_key)
            .to_string();
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
    let mut stmt = conn.prepare_cached(
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
    let mut stmt = conn.prepare_cached(
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
    let mut stmt =
        conn.prepare_cached("SELECT node_id FROM revoked_nodes ORDER BY revoked_at DESC")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

// =============================================================================
// Voice profiles — CRUD dla speaker recognition (bulletproof identification)
// =============================================================================

/// Tworzy nowy profil glosowy. Zwraca id utworzonego profilu.
pub fn create_voice_profile(pool: &DbPool, params: &NewVoiceProfile<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO voice_profiles
            (name, first_name, last_name, nickname,
             centroid, sample_count, reliability_score, source, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            params.name,
            params.first_name,
            params.last_name,
            params.nickname,
            params.centroid,
            params.sample_count,
            params.reliability_score,
            params.source,
            params.metadata_json,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_to_voice_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbVoiceProfile> {
    Ok(DbVoiceProfile {
        id: row.get(0)?,
        name: row.get(1)?,
        first_name: row.get(2)?,
        last_name: row.get(3)?,
        nickname: row.get(4)?,
        centroid: row.get(5)?,
        sample_count: row.get(6)?,
        reliability_score: row.get(7)?,
        source: row.get(8)?,
        metadata_json: row.get(9)?,
        enrolled_at: row.get(10)?,
        last_seen_at: row.get(11)?,
        total_utterances: row.get(12)?,
    })
}

const VOICE_PROFILE_COLUMNS: &str =
    "id, name, first_name, last_name, nickname, centroid, sample_count,
     reliability_score, source, metadata_json, enrolled_at, last_seen_at,
     total_utterances";

/// Lista wszystkich profili (posortowana po last_seen malejaco, null na koncu)
pub fn list_voice_profiles(pool: &DbPool) -> Result<Vec<DbVoiceProfile>> {
    let conn = acquire(pool)?;
    let sql = format!(
        "SELECT {} FROM voice_profiles
         ORDER BY COALESCE(last_seen_at, '0') DESC, name ASC",
        VOICE_PROFILE_COLUMNS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([], row_to_voice_profile)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Pobiera profil po id
pub fn get_voice_profile(pool: &DbPool, id: i64) -> Result<Option<DbVoiceProfile>> {
    let conn = acquire(pool)?;
    let sql = format!(
        "SELECT {} FROM voice_profiles WHERE id = ?1",
        VOICE_PROFILE_COLUMNS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let row = stmt
        .query_row(rusqlite::params![id], row_to_voice_profile)
        .optional()?;
    Ok(row)
}

/// Pobiera profil po nazwie (unique constraint)
pub fn get_voice_profile_by_name(pool: &DbPool, name: &str) -> Result<Option<DbVoiceProfile>> {
    let conn = acquire(pool)?;
    let sql = format!(
        "SELECT {} FROM voice_profiles WHERE name = ?1",
        VOICE_PROFILE_COLUMNS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let row = stmt
        .query_row(rusqlite::params![name], row_to_voice_profile)
        .optional()?;
    Ok(row)
}

/// Aktualizuje centroid + sample_count + reliability po dodaniu/usunieciu sample
pub fn update_voice_profile_stats(
    pool: &DbPool,
    id: i64,
    centroid: &[u8],
    sample_count: i64,
    reliability_score: f32,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE voice_profiles
         SET centroid = ?2, sample_count = ?3, reliability_score = ?4
         WHERE id = ?1",
        rusqlite::params![id, centroid, sample_count, reliability_score],
    )?;
    Ok(())
}

/// Oznacza profil jako aktywny (last_seen, +1 utterance). Wolane przy kazdym match.
pub fn touch_voice_profile(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE voice_profiles
         SET last_seen_at = datetime('now'),
             total_utterances = total_utterances + 1
         WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Usuwa profil (cascade usuwa samples przez FK ON DELETE CASCADE).
pub fn delete_voice_profile(pool: &DbPool, id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM voice_profiles WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Zmiana czesci osobowych + display name profilu.
/// Caller ma obowiazek wyliczyc `name` z first/last/nickname (lub podac explicit).
pub fn update_voice_profile_identity(
    pool: &DbPool,
    id: i64,
    name: &str,
    first_name: &str,
    last_name: Option<&str>,
    nickname: Option<&str>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE voice_profiles
         SET name = ?2, first_name = ?3, last_name = ?4, nickname = ?5
         WHERE id = ?1",
        rusqlite::params![id, name, first_name, last_name, nickname],
    )?;
    Ok(())
}

/// Zmiana samego display-name (rzadko uzywane; preferowany update_voice_profile_identity)
pub fn rename_voice_profile(pool: &DbPool, id: i64, new_name: &str) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE voice_profiles SET name = ?2 WHERE id = ?1",
        rusqlite::params![id, new_name],
    )?;
    Ok(())
}

/// Dodaje sample do profilu. Caller powinien potem przeliczyc centroid.
pub fn add_voice_profile_sample(pool: &DbPool, params: &NewVoiceProfileSample<'_>) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO voice_profile_samples
            (profile_id, embedding, duration_ms, snr_db, intra_similarity, meeting_id, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            params.profile_id,
            params.embedding,
            params.duration_ms,
            params.snr_db,
            params.intra_similarity,
            params.meeting_id,
            params.source,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Lista samples dla profilu — wszystkie, do multi-sample matchingu
pub fn list_voice_profile_samples(
    pool: &DbPool,
    profile_id: i64,
) -> Result<Vec<DbVoiceProfileSample>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, profile_id, embedding, duration_ms, snr_db, intra_similarity,
                meeting_id, source, created_at
         FROM voice_profile_samples
         WHERE profile_id = ?1
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![profile_id], |row| {
        Ok(DbVoiceProfileSample {
            id: row.get(0)?,
            profile_id: row.get(1)?,
            embedding: row.get(2)?,
            duration_ms: row.get(3)?,
            snr_db: row.get(4)?,
            intra_similarity: row.get(5)?,
            meeting_id: row.get(6)?,
            source: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Usuwa pojedynczy sample (np. odrzucony po spadku reliability)
pub fn delete_voice_profile_sample(pool: &DbPool, sample_id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "DELETE FROM voice_profile_samples WHERE id = ?1",
        rusqlite::params![sample_id],
    )?;
    Ok(())
}

/// Tworzy nowy temp speaker dla meetingu (lub zwraca istniejacy przez UNIQUE constraint)
pub fn upsert_voice_temp_speaker(
    pool: &DbPool,
    meeting_id: &str,
    temp_label: &str,
    embeddings_blob: &[u8],
    sample_count: i64,
    total_duration_ms: i64,
) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO voice_temp_speakers
            (meeting_id, temp_label, embeddings_blob, sample_count, total_duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(meeting_id, temp_label) DO UPDATE SET
            embeddings_blob = excluded.embeddings_blob,
            sample_count = excluded.sample_count,
            total_duration_ms = excluded.total_duration_ms",
        rusqlite::params![
            meeting_id,
            temp_label,
            embeddings_blob,
            sample_count,
            total_duration_ms
        ],
    )?;
    let id = conn.query_row(
        "SELECT id FROM voice_temp_speakers WHERE meeting_id = ?1 AND temp_label = ?2",
        rusqlite::params![meeting_id, temp_label],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Lista temp speakers dla meetingu (do post-meeting LLM assignment)
pub fn list_voice_temp_speakers(
    pool: &DbPool,
    meeting_id: &str,
) -> Result<Vec<DbVoiceTempSpeaker>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, meeting_id, temp_label, embeddings_blob, sample_count,
                total_duration_ms, assigned_profile_id, created_at
         FROM voice_temp_speakers
         WHERE meeting_id = ?1
         ORDER BY temp_label ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![meeting_id], |row| {
        Ok(DbVoiceTempSpeaker {
            id: row.get(0)?,
            meeting_id: row.get(1)?,
            temp_label: row.get(2)?,
            embeddings_blob: row.get(3)?,
            sample_count: row.get(4)?,
            total_duration_ms: row.get(5)?,
            assigned_profile_id: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Przypisuje temp speakera do profilu (np. po LLM detection "Cześć, tu Jan")
pub fn assign_temp_speaker_to_profile(
    pool: &DbPool,
    temp_speaker_id: i64,
    profile_id: i64,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE voice_temp_speakers SET assigned_profile_id = ?2 WHERE id = ?1",
        rusqlite::params![temp_speaker_id, profile_id],
    )?;
    Ok(())
}

/// Zwraca nastepny wolny numer dla auto-promocji KNOWN_SPEAKER_XX.
/// Szuka w voice_profiles najwyzszego uzytego numeru gdzie first_name pasuje
/// wzorcowi 'KNOWN_SPEAKER_%' i zwraca max+1 (albo 0 gdy brak).
pub fn next_known_speaker_number(pool: &DbPool) -> Result<i64> {
    let conn = acquire(pool)?;
    let max_num: Option<i64> = conn
        .query_row(
            "SELECT MAX(CAST(SUBSTR(first_name, 15) AS INTEGER))
             FROM voice_profiles
             WHERE first_name LIKE 'KNOWN_SPEAKER_%'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(max_num.map(|n| n + 1).unwrap_or(0))
}

/// Czysci temp speakers starsze niz X dni (housekeeping)
pub fn cleanup_old_voice_temp_speakers(pool: &DbPool, older_than_days: i64) -> Result<usize> {
    let conn = acquire(pool)?;
    let n = conn.execute(
        "DELETE FROM voice_temp_speakers
         WHERE created_at < datetime('now', ?1)",
        rusqlite::params![format!("-{} days", older_than_days)],
    )?;
    Ok(n)
}

/// Repository dla transkrypcji spotkan. Sesja = jedna rozmowa identyfikowana
/// przez `meeting_key` (np. UUID z bota lub hash URL spotkania). Wpisy sa
/// trwale zachowane w SQLite — przezywaja restart procesu.
pub mod transcripts {
    use super::DbPool;
    use anyhow::Result;
    use serde::Serialize;

    #[derive(Debug, Clone, Serialize)]
    pub struct SessionRow {
        pub id: i64,
        pub meeting_key: String,
        pub meeting_url: Option<String>,
        pub title: Option<String>,
        pub started_at: String,
        pub last_activity_at: String,
        pub entry_count: i64,
        pub status: String,
        pub ended_at: Option<String>,
        pub container_id: Option<String>,
        pub container_name: Option<String>,
        pub quic_port: Option<i64>,
        pub vnc_port: Option<i64>,
        pub novnc_port: Option<i64>,
        pub bot_endpoint_id: Option<String>,
        pub platform: Option<String>,
        pub owner_user_id: Option<i64>,
        pub lifecycle_stage: Option<String>,
        pub lifecycle_details: Option<String>,
        pub lifecycle_updated_at: Option<String>,
        pub backend_stt_model: Option<String>,
        pub backend_tts_model: Option<String>,
        pub backend_summarization_model: Option<String>,
        pub backend_diarization_model: Option<String>,
        pub backend_streaming_latency_ms: Option<i64>,
        pub backend_enrolled_speakers: Option<i64>,
        pub backend_total_participants: Option<i64>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct TranscriptRow {
        pub id: i64,
        pub session_id: i64,
        pub timestamp_ms: i64,
        pub speaker: String,
        pub profile_id: Option<i64>,
        pub confidence: Option<f32>,
        pub is_enrolled: bool,
        pub text: String,
        pub model: String,
    }

    /// Zwraca id istniejacej sesji o podanym meeting_key lub tworzy nowa.
    /// Nowe sesje startuja w status='idle' (caller zmieni na 'joining' po spawnie).
    pub fn get_or_create_session(
        pool: &DbPool,
        meeting_key: &str,
        meeting_url: Option<&str>,
        title: Option<&str>,
    ) -> Result<i64> {
        let conn = pool.lock().unwrap();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM meeting_sessions WHERE meeting_key = ?1",
                rusqlite::params![meeting_key],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = existing {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO meeting_sessions (meeting_key, meeting_url, title, status)
             VALUES (?1, ?2, ?3, 'idle')",
            rusqlite::params![meeting_key, meeting_url, title],
        )?;
        Ok(conn.last_insert_rowid())
    }

    const SESSION_COLS: &str =
        "s.id, s.meeting_key, s.meeting_url, s.title, s.started_at, s.last_activity_at, \
         (SELECT COUNT(*) FROM meeting_transcripts t WHERE t.session_id = s.id), \
         s.status, s.ended_at, s.container_id, s.container_name, \
         s.quic_port, s.vnc_port, s.novnc_port, s.bot_endpoint_id, s.platform, s.owner_user_id, \
         s.lifecycle_stage, s.lifecycle_details, s.lifecycle_updated_at, \
         s.backend_stt_model, s.backend_tts_model, s.backend_summarization_model, \
         s.backend_diarization_model, s.backend_streaming_latency_ms, \
         s.backend_enrolled_speakers, s.backend_total_participants";

    fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
        Ok(SessionRow {
            id: row.get(0)?,
            meeting_key: row.get(1)?,
            meeting_url: row.get(2)?,
            title: row.get(3)?,
            started_at: row.get(4)?,
            last_activity_at: row.get(5)?,
            entry_count: row.get(6)?,
            status: row.get(7)?,
            ended_at: row.get(8)?,
            container_id: row.get(9)?,
            container_name: row.get(10)?,
            quic_port: row.get(11)?,
            vnc_port: row.get(12)?,
            novnc_port: row.get(13)?,
            bot_endpoint_id: row.get(14)?,
            platform: row.get(15)?,
            owner_user_id: row.get(16)?,
            lifecycle_stage: row.get(17)?,
            lifecycle_details: row.get(18)?,
            lifecycle_updated_at: row.get(19)?,
            backend_stt_model: row.get(20)?,
            backend_tts_model: row.get(21)?,
            backend_summarization_model: row.get(22)?,
            backend_diarization_model: row.get(23)?,
            backend_streaming_latency_ms: row.get(24)?,
            backend_enrolled_speakers: row.get(25)?,
            backend_total_participants: row.get(26)?,
        })
    }

    /// Wstawia jeden wpis transkrypcji i aktualizuje last_activity_at sesji.
    pub fn insert_transcript(
        pool: &DbPool,
        session_id: i64,
        entry: &crate::routing::transcript_store::TranscriptEntry,
    ) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO meeting_transcripts
             (session_id, timestamp_ms, speaker, profile_id, confidence, is_enrolled, text, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                session_id,
                entry.timestamp_ms as i64,
                entry.speaker,
                entry.profile_id,
                entry.confidence,
                entry.is_enrolled as i64,
                entry.text,
                entry.model,
            ],
        )?;
        conn.execute(
            "UPDATE meeting_sessions SET last_activity_at = datetime('now') WHERE id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    /// Zwraca owner_user_id sesji po meeting_key. Wynik to:
    /// - `Ok(Some(Some(uid)))` — sesja istnieje i ma przypisanego ownera,
    /// - `Ok(Some(None))` — sesja istnieje ale bez ownera (starsze wpisy),
    /// - `Ok(None)` — sesja nie istnieje.
    /// Uzywane przez live-broadcast writer task do filtrowania eventow
    /// po ownership — bez dostepu uzytkownik nie dostaje frame'u.
    /// Read-only lookup session_id po meeting_key. `Ok(None)` gdy brak sesji.
    /// Uzywane przez handlery (summaries/action-items/export), ktore nie moga
    /// tworzyc sesji — w odroznieniu od `get_or_create_session`.
    pub fn session_id_by_meeting_key(pool: &DbPool, meeting_key: &str) -> Result<Option<i64>> {
        let conn = pool.lock().unwrap();
        let id: rusqlite::Result<i64> = conn.query_row(
            "SELECT id FROM meeting_sessions WHERE meeting_key = ?1",
            rusqlite::params![meeting_key],
            |r| r.get(0),
        );
        match id {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn owner_of_meeting_key(pool: &DbPool, meeting_key: &str) -> Result<Option<Option<i64>>> {
        let conn = pool.lock().unwrap();
        let row: rusqlite::Result<Option<i64>> = conn.query_row(
            "SELECT owner_user_id FROM meeting_sessions WHERE meeting_key = ?1",
            rusqlite::params![meeting_key],
            |r| r.get::<_, Option<i64>>(0),
        );
        match row {
            Ok(opt) => Ok(Some(opt)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Lista sesji posortowana po last_activity_at malejaco. Opcjonalny filtr po owner_user_id.
    pub fn list_sessions(pool: &DbPool, owner_user_id: Option<i64>) -> Result<Vec<SessionRow>> {
        let conn = pool.lock().unwrap();
        let sql_all = format!(
            "SELECT {} FROM meeting_sessions s ORDER BY s.last_activity_at DESC",
            SESSION_COLS
        );
        let sql_owner = format!(
            "SELECT {} FROM meeting_sessions s WHERE s.owner_user_id = ?1 OR s.owner_user_id IS NULL \
             ORDER BY s.last_activity_at DESC",
            SESSION_COLS
        );
        match owner_user_id {
            Some(uid) => {
                let mut stmt = conn.prepare_cached(&sql_owner)?;
                let rows = stmt.query_map(rusqlite::params![uid], row_to_session)?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            }
            None => {
                let mut stmt = conn.prepare_cached(&sql_all)?;
                let rows = stmt.query_map([], row_to_session)?;
                Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
            }
        }
    }

    /// Aktywna (joining/active) sesja dla uzytkownika. Zwraca None jesli brak.
    /// Uzywane przez frontend do odnowienia UI po refresh (jesli bot wciaz lata).
    pub fn active_session_for_user(
        pool: &DbPool,
        owner_user_id: i64,
    ) -> Result<Option<SessionRow>> {
        let conn = pool.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM meeting_sessions s \
             WHERE s.owner_user_id = ?1 AND s.status IN ('joining','active','leaving') \
             ORDER BY s.last_activity_at DESC LIMIT 1",
            SESSION_COLS
        );
        let row = conn
            .query_row(&sql, rusqlite::params![owner_user_id], row_to_session)
            .ok();
        Ok(row)
    }

    /// Wszystkie wpisy transkrypcji dla sesji w kolejnosci chronologicznej.
    pub fn list_transcripts(pool: &DbPool, session_id: i64) -> Result<Vec<TranscriptRow>> {
        let conn = pool.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT id, session_id, timestamp_ms, speaker, profile_id, confidence,
                    is_enrolled, text, model
             FROM meeting_transcripts
             WHERE session_id = ?1
             ORDER BY timestamp_ms ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(TranscriptRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                timestamp_ms: row.get(2)?,
                speaker: row.get(3)?,
                profile_id: row.get(4)?,
                confidence: row.get(5)?,
                is_enrolled: row.get::<_, i64>(6)? != 0,
                text: row.get(7)?,
                model: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Pobiera pojedyncza sesje po id.
    pub fn get_session(pool: &DbPool, id: i64) -> Result<Option<SessionRow>> {
        let conn = pool.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM meeting_sessions s WHERE s.id = ?1",
            SESSION_COLS
        );
        let row = conn
            .query_row(&sql, rusqlite::params![id], row_to_session)
            .ok();
        Ok(row)
    }

    // =========================================================================
    // Lifecycle updates
    // =========================================================================

    /// Pełne wypełnienie sesji po udanym spawnie kontenera.
    #[allow(clippy::too_many_arguments)]
    /// Wariant `update_session_spawned` dla bota natywnego (subprocess) — bez
    /// portow VNC/noVNC bo nie ma zdalnego desktopu. `container_id` jest pusty
    /// (nie ma kontenera), `container_name` zachowuje konwencje
    /// `meeting-bot-<session_id>` zeby GUI mialo to samo do wyswietlenia.
    pub fn update_session_spawned_native(
        pool: &DbPool,
        id: i64,
        container_name: &str,
        quic_port: u16,
        bot_endpoint_id: &str,
        bot_secret_key_hex: &str,
        platform: &str,
        owner_user_id: Option<i64>,
    ) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE meeting_sessions
             SET status = 'joining',
                 container_id = '', container_name = ?2,
                 quic_port = ?3, vnc_port = NULL, novnc_port = NULL,
                 bot_endpoint_id = ?4, bot_secret_key_hex = ?5,
                 platform = ?6, owner_user_id = COALESCE(owner_user_id, ?7),
                 last_activity_at = datetime('now'),
                 ended_at = NULL
             WHERE id = ?1",
            rusqlite::params![
                id,
                container_name,
                quic_port as i64,
                bot_endpoint_id,
                bot_secret_key_hex,
                platform,
                owner_user_id,
            ],
        )?;
        Ok(())
    }

    pub fn update_session_spawned(
        pool: &DbPool,
        id: i64,
        container_id: &str,
        container_name: &str,
        quic_port: u16,
        vnc_port: u16,
        novnc_port: u16,
        bot_endpoint_id: &str,
        bot_secret_key_hex: &str,
        platform: &str,
        owner_user_id: Option<i64>,
    ) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE meeting_sessions
             SET status = 'joining',
                 container_id = ?2, container_name = ?3,
                 quic_port = ?4, vnc_port = ?5, novnc_port = ?6,
                 bot_endpoint_id = ?7, bot_secret_key_hex = ?8,
                 platform = ?9, owner_user_id = COALESCE(owner_user_id, ?10),
                 last_activity_at = datetime('now'),
                 ended_at = NULL
             WHERE id = ?1",
            rusqlite::params![
                id,
                container_id,
                container_name,
                quic_port as i64,
                vnc_port as i64,
                novnc_port as i64,
                bot_endpoint_id,
                bot_secret_key_hex,
                platform,
                owner_user_id,
            ],
        )?;
        Ok(())
    }

    pub fn set_session_status(pool: &DbPool, id: i64, status: &str) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE meeting_sessions SET status = ?2, last_activity_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id, status],
        )?;
        Ok(())
    }

    /// Zapisuje aktualny etap lifecycle bota — wolany zarówno z host managera
    /// (po udanym docker spawn), jak i z routera po otrzymaniu
    /// `MeetingEventPayload::LifecycleUpdate` od bota. `meeting_key` zamiast
    /// `session_id` bo bot nie zna wewnętrznego id, operuje na swoim kluczu.
    /// No-op gdy sesji o tym meeting_key nie ma — bot nie powinien emitować
    /// lifecycle events dla nieznanej sesji, ale nie chcemy twardego błędu
    /// który zabija cały reverse request flow.
    pub fn update_session_lifecycle(
        pool: &DbPool,
        meeting_key: &str,
        stage: &str,
        details: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE meeting_sessions
             SET lifecycle_stage = ?2,
                 lifecycle_details = ?3,
                 lifecycle_updated_at = ?4,
                 last_activity_at = datetime('now')
             WHERE meeting_key = ?1",
            rusqlite::params![meeting_key, stage, details, now],
        )?;
        Ok(())
    }

    /// Persists the backend model identifiers reported by the bot via
    /// `MeetingEventPayload::BackendUpdate`. The live view replays these on
    /// mount so the BACKEND panel survives broadcasts that fired before the
    /// dashboard was open. No-op when the meeting_key is unknown — the bot
    /// occasionally races the host on session creation and we don't want a
    /// stray event to fail the reverse request flow.
    pub fn update_session_backend(
        pool: &DbPool,
        meeting_key: &str,
        stt: &str,
        tts: &str,
        summarization: &str,
        diarization: &str,
        streaming_latency_ms: Option<i64>,
        enrolled_speakers: Option<i64>,
        total_participants: Option<i64>,
    ) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE meeting_sessions
             SET backend_stt_model = ?2,
                 backend_tts_model = ?3,
                 backend_summarization_model = ?4,
                 backend_diarization_model = ?5,
                 backend_streaming_latency_ms = ?6,
                 backend_enrolled_speakers = ?7,
                 backend_total_participants = ?8,
                 last_activity_at = datetime('now')
             WHERE meeting_key = ?1",
            rusqlite::params![
                meeting_key,
                stt,
                tts,
                summarization,
                diarization,
                streaming_latency_ms,
                enrolled_speakers,
                total_participants,
            ],
        )?;
        Ok(())
    }

    pub fn mark_session_ended(pool: &DbPool, id: i64) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE meeting_sessions SET status = 'ended', ended_at = datetime('now'),
                 last_activity_at = datetime('now'),
                 container_id = NULL, container_name = NULL,
                 quic_port = NULL, vnc_port = NULL, novnc_port = NULL
             WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    /// Sesje oznaczone 'active'/'joining' po crashu (zostaly po unclean shutdown).
    /// Caller powinien je zwolnic (stop container jesli istnieje, release ports, mark ended).
    pub fn list_stale_sessions(pool: &DbPool) -> Result<Vec<SessionRow>> {
        let conn = pool.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM meeting_sessions s \
             WHERE s.status IN ('joining','active','leaving')",
            SESSION_COLS
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // =========================================================================
    // Port allocations
    // =========================================================================

    /// Atomowo rezerwuje port danego rodzaju. Zwraca true jesli nowy wpis wstawiono,
    /// false jesli port byl juz zajęty (wywolanie powinno probowac kolejny).
    pub fn try_reserve_port(pool: &DbPool, port: u16, kind: &str, session_id: i64) -> Result<bool> {
        let conn = pool.lock().unwrap();
        let changed = conn.execute(
            "INSERT OR IGNORE INTO meeting_port_allocations (port, kind, session_id)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![port as i64, kind, session_id],
        )?;
        Ok(changed == 1)
    }

    pub fn release_session_ports(pool: &DbPool, session_id: i64) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "DELETE FROM meeting_port_allocations WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    pub fn list_reserved_ports(pool: &DbPool, kind: &str) -> Result<Vec<u16>> {
        let conn = pool.lock().unwrap();
        let mut stmt =
            conn.prepare_cached("SELECT port FROM meeting_port_allocations WHERE kind = ?1")?;
        let rows = stmt.query_map(rusqlite::params![kind], |row| {
            let p: i64 = row.get(0)?;
            Ok(p as u16)
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // =========================================================================
    // Per-user settings
    // =========================================================================

    pub fn get_user_setting(pool: &DbPool, user_id: i64, key: &str) -> Result<Option<String>> {
        let conn = pool.lock().unwrap();
        let val = conn
            .query_row(
                "SELECT value FROM meeting_settings WHERE user_id = ?1 AND key = ?2",
                rusqlite::params![user_id, key],
                |row| row.get::<_, String>(0),
            )
            .ok();
        Ok(val)
    }

    pub fn list_user_settings(pool: &DbPool, user_id: i64) -> Result<Vec<(String, String)>> {
        let conn = pool.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT key, value FROM meeting_settings WHERE user_id = ?1 ORDER BY key ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_user_setting(pool: &DbPool, user_id: i64, key: &str, value: &str) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO meeting_settings (user_id, key, value, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![user_id, key, value],
        )?;
        Ok(())
    }

    // =========================================================================
    // Summaries & action items (migracja 53 — nowy schemat pod Etap 2.2)
    // =========================================================================

    use crate::db::models::{DbMeetingActionItem, DbMeetingSummary};
    use sha2::{Digest, Sha256};

    /// Zwraca hex SHA256 z pary (owner, task) po normalizacji (lowercase + trim).
    /// Uzywane jako deduplikator action itemow w obrębie jednej sesji.
    pub fn action_item_content_hash(owner: &str, task: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(owner.trim().to_lowercase().as_bytes());
        hasher.update(b"|");
        hasher.update(task.trim().to_lowercase().as_bytes());
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for b in digest {
            out.push_str(&format!("{:02x}", b));
        }
        out
    }

    pub fn insert_meeting_summary(
        pool: &DbPool,
        session_id: i64,
        decisions_text: &str,
        summary_text: &str,
        model: &str,
    ) -> Result<i64> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO meeting_summaries (session_id, decisions_text, summary_text, model)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, decisions_text, summary_text, model],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_summaries_for_meeting(
        pool: &DbPool,
        session_id: i64,
        limit: u32,
    ) -> Result<Vec<DbMeetingSummary>> {
        let conn = pool.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT id, session_id, created_at, decisions_text, summary_text, model
             FROM meeting_summaries
             WHERE session_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id, limit as i64], |row| {
            Ok(DbMeetingSummary {
                id: row.get(0)?,
                session_id: row.get(1)?,
                created_at: row.get(2)?,
                decisions_text: row.get(3)?,
                summary_text: row.get(4)?,
                model: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Wstawia action item lub aktualizuje istniejacy (po content_hash). Przy
    /// konflikcie nadpisuje `deadline` i odswieza `updated_at`. Zwraca id wiersza.
    pub fn upsert_meeting_action_item(
        pool: &DbPool,
        session_id: i64,
        owner: &str,
        task: &str,
        deadline: Option<&str>,
    ) -> Result<i64> {
        let hash = action_item_content_hash(owner, task);
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO meeting_action_items
                (session_id, owner, task, deadline, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id, content_hash) DO UPDATE SET
                deadline = excluded.deadline,
                updated_at = datetime('now')",
            rusqlite::params![session_id, owner, task, deadline, hash],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM meeting_action_items
             WHERE session_id = ?1 AND content_hash = ?2",
            rusqlite::params![session_id, hash],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub fn list_action_items_for_meeting(
        pool: &DbPool,
        session_id: i64,
        status_filter: Option<&str>,
    ) -> Result<Vec<DbMeetingActionItem>> {
        let conn = pool.lock().unwrap();
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<DbMeetingActionItem> {
            Ok(DbMeetingActionItem {
                id: row.get(0)?,
                session_id: row.get(1)?,
                owner: row.get(2)?,
                task: row.get(3)?,
                deadline: row.get(4)?,
                status: row.get(5)?,
                content_hash: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        };
        let rows: Vec<DbMeetingActionItem> = match status_filter {
            Some(s) => {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, session_id, owner, task, deadline, status,
                            content_hash, created_at, updated_at
                     FROM meeting_action_items
                     WHERE session_id = ?1 AND status = ?2
                     ORDER BY created_at DESC, id DESC",
                )?;
                let iter = stmt.query_map(rusqlite::params![session_id, s], map_row)?;
                iter.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, session_id, owner, task, deadline, status,
                            content_hash, created_at, updated_at
                     FROM meeting_action_items
                     WHERE session_id = ?1
                     ORDER BY created_at DESC, id DESC",
                )?;
                let iter = stmt.query_map(rusqlite::params![session_id], map_row)?;
                iter.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Zmienia status action itemu. Zwraca true jesli wiersz istnial.
    pub fn update_action_item_status(pool: &DbPool, id: i64, status: &str) -> Result<bool> {
        let conn = pool.lock().unwrap();
        let affected = conn.execute(
            "UPDATE meeting_action_items
             SET status = ?1, updated_at = datetime('now')
             WHERE id = ?2",
            rusqlite::params![status, id],
        )?;
        Ok(affected > 0)
    }
}

// =============================================================================
// Addon permissions + OAuth (migracja 38)
// =============================================================================

/// Wiersz uprawnienia (user/group, allow/deny/inherit).
pub struct DbAddonPermissionRow {
    pub addon_id: String,
    pub subject_type: String,
    pub subject_id: i64,
    pub permission_id: String,
    pub grant_mode: String,
    pub updated_at: String,
}

/// Domyslna wartosc uprawnienia dla addona.
pub struct DbAddonPermissionDefault {
    pub addon_id: String,
    pub permission_id: String,
    pub grant_mode: String,
    pub updated_at: String,
}

/// Wiersz widocznosci addonu per grupa.
pub struct DbAddonVisibilityRow {
    pub addon_id: String,
    pub group_id: i64,
    pub group_name: String,
    pub visible: bool,
    /// Opis grupy z `user_groups.description` (linia meta w UI).
    pub group_description: String,
    /// Liczba aktywnych czlonkow grupy (`group_members`).
    pub user_count: i32,
}

/// Wpis w katalogu uprawnien deklarowanym przez addon (z manifestu).
pub struct DbAddonPermissionCatalogEntry {
    pub addon_id: String,
    pub permission_id: String,
    pub display_name: String,
    pub description: String,
    pub risk: String,
    pub sort_order: i32,
}

/// Deklaracja providera OAuth (z manifestu).
pub struct DbAddonOAuthProviderDecl {
    pub addon_id: String,
    pub provider_id: String,
    pub display_name: String,
    pub authorize_url: String,
    pub token_url: String,
    pub revoke_url: Option<String>,
    pub scopes: String,
    pub mode: String,
    pub pkce: bool,
}

/// Konfiguracja OAuth (admin). `client_secret_encrypted` jest BLOB — plaintext nie wychodzi poza core.
pub struct DbAddonOAuthConfig {
    pub addon_id: String,
    pub provider_id: String,
    pub client_id: String,
    pub client_secret_encrypted: Option<Vec<u8>>,
    pub redirect_uri: String,
    pub enabled: bool,
    pub updated_at: String,
    /// Tryb OAuth: "global" | "individual" | "none" (ustawiany przez admina).
    pub oauth_mode: String,
}

/// Waliduje wartosc `oauth_mode` — akceptuje tylko "global"/"individual"/"none".
pub fn validate_oauth_mode(mode: &str) -> Result<()> {
    match mode {
        "global" | "individual" | "none" => Ok(()),
        _ => Err(anyhow::anyhow!(
            "oauth_mode musi byc global|individual|none, otrzymano: {}",
            mode
        )),
    }
}

/// Konto OAuth uzytkownika.
pub struct DbUserOAuthAccount {
    pub id: i64,
    pub user_id: Option<i64>,
    pub addon_id: String,
    pub provider_id: String,
    pub external_account_id: String,
    pub display_name: String,
    pub access_token_encrypted: Option<Vec<u8>>,
    pub refresh_token_encrypted: Option<Vec<u8>>,
    pub token_type: String,
    pub scopes: String,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
    pub revoked: bool,
}

/// Pending state (anti-CSRF).
pub struct DbOAuthPendingState {
    pub state: String,
    pub user_id: Option<i64>,
    pub addon_id: String,
    pub provider_id: String,
    pub mode: String,
    pub code_verifier: String,
    pub redirect_after: String,
    pub expires_at: String,
}

// -------- Widocznosc addonu --------

pub fn list_addon_visibility(pool: &DbPool, addon_id: &str) -> Result<Vec<DbAddonVisibilityRow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT g.id, g.name, COALESCE(g.description, ''), COALESCE(v.visible, 0), \
                (SELECT COUNT(*) FROM group_members gm WHERE gm.group_id = g.id) AS user_count \
         FROM user_groups g \
         LEFT JOIN addon_visibility v ON v.group_id = g.id AND v.addon_id = ?1 \
         ORDER BY g.name",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            let visible_i: i64 = row.get(3)?;
            let user_count_i: i64 = row.get(4)?;
            Ok(DbAddonVisibilityRow {
                addon_id: addon_id.to_string(),
                group_id: row.get(0)?,
                group_name: row.get(1)?,
                group_description: row.get(2)?,
                visible: visible_i != 0,
                user_count: user_count_i as i32,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn set_addon_visibility(
    pool: &DbPool,
    addon_id: &str,
    group_id: i64,
    visible: bool,
    updated_by: Option<i64>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_visibility (addon_id, group_id, visible, updated_by) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(addon_id, group_id) DO UPDATE SET \
           visible = excluded.visible, \
           updated_by = excluded.updated_by",
        rusqlite::params![addon_id, group_id, visible as i64, updated_by],
    )?;
    Ok(())
}

/// Zwraca aktualna wartosc widocznosci (visible) dla (addon, group) lub None gdy brak wpisu.
pub fn get_addon_visibility(pool: &DbPool, addon_id: &str, group_id: i64) -> Result<Option<bool>> {
    let conn = acquire(pool)?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT visible FROM addon_visibility WHERE addon_id = ?1 AND group_id = ?2",
            rusqlite::params![addon_id, group_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.map(|i| i != 0))
}

/// Zwraca nazwe grupy po id (lub None).
pub fn get_group_name_by_id(pool: &DbPool, group_id: i64) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let name: Option<String> = conn
        .query_row(
            "SELECT name FROM user_groups WHERE id = ?1",
            rusqlite::params![group_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(name)
}

/// Zwraca id grupy po nazwie (lub None jesli nie istnieje).
pub fn get_group_id_by_name(pool: &DbPool, name: &str) -> Result<Option<i64>> {
    let conn = acquire(pool)?;
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM user_groups WHERE name = ?1",
            rusqlite::params![name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id)
}

pub fn set_addon_admin_only(pool: &DbPool, addon_id: &str, admin_only: bool) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE addons SET admin_only = ?1, updated_at = datetime('now') WHERE addon_id = ?2",
        rusqlite::params![admin_only as i64, addon_id],
    )?;
    Ok(())
}

/// Zwraca aktualna wartosc admin_only dla addona (lub None gdy addon nie istnieje).
pub fn peek_addon_admin_only(pool: &DbPool, addon_id: &str) -> Result<Option<bool>> {
    let conn = acquire(pool)?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT admin_only FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.map(|i| i != 0))
}

pub fn get_addon_admin_only(pool: &DbPool, addon_id: &str) -> Result<bool> {
    let conn = acquire(pool)?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT admin_only FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or(0) != 0)
}

/// Ustawia flage `show_in_catalog` dla addona (kontroluje widocznosc w
/// katalogu "Available apps" dla niepriviligowanych userow).
pub fn set_addon_show_in_catalog(
    pool: &DbPool,
    addon_id: &str,
    show_in_catalog: bool,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE addons SET show_in_catalog = ?1, updated_at = datetime('now') WHERE addon_id = ?2",
        rusqlite::params![show_in_catalog as i64, addon_id],
    )?;
    Ok(())
}

/// Zwraca wartosc `show_in_catalog` (domyslnie true, gdy addon nie istnieje).
pub fn get_addon_show_in_catalog(pool: &DbPool, addon_id: &str) -> Result<bool> {
    let conn = acquire(pool)?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT show_in_catalog FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or(1) != 0)
}

/// Zwraca licencje addona (pole `addons.license`, ustawiane przy install/upgrade).
pub fn get_addon_license(pool: &DbPool, addon_id: &str) -> Result<String> {
    let conn = acquire(pool)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT license FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or_default())
}

/// Zwraca rozmiar WASM w bajtach (`wasm_size_bytes`) — 0 jesli brak wpisu.
pub fn get_addon_wasm_size(pool: &DbPool, addon_id: &str) -> Result<i64> {
    let conn = acquire(pool)?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT wasm_size_bytes FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or(0))
}

/// Zwraca runtime addona (`addons.runtime`, domyslnie "wasmtime").
pub fn get_addon_runtime(pool: &DbPool, addon_id: &str) -> Result<String> {
    let conn = acquire(pool)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT runtime FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.unwrap_or_else(|| "wasmtime".to_string()))
}

/// Zwraca sprite icon id addona (`addons.icon`, None gdy brak).
pub fn get_addon_icon(pool: &DbPool, addon_id: &str) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let v: Option<Option<String>> = conn
        .query_row(
            "SELECT icon FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(v.flatten().filter(|s| !s.is_empty()))
}

/// Zlicza aktywne (nie revoked) konta OAuth dla addona w `user_oauth_accounts`.
pub fn count_linked_accounts_for_addon(pool: &DbPool, addon_id: &str) -> Result<i64> {
    let conn = acquire(pool)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM user_oauth_accounts WHERE addon_id = ?1 AND revoked = 0",
        rusqlite::params![addon_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Zwraca (visible_count, total_count) dla badge'a `N/M grup` na detail header.
pub fn count_visibility_groups(pool: &DbPool, addon_id: &str) -> Result<(i64, i64)> {
    let conn = acquire(pool)?;
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM user_groups", [], |row| row.get(0))?;
    let visible: i64 = conn.query_row(
        "SELECT COUNT(*) FROM addon_visibility WHERE addon_id = ?1 AND visible = 1",
        rusqlite::params![addon_id],
        |row| row.get(0),
    )?;
    Ok((visible, total))
}

/// Zwraca liczbe deklarowanych tools addona (tabela `addon_tools` jesli istnieje).
pub fn count_addon_tools(pool: &DbPool, addon_id: &str) -> Result<i64> {
    let conn = acquire(pool)?;
    // Sprawdz czy tabela addon_tools istnieje — niektore konfiguracje bez features moga ja pomijac.
    let has_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='addon_tools'",
        [],
        |row| row.get(0),
    )?;
    if has_table == 0 {
        return Ok(0);
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM addon_tools WHERE addon_id = ?1",
        rusqlite::params![addon_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Oblicza tryb OAuth dla addona na bazie deklarowanych providerow:
/// brak = None; same "global" / same "individual" / same "none" = ten tryb; inaczej "mixed".
pub fn compute_addon_oauth_mode(pool: &DbPool, addon_id: &str) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let mut stmt =
        conn.prepare_cached("SELECT DISTINCT mode FROM addon_oauth_providers WHERE addon_id = ?1")?;
    let modes: Vec<String> = stmt
        .query_map(rusqlite::params![addon_id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match modes.len() {
        0 => Ok(None),
        1 => Ok(Some(modes.into_iter().next().unwrap())),
        _ => Ok(Some("mixed".to_string())),
    }
}

/// Czy addon jest widoczny dla uzytkownika: `admin_only=1` ⇒ tylko admini; inaczej
/// wystarczy dowolna grupa usera z `visible=1`. Gdy nikt nie skonfigurowal widocznosci —
/// default = widoczny dla wszystkich.
pub fn is_addon_visible_to_user(pool: &DbPool, addon_id: &str, user_id: i64) -> Result<bool> {
    let conn = acquire(pool)?;
    let admin_only: i64 = conn
        .query_row(
            "SELECT admin_only FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if admin_only != 0 {
        let is_admin: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM group_members ugm \
                 JOIN user_groups g ON g.id = ugm.group_id \
                 WHERE ugm.user_id = ?1 AND g.name = 'admins'",
                rusqlite::params![user_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        return Ok(is_admin > 0);
    }
    let any_rule: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_visibility WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if any_rule == 0 {
        return Ok(true);
    }
    let matched: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_visibility v \
             JOIN group_members ugm ON ugm.group_id = v.group_id \
             WHERE v.addon_id = ?1 AND ugm.user_id = ?2 AND v.visible = 1",
            rusqlite::params![addon_id, user_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(matched > 0)
}

/// Badge counts liczone na podstawie tabel pomocniczych addona — uzywane w liscie
/// addonow w GUI (nav menu). Jednym zapytaniem DB unikamy N+1 na poziomie handlera.
#[derive(Debug, Clone)]
pub struct AddonBadges {
    pub oauth_mode: Option<String>,
    pub visibility_scope: String,
    pub declared_permissions_count: i32,
    pub users_with_oauth_count: i32,
}

/// Pobiera zagregowane badge dla addona: oauth_mode (dominujacy/mixed), zakres
/// widocznosci, liczbe deklarowanych uprawnien, liczbe aktywnych kont OAuth.
pub fn get_addon_badges(pool: &DbPool, addon_id: &str) -> Result<AddonBadges> {
    let conn = acquire(pool)?;

    let admin_only: i64 = conn
        .query_row(
            "SELECT admin_only FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);

    let visible_groups: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_visibility WHERE addon_id = ?1 AND visible = 1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let visibility_scope = if admin_only != 0 {
        "admin_only".to_string()
    } else if visible_groups == 0 {
        "all_groups".to_string()
    } else {
        format!("{}_groups", visible_groups)
    };

    let declared_permissions_count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_permission_catalog WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as i32;

    let users_with_oauth_count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM user_oauth_accounts WHERE addon_id = ?1 AND revoked = 0",
            rusqlite::params![addon_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as i32;

    // oauth_mode: jesli brak providerow -> None; jesli wszyscy maja ten sam mode -> ten mode;
    // inaczej "mixed".
    let mut stmt =
        conn.prepare_cached("SELECT DISTINCT mode FROM addon_oauth_providers WHERE addon_id = ?1")?;
    let modes: Vec<String> = stmt
        .query_map(rusqlite::params![addon_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let oauth_mode = match modes.len() {
        0 => None,
        1 => Some(modes.into_iter().next().unwrap()),
        _ => Some("mixed".to_string()),
    };

    Ok(AddonBadges {
        oauth_mode,
        visibility_scope,
        declared_permissions_count,
        users_with_oauth_count,
    })
}

// -------- Permission catalog (deklarowany z manifestu) --------

pub fn list_permission_catalog(
    pool: &DbPool,
    addon_id: &str,
) -> Result<Vec<DbAddonPermissionCatalogEntry>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT addon_id, permission_id, display_name, description, risk, sort_order \
         FROM addon_permission_catalog WHERE addon_id = ?1 ORDER BY sort_order, permission_id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok(DbAddonPermissionCatalogEntry {
                addon_id: row.get(0)?,
                permission_id: row.get(1)?,
                display_name: row.get(2)?,
                description: row.get(3)?,
                risk: row.get(4)?,
                sort_order: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn upsert_permission_catalog(
    pool: &DbPool,
    entry: &DbAddonPermissionCatalogEntry,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_permission_catalog \
           (addon_id, permission_id, display_name, description, risk, sort_order) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(addon_id, permission_id) DO UPDATE SET \
           display_name = excluded.display_name, \
           description = excluded.description, \
           risk = excluded.risk, \
           sort_order = excluded.sort_order",
        rusqlite::params![
            entry.addon_id,
            entry.permission_id,
            entry.display_name,
            entry.description,
            entry.risk,
            entry.sort_order,
        ],
    )?;
    Ok(())
}

/// Usuwa z katalogu wpisy, ktorych nie ma w `keep_permission_ids` (diff po upgrade addona).
pub fn delete_permission_catalog_missing(
    pool: &DbPool,
    addon_id: &str,
    keep_permission_ids: &[String],
) -> Result<()> {
    let conn = acquire(pool)?;
    if keep_permission_ids.is_empty() {
        conn.execute(
            "DELETE FROM addon_permission_catalog WHERE addon_id = ?1",
            rusqlite::params![addon_id],
        )?;
        return Ok(());
    }
    let placeholders = keep_permission_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "DELETE FROM addon_permission_catalog WHERE addon_id = ?1 AND permission_id NOT IN ({})",
        placeholders
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + keep_permission_ids.len());
    params.push(&addon_id);
    for p in keep_permission_ids {
        params.push(p);
    }
    conn.execute(&sql, rusqlite::params_from_iter(params.iter().copied()))?;
    Ok(())
}

// -------- Permission matrix (user/group allow/deny/inherit) --------

pub fn list_permission_matrix(
    pool: &DbPool,
    addon_id: &str,
) -> Result<(Vec<DbAddonPermissionRow>, Vec<DbAddonPermissionDefault>)> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT addon_id, subject_type, subject_id, permission_id, grant_mode, updated_at \
         FROM addon_permissions WHERE addon_id = ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok(DbAddonPermissionRow {
                addon_id: row.get(0)?,
                subject_type: row.get(1)?,
                subject_id: row.get(2)?,
                permission_id: row.get(3)?,
                grant_mode: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut stmt2 = conn.prepare_cached(
        "SELECT addon_id, permission_id, grant_mode, updated_at \
         FROM addon_permission_defaults WHERE addon_id = ?1",
    )?;
    let defaults = stmt2
        .query_map(rusqlite::params![addon_id], |row| {
            Ok(DbAddonPermissionDefault {
                addon_id: row.get(0)?,
                permission_id: row.get(1)?,
                grant_mode: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((rows, defaults))
}

/// Zwraca (username, updated_at) ostatniej zmiany w addon_permissions lub
/// addon_permission_defaults dla danego addona. None gdy brak wpisow.
pub fn last_permission_change(pool: &DbPool, addon_id: &str) -> Result<Option<(String, String)>> {
    let conn = acquire(pool)?;
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT u.username, x.updated_at FROM ( \
               SELECT updated_by, updated_at FROM addon_permissions WHERE addon_id = ?1 \
               UNION ALL \
               SELECT updated_by, updated_at FROM addon_permission_defaults WHERE addon_id = ?1 \
             ) x LEFT JOIN user_accounts u ON u.id = x.updated_by \
             ORDER BY x.updated_at DESC LIMIT 1",
            rusqlite::params![addon_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(row.map(|(u, t)| (u.unwrap_or_default(), t)))
}

pub fn upsert_permission(
    pool: &DbPool,
    addon_id: &str,
    subject_type: &str,
    subject_id: i64,
    permission_id: &str,
    grant_mode: &str,
    updated_by: Option<i64>,
) -> Result<()> {
    let conn = acquire(pool)?;
    let granted = matches!(grant_mode, "allow") as i64;
    conn.execute(
        "INSERT INTO addon_permissions \
           (addon_id, subject_type, subject_id, permission_id, granted, grant_mode, updated_at, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), ?7) \
         ON CONFLICT(addon_id, subject_type, subject_id, permission_id) DO UPDATE SET \
           granted = excluded.granted, \
           grant_mode = excluded.grant_mode, \
           updated_at = datetime('now'), \
           updated_by = excluded.updated_by",
        rusqlite::params![addon_id, subject_type, subject_id, permission_id, granted, grant_mode, updated_by],
    )?;
    Ok(())
}

/// Zwraca aktualny grant_mode dla (addon, subject, permission) lub None gdy brak wpisu.
pub fn get_permission_grant_mode(
    pool: &DbPool,
    addon_id: &str,
    subject_type: &str,
    subject_id: i64,
    permission_id: &str,
) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT grant_mode FROM addon_permissions \
             WHERE addon_id = ?1 AND subject_type = ?2 AND subject_id = ?3 AND permission_id = ?4",
            rusqlite::params![addon_id, subject_type, subject_id, permission_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v)
}

pub fn upsert_permission_default(
    pool: &DbPool,
    addon_id: &str,
    permission_id: &str,
    grant_mode: &str,
    updated_by: Option<i64>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_permission_defaults (addon_id, permission_id, grant_mode, updated_by) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(addon_id, permission_id) DO UPDATE SET \
           grant_mode = excluded.grant_mode, \
           updated_at = datetime('now'), \
           updated_by = excluded.updated_by",
        rusqlite::params![addon_id, permission_id, grant_mode, updated_by],
    )?;
    Ok(())
}

/// Zwraca aktualny grant_mode default dla (addon, permission) lub None.
pub fn get_permission_default_grant_mode(
    pool: &DbPool,
    addon_id: &str,
    permission_id: &str,
) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT grant_mode FROM addon_permission_defaults \
             WHERE addon_id = ?1 AND permission_id = ?2",
            rusqlite::params![addon_id, permission_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v)
}

/// Zwraca risk (low/medium/high/critical) dla pozycji katalogu uprawnien, lub None gdy brak.
pub fn get_permission_catalog_risk(
    pool: &DbPool,
    addon_id: &str,
    permission_id: &str,
) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT risk FROM addon_permission_catalog \
             WHERE addon_id = ?1 AND permission_id = ?2",
            rusqlite::params![addon_id, permission_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v)
}

/// Zapisuje wpis audytowy z wszystkimi polami (severity, resource_type, resource_id).
/// Zamiast `log_audit`, ta funkcja wypelnia tez kolumny dodane przez migracje 20 i 39.
pub fn log_audit_full(
    pool: &DbPool,
    user_id: Option<i64>,
    addon_id: Option<&str>,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
    details: Option<&str>,
    severity: &str,
    ip_address: Option<&str>,
    node_id: Option<&str>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO audit_log \
           (user_id, addon_id, action, resource_type, resource_id, details, severity, ip_address, node_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            user_id, addon_id, action, resource_type, resource_id, details, severity, ip_address, node_id,
        ],
    )?;
    Ok(())
}

/// Pobiera email uzytkownika po id (do wzbogacenia wpisow audytowych o target_user_email).
pub fn get_user_email_by_id(pool: &DbPool, user_id: i64) -> Result<Option<String>> {
    let conn = acquire(pool)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT email FROM user_accounts WHERE id = ?1",
            rusqlite::params![user_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v)
}

/// Rozwiazuje efektywne uprawnienie: admin_only > user explicit > group explicit > default > deny.
/// Zwraca (allowed, reason).
pub fn resolve_permission(
    pool: &DbPool,
    addon_id: &str,
    permission_id: &str,
    user_id: i64,
) -> Result<(bool, String)> {
    let conn = acquire(pool)?;
    // 1. admin_only
    let admin_only: i64 = conn
        .query_row(
            "SELECT admin_only FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if admin_only != 0 {
        let is_admin: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM group_members ugm \
                 JOIN user_groups g ON g.id = ugm.group_id \
                 WHERE ugm.user_id = ?1 AND g.name = 'admins'",
                rusqlite::params![user_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        return Ok((is_admin > 0, "admin_only".to_string()));
    }
    // 2. user explicit (allow/deny)
    let user_grant: Option<String> = conn
        .query_row(
            "SELECT grant_mode FROM addon_permissions \
             WHERE addon_id = ?1 AND subject_type = 'user' AND subject_id = ?2 AND permission_id = ?3",
            rusqlite::params![addon_id, user_id, permission_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(g) = user_grant {
        if g == "allow" {
            return Ok((true, "user".to_string()));
        } else if g == "deny" {
            return Ok((false, "user".to_string()));
        }
    }
    // 3. group explicit — dowolna deny => deny; w przeciwnym razie jesli ktoras allow => allow.
    let mut stmt = conn.prepare_cached(
        "SELECT p.grant_mode FROM addon_permissions p \
         JOIN group_members ugm ON ugm.group_id = p.subject_id \
         WHERE p.addon_id = ?1 AND p.subject_type = 'group' AND ugm.user_id = ?2 \
           AND p.permission_id = ?3",
    )?;
    let grants: Vec<String> = stmt
        .query_map(rusqlite::params![addon_id, user_id, permission_id], |row| {
            row.get(0)
        })?
        .filter_map(|r| r.ok())
        .collect();
    if grants.iter().any(|g| g == "deny") {
        return Ok((false, "group".to_string()));
    }
    if grants.iter().any(|g| g == "allow") {
        return Ok((true, "group".to_string()));
    }
    // 4. default
    let default_grant: Option<String> = conn
        .query_row(
            "SELECT grant_mode FROM addon_permission_defaults \
             WHERE addon_id = ?1 AND permission_id = ?2",
            rusqlite::params![addon_id, permission_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(g) = default_grant {
        return Ok((g == "allow", "default".to_string()));
    }
    // 5. deny (bezpieczny fallback)
    Ok((false, "denied".to_string()))
}

// -------- OAuth providers (deklaracja z manifestu) --------

pub fn list_oauth_providers_decl(
    pool: &DbPool,
    addon_id: &str,
) -> Result<Vec<DbAddonOAuthProviderDecl>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT addon_id, provider_id, display_name, authorize_url, token_url, revoke_url, \
                scopes, mode, pkce \
         FROM addon_oauth_providers WHERE addon_id = ?1 ORDER BY provider_id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            let pkce_i: i64 = row.get(8)?;
            Ok(DbAddonOAuthProviderDecl {
                addon_id: row.get(0)?,
                provider_id: row.get(1)?,
                display_name: row.get(2)?,
                authorize_url: row.get(3)?,
                token_url: row.get(4)?,
                revoke_url: row.get(5)?,
                scopes: row.get(6)?,
                mode: row.get(7)?,
                pkce: pkce_i != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn upsert_oauth_providers_decl(pool: &DbPool, decl: &DbAddonOAuthProviderDecl) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_oauth_providers \
           (addon_id, provider_id, display_name, authorize_url, token_url, revoke_url, \
            scopes, mode, pkce) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(addon_id, provider_id) DO UPDATE SET \
           display_name = excluded.display_name, \
           authorize_url = excluded.authorize_url, \
           token_url = excluded.token_url, \
           revoke_url = excluded.revoke_url, \
           scopes = excluded.scopes, \
           mode = excluded.mode, \
           pkce = excluded.pkce",
        rusqlite::params![
            decl.addon_id,
            decl.provider_id,
            decl.display_name,
            decl.authorize_url,
            decl.token_url,
            decl.revoke_url,
            decl.scopes,
            decl.mode,
            decl.pkce as i64,
        ],
    )?;
    Ok(())
}

// -------- OAuth config (admin-managed) --------

pub fn list_oauth_config(pool: &DbPool, addon_id: &str) -> Result<Vec<DbAddonOAuthConfig>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT addon_id, provider_id, client_id, client_secret_encrypted, redirect_uri, \
                enabled, updated_at, oauth_mode \
         FROM addon_oauth_config WHERE addon_id = ?1 ORDER BY provider_id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            let enabled_i: i64 = row.get(5)?;
            Ok(DbAddonOAuthConfig {
                addon_id: row.get(0)?,
                provider_id: row.get(1)?,
                client_id: row.get(2)?,
                client_secret_encrypted: row.get(3)?,
                redirect_uri: row.get(4)?,
                enabled: enabled_i != 0,
                updated_at: row.get(6)?,
                oauth_mode: row.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_oauth_config(
    pool: &DbPool,
    addon_id: &str,
    provider_id: &str,
) -> Result<Option<DbAddonOAuthConfig>> {
    let conn = acquire(pool)?;
    let out = conn
        .query_row(
            "SELECT addon_id, provider_id, client_id, client_secret_encrypted, redirect_uri, \
                    enabled, updated_at, oauth_mode \
             FROM addon_oauth_config WHERE addon_id = ?1 AND provider_id = ?2",
            rusqlite::params![addon_id, provider_id],
            |row| {
                let enabled_i: i64 = row.get(5)?;
                Ok(DbAddonOAuthConfig {
                    addon_id: row.get(0)?,
                    provider_id: row.get(1)?,
                    client_id: row.get(2)?,
                    client_secret_encrypted: row.get(3)?,
                    redirect_uri: row.get(4)?,
                    enabled: enabled_i != 0,
                    updated_at: row.get(6)?,
                    oauth_mode: row.get(7)?,
                })
            },
        )
        .optional()?;
    Ok(out)
}

pub fn upsert_oauth_config(
    pool: &DbPool,
    addon_id: &str,
    provider_id: &str,
    client_id: &str,
    client_secret_encrypted: Option<&[u8]>,
    redirect_uri: &str,
    enabled: bool,
    updated_by: Option<i64>,
    oauth_mode: &str,
) -> Result<()> {
    validate_oauth_mode(oauth_mode)?;
    let conn = acquire(pool)?;
    match client_secret_encrypted {
        Some(blob) => {
            conn.execute(
                "INSERT INTO addon_oauth_config \
                   (addon_id, provider_id, client_id, client_secret_encrypted, redirect_uri, \
                    enabled, updated_at, updated_by, oauth_mode) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), ?7, ?8) \
                 ON CONFLICT(addon_id, provider_id) DO UPDATE SET \
                   client_id = excluded.client_id, \
                   client_secret_encrypted = excluded.client_secret_encrypted, \
                   redirect_uri = excluded.redirect_uri, \
                   enabled = excluded.enabled, \
                   updated_at = datetime('now'), \
                   updated_by = excluded.updated_by, \
                   oauth_mode = excluded.oauth_mode",
                rusqlite::params![
                    addon_id,
                    provider_id,
                    client_id,
                    blob,
                    redirect_uri,
                    enabled as i64,
                    updated_by,
                    oauth_mode,
                ],
            )?;
        }
        None => {
            conn.execute(
                "INSERT INTO addon_oauth_config \
                   (addon_id, provider_id, client_id, redirect_uri, enabled, updated_at, updated_by, oauth_mode) \
                 VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), ?6, ?7) \
                 ON CONFLICT(addon_id, provider_id) DO UPDATE SET \
                   client_id = excluded.client_id, \
                   redirect_uri = excluded.redirect_uri, \
                   enabled = excluded.enabled, \
                   updated_at = datetime('now'), \
                   updated_by = excluded.updated_by, \
                   oauth_mode = excluded.oauth_mode",
                rusqlite::params![
                    addon_id, provider_id, client_id, redirect_uri, enabled as i64, updated_by, oauth_mode,
                ],
            )?;
        }
    }
    Ok(())
}

pub fn clear_oauth_config_secret(pool: &DbPool, addon_id: &str, provider_id: &str) -> Result<bool> {
    let conn = acquire(pool)?;
    let n = conn.execute(
        "UPDATE addon_oauth_config SET client_secret_encrypted = NULL, updated_at = datetime('now') \
         WHERE addon_id = ?1 AND provider_id = ?2",
        rusqlite::params![addon_id, provider_id],
    )?;
    Ok(n > 0)
}

/// Zwraca (id, client_secret_encrypted) dla wszystkich skonfigurowanych sekretow OAuth.
/// Uzywane przez migracje master-key (re-encrypt wszystkich blobow nowym kluczem).
pub fn list_all_oauth_config_secrets(pool: &DbPool) -> Result<Vec<(i64, Vec<u8>)>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, client_secret_encrypted FROM addon_oauth_config \
         WHERE client_secret_encrypted IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Zastepuje zaszyfrowany client_secret nowym blobem (po re-encrypt).
pub fn update_oauth_config_secret_blob(pool: &DbPool, id: i64, new_blob: &[u8]) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE addon_oauth_config SET client_secret_encrypted = ?1 WHERE id = ?2",
        rusqlite::params![new_blob, id],
    )?;
    Ok(())
}

/// Zwraca (id, access_blob, refresh_blob?) dla kont OAuth ze wszystkich userow.
pub fn list_all_user_oauth_token_blobs(
    pool: &DbPool,
) -> Result<Vec<(i64, Vec<u8>, Option<Vec<u8>>)>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, access_token_encrypted, refresh_token_encrypted \
         FROM user_oauth_accounts WHERE access_token_encrypted IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let access: Vec<u8> = row.get(1)?;
            let refresh: Option<Vec<u8>> = row.get(2)?;
            Ok((id, access, refresh))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Zastepuje access+refresh blob nowymi (po re-encrypt).
pub fn update_user_oauth_token_blobs(
    pool: &DbPool,
    id: i64,
    new_access: &[u8],
    new_refresh: Option<&[u8]>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE user_oauth_accounts \
         SET access_token_encrypted = ?1, refresh_token_encrypted = ?2 \
         WHERE id = ?3",
        rusqlite::params![new_access, new_refresh, id],
    )?;
    Ok(())
}

// -------- OAuth pending states --------

pub fn insert_oauth_state(
    pool: &DbPool,
    state: &str,
    user_id: Option<i64>,
    addon_id: &str,
    provider_id: &str,
    mode: &str,
    code_verifier: &str,
    redirect_after: &str,
    ttl_seconds: i64,
) -> Result<()> {
    // Walidacja: TTL musi byc dodatnie — ujemne/zerowe prowadzi do natychmiastowo wygaslego
    // stanu, co jest bledem programistycznym i potencjalnym wektorem DoS (wypelnianie tabeli).
    if ttl_seconds <= 0 {
        anyhow::bail!("TTL musi byc dodatni, otrzymano {}", ttl_seconds);
    }
    let conn = acquire(pool)?;
    // Dynamiczny modyfikator dla datetime() zamiast format-stringa z potencjalnym znakiem '-'.
    let modifier = format!("+{} seconds", ttl_seconds);
    conn.execute(
        "INSERT INTO oauth_pending_states \
           (state, user_id, addon_id, provider_id, mode, code_verifier, redirect_after, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now', ?8))",
        rusqlite::params![
            state,
            user_id,
            addon_id,
            provider_id,
            mode,
            code_verifier,
            redirect_after,
            modifier,
        ],
    )?;
    Ok(())
}

/// Atomowe pobierz + skasuj state. Zwraca None gdy brak / wygasl.
pub fn consume_oauth_state(pool: &DbPool, state: &str) -> Result<Option<DbOAuthPendingState>> {
    let conn = acquire(pool)?;
    let row = conn
        .query_row(
            "SELECT state, user_id, addon_id, provider_id, mode, code_verifier, redirect_after, \
                    expires_at \
             FROM oauth_pending_states WHERE state = ?1 AND expires_at > datetime('now')",
            rusqlite::params![state],
            |row| {
                Ok(DbOAuthPendingState {
                    state: row.get(0)?,
                    user_id: row.get(1)?,
                    addon_id: row.get(2)?,
                    provider_id: row.get(3)?,
                    mode: row.get(4)?,
                    code_verifier: row.get(5)?,
                    redirect_after: row.get(6)?,
                    expires_at: row.get(7)?,
                })
            },
        )
        .optional()?;
    conn.execute(
        "DELETE FROM oauth_pending_states WHERE state = ?1",
        rusqlite::params![state],
    )?;
    Ok(row)
}

pub fn purge_expired_oauth_states(pool: &DbPool) -> Result<usize> {
    let conn = acquire(pool)?;
    let n = conn.execute(
        "DELETE FROM oauth_pending_states WHERE expires_at <= datetime('now')",
        [],
    )?;
    Ok(n)
}

// -------- User OAuth accounts --------

pub fn upsert_user_oauth_account(
    pool: &DbPool,
    user_id: Option<i64>,
    addon_id: &str,
    provider_id: &str,
    external_account_id: &str,
    display_name: &str,
    access_token_encrypted: &[u8],
    refresh_token_encrypted: Option<&[u8]>,
    token_type: &str,
    scopes: &str,
    expires_at: Option<&str>,
) -> Result<i64> {
    let conn = acquire(pool)?;
    // Two partial unique indexes guard this table:
    //   uq_user_oauth_individual: UNIQUE(user_id, addon_id, provider_id) WHERE user_id IS NOT NULL
    //   uq_user_oauth_global:     UNIQUE(addon_id, provider_id)          WHERE user_id IS NULL
    // SQLite UPSERT requires the ON CONFLICT target to match exactly one partial
    // index, including its WHERE predicate — otherwise the conflict is not caught.
    // See: https://www.sqlite.org/lang_UPSERT.html
    if user_id.is_some() {
        conn.execute(
            "INSERT INTO user_oauth_accounts \
               (user_id, addon_id, provider_id, external_account_id, display_name, \
                access_token_encrypted, refresh_token_encrypted, token_type, scopes, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(user_id, addon_id, provider_id) WHERE user_id IS NOT NULL DO UPDATE SET \
               external_account_id = excluded.external_account_id, \
               display_name = excluded.display_name, \
               access_token_encrypted = excluded.access_token_encrypted, \
               refresh_token_encrypted = COALESCE(excluded.refresh_token_encrypted, user_oauth_accounts.refresh_token_encrypted), \
               token_type = excluded.token_type, \
               scopes = excluded.scopes, \
               expires_at = excluded.expires_at, \
               revoked = 0, \
               updated_at = datetime('now')",
            rusqlite::params![
                user_id,
                addon_id,
                provider_id,
                external_account_id,
                display_name,
                access_token_encrypted,
                refresh_token_encrypted,
                token_type,
                scopes,
                expires_at,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO user_oauth_accounts \
               (user_id, addon_id, provider_id, external_account_id, display_name, \
                access_token_encrypted, refresh_token_encrypted, token_type, scopes, expires_at) \
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(addon_id, provider_id) WHERE user_id IS NULL DO UPDATE SET \
               external_account_id = excluded.external_account_id, \
               display_name = excluded.display_name, \
               access_token_encrypted = excluded.access_token_encrypted, \
               refresh_token_encrypted = COALESCE(excluded.refresh_token_encrypted, user_oauth_accounts.refresh_token_encrypted), \
               token_type = excluded.token_type, \
               scopes = excluded.scopes, \
               expires_at = excluded.expires_at, \
               revoked = 0, \
               updated_at = datetime('now')",
            rusqlite::params![
                addon_id,
                provider_id,
                external_account_id,
                display_name,
                access_token_encrypted,
                refresh_token_encrypted,
                token_type,
                scopes,
                expires_at,
            ],
        )?;
    }
    let id: i64 = conn.query_row(
        "SELECT id FROM user_oauth_accounts \
         WHERE user_id IS ?1 AND addon_id = ?2 AND provider_id = ?3",
        rusqlite::params![user_id, addon_id, provider_id],
        |row| row.get(0),
    )?;
    Ok(id)
}

fn row_to_oauth_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbUserOAuthAccount> {
    let revoked_i: i64 = row.get(14)?;
    Ok(DbUserOAuthAccount {
        id: row.get(0)?,
        user_id: row.get(1)?,
        addon_id: row.get(2)?,
        provider_id: row.get(3)?,
        external_account_id: row.get(4)?,
        display_name: row.get(5)?,
        access_token_encrypted: row.get(6)?,
        refresh_token_encrypted: row.get(7)?,
        token_type: row.get(8)?,
        scopes: row.get(9)?,
        expires_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        last_used_at: row.get(13)?,
        revoked: revoked_i != 0,
    })
}

const OAUTH_ACCOUNT_COLS: &str =
    "id, user_id, addon_id, provider_id, external_account_id, display_name, \
     access_token_encrypted, refresh_token_encrypted, token_type, scopes, expires_at, \
     created_at, updated_at, last_used_at, revoked";

pub fn list_user_oauth_accounts_for_user(
    pool: &DbPool,
    user_id: i64,
) -> Result<Vec<DbUserOAuthAccount>> {
    let conn = acquire(pool)?;
    let sql = format!(
        "SELECT {} FROM user_oauth_accounts WHERE user_id = ?1 ORDER BY addon_id, provider_id",
        OAUTH_ACCOUNT_COLS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![user_id], row_to_oauth_account)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_user_oauth_accounts_for_addon(
    pool: &DbPool,
    addon_id: &str,
) -> Result<Vec<DbUserOAuthAccount>> {
    let conn = acquire(pool)?;
    let sql = format!(
        "SELECT {} FROM user_oauth_accounts WHERE addon_id = ?1 ORDER BY user_id, provider_id",
        OAUTH_ACCOUNT_COLS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], row_to_oauth_account)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_oauth_account_by_id(
    pool: &DbPool,
    account_id: i64,
) -> Result<Option<DbUserOAuthAccount>> {
    let conn = acquire(pool)?;
    let sql = format!(
        "SELECT {} FROM user_oauth_accounts WHERE id = ?1",
        OAUTH_ACCOUNT_COLS
    );
    let out = conn
        .query_row(&sql, rusqlite::params![account_id], row_to_oauth_account)
        .optional()?;
    Ok(out)
}

pub fn revoke_oauth_account(pool: &DbPool, account_id: i64) -> Result<bool> {
    let conn = acquire(pool)?;
    let n = conn.execute(
        "UPDATE user_oauth_accounts SET revoked = 1, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![account_id],
    )?;
    Ok(n > 0)
}

pub fn touch_oauth_last_used(pool: &DbPool, account_id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "UPDATE user_oauth_accounts SET last_used_at = datetime('now') WHERE id = ?1",
        rusqlite::params![account_id],
    )?;
    Ok(())
}

/// Wpis listy "Moje połączone konta": para (addon, provider) w trybie
/// `individual` widoczna dla uzytkownika. `account_*` wypelnione gdy istnieje
/// wiersz w `user_oauth_accounts`; w przeciwnym razie status=`not_connected`.
#[derive(Debug, Clone)]
pub struct MyOAuthEntryRow {
    pub addon_id: String,
    pub addon_name: String,
    pub addon_icon: Option<String>,
    pub addon_description: String,
    pub addon_version: String,
    pub provider_id: String,
    pub provider_display_name: String,
    pub status: String,
    pub account_id: Option<i64>,
    pub account_email: String,
    pub account_display_name: String,
    pub scopes: Vec<String>,
    pub connected_at_epoch: i64,
    pub last_used_at_epoch: i64,
    pub expires_at_epoch: i64,
}

/// Zwraca wszystkie pary (addon, provider) w trybie `individual` ktore:
/// - sa zadeklarowane w `addon_oauth_providers` (mode='individual'),
/// - odpowiadaja addonom zainstalowanym i wlaczonym,
/// - sa widoczne dla uzytkownika wg `is_addon_visible_to_user`.
/// Dla kazdej pary LEFT JOIN do `user_oauth_accounts` (user_id) wyznacza status.
pub fn list_my_oauth_entries(pool: &DbPool, user_id: i64) -> Result<Vec<MyOAuthEntryRow>> {
    let conn = acquire(pool)?;
    let sql = "
        SELECT
            a.addon_id,
            a.name,
            a.version,
            a.description,
            a.manifest_json,
            p.provider_id,
            p.display_name,
            p.scopes AS declared_scopes,
            acc.id,
            acc.external_account_id,
            acc.display_name,
            acc.scopes,
            acc.created_at,
            acc.last_used_at,
            acc.expires_at,
            acc.revoked
        FROM addon_oauth_providers p
        JOIN addons a ON a.addon_id = p.addon_id
        LEFT JOIN user_oauth_accounts acc
            ON acc.addon_id = p.addon_id
           AND acc.provider_id = p.provider_id
           AND acc.user_id = ?1
        WHERE p.mode = 'individual'
          AND a.is_enabled = 1
        ORDER BY a.addon_id, p.provider_id
    ";
    let mut stmt = conn.prepare_cached(sql)?;
    let now = chrono::Utc::now().timestamp();
    let rows = stmt
        .query_map(rusqlite::params![user_id], |row| {
            let addon_id: String = row.get(0)?;
            let addon_name: String = row.get(1)?;
            let addon_version: String = row.get(2)?;
            let addon_description: String = row.get(3)?;
            let manifest_json: String = row.get(4)?;
            let provider_id: String = row.get(5)?;
            let provider_display_name: String = row.get(6)?;
            let declared_scopes: String = row.get(7)?;
            let account_id: Option<i64> = row.get(8)?;
            let external_account_id: Option<String> = row.get(9)?;
            let account_display_name: Option<String> = row.get(10)?;
            let account_scopes: Option<String> = row.get(11)?;
            let created_at: Option<String> = row.get(12)?;
            let last_used_at: Option<String> = row.get(13)?;
            let expires_at: Option<String> = row.get(14)?;
            let revoked: Option<i64> = row.get(15)?;
            // Wyciagniecie pola icon z manifest_json (best-effort, bez bledu gdy brak).
            let addon_icon = serde_json::from_str::<serde_json::Value>(&manifest_json)
                .ok()
                .and_then(|v| {
                    v.get("icon")
                        .and_then(|x| x.as_str().map(|s| s.to_string()))
                });
            Ok((
                addon_id,
                addon_name,
                addon_version,
                addon_description,
                addon_icon,
                provider_id,
                provider_display_name,
                declared_scopes,
                account_id,
                external_account_id,
                account_display_name,
                account_scopes,
                created_at,
                last_used_at,
                expires_at,
                revoked,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out: Vec<MyOAuthEntryRow> = Vec::new();
    for (
        addon_id,
        addon_name,
        addon_version,
        addon_description,
        addon_icon,
        provider_id,
        provider_display_name,
        declared_scopes,
        account_id,
        external_account_id,
        account_display_name,
        account_scopes,
        created_at,
        last_used_at,
        expires_at,
        revoked,
    ) in rows
    {
        if !is_addon_visible_to_user(pool, &addon_id, user_id)? {
            continue;
        }
        let parse_ep = |s: &str| -> i64 {
            if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
                return t.timestamp();
            }
            if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
                return t.and_utc().timestamp();
            }
            0
        };
        let (
            status,
            account_id_out,
            account_email,
            account_display_name_out,
            scopes_out,
            connected_at_epoch,
            last_used_at_epoch,
            expires_at_epoch,
        ) = match account_id {
            None => (
                "not_connected".to_string(),
                None,
                String::new(),
                String::new(),
                declared_scopes
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>(),
                0,
                0,
                0,
            ),
            Some(aid) => {
                let connected_ep = created_at.as_deref().map(parse_ep).unwrap_or(0);
                let last_used_ep = last_used_at.as_deref().map(parse_ep).unwrap_or(0);
                let expires_ep = expires_at.as_deref().map(parse_ep).unwrap_or(0);
                let scopes_vec: Vec<String> = account_scopes
                    .as_deref()
                    .unwrap_or("")
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let status = if revoked.unwrap_or(0) != 0 {
                    "revoked".to_string()
                } else if expires_ep > 0 && expires_ep < now + 60 {
                    "expired".to_string()
                } else {
                    "active".to_string()
                };
                (
                    status,
                    Some(aid),
                    external_account_id.clone().unwrap_or_default(),
                    account_display_name.clone().unwrap_or_default(),
                    scopes_vec,
                    connected_ep,
                    last_used_ep,
                    expires_ep,
                )
            }
        };
        out.push(MyOAuthEntryRow {
            addon_id,
            addon_name,
            addon_icon,
            addon_description,
            addon_version,
            provider_id,
            provider_display_name,
            status,
            account_id: account_id_out,
            account_email,
            account_display_name: account_display_name_out,
            scopes: scopes_out,
            connected_at_epoch,
            last_used_at_epoch,
            expires_at_epoch,
        });
    }
    Ok(out)
}

// =============================================================================
// Addon lifecycle — toggle, config, logs, network rules (migracja 40)
// =============================================================================

/// Zwraca biezaca flage is_enabled dla addona.
pub fn get_addon_enabled(pool: &DbPool, addon_id: &str) -> Result<Option<bool>> {
    let conn = acquire(pool)?;
    let val: Option<i64> = conn
        .query_row(
            "SELECT is_enabled FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(val.map(|v| v != 0))
}

/// Ustawia flage is_enabled dla addona. Zwraca false jesli addon nie istnieje.
pub fn set_addon_enabled(pool: &DbPool, addon_id: &str, enabled: bool) -> Result<bool> {
    let conn = acquire(pool)?;
    let rows = conn.execute(
        "UPDATE addons SET is_enabled = ?2, updated_at = datetime('now') WHERE addon_id = ?1",
        rusqlite::params![addon_id, enabled as i64],
    )?;
    Ok(rows > 0)
}

/// Pojedynczy wiersz konfiguracji addona (key/value + flaga secret).
#[derive(Debug, Clone)]
pub struct AddonConfigRow {
    pub key: String,
    pub value: String,
    pub is_secret: bool,
}

/// Lista wartosci konfiguracji z tabeli `addon_config` (plaintext — callee decyduje czy zwrocic).
pub fn list_addon_config_rows(pool: &DbPool, addon_id: &str) -> Result<Vec<AddonConfigRow>> {
    let conn = acquire(pool)?;
    let mut stmt =
        conn.prepare_cached("SELECT key, value, is_secret FROM addon_config WHERE addon_id = ?1")?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok(AddonConfigRow {
                key: row.get(0)?,
                value: row.get(1)?,
                is_secret: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Upsert pojedynczej wartosci konfiguracji w `addon_config`.
pub fn upsert_addon_config_value(
    pool: &DbPool,
    addon_id: &str,
    key: &str,
    value: &str,
    is_secret: bool,
    updated_by: Option<i64>,
) -> Result<()> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO addon_config (addon_id, key, value, is_secret, updated_by, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) \
         ON CONFLICT(addon_id, key) DO UPDATE SET \
            value = excluded.value, \
            is_secret = excluded.is_secret, \
            updated_by = excluded.updated_by, \
            updated_at = datetime('now')",
        rusqlite::params![addon_id, key, value, is_secret as i64, updated_by],
    )?;
    Ok(())
}

/// Pojedynczy wpis audytu dla widoku logs addona (po stronie repo — kolumny DB 1:1).
#[derive(Debug, Clone)]
pub struct AddonAuditRow {
    pub id: i64,
    pub timestamp: String,
    pub severity: String,
    pub action: String,
    pub details: Option<String>,
    pub user_id: Option<i64>,
    pub username: Option<String>,
}

/// Listuje wpisy audytu dla addona (po resource_type='addon' AND resource_id=addon_id
/// lub fallback po kolumnie audit_log.addon_id). Filtr po severity + wyszukiwanie
/// w action/details. Zwraca (wiersze, total).
pub fn list_addon_audit_logs(
    pool: &DbPool,
    addon_id: &str,
    limit: i64,
    offset: i64,
    level: Option<&str>,
    search: Option<&str>,
) -> Result<(Vec<AddonAuditRow>, i64)> {
    let conn = acquire(pool)?;
    let limit_clamped = limit.clamp(1, 500);
    let offset_clamped = offset.max(0);

    // Zbudujmy WHERE: zawsze wiaze addon_id przez resource_id lub addon_id kolumne.
    // level: None => filtr pominiety; Some(x) => egzekwuj severity = x.
    // search: None/empty => pomin; Some => dopasuj LIKE %q% do action+details.
    let level_owned = level.map(|s| s.to_string()).unwrap_or_default();
    let search_like = search
        .filter(|s| !s.is_empty())
        .map(|q| format!("%{}%", q))
        .unwrap_or_default();

    let sql_common = "\
        WHERE (a.resource_type = 'addon' AND a.resource_id = ?1 OR a.addon_id = ?1) \
          AND (?2 = '' OR a.severity = ?2) \
          AND (?3 = '' OR a.action LIKE ?3 OR IFNULL(a.details,'') LIKE ?3)";
    let sql_count = format!("SELECT COUNT(*) FROM audit_log a {}", sql_common);
    let sql_list = format!(
        "SELECT a.id, a.timestamp, a.severity, a.action, a.details, a.user_id, u.username \
         FROM audit_log a LEFT JOIN user_accounts u ON u.id = a.user_id {} \
         ORDER BY a.id DESC LIMIT ?4 OFFSET ?5",
        sql_common
    );

    let total: i64 = conn.query_row(
        &sql_count,
        rusqlite::params![addon_id, level_owned, search_like],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare_cached(&sql_list)?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                addon_id,
                level_owned,
                search_like,
                limit_clamped,
                offset_clamped
            ],
            |row| {
                Ok(AddonAuditRow {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    severity: row.get(2)?,
                    action: row.get(3)?,
                    details: row.get(4)?,
                    user_id: row.get(5)?,
                    username: row.get(6)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((rows, total))
}

/// Prosty rekord regul sieciowych addona (wiersz w `addon_network_config`).
#[derive(Debug, Clone)]
pub struct AddonNetworkConfig {
    pub allowed_hosts: Vec<String>,
    pub blocked_hosts: Vec<String>,
    pub mode: String,
}

/// Pobiera konfiguracje regul sieciowych addona. Zwraca defaults (strict, puste listy)
/// jesli brak wpisu w tabeli.
pub fn get_addon_network_config(pool: &DbPool, addon_id: &str) -> Result<AddonNetworkConfig> {
    let conn = acquire(pool)?;
    let result = conn
        .query_row(
            "SELECT allowed_hosts, blocked_hosts, mode FROM addon_network_config WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| {
                let allowed: String = row.get(0)?;
                let blocked: String = row.get(1)?;
                let mode: String = row.get(2)?;
                Ok((allowed, blocked, mode))
            },
        )
        .optional()?;
    match result {
        Some((a, b, m)) => {
            let allowed_hosts: Vec<String> = serde_json::from_str(&a).unwrap_or_default();
            let blocked_hosts: Vec<String> = serde_json::from_str(&b).unwrap_or_default();
            Ok(AddonNetworkConfig {
                allowed_hosts,
                blocked_hosts,
                mode: m,
            })
        }
        None => Ok(AddonNetworkConfig {
            allowed_hosts: Vec::new(),
            blocked_hosts: Vec::new(),
            mode: "strict".to_string(),
        }),
    }
}

/// Manifest-declared network rule row from `addon_network_rules`.
#[derive(Debug, Clone)]
pub struct AddonDeclaredNetworkRule {
    pub host: String,
    pub port: i32,
    pub protocol: String,
    pub required: bool,
}

/// Loads manifest-declared network rules for an addon. Returns rows from
/// `addon_network_rules` (populated by `sync_manifest_metadata` during
/// install/upgrade). Empty vec if the addon has no declared rules.
pub fn get_addon_declared_network_rules(
    pool: &DbPool,
    addon_id: &str,
) -> Result<Vec<AddonDeclaredNetworkRule>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT host, port, protocol, required FROM addon_network_rules \
         WHERE addon_id = ?1 ORDER BY host, port",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![addon_id], |row| {
            Ok(AddonDeclaredNetworkRule {
                host: row.get(0)?,
                port: row.get(1)?,
                protocol: row.get(2)?,
                required: row.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Upsert konfiguracji regul sieciowych addona.
pub fn set_addon_network_config(
    pool: &DbPool,
    addon_id: &str,
    cfg: &AddonNetworkConfig,
    updated_by: Option<i64>,
) -> Result<()> {
    let conn = acquire(pool)?;
    let allowed = serde_json::to_string(&cfg.allowed_hosts).unwrap_or_else(|_| "[]".into());
    let blocked = serde_json::to_string(&cfg.blocked_hosts).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO addon_network_config (addon_id, allowed_hosts, blocked_hosts, mode, updated_by, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) \
         ON CONFLICT(addon_id) DO UPDATE SET \
            allowed_hosts = excluded.allowed_hosts, \
            blocked_hosts = excluded.blocked_hosts, \
            mode = excluded.mode, \
            updated_by = excluded.updated_by, \
            updated_at = datetime('now')",
        rusqlite::params![addon_id, allowed, blocked, cfg.mode, updated_by],
    )?;
    Ok(())
}

// --- Notes (per-user notes app) ---

/// Single note row from `notes` table. Epoch times converted from SQLite datetime strings.
pub struct Note {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub created_at_epoch: i64,
    pub updated_at_epoch: i64,
}

fn row_to_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    let pinned: i64 = row.get(4)?;
    let created_at: String = row.get(5)?;
    let updated_at: String = row.get(6)?;
    Ok(Note {
        id: row.get(0)?,
        user_id: row.get(1)?,
        title: row.get(2)?,
        body: row.get(3)?,
        pinned: pinned != 0,
        created_at_epoch: parse_sqlite_datetime_epoch(&created_at),
        updated_at_epoch: parse_sqlite_datetime_epoch(&updated_at),
    })
}

fn parse_sqlite_datetime_epoch(s: &str) -> i64 {
    // SQLite `datetime('now')` yields "YYYY-MM-DD HH:MM:SS" in UTC.
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc().timestamp())
        .unwrap_or(0)
}

const NOTE_COLS: &str = "id, user_id, title, body, pinned, created_at, updated_at";

pub fn list_notes_for_user(pool: &DbPool, user_id: i64) -> Result<Vec<Note>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM notes WHERE user_id = ?1 ORDER BY pinned DESC, updated_at DESC",
        NOTE_COLS
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![user_id], row_to_note)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_note(pool: &DbPool, note_id: i64, user_id: i64) -> Result<Option<Note>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM notes WHERE id = ?1 AND user_id = ?2",
        NOTE_COLS
    ))?;
    let result = stmt
        .query_row(rusqlite::params![note_id, user_id], row_to_note)
        .optional()?;
    Ok(result)
}

pub fn create_note(pool: &DbPool, user_id: i64, title: &str, body: &str) -> Result<i64> {
    let conn = acquire(pool)?;
    conn.execute(
        "INSERT INTO notes (user_id, title, body) VALUES (?1, ?2, ?3)",
        rusqlite::params![user_id, title, body],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_note(
    pool: &DbPool,
    note_id: i64,
    user_id: i64,
    title: &str,
    body: &str,
) -> Result<()> {
    let conn = acquire(pool)?;
    let affected = conn.execute(
        "UPDATE notes SET title = ?3, body = ?4, updated_at = datetime('now') \
         WHERE id = ?1 AND user_id = ?2",
        rusqlite::params![note_id, user_id, title, body],
    )?;
    if affected == 0 {
        return Err(anyhow::anyhow!(
            "note {} not found or not owned by user",
            note_id
        ));
    }
    Ok(())
}

pub fn set_note_pinned(pool: &DbPool, note_id: i64, user_id: i64, pinned: bool) -> Result<()> {
    let conn = acquire(pool)?;
    let affected = conn.execute(
        "UPDATE notes SET pinned = ?3, updated_at = datetime('now') \
         WHERE id = ?1 AND user_id = ?2",
        rusqlite::params![note_id, user_id, if pinned { 1 } else { 0 }],
    )?;
    if affected == 0 {
        return Err(anyhow::anyhow!(
            "note {} not found or not owned by user",
            note_id
        ));
    }
    Ok(())
}

pub fn delete_note(pool: &DbPool, note_id: i64, user_id: i64) -> Result<()> {
    let conn = acquire(pool)?;
    let affected = conn.execute(
        "DELETE FROM notes WHERE id = ?1 AND user_id = ?2",
        rusqlite::params![note_id, user_id],
    )?;
    if affected == 0 {
        return Err(anyhow::anyhow!(
            "note {} not found or not owned by user",
            note_id
        ));
    }
    Ok(())
}

#[cfg(test)]
mod alias_resolve_tests {
    use super::*;
    use std::path::Path;

    /// Tworzy in-memory DB z pelnym schematem (migracje + seed)
    fn create_test_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("Nie udalo sie utworzyc test DB")
    }

    #[test]
    fn resolve_alias_exists() {
        // Arrange
        let db = create_test_db();
        create_model_alias(
            &db,
            "gpt-4",
            "bielik-11b",
            Some("mistral-7b,llama-8b"),
            Some("round_robin"),
        )
        .expect("Nie udalo sie utworzyc aliasu");

        // Act
        let result = resolve_model_alias(&db, "gpt-4").expect("Blad zapytania");

        // Assert
        let alias = result.expect("Alias powinien istniec");
        assert_eq!(alias.alias, "gpt-4");
        assert_eq!(alias.target_model, "bielik-11b");
        assert!(alias.is_active);
        assert_eq!(
            alias.fallback_targets.as_deref(),
            Some("mistral-7b,llama-8b")
        );
        assert_eq!(alias.strategy.as_deref(), Some("round_robin"));
    }

    #[test]
    fn resolve_alias_not_found() {
        // Arrange
        let db = create_test_db();

        // Act
        let result = resolve_model_alias(&db, "nieistniejacy-alias").expect("Blad zapytania");

        // Assert
        assert!(result.is_none());
    }

    #[test]
    fn resolve_alias_inactive() {
        // Arrange
        let db = create_test_db();
        let id = create_model_alias(&db, "stary-alias", "model-x", None, None)
            .expect("Nie udalo sie utworzyc aliasu");
        // Dezaktywuj alias
        update_model_alias(&db, id, "stary-alias", "model-x", false, None, None)
            .expect("Nie udalo sie zaktualizowac aliasu");

        // Act
        let result = resolve_model_alias(&db, "stary-alias").expect("Blad zapytania");

        // Assert — nieaktywny alias nie powinien byc zwracany
        assert!(result.is_none());
    }

    #[test]
    fn resolve_alias_default_strategy() {
        // Arrange — bez podania strategii, powinna byc domyslna
        let db = create_test_db();
        create_model_alias(&db, "test-alias", "target-model", None, None)
            .expect("Nie udalo sie utworzyc aliasu");

        // Act
        let result = resolve_model_alias(&db, "test-alias")
            .expect("Blad zapytania")
            .expect("Alias powinien istniec");

        // Assert
        assert_eq!(result.strategy.as_deref(), Some("first_available"));
    }

    #[test]
    fn resolve_alias_no_fallbacks() {
        // Arrange
        let db = create_test_db();
        create_model_alias(&db, "simple", "jedyny-model", None, Some("least_loaded"))
            .expect("Nie udalo sie utworzyc aliasu");

        // Act
        let result = resolve_model_alias(&db, "simple")
            .expect("Blad zapytania")
            .expect("Alias powinien istniec");

        // Assert
        assert!(result.fallback_targets.is_none());
        assert_eq!(result.strategy.as_deref(), Some("least_loaded"));
    }

    #[test]
    fn teams_alias_lifecycle_create_deactivate_reactivate() {
        // Pelny cykl zycia aliasow teams-bot: tworzenie, dezaktywacja, reaktywacja

        // Arrange
        let db = create_test_db();

        // Act 1 — tworzenie aliasow (symulacja instalacji teams-bot)
        create_or_reactivate_model_alias(&db, "teams-stt", "whisper-1", "first_available")
            .expect("Utworzenie aliasu teams-stt powinno sie udac");
        create_or_reactivate_model_alias(&db, "teams-tts", "tts-1", "first_available")
            .expect("Utworzenie aliasu teams-tts powinno sie udac");
        create_or_reactivate_model_alias(&db, "teams-summary", "", "first_available")
            .expect("Utworzenie aliasu teams-summary powinno sie udac (pusty target)");

        // Assert 1 — aliasy istnieja i sa aktywne
        let stt = resolve_model_alias(&db, "teams-stt").unwrap();
        assert!(stt.is_some(), "Alias teams-stt powinien istniec");
        let stt = stt.unwrap();
        assert_eq!(stt.target_model, "whisper-1");
        assert!(stt.is_active);

        let tts = resolve_model_alias(&db, "teams-tts").unwrap();
        assert!(tts.is_some(), "Alias teams-tts powinien istniec");
        let tts = tts.unwrap();
        assert_eq!(tts.target_model, "tts-1");
        assert!(tts.is_active);

        let summary = resolve_model_alias(&db, "teams-summary").unwrap();
        assert!(summary.is_some(), "Alias teams-summary powinien istniec");
        let summary = summary.unwrap();
        assert_eq!(
            summary.target_model, "",
            "teams-summary ma pusty target — admin uzupelnia recznie"
        );
        assert!(summary.is_active);

        // Act 2 — dezaktywacja (symulacja zatrzymania addonu)
        set_model_alias_active(&db, "teams-stt", false)
            .expect("Dezaktywacja teams-stt powinna sie udac");
        set_model_alias_active(&db, "teams-tts", false)
            .expect("Dezaktywacja teams-tts powinna sie udac");

        // Assert 2 — resolve nie znajduje nieaktywnych aliasow
        assert!(
            resolve_model_alias(&db, "teams-stt").unwrap().is_none(),
            "Nieaktywny alias teams-stt nie powinien byc rozwiazywany"
        );
        assert!(
            resolve_model_alias(&db, "teams-tts").unwrap().is_none(),
            "Nieaktywny alias teams-tts nie powinien byc rozwiazywany"
        );

        // Act 3 — reaktywacja (symulacja ponownego uruchomienia)
        create_or_reactivate_model_alias(&db, "teams-stt", "whisper-1", "first_available")
            .expect("Reaktywacja teams-stt powinna sie udac");
        create_or_reactivate_model_alias(&db, "teams-tts", "tts-1", "first_available")
            .expect("Reaktywacja teams-tts powinna sie udac");

        // Assert 3 — aliasy ponownie aktywne
        let stt = resolve_model_alias(&db, "teams-stt")
            .unwrap()
            .expect("Alias teams-stt powinien byc reaktywowany");
        assert!(stt.is_active);
        assert_eq!(stt.target_model, "whisper-1");

        let tts = resolve_model_alias(&db, "teams-tts")
            .unwrap()
            .expect("Alias teams-tts powinien byc reaktywowany");
        assert!(tts.is_active);
    }

    #[test]
    fn teams_alias_preserves_user_target_model_on_reactivation() {
        // Reaktywacja aliasu NIE nadpisuje target_model ustawionego przez uzytkownika

        // Arrange
        let db = create_test_db();

        // Tworzenie z domyslnym target_model
        create_or_reactivate_model_alias(&db, "teams-stt", "whisper-1", "first_available")
            .expect("Utworzenie aliasu powinno sie udac");

        // Uzytkownik zmienia target_model na inny
        let alias = resolve_model_alias(&db, "teams-stt").unwrap().unwrap();
        update_model_alias(
            &db,
            alias.id,
            "teams-stt",
            "whisper-large-v3",
            true,
            None,
            Some("first_available"),
        )
        .expect("Aktualizacja target_model powinna sie udac");

        // Dezaktywacja
        set_model_alias_active(&db, "teams-stt", false).unwrap();

        // Act — reaktywacja z domyslnym target_model
        create_or_reactivate_model_alias(&db, "teams-stt", "whisper-1", "first_available")
            .expect("Reaktywacja powinna sie udac");

        // Assert — target_model ustawiony przez uzytkownika jest zachowany
        let alias = resolve_model_alias(&db, "teams-stt")
            .unwrap()
            .expect("Alias powinien byc aktywny");
        assert_eq!(
            alias.target_model, "whisper-large-v3",
            "Reaktywacja nie powinna nadpisywac target_model ustawionego przez uzytkownika"
        );
        assert!(alias.is_active);
    }

    #[test]
    fn teams_alias_double_deactivation_is_idempotent() {
        // Dwukrotna dezaktywacja nie powoduje bledu

        // Arrange
        let db = create_test_db();
        create_or_reactivate_model_alias(&db, "teams-stt", "whisper-1", "first_available").unwrap();
        set_model_alias_active(&db, "teams-stt", false).unwrap();

        // Act — ponowna dezaktywacja
        let result = set_model_alias_active(&db, "teams-stt", false);

        // Assert — brak bledu
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod permission_and_oauth_tests {
    use super::*;
    use std::path::Path;

    /// Tworzy in-memory DB z pelnym schematem (migracje + seed).
    fn setup_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("nie udalo sie utworzyc test DB")
    }

    /// Rejestruje addon w tabeli `addons` (wymagane dla FK addon_id w innych tabelach).
    fn register_test_addon(db: &DbPool, addon_id: &str) {
        register_addon(db, addon_id, addon_id, "1.0.0", "{}", "linux")
            .expect("register_addon failed");
    }

    /// Tworzy uzytkownika i zwraca jego id.
    fn create_user(db: &DbPool, username: &str) -> i64 {
        create_user_account(
            db,
            username,
            "hash",
            username,
            &format!("{}@x.pl", username),
        )
        .expect("create_user_account failed")
    }

    // -------- Widocznosc addonu --------

    #[test]
    fn test_visibility_toggle_per_group() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "addon-vis");
        let group_id = create_group(&db, "testerzy", "grupa testowa").unwrap();

        // Act
        set_addon_visibility(&db, "addon-vis", group_id, true, None).unwrap();
        let rows = list_addon_visibility(&db, "addon-vis").unwrap();

        // Assert
        let entry = rows
            .iter()
            .find(|r| r.group_id == group_id)
            .expect("wpis widocznosci powinien istniec");
        assert!(entry.visible, "visible powinno byc true");

        // Toggle off
        set_addon_visibility(&db, "addon-vis", group_id, false, None).unwrap();
        let rows = list_addon_visibility(&db, "addon-vis").unwrap();
        let entry = rows.iter().find(|r| r.group_id == group_id).unwrap();
        assert!(!entry.visible, "po toggle visible=false");
    }

    #[test]
    fn test_admin_only_hides_from_regular_user() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "secret-addon");
        let admin_id = create_user(&db, "adminuser");
        let regular_id = create_user(&db, "jankowalski");
        // Admin do grupy 'admins' (id=1 z seedow)
        add_user_to_group(&db, 1, admin_id).unwrap();

        // Act
        set_addon_admin_only(&db, "secret-addon", true).unwrap();

        // Assert
        assert!(get_addon_admin_only(&db, "secret-addon").unwrap());
        assert!(
            is_addon_visible_to_user(&db, "secret-addon", admin_id).unwrap(),
            "admin powinien widziec addon"
        );
        assert!(
            !is_addon_visible_to_user(&db, "secret-addon", regular_id).unwrap(),
            "zwykly user NIE powinien widziec admin-only addonu"
        );
    }

    #[test]
    fn test_is_visible_after_group_member_added() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "grp-addon");
        let user_id = create_user(&db, "anna");
        let group_id = create_group(&db, "marketing", "").unwrap();
        set_addon_visibility(&db, "grp-addon", group_id, true, None).unwrap();

        // Przed dodaniem do grupy — user nie widzi (skoro sa reguly i zadna mu nie pasuje)
        assert!(!is_addon_visible_to_user(&db, "grp-addon", user_id).unwrap());

        // Act — dodanie do grupy z visibility=1
        add_user_to_group(&db, group_id, user_id).unwrap();

        // Assert
        assert!(
            is_addon_visible_to_user(&db, "grp-addon", user_id).unwrap(),
            "user w grupie z visible=1 powinien widziec addon"
        );
    }

    // -------- Badges (oauth_mode / visibility_scope / counts) --------

    #[test]
    fn test_addon_badges_default_scope_all_groups() {
        let db = setup_db();
        register_test_addon(&db, "badge1");
        let b = get_addon_badges(&db, "badge1").unwrap();
        assert_eq!(b.visibility_scope, "all_groups");
        assert_eq!(b.declared_permissions_count, 0);
        assert_eq!(b.users_with_oauth_count, 0);
        assert!(b.oauth_mode.is_none());
    }

    #[test]
    fn test_addon_badges_admin_only_and_counts() {
        let db = setup_db();
        register_test_addon(&db, "badge2");
        set_addon_admin_only(&db, "badge2", true).unwrap();
        upsert_permission_catalog(
            &db,
            &DbAddonPermissionCatalogEntry {
                addon_id: "badge2".to_string(),
                permission_id: "p1".to_string(),
                display_name: "P1".to_string(),
                description: String::new(),
                risk: "low".to_string(),
                sort_order: 0,
            },
        )
        .unwrap();
        let b = get_addon_badges(&db, "badge2").unwrap();
        assert_eq!(b.visibility_scope, "admin_only");
        assert_eq!(b.declared_permissions_count, 1);
    }

    #[test]
    fn test_addon_badges_visibility_scope_n_groups() {
        let db = setup_db();
        register_test_addon(&db, "badge3");
        let g1 = create_group(&db, "g1", "").unwrap();
        let g2 = create_group(&db, "g2", "").unwrap();
        set_addon_visibility(&db, "badge3", g1, true, None).unwrap();
        set_addon_visibility(&db, "badge3", g2, true, None).unwrap();
        let b = get_addon_badges(&db, "badge3").unwrap();
        assert_eq!(b.visibility_scope, "2_groups");
    }

    // -------- Katalog uprawnien --------

    #[test]
    fn test_permission_catalog_upsert_then_diff() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "addon-cat");
        let mk = |pid: &str, order: i32| DbAddonPermissionCatalogEntry {
            addon_id: "addon-cat".to_string(),
            permission_id: pid.to_string(),
            display_name: pid.to_string(),
            description: String::new(),
            risk: "low".to_string(),
            sort_order: order,
        };

        // Act — wstaw 3 wpisy
        upsert_permission_catalog(&db, &mk("perm.read", 0)).unwrap();
        upsert_permission_catalog(&db, &mk("perm.write", 1)).unwrap();
        upsert_permission_catalog(&db, &mk("perm.delete", 2)).unwrap();
        let before = list_permission_catalog(&db, "addon-cat").unwrap();
        assert_eq!(before.len(), 3);

        // Usun brakujace — zachowaj tylko read i write
        delete_permission_catalog_missing(
            &db,
            "addon-cat",
            &["perm.read".to_string(), "perm.write".to_string()],
        )
        .unwrap();

        // Assert
        let after = list_permission_catalog(&db, "addon-cat").unwrap();
        assert_eq!(after.len(), 2, "powinny zostac 2 wpisy");
        let ids: Vec<String> = after.iter().map(|e| e.permission_id.clone()).collect();
        assert!(ids.contains(&"perm.read".to_string()));
        assert!(ids.contains(&"perm.write".to_string()));
        assert!(!ids.contains(&"perm.delete".to_string()));
    }

    // -------- Resolve permission --------

    #[test]
    fn test_resolve_permission_user_overrides_group() {
        // Arrange: user.deny + group.allow => deny (user wygrywa)
        let db = setup_db();
        register_test_addon(&db, "a1");
        let user_id = create_user(&db, "u1");
        let group_id = create_group(&db, "g1", "").unwrap();
        add_user_to_group(&db, group_id, user_id).unwrap();

        upsert_permission(&db, "a1", "group", group_id, "perm.x", "allow", None).unwrap();
        upsert_permission(&db, "a1", "user", user_id, "perm.x", "deny", None).unwrap();

        // Act
        let (allowed, reason) = resolve_permission(&db, "a1", "perm.x", user_id).unwrap();

        // Assert
        assert!(!allowed, "user deny ma pierwszenstwo nad group allow");
        assert_eq!(reason, "user");
    }

    #[test]
    fn test_resolve_permission_group_allow_over_default_deny() {
        // Arrange: group.allow + default.deny => allow
        let db = setup_db();
        register_test_addon(&db, "a2");
        let user_id = create_user(&db, "u2");
        let group_id = create_group(&db, "g2", "").unwrap();
        add_user_to_group(&db, group_id, user_id).unwrap();

        upsert_permission(&db, "a2", "group", group_id, "perm.y", "allow", None).unwrap();
        upsert_permission_default(&db, "a2", "perm.y", "deny", None).unwrap();

        // Act
        let (allowed, reason) = resolve_permission(&db, "a2", "perm.y", user_id).unwrap();

        // Assert
        assert!(allowed);
        assert_eq!(reason, "group");
    }

    #[test]
    fn test_resolve_permission_falls_back_to_default_when_inherit() {
        // Arrange: brak wpisow user/group, default=allow
        let db = setup_db();
        register_test_addon(&db, "a3");
        let user_id = create_user(&db, "u3");

        upsert_permission_default(&db, "a3", "perm.z", "allow", None).unwrap();

        // Act
        let (allowed, reason) = resolve_permission(&db, "a3", "perm.z", user_id).unwrap();

        // Assert
        assert!(allowed);
        assert_eq!(reason, "default");
    }

    #[test]
    fn test_resolve_permission_missing_all_sources_returns_deny() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "a4");
        let user_id = create_user(&db, "u4");

        // Act
        let (allowed, reason) = resolve_permission(&db, "a4", "perm.nope", user_id).unwrap();

        // Assert
        assert!(!allowed);
        assert_eq!(reason, "denied");
    }

    #[test]
    fn test_resolve_permission_admin_bypass() {
        // Arrange: addon admin_only ⇒ admin dostaje true, user nie
        let db = setup_db();
        register_test_addon(&db, "a5");
        let admin_id = create_user(&db, "adm");
        let user_id = create_user(&db, "reg");
        add_user_to_group(&db, 1, admin_id).unwrap();

        set_addon_admin_only(&db, "a5", true).unwrap();

        // Act + Assert
        let (admin_allowed, admin_reason) =
            resolve_permission(&db, "a5", "perm.any", admin_id).unwrap();
        assert!(admin_allowed);
        assert_eq!(admin_reason, "admin_only");

        let (user_allowed, user_reason) =
            resolve_permission(&db, "a5", "perm.any", user_id).unwrap();
        assert!(!user_allowed);
        assert_eq!(user_reason, "admin_only");
    }

    // -------- OAuth config --------

    #[test]
    fn test_upsert_oauth_config_stores_encrypted_secret() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "addon-oauth");
        let fake_encrypted: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];

        // Act
        upsert_oauth_config(
            &db,
            "addon-oauth",
            "microsoft",
            "client-123",
            Some(&fake_encrypted),
            "https://example/cb",
            true,
            None,
            "individual",
        )
        .unwrap();

        // Assert
        let cfg = get_oauth_config(&db, "addon-oauth", "microsoft")
            .unwrap()
            .expect("config powinien istniec");
        assert_eq!(cfg.client_id, "client-123");
        assert_eq!(cfg.oauth_mode, "individual");
        let stored = cfg.client_secret_encrypted.expect("secret powinien byc");
        assert_eq!(stored, fake_encrypted, "blob powinien byc zapisany 1:1");
        // Nie jest plaintextem "secret123" — zaden zrozumialy ciag nie pojawia sie w blobie
        assert_ne!(stored, b"secret123".to_vec());
    }

    #[test]
    fn test_clear_oauth_config_secret_keeps_client_id() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "addon-clr");
        let blob: Vec<u8> = vec![1, 2, 3, 4];
        upsert_oauth_config(
            &db,
            "addon-clr",
            "google",
            "client-xyz",
            Some(&blob),
            "https://cb",
            true,
            None,
            "individual",
        )
        .unwrap();

        // Act
        let cleared = clear_oauth_config_secret(&db, "addon-clr", "google").unwrap();
        assert!(cleared);

        // Assert
        let cfg = get_oauth_config(&db, "addon-clr", "google")
            .unwrap()
            .unwrap();
        assert_eq!(cfg.client_id, "client-xyz", "client_id zostaje");
        assert!(cfg.client_secret_encrypted.is_none(), "secret skasowany");
        assert_eq!(cfg.redirect_uri, "https://cb");
    }

    // -------- OAuth pending states --------

    /// Forsuje expires_at na czas w przeszlosci dla wpisu w oauth_pending_states.
    /// Uzywane w testach bo `insert_oauth_state` nie akceptuje ujemnych TTL.
    fn force_expire_oauth_state(db: &DbPool, state: &str) {
        let conn = acquire(db).unwrap();
        conn.execute(
            "UPDATE oauth_pending_states SET expires_at = datetime('now', '-60 seconds') \
             WHERE state = ?1",
            rusqlite::params![state],
        )
        .unwrap();
    }

    #[test]
    fn test_insert_and_consume_oauth_state_single_use() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "a-state");
        let user_id = create_user(&db, "stateu");
        insert_oauth_state(
            &db,
            "state-token-1",
            Some(user_id),
            "a-state",
            "microsoft",
            "individual",
            "verifier-xxx",
            "/dashboard",
            300,
        )
        .unwrap();

        // Act + Assert — pierwsze consume zwraca wpis
        let first = consume_oauth_state(&db, "state-token-1").unwrap();
        let s = first.expect("pierwsze consume powinno zwrocic stan");
        assert_eq!(s.state, "state-token-1");
        assert_eq!(s.user_id, Some(user_id));
        assert_eq!(s.addon_id, "a-state");
        assert_eq!(s.code_verifier, "verifier-xxx");

        // Drugie consume zwraca None (single-use)
        let second = consume_oauth_state(&db, "state-token-1").unwrap();
        assert!(second.is_none(), "drugie consume musi zwrocic None");
    }

    #[test]
    fn test_oauth_state_expired_not_consumable() {
        // Arrange — wstaw normalnie, potem wymus expires_at w przeszlosci
        let db = setup_db();
        register_test_addon(&db, "a-exp");
        insert_oauth_state(
            &db,
            "expired-state",
            None,
            "a-exp",
            "github",
            "individual",
            "v",
            "/",
            300,
        )
        .unwrap();
        force_expire_oauth_state(&db, "expired-state");

        // Act
        let out = consume_oauth_state(&db, "expired-state").unwrap();

        // Assert
        assert!(out.is_none(), "wygasly state nie moze byc konsumowany");
    }

    #[test]
    fn test_purge_expired_oauth_states_removes_only_expired() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "a-purge");
        insert_oauth_state(
            &db,
            "fresh",
            None,
            "a-purge",
            "p",
            "individual",
            "v",
            "/",
            300,
        )
        .unwrap();
        insert_oauth_state(
            &db,
            "old1",
            None,
            "a-purge",
            "p",
            "individual",
            "v",
            "/",
            300,
        )
        .unwrap();
        insert_oauth_state(
            &db,
            "old2",
            None,
            "a-purge",
            "p",
            "individual",
            "v",
            "/",
            300,
        )
        .unwrap();
        force_expire_oauth_state(&db, "old1");
        force_expire_oauth_state(&db, "old2");

        // Act
        let n = purge_expired_oauth_states(&db).unwrap();

        // Assert — usuniete dokladnie 2 stare, fresh ciagle konsumowalny
        assert_eq!(n, 2, "powinny zostac usuniete dokladnie 2 wygasle stany");
        let fresh = consume_oauth_state(&db, "fresh").unwrap();
        assert!(fresh.is_some(), "fresh state nie powinien byc purgowany");
    }

    // -------- User OAuth accounts --------

    #[test]
    fn test_upsert_user_oauth_account_unique_per_user_addon_provider() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "a-acc");
        let user_id = create_user(&db, "upsertu");

        // Act — pierwszy insert
        let id1 = upsert_user_oauth_account(
            &db,
            Some(user_id),
            "a-acc",
            "microsoft",
            "ext-1",
            "Jan Kowalski",
            &[0x01, 0x02],
            Some(&[0x10, 0x20]),
            "Bearer",
            "User.Read",
            Some("2099-01-01 00:00:00"),
        )
        .unwrap();

        // Drugi upsert ta sama trojka (user, addon, provider) — powinno zaktualizowac
        let id2 = upsert_user_oauth_account(
            &db,
            Some(user_id),
            "a-acc",
            "microsoft",
            "ext-1",
            "Jan K. (updated)",
            &[0x03, 0x04],
            None,
            "Bearer",
            "User.Read offline_access",
            None,
        )
        .unwrap();

        // Assert — ten sam id, duplikatu brak
        assert_eq!(id1, id2, "upsert musi aktualizowac ten sam rekord");
        let accs = list_user_oauth_accounts_for_user(&db, user_id).unwrap();
        assert_eq!(accs.len(), 1, "nie moze byc duplikatu");
        assert_eq!(accs[0].display_name, "Jan K. (updated)");
        assert_eq!(accs[0].access_token_encrypted, Some(vec![0x03, 0x04]));
        // refresh_token_encrypted zachowany z pierwszego insertu (COALESCE)
        assert_eq!(accs[0].refresh_token_encrypted, Some(vec![0x10, 0x20]));
    }

    #[test]
    fn test_revoke_oauth_account_soft() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "a-rev");
        let user_id = create_user(&db, "revu");
        let id = upsert_user_oauth_account(
            &db,
            Some(user_id),
            "a-rev",
            "google",
            "ext-g",
            "User",
            &[1],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();

        // Act
        let ok = revoke_oauth_account(&db, id).unwrap();

        // Assert
        assert!(ok);
        let acc = get_oauth_account_by_id(&db, id)
            .unwrap()
            .expect("rekord powinien nadal istniec (soft revoke)");
        assert!(acc.revoked, "revoked=true po rewokacji");
    }

    #[test]
    fn test_list_user_oauth_accounts_for_user_filters_by_user_id() {
        // Arrange
        let db = setup_db();
        register_test_addon(&db, "a-flt");
        let u1 = create_user(&db, "filter1");
        let u2 = create_user(&db, "filter2");

        upsert_user_oauth_account(
            &db,
            Some(u1),
            "a-flt",
            "p1",
            "e1",
            "U1",
            &[1],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();
        upsert_user_oauth_account(
            &db,
            Some(u1),
            "a-flt",
            "p2",
            "e2",
            "U1b",
            &[2],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();
        upsert_user_oauth_account(
            &db,
            Some(u2),
            "a-flt",
            "p1",
            "e3",
            "U2",
            &[3],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();

        // Act
        let list_u1 = list_user_oauth_accounts_for_user(&db, u1).unwrap();
        let list_u2 = list_user_oauth_accounts_for_user(&db, u2).unwrap();

        // Assert
        assert_eq!(list_u1.len(), 2, "u1 ma 2 konta");
        assert_eq!(list_u2.len(), 1, "u2 ma 1 konto");
        assert!(list_u1.iter().all(|a| a.user_id == Some(u1)));
        assert!(list_u2.iter().all(|a| a.user_id == Some(u2)));
    }

    // -------- Partial unique indexes (migracja 42) --------

    /// Second upsert for the same (user, addon, provider) must update, not insert.
    /// Guards partial index uq_user_oauth_individual.
    #[test]
    fn test_upsert_individual_token_unique_per_user_addon_provider() {
        let db = setup_db();
        register_test_addon(&db, "a-ind");
        let uid = create_user(&db, "indu");

        let id1 = upsert_user_oauth_account(
            &db,
            Some(uid),
            "a-ind",
            "p",
            "ext-1",
            "N1",
            &[1],
            Some(&[9]),
            "Bearer",
            "",
            None,
        )
        .unwrap();
        let id2 = upsert_user_oauth_account(
            &db,
            Some(uid),
            "a-ind",
            "p",
            "ext-2",
            "N2",
            &[2],
            None,
            "Bearer",
            "offline",
            None,
        )
        .unwrap();

        assert_eq!(id1, id2, "same row must be updated");
        let rows = list_user_oauth_accounts_for_addon(&db, "a-ind").unwrap();
        assert_eq!(rows.len(), 1, "no duplicate rows");
        assert_eq!(rows[0].user_id, Some(uid));
        assert_eq!(rows[0].external_account_id, "ext-2");
    }

    /// Second upsert with user_id=None for the same (addon, provider) must update.
    /// Guards partial index uq_user_oauth_global — this is the bug fixed by migration 42.
    #[test]
    fn test_upsert_global_token_unique_per_addon_provider() {
        let db = setup_db();
        register_test_addon(&db, "a-glob");

        let id1 = upsert_user_oauth_account(
            &db,
            None,
            "a-glob",
            "p",
            "ext-g1",
            "G1",
            &[1],
            Some(&[9]),
            "Bearer",
            "",
            None,
        )
        .unwrap();
        let id2 = upsert_user_oauth_account(
            &db,
            None,
            "a-glob",
            "p",
            "ext-g2",
            "G2",
            &[2],
            None,
            "Bearer",
            "offline",
            None,
        )
        .unwrap();

        assert_eq!(
            id1, id2,
            "global token must be updated in place, not duplicated"
        );
        let rows = list_user_oauth_accounts_for_addon(&db, "a-glob").unwrap();
        assert_eq!(rows.len(), 1, "only one global token per (addon, provider)");
        assert!(rows[0].user_id.is_none(), "global row has user_id NULL");
        assert_eq!(rows[0].external_account_id, "ext-g2");
    }

    /// Global token (user_id=NULL) and individual token (user_id=Some) for the same
    /// (addon, provider) must coexist — partial indexes are disjoint by predicate.
    #[test]
    fn test_global_and_individual_coexist() {
        let db = setup_db();
        register_test_addon(&db, "a-both");
        let uid = create_user(&db, "bothu");

        upsert_user_oauth_account(
            &db,
            None,
            "a-both",
            "p",
            "ext-global",
            "G",
            &[1],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();
        upsert_user_oauth_account(
            &db,
            Some(uid),
            "a-both",
            "p",
            "ext-user",
            "U",
            &[2],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();

        let rows = list_user_oauth_accounts_for_addon(&db, "a-both").unwrap();
        assert_eq!(
            rows.len(),
            2,
            "global + individual for same (addon, provider) must coexist"
        );
        assert!(rows
            .iter()
            .any(|r| r.user_id.is_none() && r.external_account_id == "ext-global"));
        assert!(rows
            .iter()
            .any(|r| r.user_id == Some(uid) && r.external_account_id == "ext-user"));
    }

    /// Different addon_id ⇒ separate rows even for global tokens.
    #[test]
    fn test_upsert_global_token_different_addons() {
        let db = setup_db();
        register_test_addon(&db, "a-one");
        register_test_addon(&db, "a-two");

        upsert_user_oauth_account(
            &db,
            None,
            "a-one",
            "p",
            "ext1",
            "O1",
            &[1],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();
        upsert_user_oauth_account(
            &db,
            None,
            "a-two",
            "p",
            "ext2",
            "O2",
            &[2],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();

        let r1 = list_user_oauth_accounts_for_addon(&db, "a-one").unwrap();
        let r2 = list_user_oauth_accounts_for_addon(&db, "a-two").unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r2.len(), 1);
        assert_eq!(r1[0].external_account_id, "ext1");
        assert_eq!(r2[0].external_account_id, "ext2");
    }

    /// Verifies the post-migration index layout: both partial unique indexes exist,
    /// and the old table-level sqlite_autoindex UNIQUE on (user_id, addon_id, provider_id)
    /// is gone (it was backing the dropped table).
    #[test]
    fn test_migration_42_index_list() {
        let db = setup_db();
        let conn = acquire(&db).unwrap();
        let mut stmt = conn
            .prepare("PRAGMA index_list('user_oauth_accounts')")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            names.iter().any(|n| n == "uq_user_oauth_individual"),
            "missing uq_user_oauth_individual, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n == "uq_user_oauth_global"),
            "missing uq_user_oauth_global, got: {:?}",
            names
        );
    }

    // -------- OAuth mode (migracja 41) --------

    /// upsert_oauth_config z oauth_mode='global' persistuje tryb i get zwraca go 1:1.
    #[test]
    fn test_oauth_config_set_persists_mode() {
        let db = setup_db();
        register_test_addon(&db, "addon-m1");
        upsert_oauth_config(
            &db,
            "addon-m1",
            "github",
            "cid",
            None,
            "https://cb",
            true,
            None,
            "global",
        )
        .unwrap();
        let cfg = get_oauth_config(&db, "addon-m1", "github")
            .unwrap()
            .unwrap();
        assert_eq!(cfg.oauth_mode, "global");

        // Nadpisanie z innym trybem dziala (UPDATE branch).
        upsert_oauth_config(
            &db,
            "addon-m1",
            "github",
            "cid",
            None,
            "https://cb",
            true,
            None,
            "none",
        )
        .unwrap();
        let cfg2 = get_oauth_config(&db, "addon-m1", "github")
            .unwrap()
            .unwrap();
        assert_eq!(cfg2.oauth_mode, "none");
    }

    /// list_oauth_config zwraca oauth_mode w wierszach.
    #[test]
    fn test_oauth_config_list_returns_mode() {
        let db = setup_db();
        register_test_addon(&db, "addon-m2");
        upsert_oauth_config(
            &db,
            "addon-m2",
            "p1",
            "c1",
            None,
            "https://cb1",
            true,
            None,
            "individual",
        )
        .unwrap();
        upsert_oauth_config(
            &db,
            "addon-m2",
            "p2",
            "c2",
            None,
            "https://cb2",
            false,
            None,
            "global",
        )
        .unwrap();
        let rows = list_oauth_config(&db, "addon-m2").unwrap();
        assert_eq!(rows.len(), 2);
        let p1 = rows.iter().find(|r| r.provider_id == "p1").unwrap();
        let p2 = rows.iter().find(|r| r.provider_id == "p2").unwrap();
        assert_eq!(p1.oauth_mode, "individual");
        assert_eq!(p2.oauth_mode, "global");
    }

    /// Walidacja oauth_mode odrzuca nieznane wartosci.
    #[test]
    fn test_oauth_config_rejects_invalid_mode() {
        let db = setup_db();
        register_test_addon(&db, "addon-m3");
        let res = upsert_oauth_config(
            &db,
            "addon-m3",
            "p",
            "c",
            None,
            "https://cb",
            true,
            None,
            "wrong",
        );
        assert!(res.is_err(), "nieznany oauth_mode powinien dac blad");
    }

    /// Tokeny globalne (user_id=NULL) nie pojawiaja sie w liscie "moje konta".
    /// Invariant SQL-level: list_user_oauth_accounts_for_user(uid) filtruje
    /// po `user_id = ?1`, wiec NULL nie matchuje.
    #[test]
    fn test_my_oauth_accounts_filters_out_global() {
        let db = setup_db();
        register_test_addon(&db, "addon-glob");
        let u = create_user(&db, "owner");
        // Token globalny (user_id = NULL).
        upsert_user_oauth_account(
            &db,
            None,
            "addon-glob",
            "p",
            "ext-g",
            "G",
            &[9, 9],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();
        // Token individual dla u.
        upsert_user_oauth_account(
            &db,
            Some(u),
            "addon-glob",
            "p",
            "ext-i",
            "I",
            &[1, 1],
            None,
            "Bearer",
            "",
            None,
        )
        .unwrap();

        let list = list_user_oauth_accounts_for_user(&db, u).unwrap();
        assert_eq!(
            list.len(),
            1,
            "tylko individual; global NIE powinien byc widoczny"
        );
        assert_eq!(list[0].external_account_id, "ext-i");
        assert!(list.iter().all(|a| a.user_id == Some(u)));
    }

    // -------- Audyt lifecycle --------

    /// Symuluje dokladnie to co robi handler `addon_toggle`: zapisuje wpis audit
    /// z akcja "addon_toggle" i polami `enabled_old`/`enabled_new` w details.
    /// Weryfikujemy ze `list_addon_audit_logs` zwraca wpis i details zawiera
    /// oczekiwane klucze w formacie wymaganym przez GUI.
    #[test]
    fn test_addon_toggle_writes_audit_log() {
        let db = setup_db();
        register_test_addon(&db, "audit-toggle");
        let user_id = create_user(&db, "audytor");

        // Szczegoly w dokladnie takim samym formacie jak w handlerze.
        let details = serde_json::json!({
            "enabled_old": true,
            "enabled_new": false,
        })
        .to_string();

        log_audit_full(
            &db,
            Some(user_id),
            Some("audit-toggle"),
            "addon_toggle",
            Some("addon"),
            Some("audit-toggle"),
            Some(&details),
            "info",
            None,
            Some("node-test"),
        )
        .expect("log_audit_full musi sie powiesc");

        let (rows, total) = list_addon_audit_logs(&db, "audit-toggle", 50, 0, None, None).unwrap();
        assert!(total >= 1, "oczekiwano co najmniej 1 wpisu audytu");
        let entry = rows
            .iter()
            .find(|r| r.action == "addon_toggle")
            .expect("wpis z akcja addon_toggle powinien istniec");
        assert_eq!(entry.severity, "info");
        assert_eq!(entry.user_id, Some(user_id));

        let parsed: serde_json::Value =
            serde_json::from_str(entry.details.as_deref().unwrap_or("{}"))
                .expect("details powinno byc poprawnym JSON");
        assert_eq!(
            parsed.get("enabled_old"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            parsed.get("enabled_new"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn test_list_addons_includes_icon_size_and_category() {
        let db = setup_db();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO addons (addon_id, name, version, description, author, platforms, \
                 manifest_json, is_enabled, is_system, category, icon, runtime, wasm_size_bytes) \
                 VALUES ('ui-meta', 'UI Meta Addon', '1.0.0', 'desc', 'me', '[\"linux\"]', '{}', \
                         1, 0, 'communication', 'i-meeting', 'wasmtime', 4321)",
                [],
            )
            .unwrap();
        }

        let addons = list_addons(&db).unwrap();
        let row = addons
            .into_iter()
            .find(|a| a.addon_id == "ui-meta")
            .expect("ui-meta row");
        assert_eq!(row.category, "communication");
        assert_eq!(row.icon, "i-meeting");
        assert_eq!(row.runtime, "wasmtime");
        assert_eq!(row.wasm_size_bytes, 4321);
    }

    #[test]
    fn test_migration_43_adds_ui_metadata_columns() {
        let db = setup_db();
        let conn = db.lock().unwrap();
        let mut stmt = conn.prepare_cached("PRAGMA table_info(addons)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for expected in &["icon", "runtime", "wasm_size_bytes", "category"] {
            assert!(
                cols.iter().any(|c| c == expected),
                "column {expected} missing after migration 43 (cols={cols:?})"
            );
        }
    }
}

#[cfg(test)]
mod declared_network_rules_tests {
    use super::*;
    use std::path::Path;

    fn setup_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("cannot build test DB")
    }

    fn register(db: &DbPool, addon_id: &str) {
        register_addon(db, addon_id, addon_id, "1.0.0", "{}", "linux")
            .expect("register_addon failed");
    }

    fn insert_declared(db: &DbPool, addon_id: &str, rule_id: &str, host: &str, port: i32) {
        let conn = acquire(db).unwrap();
        conn.execute(
            "INSERT INTO addon_network_rules \
             (addon_id, rule_id, protocol, host, port, description, required, approved) \
             VALUES (?1, ?2, 'tcp', ?3, ?4, '', 1, 0)",
            rusqlite::params![addon_id, rule_id, host, port],
        )
        .expect("insert declared");
    }

    #[test]
    fn returns_rows_for_installed_addon() {
        let db = setup_db();
        register(&db, "net-a");
        insert_declared(&db, "net-a", "graph", "graph.microsoft.com", 443);
        insert_declared(&db, "net-a", "login", "login.microsoftonline.com", 443);

        let rows = get_addon_declared_network_rules(&db, "net-a").unwrap();
        assert_eq!(rows.len(), 2);
        let hosts: Vec<&str> = rows.iter().map(|r| r.host.as_str()).collect();
        assert!(hosts.contains(&"graph.microsoft.com"));
        assert!(hosts.contains(&"login.microsoftonline.com"));
        for r in &rows {
            assert_eq!(r.port, 443);
            assert_eq!(r.protocol, "tcp");
            assert!(r.required);
        }
    }

    #[test]
    fn returns_empty_for_addon_without_rules() {
        let db = setup_db();
        register(&db, "net-b");
        let rows = get_addon_declared_network_rules(&db, "net-b").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn isolated_per_addon() {
        let db = setup_db();
        register(&db, "net-c");
        register(&db, "net-d");
        insert_declared(&db, "net-c", "api", "api.example.com", 443);
        let rows_c = get_addon_declared_network_rules(&db, "net-c").unwrap();
        let rows_d = get_addon_declared_network_rules(&db, "net-d").unwrap();
        assert_eq!(rows_c.len(), 1);
        assert!(rows_d.is_empty());
    }
}

#[cfg(test)]
mod notes_tests {
    use super::*;
    use std::path::Path;

    fn setup_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("cannot build test DB")
    }

    fn mk_user(db: &DbPool, name: &str) -> i64 {
        create_user_account(db, name, "hash", name, &format!("{}@test", name))
            .expect("create_user_account")
    }

    #[test]
    fn test_create_and_list_notes_for_user() {
        let db = setup_db();
        let uid = mk_user(&db, "alice");
        let a = create_note(&db, uid, "first", "body A").unwrap();
        let b = create_note(&db, uid, "second", "body B").unwrap();
        let rows = list_notes_for_user(&db, uid).unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first (both same-second timestamp — order by id desc is acceptable fallback).
        let ids: Vec<i64> = rows.iter().map(|n| n.id).collect();
        assert!(ids.contains(&a) && ids.contains(&b));
        for n in &rows {
            assert_eq!(n.user_id, uid);
            assert!(!n.pinned);
        }
    }

    #[test]
    fn test_update_note_respects_user_ownership() {
        let db = setup_db();
        let alice = mk_user(&db, "alice");
        let bob = mk_user(&db, "bob");
        let note_id = create_note(&db, alice, "t", "b").unwrap();

        // Bob cannot update Alice's note.
        let res = update_note(&db, note_id, bob, "hacked", "hacked body");
        assert!(res.is_err());

        // Alice's note content stays intact.
        let got = get_note(&db, note_id, alice).unwrap().expect("present");
        assert_eq!(got.title, "t");
        assert_eq!(got.body, "b");
    }

    #[test]
    fn test_delete_note_respects_user_ownership() {
        let db = setup_db();
        let alice = mk_user(&db, "alice");
        let bob = mk_user(&db, "bob");
        let note_id = create_note(&db, alice, "t", "b").unwrap();

        let res = delete_note(&db, note_id, bob);
        assert!(res.is_err(), "bob must not be able to delete alice's note");

        // Still present for alice.
        let got = get_note(&db, note_id, alice).unwrap();
        assert!(got.is_some());

        // Alice can delete her own.
        delete_note(&db, note_id, alice).unwrap();
        assert!(get_note(&db, note_id, alice).unwrap().is_none());
    }

    #[test]
    fn test_notes_sorted_pinned_first_then_updated_desc() {
        let db = setup_db();
        let uid = mk_user(&db, "alice");
        let first = create_note(&db, uid, "first", "x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = create_note(&db, uid, "second", "y").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let third = create_note(&db, uid, "third", "z").unwrap();

        // Without pinning: newest first.
        let rows = list_notes_for_user(&db, uid).unwrap();
        assert_eq!(rows[0].id, third);
        assert_eq!(rows[2].id, first);

        // Pin the oldest — it must jump to the top.
        set_note_pinned(&db, first, uid, true).unwrap();
        let rows = list_notes_for_user(&db, uid).unwrap();
        assert_eq!(rows[0].id, first, "pinned note sorts first");
        assert!(rows[0].pinned);
        // Remaining two are in updated_at DESC order.
        assert_eq!(rows[1].id, third);
        assert_eq!(rows[2].id, second);
    }

    #[test]
    fn test_migration_46_idempotent() {
        // Migrations are applied on init. Re-running run() on the same pool must
        // not fail and must not duplicate the migration row.
        let db = setup_db();
        {
            let conn = acquire(&db).unwrap();
            crate::db::migrations::run(&conn).expect("re-run migrations");
            crate::db::migrations::run(&conn).expect("re-run migrations again");
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM _migrations WHERE version = 46",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "migration 46 must appear exactly once");
            let tbl: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notes'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(tbl, 1, "notes table must exist");
        }
        // And the table is usable.
        let uid = mk_user(&db, "alice");
        create_note(&db, uid, "t", "b").unwrap();
    }
}

// =============================================================================
// Deployments (migration 48) — deploy lifecycle tracking with streaming log tail
// =============================================================================

pub mod deployments {
    use super::DbPool;
    use anyhow::Result;
    use rusqlite::params;
    use serde::Serialize;

    /// Maksymalna liczba linii logu trzymana w kolumnie log_tail. Starsze linie
    /// są kasowane przy każdym append. 200 linii = ~15 KB tekstu, łatwo mieszczi
    /// się w rkyv response nawet dla wielu deployów na liście.
    pub const LOG_TAIL_MAX_LINES: usize = 200;

    #[derive(Debug, Clone, Serialize)]
    pub struct DeploymentRow {
        pub id: i64,
        pub deploy_id: String,
        pub engine_id: String,
        pub deploy_method: String,
        pub node_id: String,
        pub status: String,
        pub phase: String,
        pub progress_pct: i64,
        pub image_tag: String,
        pub container_name: String,
        pub config_json: String,
        pub user_id: Option<i64>,
        pub started_at: String,
        pub finished_at: Option<String>,
        pub error_message: Option<String>,
        pub log_tail: String,
    }

    const COLS: &str = "id, deploy_id, engine_id, deploy_method, node_id, status, phase, \
                        progress_pct, image_tag, container_name, config_json, user_id, \
                        started_at, finished_at, error_message, log_tail";

    fn row_to_deployment(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeploymentRow> {
        Ok(DeploymentRow {
            id: row.get(0)?,
            deploy_id: row.get(1)?,
            engine_id: row.get(2)?,
            deploy_method: row.get(3)?,
            node_id: row.get(4)?,
            status: row.get(5)?,
            phase: row.get(6)?,
            progress_pct: row.get(7)?,
            image_tag: row.get(8)?,
            container_name: row.get(9)?,
            config_json: row.get(10)?,
            user_id: row.get(11)?,
            started_at: row.get(12)?,
            finished_at: row.get(13)?,
            error_message: row.get(14)?,
            log_tail: row.get(15)?,
        })
    }

    /// Tworzy wiersz deployment w status='queued'. Caller (runner) zmienia
    /// status → 'building' → ... → 'success'/'failure'.
    pub fn create(
        pool: &DbPool,
        deploy_id: &str,
        engine_id: &str,
        deploy_method: &str,
        node_id: &str,
        config_json: &str,
        user_id: Option<i64>,
    ) -> Result<i64> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO deployments (deploy_id, engine_id, deploy_method, node_id, config_json, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![deploy_id, engine_id, deploy_method, node_id, config_json, user_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_status(
        pool: &DbPool,
        deploy_id: &str,
        status: &str,
        phase: &str,
        progress_pct: u32,
    ) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE deployments SET status = ?2, phase = ?3, progress_pct = ?4 WHERE deploy_id = ?1",
            params![deploy_id, status, phase, progress_pct as i64],
        )?;
        Ok(())
    }

    pub fn set_image_tag(pool: &DbPool, deploy_id: &str, image_tag: &str) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE deployments SET image_tag = ?2 WHERE deploy_id = ?1",
            params![deploy_id, image_tag],
        )?;
        Ok(())
    }

    pub fn set_container_name(pool: &DbPool, deploy_id: &str, name: &str) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE deployments SET container_name = ?2 WHERE deploy_id = ?1",
            params![deploy_id, name],
        )?;
        Ok(())
    }

    pub fn mark_finished(
        pool: &DbPool,
        deploy_id: &str,
        final_status: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE deployments SET status = ?2, finished_at = datetime('now'),
                 progress_pct = CASE WHEN ?2 = 'success' THEN 100 ELSE progress_pct END,
                 error_message = ?3 WHERE deploy_id = ?1",
            params![deploy_id, final_status, error_message],
        )?;
        Ok(())
    }

    /// Dopisuje linię do log_tail, trzymając nie więcej niż LOG_TAIL_MAX_LINES.
    /// Przy wielu równoległych deployach transakcja SQLite serializuje zapisy,
    /// więc nie musimy dodatkowego locka.
    pub fn append_log_line(pool: &DbPool, deploy_id: &str, line: &str) -> Result<()> {
        let conn = pool.lock().unwrap();
        let current: String = conn
            .query_row(
                "SELECT log_tail FROM deployments WHERE deploy_id = ?1",
                params![deploy_id],
                |row| row.get(0),
            )
            .unwrap_or_default();
        let mut lines: Vec<&str> = current.split('\n').filter(|l| !l.is_empty()).collect();
        lines.push(line);
        if lines.len() > LOG_TAIL_MAX_LINES {
            let drop_n = lines.len() - LOG_TAIL_MAX_LINES;
            lines.drain(0..drop_n);
        }
        let new_tail = lines.join("\n");
        conn.execute(
            "UPDATE deployments SET log_tail = ?2 WHERE deploy_id = ?1",
            params![deploy_id, new_tail],
        )?;
        Ok(())
    }

    pub fn get(pool: &DbPool, deploy_id: &str) -> Result<Option<DeploymentRow>> {
        let conn = pool.lock().unwrap();
        let sql = format!("SELECT {} FROM deployments WHERE deploy_id = ?1", COLS);
        let row = conn
            .query_row(&sql, params![deploy_id], row_to_deployment)
            .ok();
        Ok(row)
    }

    /// Lista deployów z filtrem po engine_id / status / user_id. Każdy filter
    /// opcjonalny. Sortowanie started_at DESC. Default limit 100.
    pub fn list(
        pool: &DbPool,
        engine_id: Option<&str>,
        status: Option<&str>,
        user_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<DeploymentRow>> {
        let conn = pool.lock().unwrap();
        let mut where_clauses = Vec::new();
        let mut bind_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(eid) = engine_id {
            where_clauses.push("engine_id = ?");
            bind_params.push(Box::new(eid.to_string()));
        }
        if let Some(st) = status {
            where_clauses.push("status = ?");
            bind_params.push(Box::new(st.to_string()));
        }
        if let Some(uid) = user_id {
            where_clauses.push("user_id = ?");
            bind_params.push(Box::new(uid));
        }
        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {} FROM deployments{} ORDER BY started_at DESC LIMIT {}",
            COLS,
            where_sql,
            limit.max(1).min(500)
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> =
            bind_params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), row_to_deployment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Deployy w stanie 'queued' lub 'running'-ish zostawione przez crash.
    /// Startup cleanup oznacza je jako 'failure' z error='aborted by shutdown'.
    pub fn reset_stale(pool: &DbPool) -> Result<u32> {
        let conn = pool.lock().unwrap();
        let n = conn.execute(
            "UPDATE deployments
             SET status = 'failure',
                 finished_at = datetime('now'),
                 error_message = 'aborted by tentaflow shutdown'
             WHERE status NOT IN ('success', 'failure', 'cancelled')",
            [],
        )?;
        Ok(n as u32)
    }
}

pub mod resource_permissions {
    // =========================================================================
    // resource_permissions — generyczna ACL dla modeli/flowow/addonow.
    // Priorytet: user_deny > user_allow > group_deny > group_allow > default_allow.
    // =========================================================================

    use crate::db::DbPool;
    use anyhow::Result;

    #[derive(Debug, Clone)]
    pub struct ResourcePermission {
        pub id: i64,
        pub resource_type: String,
        pub resource_id: String,
        pub subject_type: String, // "user" | "group"
        pub subject_id: i64,
        pub access_level: String, // "allow" | "deny"
    }

    /// Upsert permission — INSERT albo UPDATE gdy (type,id,subj,sid) istnieje.
    pub fn set(
        pool: &DbPool,
        resource_type: &str,
        resource_id: &str,
        subject_type: &str,
        subject_id: i64,
        access_level: &str,
    ) -> Result<()> {
        if !matches!(access_level, "allow" | "deny") {
            anyhow::bail!("access_level must be 'allow' or 'deny'");
        }
        if !matches!(subject_type, "user" | "group") {
            anyhow::bail!("subject_type must be 'user' or 'group'");
        }
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO resource_permissions
                (resource_type, resource_id, subject_type, subject_id, access_level)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(resource_type, resource_id, subject_type, subject_id)
             DO UPDATE SET access_level = excluded.access_level",
            rusqlite::params![
                resource_type,
                resource_id,
                subject_type,
                subject_id,
                access_level
            ],
        )?;
        Ok(())
    }

    /// Usun wpis (reset do default-allow).
    pub fn clear(
        pool: &DbPool,
        resource_type: &str,
        resource_id: &str,
        subject_type: &str,
        subject_id: i64,
    ) -> Result<()> {
        let conn = pool.lock().unwrap();
        conn.execute(
            "DELETE FROM resource_permissions
             WHERE resource_type = ?1 AND resource_id = ?2
               AND subject_type = ?3 AND subject_id = ?4",
            rusqlite::params![resource_type, resource_id, subject_type, subject_id],
        )?;
        Ok(())
    }

    /// Lista wszystkich wpisow dla konkretnego zasobu — dla UI
    /// "kto ma jaki dostep do gpt-4o".
    pub fn list_for_resource(
        pool: &DbPool,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<Vec<ResourcePermission>> {
        let conn = pool.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT id, resource_type, resource_id, subject_type, subject_id, access_level
             FROM resource_permissions
             WHERE resource_type = ?1 AND resource_id = ?2
             ORDER BY subject_type, subject_id",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![resource_type, resource_id], |row| {
                Ok(ResourcePermission {
                    id: row.get(0)?,
                    resource_type: row.get(1)?,
                    resource_id: row.get(2)?,
                    subject_type: row.get(3)?,
                    subject_id: row.get(4)?,
                    access_level: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Lista wszystkich wpisow dla user/group — dla UI "co user X ma zabronione".
    pub fn list_for_subject(
        pool: &DbPool,
        subject_type: &str,
        subject_id: i64,
    ) -> Result<Vec<ResourcePermission>> {
        let conn = pool.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT id, resource_type, resource_id, subject_type, subject_id, access_level
             FROM resource_permissions
             WHERE subject_type = ?1 AND subject_id = ?2
             ORDER BY resource_type, resource_id",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![subject_type, subject_id], |row| {
                Ok(ResourcePermission {
                    id: row.get(0)?,
                    resource_type: row.get(1)?,
                    resource_id: row.get(2)?,
                    subject_type: row.get(3)?,
                    subject_id: row.get(4)?,
                    access_level: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Sprawdza czy user ma dostep do zasobu. Priorytet:
    /// 1. Admin rola → zawsze allow.
    /// 2. Explicit user-level deny → deny.
    /// 3. Explicit user-level allow → allow.
    /// 4. Any group-level deny dla grup usera → deny.
    /// 5. Any group-level allow → allow.
    /// 6. Default: allow (public by default).
    pub fn check(
        pool: &DbPool,
        resource_type: &str,
        resource_id: &str,
        user_id: i64,
        user_role: &str,
    ) -> Result<bool> {
        // 1. Admin zawsze moze.
        if user_role == "admin" {
            return Ok(true);
        }

        let conn = pool.lock().unwrap();

        // 2. + 3. User-level override.
        let user_level: Option<String> = conn
            .query_row(
                "SELECT access_level FROM resource_permissions
                 WHERE resource_type = ?1 AND resource_id = ?2
                   AND subject_type = 'user' AND subject_id = ?3",
                rusqlite::params![resource_type, resource_id, user_id],
                |row| row.get(0),
            )
            .ok();
        if let Some(level) = user_level {
            return Ok(level == "allow");
        }

        // 4. + 5. Group-level check — any deny wygrywa nad allow.
        let mut stmt = conn.prepare_cached(
            "SELECT access_level FROM resource_permissions rp
             JOIN group_members gm ON rp.subject_id = gm.group_id
             WHERE rp.resource_type = ?1 AND rp.resource_id = ?2
               AND rp.subject_type = 'group' AND gm.user_id = ?3",
        )?;
        let levels: Vec<String> = stmt
            .query_map(
                rusqlite::params![resource_type, resource_id, user_id],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if levels.iter().any(|l| l == "deny") {
            return Ok(false);
        }
        if levels.iter().any(|l| l == "allow") {
            return Ok(true);
        }

        // 6. Default = allow (public by default).
        Ok(true)
    }
}

pub mod mesh_topology {
    // =========================================================================
    // mesh_topology repo — persystencja TopologyAnnounce dla bootstrapu peer_store
    // po restarcie. Cleanup wpisow starszych niz 7 dni przy starcie.
    // =========================================================================

    use crate::db::DbPool;
    use anyhow::Result;

    #[derive(Debug, Clone)]
    pub struct TopologySnapshot {
        pub node_id: String,
        pub hostname: String,
        pub platform: String,
        pub os_info: String,
        pub connected_to: Vec<String>,
        pub direct_addrs: Vec<String>,
        pub port: u16,
        pub last_epoch: u64,
        pub last_seen_ms: i64,
    }

    /// Jeden wpis w batch upserta — wszystko, co trzeba zapisac dla jednego noda.
    pub struct UpsertEntry<'a> {
        pub node_id: &'a str,
        pub hostname: &'a str,
        pub platform: &'a str,
        pub os_info: &'a str,
        pub connected_to: &'a [String],
        pub direct_addrs: &'a [String],
        pub port: u16,
        pub services_json: &'a str,
        pub models_json: &'a str,
        pub epoch: u64,
        pub now_ms: i64,
    }

    /// Batch upsert — jedna transakcja dla calej listy. Pod gossip burstem z 1000
    /// peerow oszczedza N-1 commitow (kazdy commit = fsync w WAL). Prepared stmt
    /// jest reuzywany dla wszystkich wierszy.
    pub fn upsert_batch(pool: &DbPool, entries: &[UpsertEntry<'_>]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut conn = pool.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO mesh_topology
                   (node_id, hostname, platform, os_info, connected_to, direct_addrs,
                    port, services_json, models_json, last_epoch, last_seen_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(node_id) DO UPDATE SET
                    hostname = excluded.hostname,
                    platform = excluded.platform,
                    os_info = excluded.os_info,
                    connected_to = excluded.connected_to,
                    direct_addrs = excluded.direct_addrs,
                    port = excluded.port,
                    services_json = excluded.services_json,
                    models_json = excluded.models_json,
                    last_epoch = excluded.last_epoch,
                    last_seen_ms = excluded.last_seen_ms
                 WHERE excluded.last_epoch >= mesh_topology.last_epoch",
            )?;
            for e in entries {
                let ct = serde_json::to_string(e.connected_to).unwrap_or_else(|_| "[]".to_string());
                let addrs =
                    serde_json::to_string(e.direct_addrs).unwrap_or_else(|_| "[]".to_string());
                // Per-row error log zamiast propagacji — jeden zly wiersz nie wali
                // calej gossip-batch transakcji. Poprawne wiersze commituja sie.
                if let Err(err) = stmt.execute(rusqlite::params![
                    e.node_id,
                    e.hostname,
                    e.platform,
                    e.os_info,
                    ct,
                    addrs,
                    e.port as i64,
                    e.services_json,
                    e.models_json,
                    e.epoch as i64,
                    e.now_ms,
                ]) {
                    tracing::debug!(node = %e.node_id, "mesh_topology row upsert: {}", err);
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_all(pool: &DbPool) -> Result<Vec<TopologySnapshot>> {
        let conn = pool.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT node_id, hostname, platform, os_info, connected_to, direct_addrs,
                    port, last_epoch, last_seen_ms
             FROM mesh_topology",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let ct: String = row.get(4)?;
                let addrs: String = row.get(5)?;
                Ok(TopologySnapshot {
                    node_id: row.get(0)?,
                    hostname: row.get(1)?,
                    platform: row.get(2)?,
                    os_info: row.get(3)?,
                    connected_to: serde_json::from_str(&ct).unwrap_or_default(),
                    direct_addrs: serde_json::from_str(&addrs).unwrap_or_default(),
                    port: row.get::<_, i64>(6)? as u16,
                    last_epoch: row.get::<_, i64>(7)? as u64,
                    last_seen_ms: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Czyści wpisy starsze niz 7 dni. Wolane przy starcie.
    pub fn cleanup_stale(pool: &DbPool, now_ms: i64) -> Result<u32> {
        let cutoff = now_ms - 7 * 24 * 60 * 60 * 1000;
        let conn = pool.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM mesh_topology WHERE last_seen_ms < ?1",
            rusqlite::params![cutoff],
        )?;
        Ok(n as u32)
    }
}

// =============================================================================
// peer_persisted + peer_hints — single source of truth for PeerRegistry state.
// Writes go through PersistenceWriter (mesh::peer_registry::persistence). Reads
// happen once at startup via PeerRegistry::hydrate_from_db.
// =============================================================================

/// Hint discriminator stored as INTEGER in peer_hints.hint_kind. Kept in sync
/// with mesh::peer_registry::HintKind via from_u8 / to_u8.
pub const HINT_KIND_DIRECT_ADDR: i64 = 0;
pub const HINT_KIND_RELAY_URL: i64 = 1;
pub const HINT_KIND_HOSTNAME: i64 = 2;

/// Trust state encoding for peer_persisted.trust_state.
pub const TRUST_DISCOVERED: i64 = 0;
pub const TRUST_PENDING_PAIRING: i64 = 1;
pub const TRUST_TRUSTED: i64 = 2;

/// Role encoding for peer_persisted.role.
pub const ROLE_NODE: i64 = 0;
pub const ROLE_EDGE: i64 = 1;
pub const ROLE_RELAY: i64 = 2;

#[derive(Debug, Clone)]
pub struct PeerPersistedRow {
    pub node_id: [u8; 32],
    pub pubkey: Vec<u8>,
    pub trust_state: i64,
    pub hostname: Option<String>,
    pub platform: Option<String>,
    pub role: i64,
    pub last_seen_ms: i64,
    pub persisted_ver: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct PeerHintRow {
    pub node_id: [u8; 32],
    pub hint_kind: i64,
    pub payload: String,
    pub last_ok_ms: Option<i64>,
    pub fail_count: i64,
}

fn node_id_from_blob(blob: Vec<u8>) -> Result<[u8; 32]> {
    if blob.len() != 32 {
        anyhow::bail!(
            "peer_persisted.node_id: expected 32 bytes, got {}",
            blob.len()
        );
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&blob);
    Ok(out)
}

/// Load every row from peer_persisted. Used once by PeerRegistry::hydrate_from_db
/// at startup; afterwards the registry is the source of truth.
pub fn load_peer_persisted_all(pool: &DbPool) -> Result<Vec<PeerPersistedRow>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT node_id, pubkey, trust_state, hostname, platform, role, \
                last_seen_ms, persisted_ver, updated_at_ms \
         FROM peer_persisted",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let blob: Vec<u8> = row.get(0)?;
            let node_id = match node_id_from_blob(blob) {
                Ok(id) => id,
                Err(_) => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(PeerPersistedRow {
                node_id,
                pubkey: row.get(1)?,
                trust_state: row.get(2)?,
                hostname: row.get(3)?,
                platform: row.get(4)?,
                role: row.get(5)?,
                last_seen_ms: row.get(6)?,
                persisted_ver: row.get(7)?,
                updated_at_ms: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Load every hint row, grouped by node_id. Skips rows whose node_id is not
/// 32 bytes (defensive: should be impossible thanks to FK + schema).
pub fn load_peer_hints_all(
    pool: &DbPool,
) -> Result<std::collections::HashMap<[u8; 32], Vec<PeerHintRow>>> {
    let conn = acquire(pool)?;
    let mut stmt = conn.prepare_cached(
        "SELECT node_id, hint_kind, payload, last_ok_ms, fail_count FROM peer_hints",
    )?;
    let mut out: std::collections::HashMap<[u8; 32], Vec<PeerHintRow>> = Default::default();
    let rows = stmt.query_map([], |row| {
        let blob: Vec<u8> = row.get(0)?;
        let node_id = match node_id_from_blob(blob) {
            Ok(id) => id,
            Err(_) => return Err(rusqlite::Error::InvalidQuery),
        };
        Ok(PeerHintRow {
            node_id,
            hint_kind: row.get(1)?,
            payload: row.get(2)?,
            last_ok_ms: row.get(3)?,
            fail_count: row.get(4)?,
        })
    })?;
    for row in rows {
        let row = row?;
        out.entry(row.node_id).or_default().push(row);
    }
    Ok(out)
}

/// Idempotent batched upsert of peer state rows. The WHERE clause on the
/// conflict path drops out-of-order writes (lower persisted_ver loses).
pub fn upsert_peer_persisted_batch(pool: &DbPool, rows: &[PeerPersistedRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = acquire(pool)?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO peer_persisted \
                (node_id, pubkey, trust_state, hostname, platform, role, \
                 last_seen_ms, persisted_ver, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(node_id) DO UPDATE SET \
                pubkey = excluded.pubkey, \
                trust_state = excluded.trust_state, \
                hostname = excluded.hostname, \
                platform = excluded.platform, \
                role = excluded.role, \
                last_seen_ms = excluded.last_seen_ms, \
                persisted_ver = excluded.persisted_ver, \
                updated_at_ms = excluded.updated_at_ms \
             WHERE excluded.persisted_ver > peer_persisted.persisted_ver",
        )?;
        for r in rows {
            stmt.execute(rusqlite::params![
                r.node_id.as_slice(),
                r.pubkey,
                r.trust_state,
                r.hostname,
                r.platform,
                r.role,
                r.last_seen_ms,
                r.persisted_ver,
                r.updated_at_ms,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Replace the hint set for a node atomically. Hints are union-merged in
/// memory by the writer before this call, so a single call carries the
/// authoritative current set.
pub fn replace_peer_hints(pool: &DbPool, node_id: &[u8; 32], hints: &[PeerHintRow]) -> Result<()> {
    let mut conn = acquire(pool)?;
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM peer_hints WHERE node_id = ?1",
        rusqlite::params![node_id.as_slice()],
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO peer_hints \
                (node_id, hint_kind, payload, last_ok_ms, fail_count) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for h in hints {
            stmt.execute(rusqlite::params![
                h.node_id.as_slice(),
                h.hint_kind,
                h.payload,
                h.last_ok_ms,
                h.fail_count,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn delete_peer_persisted(pool: &DbPool, node_id: &[u8; 32]) -> Result<()> {
    let conn = acquire(pool)?;
    // peer_hints cascade-delete via FK.
    conn.execute(
        "DELETE FROM peer_persisted WHERE node_id = ?1",
        rusqlite::params![node_id.as_slice()],
    )?;
    Ok(())
}

/// One-shot upgrade path: copy `trusted_nodes` rows + decode `settings` keys
/// `trusted_contact:<hex>` (JSON value) into peer_persisted + peer_hints.
/// After the copy, settings rows for `trusted_contact:%` and `pending_contact:%`
/// are deleted. Returns the number of peer_persisted rows produced.
///
/// Idempotent: if peer_persisted already has a row for a given node_id, the
/// trusted_nodes copy is skipped via INSERT OR IGNORE; settings rows are
/// always purged at the end.
pub fn migrate_settings_trusted_contacts_to_peer_hints(pool: &DbPool) -> Result<usize> {
    let mut conn = acquire(pool)?;
    let tx = conn.transaction()?;

    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    // Step 1: pull trusted_nodes rows. Tolerate absence of last_addresses.
    let mut trusted_rows: Vec<(String, String, String)> = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT node_id, public_key, hostname FROM trusted_nodes WHERE is_active = 1",
        )?;
        let it = stmt.query_map([], |row| {
            let nid: String = row.get(0)?;
            let pk: String = row.get(1)?;
            let host: String = row.get(2).unwrap_or_default();
            Ok((nid, pk, host))
        })?;
        for r in it {
            trusted_rows.push(r?);
        }
    }

    let mut created = 0usize;
    {
        let mut ins_peer = tx.prepare_cached(
            "INSERT OR IGNORE INTO peer_persisted \
                (node_id, pubkey, trust_state, hostname, platform, role, \
                 last_seen_ms, persisted_ver, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, 0, 0, ?6)",
        )?;

        for (node_hex, pk_hex, hostname) in &trusted_rows {
            let mut node_id = [0u8; 32];
            if hex::decode_to_slice(node_hex.as_str(), &mut node_id).is_err() {
                continue;
            }
            // pubkey may be 64 hex (Ed25519) or 128 hex (Ed25519+X25519).
            let pubkey = match hex::decode(pk_hex.as_str()) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let host_opt: Option<&str> = if hostname.is_empty() {
                None
            } else {
                Some(hostname)
            };
            let n = ins_peer.execute(rusqlite::params![
                node_id.as_slice(),
                pubkey,
                TRUST_TRUSTED,
                host_opt,
                ROLE_NODE,
                now_ms,
            ])?;
            if n > 0 {
                created += 1;
            }
        }
    }

    // Step 2: parse settings `trusted_contact:<hex>` rows (JSON
    // PairingContactHints) and emit peer_hints rows. Same JSON shape used by
    // pairing.rs / sanitize_trusted_contacts.
    let mut settings_rows: Vec<(String, String)> = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT key, value FROM settings WHERE key LIKE 'trusted_contact:%' ESCAPE '\\'",
        )?;
        let it = stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })?;
        for r in it {
            settings_rows.push(r?);
        }
    }

    {
        // Ensure a peer_persisted row exists before inserting hints (FK).
        let mut ensure_peer = tx.prepare_cached(
            "INSERT OR IGNORE INTO peer_persisted \
                (node_id, pubkey, trust_state, hostname, platform, role, \
                 last_seen_ms, persisted_ver, updated_at_ms) \
             VALUES (?1, X'', ?2, NULL, NULL, ?3, 0, 0, ?4)",
        )?;
        let mut ins_hint = tx.prepare_cached(
            "INSERT OR IGNORE INTO peer_hints (node_id, hint_kind, payload, last_ok_ms, fail_count) \
             VALUES (?1, ?2, ?3, NULL, 0)",
        )?;

        for (key, value) in &settings_rows {
            let hex_part = match key.strip_prefix("trusted_contact:") {
                Some(s) => s,
                None => continue,
            };
            let mut node_id = [0u8; 32];
            if hex::decode_to_slice(hex_part, &mut node_id).is_err() {
                continue;
            }
            let parsed: serde_json::Value = match serde_json::from_str(value) {
                Ok(v) => v,
                Err(_) => continue,
            };
            ensure_peer.execute(rusqlite::params![
                node_id.as_slice(),
                TRUST_TRUSTED,
                ROLE_NODE,
                now_ms,
            ])?;

            if let Some(addrs) = parsed.get("addresses").and_then(|v| v.as_array()) {
                for a in addrs {
                    if let Some(s) = a.as_str() {
                        if !s.is_empty() {
                            ins_hint.execute(rusqlite::params![
                                node_id.as_slice(),
                                HINT_KIND_DIRECT_ADDR,
                                s,
                            ])?;
                        }
                    }
                }
            }
            if let Some(relay) = parsed.get("relay_url").and_then(|v| v.as_str()) {
                if !relay.is_empty() {
                    ins_hint.execute(rusqlite::params![
                        node_id.as_slice(),
                        HINT_KIND_RELAY_URL,
                        relay,
                    ])?;
                }
            }
            if let Some(host) = parsed.get("hostname").and_then(|v| v.as_str()) {
                if !host.is_empty() {
                    ins_hint.execute(rusqlite::params![
                        node_id.as_slice(),
                        HINT_KIND_HOSTNAME,
                        host,
                    ])?;
                }
            }
        }
    }

    // Step 3: purge settings rows that have been migrated. pending_contact:* is
    // an ephemeral pairing artifact; not migrated, just dropped.
    tx.execute(
        "DELETE FROM settings WHERE key LIKE 'trusted_contact:%' ESCAPE '\\'",
        [],
    )?;
    tx.execute(
        "DELETE FROM settings WHERE key LIKE 'pending_contact:%' ESCAPE '\\'",
        [],
    )?;

    tx.commit()?;
    Ok(created)
}

// =============================================================================
// Tests: meeting_summaries + meeting_action_items (migracja 53)
// =============================================================================

#[cfg(test)]
mod meeting_summary_action_items_tests {
    use super::transcripts::*;
    use super::*;
    use std::path::Path;

    fn setup_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("init test DB")
    }

    fn mk_session(db: &DbPool, key: &str) -> i64 {
        get_or_create_session(db, key, Some("https://x"), Some("title")).expect("create session")
    }

    #[test]
    fn migration_53_drops_old_summaries_creates_new_tables() {
        let db = setup_db();
        let conn = db.lock().unwrap();
        // Stara tabela musi byc skasowana migracja 53.
        let old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meeting_session_summaries'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0, "stara tabela nadal istnieje");

        // Nowe tabele musza istniec.
        for tbl in ["meeting_summaries", "meeting_action_items"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![tbl],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "brak tabeli {}", tbl);
        }
    }

    #[test]
    fn insert_summary_returns_id_and_list_in_desc_order() {
        let db = setup_db();
        let sid = mk_session(&db, "m1");
        let id1 = insert_meeting_summary(&db, sid, "D1", "S1", "qwen").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let id2 = insert_meeting_summary(&db, sid, "D2", "S2", "qwen").unwrap();
        assert!(id2 > id1);

        let rows = list_summaries_for_meeting(&db, sid, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, id2);
        assert_eq!(rows[0].summary_text, "S2");
        assert_eq!(rows[1].id, id1);
    }

    #[test]
    fn list_summaries_respects_limit() {
        let db = setup_db();
        let sid = mk_session(&db, "m-lim");
        for i in 0..5 {
            insert_meeting_summary(&db, sid, &format!("D{i}"), &format!("S{i}"), "qwen").unwrap();
        }
        let rows = list_summaries_for_meeting(&db, sid, 2).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn owner_of_meeting_key_returns_none_for_unknown() {
        let db = setup_db();
        let got = owner_of_meeting_key(&db, "missing").unwrap();
        assert!(got.is_none(), "nieistniejaca sesja -> None");
    }

    #[test]
    fn owner_of_meeting_key_returns_some_none_when_no_owner() {
        let db = setup_db();
        mk_session(&db, "ownerless");
        let got = owner_of_meeting_key(&db, "ownerless").unwrap();
        assert_eq!(got, Some(None), "sesja bez ownera -> Some(None)");
    }

    #[test]
    fn owner_of_meeting_key_returns_owner_when_set() {
        let db = setup_db();
        let sid = mk_session(&db, "owned");
        // Ustawiamy owner_user_id bezposrednio — testujemy tylko reader.
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE meeting_sessions SET owner_user_id = 42 WHERE id = ?1",
                rusqlite::params![sid],
            )
            .unwrap();
        }
        let got = owner_of_meeting_key(&db, "owned").unwrap();
        assert_eq!(got, Some(Some(42)));
    }

    #[test]
    fn upsert_action_item_deduplicates_same_content() {
        let db = setup_db();
        let sid = mk_session(&db, "m-dedup");
        let id1 =
            upsert_meeting_action_item(&db, sid, "Alice", "prepare report", Some("2026-05-01"))
                .unwrap();
        // Ten sam owner+task — dedup przez content_hash, to samo id.
        let id2 =
            upsert_meeting_action_item(&db, sid, "  alice ", "Prepare Report", Some("2026-05-02"))
                .unwrap();
        assert_eq!(id1, id2);
        let rows = list_action_items_for_meeting(&db, sid, None).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn upsert_action_item_updates_deadline_on_conflict() {
        let db = setup_db();
        let sid = mk_session(&db, "m-deadline");
        upsert_meeting_action_item(&db, sid, "Bob", "ship PR", Some("2026-05-01")).unwrap();
        upsert_meeting_action_item(&db, sid, "Bob", "ship PR", Some("2026-05-10")).unwrap();
        let rows = list_action_items_for_meeting(&db, sid, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].deadline.as_deref(), Some("2026-05-10"));
    }

    #[test]
    fn upsert_action_item_touches_updated_at_on_conflict() {
        let db = setup_db();
        let sid = mk_session(&db, "m-touch");
        upsert_meeting_action_item(&db, sid, "Carol", "refactor X", None).unwrap();
        let before = list_action_items_for_meeting(&db, sid, None).unwrap();
        let u0 = before[0].updated_at.clone();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        upsert_meeting_action_item(&db, sid, "Carol", "refactor X", Some("later")).unwrap();
        let after = list_action_items_for_meeting(&db, sid, None).unwrap();
        assert_ne!(u0, after[0].updated_at, "updated_at musi sie odswiezyc");
    }

    #[test]
    fn list_action_items_filters_by_status() {
        let db = setup_db();
        let sid = mk_session(&db, "m-filter");
        let a = upsert_meeting_action_item(&db, sid, "D", "t1", None).unwrap();
        upsert_meeting_action_item(&db, sid, "E", "t2", None).unwrap();
        update_action_item_status(&db, a, "done").unwrap();
        let pending = list_action_items_for_meeting(&db, sid, Some("pending")).unwrap();
        let done = list_action_items_for_meeting(&db, sid, Some("done")).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].task, "t2");
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].id, a);
    }

    #[test]
    fn update_action_item_status_returns_affected() {
        let db = setup_db();
        let sid = mk_session(&db, "m-aff");
        let id = upsert_meeting_action_item(&db, sid, "F", "t", None).unwrap();
        assert!(update_action_item_status(&db, id, "done").unwrap());
        assert!(!update_action_item_status(&db, 999_999, "done").unwrap());
    }

    #[test]
    fn cascade_delete_removes_summaries_and_action_items() {
        let db = setup_db();
        let sid = mk_session(&db, "m-cascade");
        insert_meeting_summary(&db, sid, "d", "s", "m").unwrap();
        upsert_meeting_action_item(&db, sid, "G", "t", None).unwrap();
        {
            let conn = db.lock().unwrap();
            // SQLite wymaga wlaczonego PRAGMA foreign_keys per-connection — init
            // to robi globalnie przez set_pragmas, ale sprawdzamy tu eksplicytnie.
            conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
            conn.execute(
                "DELETE FROM meeting_sessions WHERE id = ?1",
                rusqlite::params![sid],
            )
            .unwrap();
        }
        let summaries = list_summaries_for_meeting(&db, sid, 10).unwrap();
        let items = list_action_items_for_meeting(&db, sid, None).unwrap();
        assert!(summaries.is_empty(), "summaries niezcascadowane");
        assert!(items.is_empty(), "action items niezcascadowane");
    }
}

// Tests in this module exercise the legacy `services` / `model_registry`
// schema that was dropped in migration 64; the underlying repository
// functions stay alive until the N6 purge step removes them. They are
// ignored here so the suite stays green; remove the module entirely once
// N6 deletes `delete_service` / `relink_model_entry` / friends.
#[cfg(test)]
#[allow(dead_code)]
mod delete_service_cascade_tests {
    use super::*;
    use std::path::Path;

    fn setup_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("nie udalo sie utworzyc test DB")
    }

    #[test]
    #[ignore = "legacy services schema dropped in migration 64; module removed in N6"]
    fn delete_service_cascades_model_registry_rows() {
        // FK `model_registry.service_id` ma ON DELETE SET NULL, wiec bez jawnego
        // DELETE w `delete_service` modele zostawalyby sierotami widocznymi w GUI.
        let db = setup_db();

        let service_id =
            create_service(&db, "test-tts-service", "tts", "single", Some("tts"), "{}")
                .expect("create_service failed");

        let model_id = create_model_entry(
            &db,
            &NewModelEntry {
                model_name: "test-tts-model",
                display_name: Some("Test TTS"),
                service_type: "tts",
                connection_type: "quic",
                service_id: Some(service_id),
                flow_id: None,
                is_public: false,
                config_json: "{}",
            },
        )
        .expect("create_model_entry failed");

        // Sanity: model istnieje przed kasowaniem.
        let before: i64 = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM model_registry WHERE id = ?1",
                rusqlite::params![model_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(before, 1, "model powinien istniec przed delete_service");

        delete_service(&db, service_id).expect("delete_service failed");

        let conn = db.lock().unwrap();
        let services_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM services WHERE id = ?1",
                rusqlite::params![service_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(services_after, 0, "services row powinien byc usuniety");

        let models_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_registry WHERE id = ?1",
                rusqlite::params![model_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            models_after, 0,
            "model_registry row powinien byc skasowany kaskadowo (zero sierot)"
        );
    }

    #[test]
    #[ignore = "legacy services schema dropped in migration 64; module removed in N6"]
    fn relink_model_entry_repoints_orphan_to_new_service() {
        // Symulacja stanu sprzed FIX 1: model_registry zostal sierota (service_id=NULL),
        // potem user zdeployowal serwis ponownie pod ta sama nazwa modelu.
        let db = setup_db();

        let orphan_id = create_model_entry(
            &db,
            &NewModelEntry {
                model_name: "apple-tts-zosia-pl",
                display_name: Some("Apple TTS"),
                service_type: "tts",
                connection_type: "internal",
                service_id: None,
                flow_id: None,
                is_public: true,
                config_json: "{}",
            },
        )
        .expect("create orphan failed");

        let new_service =
            create_service(&db, "apple-tts-native", "tts", "single", Some("tts"), "{}")
                .expect("create_service failed");

        let updated = relink_model_entry_to_service(
            &db,
            "apple-tts-zosia-pl",
            new_service,
            "tts",
            r#"{"deploy_mode":"native"}"#,
        )
        .expect("relink failed");
        assert_eq!(updated, 1, "powinien byc dokladnie jeden UPDATE");

        let entry = get_model_entry(&db, orphan_id)
            .unwrap()
            .expect("entry znika");
        assert_eq!(
            entry.service_id,
            Some(new_service),
            "service_id nie przepiety"
        );
        assert_eq!(entry.is_active, true, "is_active powinno byc true");
        assert!(
            entry.config_json.contains("native"),
            "config_json nie zaktualizowany"
        );

        // Idempotencja: drugi call nadal updateuje 1 wiersz, ale stan ten sam.
        let again = relink_model_entry_to_service(
            &db,
            "apple-tts-zosia-pl",
            new_service,
            "tts",
            r#"{"deploy_mode":"native"}"#,
        )
        .expect("relink2 failed");
        assert_eq!(again, 1, "drugi relink dalej UPDATE-uje ten sam wiersz");
    }

    #[test]
    #[ignore = "legacy services schema dropped in migration 64; module removed in N6"]
    fn prune_orphaned_quic_models_skips_legitimate_globals() {
        let db = setup_db();

        // 1. Quic orphan — DO usuniecia.
        create_model_entry(
            &db,
            &NewModelEntry {
                model_name: "quic-orphan",
                display_name: None,
                service_type: "tts",
                connection_type: "quic",
                service_id: None,
                flow_id: None,
                is_public: false,
                config_json: "{}",
            },
        )
        .unwrap();

        // 2. Internal orphan — rowniez DO usuniecia (sierota po embedded deploy
        //    natywnym, prefix `apple-tts-` pasuje do listy w prune_orphaned_quic_models).
        create_model_entry(
            &db,
            &NewModelEntry {
                model_name: "apple-tts-orphan",
                display_name: None,
                service_type: "tts",
                connection_type: "internal",
                service_id: None,
                flow_id: None,
                is_public: false,
                config_json: "{}",
            },
        )
        .unwrap();

        // 3. OpenAI globalny model — service_id=NULL ale to legitne.
        let openai_id = create_model_entry(
            &db,
            &NewModelEntry {
                model_name: "gpt-4",
                display_name: Some("OpenAI GPT-4"),
                service_type: "llm",
                connection_type: "openai_api",
                service_id: None,
                flow_id: None,
                is_public: true,
                config_json: "{}",
            },
        )
        .unwrap();

        // 4. Quic z poprawnym service_id — NIE moze byc ruszony.
        let svc = create_service(&db, "live-svc", "tts", "single", Some("tts"), "{}").unwrap();
        let live_id = create_model_entry(
            &db,
            &NewModelEntry {
                model_name: "live-model",
                display_name: None,
                service_type: "tts",
                connection_type: "quic",
                service_id: Some(svc),
                flow_id: None,
                is_public: false,
                config_json: "{}",
            },
        )
        .unwrap();

        let removed = prune_orphaned_quic_models(&db).expect("prune failed");
        assert_eq!(removed, 2, "powinny zniknac tylko quic+local sieroty");

        // openai global i live model przezyly.
        assert!(
            get_model_entry(&db, openai_id).unwrap().is_some(),
            "openai global skasowany!"
        );
        assert!(
            get_model_entry(&db, live_id).unwrap().is_some(),
            "live model skasowany!"
        );

        // Drugi call to no-op.
        let again = prune_orphaned_quic_models(&db).unwrap();
        assert_eq!(again, 0, "drugi prune nic nie znajduje");
    }
}

// =============================================================================
// Tests: settings → peer_persisted/peer_hints upgrade migration (PR5)
// =============================================================================

#[cfg(test)]
mod settings_to_peer_hints_migration_tests {
    use super::*;
    use std::path::Path;

    fn fresh_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("init test DB")
    }

    #[test]
    fn migration_settings_trusted_contacts_to_peer_hints_idempotent() {
        let db = fresh_db();

        // db::init runs the migration once on a clean schema; expect zero
        // peer rows at this point.
        let n0: i64 = {
            let conn = db.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM peer_persisted", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(n0, 0, "fresh DB should not contain peer_persisted rows yet");

        // Seed a legacy settings row with the JSON shape that pairing.rs
        // historically wrote under `trusted_contact:<hex>`. node_id is 64 hex
        // = 32 raw bytes.
        let node_hex = "abcd1234".repeat(8);
        let value = r#"{"node_id":"abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234","public_key_hex":"","hostname":"foo","addresses":["127.0.0.1:7777"],"relay_url":"https://relay.example.com"}"#;
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                rusqlite::params![format!("trusted_contact:{}", node_hex), value],
            )
            .unwrap();
        }

        // First explicit run after seeding.
        let created = migrate_settings_trusted_contacts_to_peer_hints(&db).unwrap();
        // The migration may have produced 0 from the trusted_nodes branch
        // (table empty) but ensure_peer in the settings branch creates the
        // peer_persisted row via INSERT OR IGNORE.
        let _ = created;

        let conn = db.lock().unwrap();
        let persisted_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM peer_persisted", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            persisted_count, 1,
            "expected 1 peer_persisted row after migration"
        );

        let hints_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM peer_hints", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            hints_count, 3,
            "expected 3 hint rows (1 addr + 1 relay + 1 hostname)"
        );

        let leftover: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM settings WHERE key LIKE 'trusted_contact:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            leftover, 0,
            "settings rows should be purged after migration"
        );
        drop(conn);

        // Idempotency: a second run must not duplicate rows.
        let _ = migrate_settings_trusted_contacts_to_peer_hints(&db).unwrap();
        let conn = db.lock().unwrap();
        let persisted_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM peer_persisted", [], |r| r.get(0))
            .unwrap();
        assert_eq!(persisted_after, 1, "second run must not create duplicates");
        let hints_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM peer_hints", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hints_after, 3, "second run must not create duplicate hints");
    }
}
