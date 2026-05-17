// =============================================================================
// File: addon/storage_sql_exec.rs — pure-async per-addon SQL exec/query
// =============================================================================
//
// Lifts the DDL guard + parameter bind + read-only enforcement + timeout
// watchdog out of `host_functions/sql.rs` into a WASM-free API. Permission
// checks remain the caller's responsibility (the WASM wrapper checks
// `sql.read` / `sql.write` against `AddonState`; flow operators check
// against the manifest before reaching here). Manifest declaration of
// `[storage] sql = true` is still verified by `acquire_pool` because it's
// a storage-level invariant, not a permission check.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use base64::Engine;
use regex::Regex;
use rusqlite::types::{Value as SqliteValue, ValueRef};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::warn;

use crate::addon::errors::AbiError;
use crate::addon::storage_sql::{get_addon_pool, AddonDbPool};

const QUERY_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Error)]
pub enum StorageSqlError {
    #[error("manifest does not declare [storage] sql = true")]
    NotDeclared,
    #[error("DDL blocked at runtime — use migrations")]
    DdlBlocked,
    #[error("query is not read-only")]
    NotReadOnly,
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("sql syntax error")]
    SqlSyntax,
    #[error("sql constraint violation")]
    SqlConstraint,
    #[error("operation timeout")]
    Timeout,
    #[error("internal sql error: {0}")]
    Internal(String),
}

impl StorageSqlError {
    /// Maps to the WASM ABI error code. Used by the host wrappers to keep
    /// the existing AbiError surface stable.
    pub fn as_abi(&self) -> AbiError {
        match self {
            StorageSqlError::NotDeclared => AbiError::Permission,
            StorageSqlError::DdlBlocked => AbiError::Permission,
            StorageSqlError::NotReadOnly => AbiError::Permission,
            StorageSqlError::InvalidParams(_) => AbiError::Operation,
            StorageSqlError::SqlSyntax => AbiError::SqlSyntax,
            StorageSqlError::SqlConstraint => AbiError::SqlConstraint,
            StorageSqlError::Timeout => AbiError::Timeout,
            StorageSqlError::Internal(_) => AbiError::Operation,
        }
    }
}

fn ddl_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"(?i)^(CREATE|ALTER|DROP|TRUNCATE|REINDEX|VACUUM|ATTACH|DETACH|PRAGMA)\b")
            .expect("ddl regex stale poprawny")
    })
}

fn read_only_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"(?i)^(SELECT|WITH|EXPLAIN)\b").expect("read-only regex stale poprawny")
    })
}

fn strip_leading_noise(q: &str) -> &str {
    let mut s = q.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = match rest.split_once('\n') {
                Some((_, after)) => after.trim_start(),
                None => "",
            };
        } else if let Some(rest) = s.strip_prefix("/*") {
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

pub fn is_ddl(query: &str) -> bool {
    ddl_regex().is_match(strip_leading_noise(query))
}

pub fn is_read_only(query: &str) -> bool {
    read_only_regex().is_match(strip_leading_noise(query))
}

pub fn query_hash_short(q: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(q.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

pub fn json_to_sqlite_value(v: &JsonValue) -> Result<SqliteValue, StorageSqlError> {
    match v {
        JsonValue::Null => Ok(SqliteValue::Null),
        JsonValue::Bool(b) => Ok(SqliteValue::Integer(if *b { 1 } else { 0 })),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(SqliteValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(SqliteValue::Real(f))
            } else {
                Err(StorageSqlError::InvalidParams("invalid number".into()))
            }
        }
        JsonValue::String(s) => Ok(SqliteValue::Text(s.clone())),
        JsonValue::Object(obj) => {
            if let Some(JsonValue::String(b64)) = obj.get("$bytes") {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|_| StorageSqlError::InvalidParams("invalid base64".into()))?;
                Ok(SqliteValue::Blob(bytes))
            } else {
                Err(StorageSqlError::InvalidParams("unknown object shape".into()))
            }
        }
        JsonValue::Array(_) => Err(StorageSqlError::InvalidParams("array not allowed".into())),
    }
}

pub fn parse_params(params_json: &str) -> Result<Vec<SqliteValue>, StorageSqlError> {
    if params_json.is_empty() {
        return Ok(Vec::new());
    }
    let v: JsonValue = serde_json::from_str(params_json)
        .map_err(|e| StorageSqlError::InvalidParams(e.to_string()))?;
    let arr = v
        .as_array()
        .ok_or_else(|| StorageSqlError::InvalidParams("expected array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        out.push(json_to_sqlite_value(item)?);
    }
    Ok(out)
}

fn sqlite_value_ref_to_json(v: ValueRef<'_>) -> Result<JsonValue, StorageSqlError> {
    Ok(match v {
        ValueRef::Null => JsonValue::Null,
        ValueRef::Integer(i) => JsonValue::from(i),
        ValueRef::Real(f) => match serde_json::Number::from_f64(f) {
            Some(n) => JsonValue::Number(n),
            None => return Err(StorageSqlError::Internal("nan/inf in result".into())),
        },
        ValueRef::Text(t) => JsonValue::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(b);
            json!({ "$bytes": b64 })
        }
    })
}

fn map_sqlite_error(e: &rusqlite::Error) -> StorageSqlError {
    if let rusqlite::Error::SqliteFailure(code, _) = e {
        match code.code {
            rusqlite::ErrorCode::ConstraintViolation => return StorageSqlError::SqlConstraint,
            rusqlite::ErrorCode::OperationInterrupted => return StorageSqlError::Timeout,
            _ => {}
        }
    }
    let s = e.to_string().to_lowercase();
    if s.contains("syntax")
        || s.contains("near")
        || s.contains("no such table")
        || s.contains("no such column")
    {
        StorageSqlError::SqlSyntax
    } else if s.contains("interrupted") {
        StorageSqlError::Timeout
    } else {
        StorageSqlError::Internal(e.to_string())
    }
}

fn acquire_pool(org_id: &str, addon_id: &str) -> Result<AddonDbPool, StorageSqlError> {
    get_addon_pool(org_id, addon_id).ok_or(StorageSqlError::NotDeclared)
}

/// Guard, ktory uruchamia watchdog thread przerywajacy zapytanie SQL po
/// uplywie `timeout_ms`. Drop guarda anuluje watek bez wycieku.
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
            let _ = j.join();
        }
    }
}

