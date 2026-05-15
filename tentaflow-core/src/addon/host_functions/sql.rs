// =============================================================================
// Plik: addon/host_functions/sql.rs
// Opis: Host functions SQL API (F1a §6.2 M1.W4 TentaVision). PELNA implementacja
//       — sql_exec_v1, sql_query_v1, sql_query_one_v1, sql_transaction_v1.
//       Per-addon SQLite (storage_sql::open_addon_db), bindowane parametry
//       (rusqlite params_from_iter — bez string concat), DDL block at runtime
//       (CREATE/ALTER/DROP wyłącznie przez migrations), payload limit 4MB
//       (PayloadKind::SqlCombined), timeout 30s. Audit z risk_class A i query
//       hash (SHA256[:16]). Error mapping rusqlite -> AbiError:
//       SQLITE_CONSTRAINT(19) -> SqlConstraint, SQLITE_ERROR(1) -> SqlSyntax.
// Uprawnienia: `sql.read` (sql_query/sql_query_one), `sql.write` (sql_exec/sql_transaction).
//              Manifest musi deklarowac [storage] sql=true; bez tego ABI fail-closed.
// =============================================================================

// ABI host functions wymaga 7-8 parametrow (ptr/len/out_ptr/out_cap/out_len_ptr +
// dla 2-input takze second_ptr/second_len) — to kontrakt, nie design smell.
#![allow(clippy::too_many_arguments)]

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use base64::Engine;
use regex::Regex;
use rusqlite::types::{Value as SqliteValue, ValueRef};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use tracing::warn;

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_string, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::addon::storage_sql::{get_addon_pool, AddonDbPool};
use crate::audit::RiskClass;

// =============================================================================
// Stale
// =============================================================================

/// Timeout wykonania pojedynczego zapytania SQL — chroni Core przed addonem
/// z patologicznym recursive CTE / cross join. Wykorzystuje
/// `Connection::get_interrupt_handle()` + watchdog thread ktory wywoluje
/// `interrupt()` po uplywie limitu.
const QUERY_TIMEOUT_MS: u64 = 30_000;

// =============================================================================
// DDL block — runtime guard
// =============================================================================

/// Regex wykrywajacy DDL statementy na poczatku zapytania. Komentarze i
/// whitespace usuwamy wczesniej przez `strip_leading_noise` — regex zakłada
/// ze pierwszy znak to slowo kluczowe. DDL zarezerwowany tylko dla migrations
/// runnera; addon nie moze go wywolac przez sql_exec/sql_query.
fn ddl_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"(?i)^(CREATE|ALTER|DROP|TRUNCATE|REINDEX|VACUUM|ATTACH|DETACH|PRAGMA)\b")
            .expect("ddl regex stale poprawny")
    })
}

/// Regex dla query read-only akceptowanych przez sql_query.
fn read_only_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"(?i)^(SELECT|WITH|EXPLAIN)\b").expect("read-only regex stale poprawny")
    })
}

/// Usuwa wiodace whitespace oraz komentarze SQL (`--` linia, `/* */` block).
/// SQLite akceptuje takie prefiksy przed wlasciwym statementem, wiec regex
/// dopasowujacy slowo kluczowe musi pracowac na wartosci po normalizacji —
/// inaczej payload `/*x*/ DROP TABLE` obchodzi guard.
fn strip_leading_noise(q: &str) -> &str {
    let mut s = q.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            // Komentarz liniowy konczy sie na nowej linii lub koncu inputa.
            s = match rest.split_once('\n') {
                Some((_, after)) => after.trim_start(),
                None => "",
            };
        } else if let Some(rest) = s.strip_prefix("/*") {
            // Komentarz blokowy konczy sie na pierwszym `*/`.
            s = match rest.split_once("*/") {
                Some((_, after)) => after.trim_start(),
                None => "",
            };
        } else {
            break;
        }
    }
    s
}

fn is_ddl(query: &str) -> bool {
    ddl_regex().is_match(strip_leading_noise(query))
}

fn is_read_only(query: &str) -> bool {
    read_only_regex().is_match(strip_leading_noise(query))
}