// =============================================================================
// Public pure-async surface — flow_runtime operators wolają bezposrednio.
// Permission check (sql.read / sql.write) jest odpowiedzialnoscia callera.
// =============================================================================

/// DML (INSERT/UPDATE/DELETE). Returns `(rows_affected, last_insert_id)`.
pub fn exec_for_addon(
    org_id: &str,
    addon_id: &str,
    query: &str,
    params: &[JsonValue],
) -> Result<(u64, i64), StorageSqlError> {
    if is_ddl(query) {
        return Err(StorageSqlError::DdlBlocked);
    }
    let pool = acquire_pool(org_id, addon_id)?;
    let params: Vec<SqliteValue> = params
        .iter()
        .map(json_to_sqlite_value)
        .collect::<Result<_, _>>()?;
    let conn = pool.get().map_err(abi_to_storage)?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);
    let bound: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let rows = conn
        .execute(query, rusqlite::params_from_iter(bound.iter().copied()))
        .map_err(|e| {
            warn!("storage_sql_exec: {}", e);
            map_sqlite_error(&e)
        })?;
    let last_id = conn.last_insert_rowid();
    Ok((rows as u64, last_id))
}

/// SELECT — wszystkie wiersze. `limit` ogranicza w pamieci (None = bez limitu).
pub fn query_for_addon(
    org_id: &str,
    addon_id: &str,
    query: &str,
    params: &[JsonValue],
    limit: Option<usize>,
) -> Result<JsonValue, StorageSqlError> {
    if is_ddl(query) || !is_read_only(query) {
        return Err(StorageSqlError::NotReadOnly);
    }
    let pool = acquire_pool(org_id, addon_id)?;
    let params: Vec<SqliteValue> = params
        .iter()
        .map(json_to_sqlite_value)
        .collect::<Result<_, _>>()?;
    let conn = pool.get().map_err(abi_to_storage)?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);

    let stmt = conn.prepare(query).map_err(|e| map_sqlite_error(&e))?;
    if !stmt.readonly() {
        return Err(StorageSqlError::NotReadOnly);
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
    Ok(json!({ "columns": col_names, "rows": out }))
}

/// SELECT zwracajacy pierwszy wiersz lub `null`.
pub fn query_one_for_addon(
    org_id: &str,
    addon_id: &str,
    query: &str,
    params: &[JsonValue],
) -> Result<JsonValue, StorageSqlError> {
    let full = query_for_addon(org_id, addon_id, query, params, Some(2))?;
    let rows = full
        .get("rows")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(if rows.is_empty() {
        json!({ "row": JsonValue::Null })
    } else {
        json!({ "row": rows[0].clone() })
    })
}

/// Atomic batch DML. Returns total `rows_affected`.
pub fn transaction_for_addon(
    org_id: &str,
    addon_id: &str,
    statements: &[(String, Vec<JsonValue>)],
) -> Result<i64, StorageSqlError> {
    for (q, _) in statements {
        if is_ddl(q) {
            return Err(StorageSqlError::DdlBlocked);
        }
    }
    let pool = acquire_pool(org_id, addon_id)?;
    let mut conn = pool.get().map_err(abi_to_storage)?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);
    let mut tx = conn.transaction().map_err(|e| map_sqlite_error(&e))?;
    tx.set_drop_behavior(rusqlite::DropBehavior::Rollback);
    let mut total: i64 = 0;
    for (q, params) in statements {
        let params: Vec<SqliteValue> = params
            .iter()
            .map(json_to_sqlite_value)
            .collect::<Result<_, _>>()?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let n = tx
            .execute(q, rusqlite::params_from_iter(bound.iter().copied()))
            .map_err(|e| map_sqlite_error(&e))?;
        total += n as i64;
    }
    tx.commit().map_err(|e| map_sqlite_error(&e))?;
    Ok(total)
}

fn abi_to_storage(e: AbiError) -> StorageSqlError {
    match e {
        AbiError::Timeout => StorageSqlError::Timeout,
        _ => StorageSqlError::Internal(format!("pool: {:?}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nan_real_value_returns_error() {
        assert!(matches!(
            sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(f64::NAN)),
            Err(StorageSqlError::Internal(_))
        ));
        assert!(matches!(
            sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(f64::INFINITY)),
            Err(StorageSqlError::Internal(_))
        ));
        assert!(matches!(
            sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(f64::NEG_INFINITY)),
            Err(StorageSqlError::Internal(_))
        ));
        assert!(matches!(
            sqlite_value_ref_to_json(rusqlite::types::ValueRef::Real(1.5)),
            Ok(JsonValue::Number(_))
        ));
    }
}