// =============================================================================
// Hash query — uzywany w audit (zamiast loga calego query, bo moze byc duzy)
// =============================================================================

fn query_hash_short(q: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(q.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

// =============================================================================
// Parametry: JSON -> rusqlite Value
// =============================================================================

/// Konwertuje JSON value na rusqlite Value zgodnie z mapowaniem §6.2:
/// string -> TEXT, integer -> INTEGER, real -> REAL, bool -> INTEGER 0/1,
/// null -> NULL, `{"$bytes":"<base64>"}` -> BLOB.
fn json_to_sqlite_value(v: &JsonValue) -> Result<SqliteValue, AbiError> {
    match v {
        JsonValue::Null => Ok(SqliteValue::Null),
        JsonValue::Bool(b) => Ok(SqliteValue::Integer(if *b { 1 } else { 0 })),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(SqliteValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(SqliteValue::Real(f))
            } else {
                Err(AbiError::Operation)
            }
        }
        JsonValue::String(s) => Ok(SqliteValue::Text(s.clone())),
        JsonValue::Object(obj) => {
            if let Some(JsonValue::String(b64)) = obj.get("$bytes") {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|_| AbiError::Operation)?;
                Ok(SqliteValue::Blob(bytes))
            } else {
                Err(AbiError::Operation)
            }
        }
        JsonValue::Array(_) => Err(AbiError::Operation),
    }
}

fn parse_params(params_json: &str) -> Result<Vec<SqliteValue>, AbiError> {
    if params_json.is_empty() {
        return Ok(Vec::new());
    }
    let v: JsonValue = serde_json::from_str(params_json).map_err(|_| AbiError::Operation)?;
    let arr = v.as_array().ok_or(AbiError::Operation)?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        out.push(json_to_sqlite_value(item)?);
    }
    Ok(out)
}

// =============================================================================
// Wynik: rusqlite Value -> JSON (do output)
// =============================================================================

fn sqlite_value_ref_to_json(v: ValueRef<'_>) -> Result<JsonValue, AbiError> {
    Ok(match v {
        ValueRef::Null => JsonValue::Null,
        ValueRef::Integer(i) => JsonValue::from(i),
        ValueRef::Real(f) => {
            // NaN/Inf nie maja reprezentacji w JSON — `Number::from_f64`
            // zwraca None i wczesniej tracilismy informacje przez fallback
            // na NULL. Zwracamy blad operacji zeby addon dostal jednoznaczny
            // sygnal data integrity issue.
            match serde_json::Number::from_f64(f) {
                Some(n) => JsonValue::Number(n),
                None => return Err(AbiError::Operation),
            }
        }
        ValueRef::Text(t) => JsonValue::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(b);
            json!({ "$bytes": b64 })
        }
    })
}

// =============================================================================
// Mapowanie bledow rusqlite -> AbiError
// =============================================================================

fn map_sqlite_error(e: &rusqlite::Error) -> AbiError {
    if let rusqlite::Error::SqliteFailure(code, _) = e {
        // code.extended_code daje dokladniejsze warianty (CONSTRAINT_UNIQUE,
        // CONSTRAINT_FK itp.). My grupujemy je przez primary code.
        match code.code {
            rusqlite::ErrorCode::ConstraintViolation => return AbiError::SqlConstraint,
            rusqlite::ErrorCode::OperationInterrupted => return AbiError::Timeout,
            _ => {}
        }
    }
    // ErrorCode::Unknown lub SqliteSingleThreadedMode itp.: zalicz jako syntax
    // (najczestszy przypadek dla parser fail). Operation jako last resort.
    let s = e.to_string().to_lowercase();
    if s.contains("syntax") || s.contains("near") || s.contains("no such table") || s.contains("no such column") {
        AbiError::SqlSyntax
    } else if s.contains("interrupted") {
        AbiError::Timeout
    } else {
        AbiError::Operation
    }
}

// =============================================================================
// Sprawdzenie czy addon zadeklarowal SQL w manifescie
// =============================================================================

fn addon_has_sql_declared(state: &AddonState) -> bool {
    state
        .manifest
        .storage
        .as_ref()
        .map(|s| s.sql)
        .unwrap_or(false)
}

// =============================================================================
// Pobranie pool dla addona (z guard na declared)
// =============================================================================

fn acquire_pool(state: &AddonState) -> Result<AddonDbPool, AbiError> {
    if !addon_has_sql_declared(state) {
        return Err(AbiError::Permission);
    }
    get_addon_pool(&state.addon_id).ok_or(AbiError::Operation)
}

// =============================================================================
// Query timeout — watchdog uzywajacy Connection::get_interrupt_handle()
// =============================================================================

/// Guard, ktory uruchamia watchdog thread przerywajacy zapytanie SQL po
/// uplywie `timeout_ms`. Po dropie guard sygnalizuje wątkowi koniec (canceled)
/// i czeka na join — bez wyciekow watków. Watchdog wywoluje
/// `InterruptHandle::interrupt()`, ktory powoduje ze biezace zapytanie zwraca
/// `SQLITE_INTERRUPT` (mapowany do `AbiError::Timeout`).
struct QueryTimeoutGuard {
    canceled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl QueryTimeoutGuard {
    fn new(conn: &rusqlite::Connection, timeout_ms: u64) -> Self {
        let canceled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let canceled_clone = std::sync::Arc::clone(&canceled);
        let handle = conn.get_interrupt_handle();
        let join = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            // Krotki sleep w petli — zeby drop guarda anulował watek szybko
            // (max 50ms latencji). Dla typowego zapytania (kilka ms) watek
            // nie ma nawet okazji sprawdzic deadline.
            loop {
                if canceled_clone.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let now = Instant::now();
                if now >= deadline {
                    handle.interrupt();
                    return;
                }
                let remaining = deadline.saturating_duration_since(now);
                let step = remaining.min(Duration::from_millis(50));
                std::thread::sleep(step);
            }
        });
        Self {
            canceled,
            join: Some(join),
        }
    }
}

impl Drop for QueryTimeoutGuard {
    fn drop(&mut self) {
        self.canceled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            // Bezpieczne — wątek konczy w max 50ms.
            let _ = j.join();
        }
    }
}

// =============================================================================
// sql_exec_v1
// =============================================================================

/// Host function: wykonuje DML (INSERT/UPDATE/DELETE) z bindowanymi parametrami.
///
/// ABI: (query_ptr, query_len, params_json_ptr, params_json_len,
///       out_ptr, out_cap, out_len_ptr) -> i32
/// Output JSON: `{"rows_affected": N, "last_insert_id": M}` (last_insert_id
/// = 0 gdy operacja to UPDATE/DELETE).
pub fn sql_exec_v1(
    mut caller: WasmCaller<'_, AddonState>,
    query_ptr: i32,
    query_len: i32,
    params_json_ptr: i32,
    params_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    // Read inputs
    let query = match read_guest_string(&memory, &caller, query_ptr, query_len) {
        Some(s) => s.to_string(),
        None => return AbiError::Operation.as_i32(),
    };
    let params_json = if params_json_len > 0 {
        match read_guest_string(&memory, &caller, params_json_ptr, params_json_len) {
            Some(s) => s.to_string(),
            None => return AbiError::Operation.as_i32(),
        }
    } else {
        String::new()
    };

    // Payload size guard.
    let combined = query.len() + params_json.len();
    if enforce_payload_size(combined, PayloadKind::SqlCombined).is_err() {
        audit_log_with_risk(
            caller.data(),
            "sql.exec",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "error",
            Some("payload too large"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }

    // Permission + manifest declaration check.
    if !check_permission(caller.data(), "sql.write", None) {
        audit_log_with_risk(
            caller.data(),
            "sql.exec",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    // DDL block.
    if is_ddl(&query) {
        audit_log_with_risk(
            caller.data(),
            "sql.exec",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "denied",
            Some("DDL blocked at runtime — uzyj migrations"),
        );
        return AbiError::Permission.as_i32();
    }

    let pool = match acquire_pool(caller.data()) {
        Ok(p) => p,
        Err(e) => return e.as_i32(),
    };

    let params = match parse_params(&params_json) {
        Ok(p) => p,
        Err(e) => return e.as_i32(),
    };

    let response = match execute_dml(&pool, &query, &params) {
        Ok(r) => r,
        Err(e) => {
            audit_log_with_risk(
                caller.data(),
                "sql.exec",
                Some("sql"),
                Some(&query_hash_short(&query)),
                RiskClass::A,
                None,
                None,
                "error",
                Some(&format!("abi_error={}", e.as_i32())),
            );
            return e.as_i32();
        }
    };

    audit_log_with_risk(
        caller.data(),
        "sql.exec",
        Some("sql"),
        Some(&query_hash_short(&query)),
        RiskClass::A,
        None,
        None,
        "ok",
        None,
    );

    let bytes = match serde_json::to_vec(&response) {
        Ok(b) => b,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    write_output_with_retry_semantics(&memory, &mut caller, &bytes, out_ptr, out_cap, out_len_ptr)
}

fn execute_dml(
    pool: &AddonDbPool,
    query: &str,
    params: &[SqliteValue],
) -> Result<JsonValue, AbiError> {
    let conn = pool.get()?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);
    let bound: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let rows_affected = conn
        .execute(query, rusqlite::params_from_iter(bound.iter().copied()))
        .map_err(|e| {
            warn!("sql_exec failed: {}", e);
            map_sqlite_error(&e)
        })?;
    let last_id = conn.last_insert_rowid();
    Ok(json!({
        "rows_affected": rows_affected,
        "last_insert_id": last_id,
    }))
}

// =============================================================================
// sql_query_v1
// =============================================================================

/// Host function: wykonuje SELECT/WITH/EXPLAIN i zwraca wszystkie wiersze.
///
/// ABI: (query_ptr, query_len, params_json_ptr, params_json_len,
///       out_ptr, out_cap, out_len_ptr) -> i32
/// Output JSON: `{"columns": ["a", "b"], "rows": [[...], [...]]}`
pub fn sql_query_v1(
    mut caller: WasmCaller<'_, AddonState>,
    query_ptr: i32,
    query_len: i32,
    params_json_ptr: i32,
    params_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let query = match read_guest_string(&memory, &caller, query_ptr, query_len) {
        Some(s) => s.to_string(),
        None => return AbiError::Operation.as_i32(),
    };
    let params_json = if params_json_len > 0 {
        match read_guest_string(&memory, &caller, params_json_ptr, params_json_len) {
            Some(s) => s.to_string(),
            None => return AbiError::Operation.as_i32(),
        }
    } else {
        String::new()
    };

    if enforce_payload_size(query.len() + params_json.len(), PayloadKind::SqlCombined).is_err() {
        audit_log_with_risk(
            caller.data(),
            "sql.query",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "error",
            Some("payload too large"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }
    if !check_permission(caller.data(), "sql.read", None) {
        audit_log_with_risk(
            caller.data(),
            "sql.query",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }
    if !is_read_only(&query) || is_ddl(&query) {
        audit_log_with_risk(
            caller.data(),
            "sql.query",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "denied",
            Some("non-readonly query — uzyj sql_exec dla DML, migrations dla DDL"),
        );
        return AbiError::Permission.as_i32();
    }
    let pool = match acquire_pool(caller.data()) {
        Ok(p) => p,
        Err(e) => return e.as_i32(),
    };
    let params = match parse_params(&params_json) {
        Ok(p) => p,
        Err(e) => return e.as_i32(),
    };

    let response = match execute_select(&pool, &query, &params, None) {
        Ok(r) => r,
        Err(e) => {
            let reason = if e.as_i32() == AbiError::Permission.as_i32() {
                Some("non-readonly statement (Statement::readonly=false)")
            } else {
                None
            };
            audit_log_with_risk(
                caller.data(),
                "sql.query",
                Some("sql"),
                Some(&query_hash_short(&query)),
                RiskClass::A,
                None,
                None,
                "denied",
                reason,
            );
            return e.as_i32();
        }
    };

    audit_log_with_risk(
        caller.data(),
        "sql.query",
        Some("sql"),
        Some(&query_hash_short(&query)),
        RiskClass::A,
        None,
        None,
        "ok",
        None,
    );
    let bytes = match serde_json::to_vec(&response) {
        Ok(b) => b,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    write_output_with_retry_semantics(&memory, &mut caller, &bytes, out_ptr, out_cap, out_len_ptr)
}

fn execute_select(
    pool: &AddonDbPool,
    query: &str,
    params: &[SqliteValue],
    limit: Option<usize>,
) -> Result<JsonValue, AbiError> {
    let conn = pool.get()?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);

    let result: Result<(Vec<String>, Vec<Vec<JsonValue>>), AbiError> = (|| {
        let stmt = conn.prepare(query).map_err(|e| map_sqlite_error(&e))?;
        // Autorytatywne potwierdzenie: parser SQLite klasyfikuje statement
        // jako read-only. Uzupelnia regex-owy guard `is_read_only` (komentarze,
        // chained statements, rzadkie konstrukcje) — jesli SQLite mowi inaczej,
        // odrzucamy mimo dopasowania regex.
        if !stmt.readonly() {
            return Err(AbiError::Permission);
        }
        let mut stmt = stmt;
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let col_count = stmt.column_count();

        let bound: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

        let mut rows = stmt
            .query(rusqlite::params_from_iter(bound.iter().copied()))
            .map_err(|e| map_sqlite_error(&e))?;

        let mut out: Vec<Vec<JsonValue>> = Vec::new();
        while let Some(row) = rows.next().map_err(|e| map_sqlite_error(&e))? {
            let mut json_row = Vec::with_capacity(col_count);
            for i in 0..col_count {
                let v = row.get_ref(i).map_err(|e| map_sqlite_error(&e))?;
                json_row.push(sqlite_value_ref_to_json(v)?);
            }
            out.push(json_row);
            if let Some(max) = limit {
                if out.len() >= max {
                    break;
                }
            }
        }
        Ok((col_names, out))
    })();
    let (columns, rows) = result?;
    Ok(json!({
        "columns": columns,
        "rows": rows,
    }))
}

// =============================================================================
// sql_query_one_v1
// =============================================================================

/// Host function: wykonuje SELECT i zwraca pierwszy wiersz lub null.
///
/// ABI jak sql_query_v1. Output: `{"row": [...] }` lub `{"row": null}`.
/// Gdy zapytanie zwraca > 1 wierszy — audit warning, ale zwracamy pierwszy.
pub fn sql_query_one_v1(
    mut caller: WasmCaller<'_, AddonState>,
    query_ptr: i32,
    query_len: i32,
    params_json_ptr: i32,
    params_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let query = match read_guest_string(&memory, &caller, query_ptr, query_len) {
        Some(s) => s.to_string(),
        None => return AbiError::Operation.as_i32(),
    };
    let params_json = if params_json_len > 0 {
        match read_guest_string(&memory, &caller, params_json_ptr, params_json_len) {
            Some(s) => s.to_string(),
            None => return AbiError::Operation.as_i32(),
        }
    } else {
        String::new()
    };
    if enforce_payload_size(query.len() + params_json.len(), PayloadKind::SqlCombined).is_err() {
        audit_log_with_risk(
            caller.data(),
            "sql.query_one",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "error",
            Some("payload too large"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }
    if !check_permission(caller.data(), "sql.read", None) {
        audit_log_with_risk(
            caller.data(),
            "sql.query_one",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }
    if !is_read_only(&query) || is_ddl(&query) {
        audit_log_with_risk(
            caller.data(),
            "sql.query_one",
            Some("sql"),
            Some(&query_hash_short(&query)),
            RiskClass::A,
            None,
            None,
            "denied",
            Some("non-readonly query — uzyj sql_exec dla DML, migrations dla DDL"),
        );
        return AbiError::Permission.as_i32();
    }
    let pool = match acquire_pool(caller.data()) {
        Ok(p) => p,
        Err(e) => return e.as_i32(),
    };
    let params = match parse_params(&params_json) {
        Ok(p) => p,
        Err(e) => return e.as_i32(),
    };

    // Pobierz 2 wiersze max (potrzebne do detekcji "wiecej niz 1").
    let result = match execute_select(&pool, &query, &params, Some(2)) {
        Ok(r) => r,
        Err(e) => {
            let reason = if e.as_i32() == AbiError::Permission.as_i32() {
                Some("non-readonly statement (Statement::readonly=false)")
            } else {
                None
            };
            audit_log_with_risk(
                caller.data(),
                "sql.query_one",
                Some("sql"),
                Some(&query_hash_short(&query)),
                RiskClass::A,
                None,
                None,
                "denied",
                reason,
            );
            return e.as_i32();
        }
    };
    let rows = result.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let response = if rows.is_empty() {
        json!({ "row": JsonValue::Null })
    } else {
        if rows.len() > 1 {
            warn!(
                "addon '{}': sql_query_one zwrocilo {} wierszy — zwracam pierwszy",
                caller.data().addon_id,
                rows.len()
            );
        }
        json!({ "row": rows[0].clone() })
    };

    audit_log_with_risk(
        caller.data(),
        "sql.query_one",
        Some("sql"),
        Some(&query_hash_short(&query)),
        RiskClass::A,
        None,
        None,
        "ok",
        None,
    );

    let bytes = match serde_json::to_vec(&response) {
        Ok(b) => b,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    write_output_with_retry_semantics(&memory, &mut caller, &bytes, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// sql_transaction_v1
// =============================================================================

/// Host function: wykonuje liste DML statementow atomowo.
///
/// ABI: (statements_json_ptr, statements_json_len, out_ptr, out_cap, out_len_ptr) -> i32
/// Input: `{"statements": [{"query": "...", "params": [...]}, ...]}`
/// Output: `{"rows_affected_total": N}`
/// Wszystkie statementy w jednej transakcji — fail któregokolwiek → rollback.
pub fn sql_transaction_v1(
    mut caller: WasmCaller<'_, AddonState>,
    statements_json_ptr: i32,
    statements_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let statements_json =
        match read_guest_string(&memory, &caller, statements_json_ptr, statements_json_len) {
            Some(s) => s.to_string(),
            None => return AbiError::Operation.as_i32(),
        };
    if enforce_payload_size(statements_json.len(), PayloadKind::SqlCombined).is_err() {
        audit_log_with_risk(
            caller.data(),
            "sql.transaction",
            Some("sql"),
            Some(&query_hash_short(&statements_json)),
            RiskClass::A,
            None,
            None,
            "error",
            Some("payload too large"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }
    if !check_permission(caller.data(), "sql.write", None) {
        audit_log_with_risk(
            caller.data(),
            "sql.transaction",
            Some("sql"),
            Some(&query_hash_short(&statements_json)),
            RiskClass::A,
            None,
            None,
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    // Parse wejscia.
    let payload: JsonValue = match serde_json::from_str(&statements_json) {
        Ok(v) => v,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    let stmts = match payload.get("statements").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return AbiError::Operation.as_i32(),
    };

    // Pre-walidacja: zaden statement nie moze byc DDL.
    for s in stmts {
        let q = s.get("query").and_then(|v| v.as_str()).unwrap_or("");
        if is_ddl(q) {
            audit_log_with_risk(
                caller.data(),
                "sql.transaction",
                Some("sql"),
                Some(&query_hash_short(&statements_json)),
                RiskClass::A,
                None,
                None,
                "denied",
                Some("DDL w transakcji blocked"),
            );
            return AbiError::Permission.as_i32();
        }
    }

    let pool = match acquire_pool(caller.data()) {
        Ok(p) => p,
        Err(e) => return e.as_i32(),
    };

    let result = execute_transaction(&pool, stmts);
    let response = match result {
        Ok(total) => json!({ "rows_affected_total": total }),
        Err(e) => {
            audit_log_with_risk(
                caller.data(),
                "sql.transaction",
                Some("sql"),
                Some(&query_hash_short(&statements_json)),
                RiskClass::A,
                None,
                None,
                "error",
                Some(&format!("abi_error={}", e.as_i32())),
            );
            return e.as_i32();
        }
    };

    audit_log_with_risk(
        caller.data(),
        "sql.transaction",
        Some("sql"),
        Some(&query_hash_short(&statements_json)),
        RiskClass::A,
        None,
        None,
        "ok",
        Some(&format!("statements={}", stmts.len())),
    );
    let bytes = match serde_json::to_vec(&response) {
        Ok(b) => b,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    write_output_with_retry_semantics(&memory, &mut caller, &bytes, out_ptr, out_cap, out_len_ptr)
}

fn execute_transaction(pool: &AddonDbPool, stmts: &[JsonValue]) -> Result<i64, AbiError> {
    let mut conn = pool.get()?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);

    let result: Result<i64, AbiError> = (|| {
        let mut tx = conn
            .transaction()
            .map_err(|e| map_sqlite_error(&e))?;
        // Domyslnie rusqlite ustawia DropBehavior::Rollback, ale ustawiamy
        // jawnie — zmiana defaultu w upstream nie moze cicho skutkowac
        // partial commit przy panice w petli statementow.
        tx.set_drop_behavior(rusqlite::DropBehavior::Rollback);
        let mut total: i64 = 0;
        for s in stmts {
            let query = s.get("query").and_then(|v| v.as_str()).ok_or(AbiError::Operation)?;
            let params_val = s.get("params").cloned().unwrap_or(JsonValue::Array(Vec::new()));
            let params_json = serde_json::to_string(&params_val).map_err(|_| AbiError::Operation)?;
            let params = parse_params(&params_json)?;
            let bound: Vec<&dyn rusqlite::ToSql> =
                params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
            let n = tx
                .execute(query, rusqlite::params_from_iter(bound.iter().copied()))
                .map_err(|e| map_sqlite_error(&e))?;
            total += n as i64;
        }
        tx.commit().map_err(|e| map_sqlite_error(&e))?;
        Ok(total)
    })();
    result
}

// =============================================================================
// Testy jednostkowe — czyste funkcje pomocnicze
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddl_detection() {
        assert!(is_ddl("CREATE TABLE x (id INTEGER)"));
        assert!(is_ddl("  create table foo (id int)"));
        assert!(is_ddl("DROP TABLE x"));
        assert!(is_ddl("ALTER TABLE x ADD COLUMN y INTEGER"));
        assert!(is_ddl("PRAGMA journal_mode=DELETE"));
        assert!(is_ddl("VACUUM"));
        assert!(!is_ddl("SELECT * FROM x"));
        assert!(!is_ddl("INSERT INTO x VALUES (1)"));
        assert!(!is_ddl("UPDATE x SET y=1"));
        assert!(!is_ddl("DELETE FROM x"));
    }

    #[test]
    fn readonly_detection() {
        assert!(is_read_only("SELECT * FROM x"));
        assert!(is_read_only("  select 1"));
        assert!(is_read_only("WITH cte AS (SELECT 1) SELECT * FROM cte"));
        assert!(is_read_only("EXPLAIN SELECT 1"));
        assert!(!is_read_only("INSERT INTO x VALUES (1)"));
        assert!(!is_read_only("UPDATE x SET y=1"));
        assert!(!is_read_only("DELETE FROM x"));
    }

    #[test]
    fn json_to_value_conversions() {
        assert!(matches!(json_to_sqlite_value(&JsonValue::Null).unwrap(), SqliteValue::Null));
        assert!(matches!(
            json_to_sqlite_value(&JsonValue::Bool(true)).unwrap(),
            SqliteValue::Integer(1)
        ));
        assert!(matches!(
            json_to_sqlite_value(&JsonValue::Bool(false)).unwrap(),
            SqliteValue::Integer(0)
        ));
        assert!(matches!(
            json_to_sqlite_value(&serde_json::json!(42)).unwrap(),
            SqliteValue::Integer(42)
        ));
        assert!(matches!(
            json_to_sqlite_value(&serde_json::json!(2.5)).unwrap(),
            SqliteValue::Real(_)
        ));
        assert!(matches!(
            json_to_sqlite_value(&serde_json::json!("hello")).unwrap(),
            SqliteValue::Text(_)
        ));
        let blob = json_to_sqlite_value(&serde_json::json!({"$bytes": "aGVsbG8="})).unwrap();
        match blob {
            SqliteValue::Blob(b) => assert_eq!(b, b"hello"),
            _ => panic!("oczekiwano BLOB"),
        }
    }

    #[test]
    fn json_to_value_array_rejected() {
        assert!(json_to_sqlite_value(&serde_json::json!([1, 2, 3])).is_err());
    }

    #[test]
    fn parse_params_empty() {
        assert!(parse_params("").unwrap().is_empty());
    }

    #[test]
    fn parse_params_array() {
        let p = parse_params(r#"["a", 1, true, null]"#).unwrap();
        assert_eq!(p.len(), 4);
    }

    #[test]
    fn query_hash_is_stable() {
        let q = "SELECT * FROM items WHERE id = ?";
        let h1 = query_hash_short(q);
        let h2 = query_hash_short(q);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16); // 8 bajtow = 16 hex chars.
    }

    #[test]
    fn strip_leading_noise_handles_comments() {
        assert_eq!(strip_leading_noise("SELECT 1"), "SELECT 1");
        assert_eq!(strip_leading_noise("   SELECT 1"), "SELECT 1");
        assert_eq!(strip_leading_noise("-- foo\nSELECT 1"), "SELECT 1");
        assert_eq!(strip_leading_noise("/* foo */ SELECT 1"), "SELECT 1");
        assert_eq!(
            strip_leading_noise("-- a\n/* b */ SELECT 1"),
            "SELECT 1"
        );
        assert_eq!(strip_leading_noise("  /* a */-- b\n  /* c */SELECT"), "SELECT");
    }

    #[test]
    fn is_ddl_blocks_comment_prefixed_ddl() {
        // Issue #1: bez `strip_leading_noise` ponizsze payload-y przechodzily.
        assert!(is_ddl("-- evil\nCREATE TABLE x (id INTEGER)"));
        assert!(is_ddl("/* evil */ CREATE TABLE x (id INTEGER)"));
        assert!(is_ddl("/* a */ -- b\nDROP TABLE items"));
        assert!(is_ddl("  -- foo\n  /* bar */ALTER TABLE x ADD COLUMN y"));
        // Sanity: zwykly DML nadal nie jest DDL nawet z komentarzem.
        assert!(!is_ddl("-- comment\nINSERT INTO x VALUES (1)"));
        assert!(!is_ddl("/* c */ SELECT 1"));
    }

    #[test]
    fn is_read_only_handles_leading_comments() {
        assert!(is_read_only("-- foo\nSELECT 1"));
        assert!(is_read_only("/* foo */ WITH t AS (SELECT 1) SELECT * FROM t"));
        assert!(!is_read_only("/* foo */ INSERT INTO x VALUES (1)"));
    }

    #[test]
    fn nan_real_value_returns_error() {
        // Issue #4: NaN/Inf nie maja reprezentacji w JSON — wczesniej cicho
        // mapowane na NULL (data loss). Teraz Err(AbiError::Operation).
        let result = sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(f64::NAN));
        assert!(matches!(result, Err(AbiError::Operation)));
        let result_inf = sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(f64::INFINITY));
        assert!(matches!(result_inf, Err(AbiError::Operation)));
        let result_neg_inf =
            sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(f64::NEG_INFINITY));
        assert!(matches!(result_neg_inf, Err(AbiError::Operation)));
        // Wartosci skonczone konwertuja sie normalnie.
        let result_ok = sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(1.5));
        assert!(matches!(result_ok, Ok(JsonValue::Number(_))));
        let result_zero = sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(0.0));
        assert!(matches!(result_zero, Ok(JsonValue::Number(_))));
    }
}
