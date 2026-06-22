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
use rusqlite::OptionalExtension;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::warn;

use crate::addon::errors::AbiError;
use crate::addon::lifecycle::parse_manifest_toml;
use crate::addon::storage_sql::{get_addon_pool, open_addon_db, AddonDbPool};
use crate::db::{repository, DbPool};
use crate::sync::ledger::OperationId;
use crate::sync::runtime::{self as sync_runtime, SqlWriteAction, SqlWriteCapture};

const QUERY_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncConflictResolution {
    KeepLocal,
    Ignore,
    AcceptRemote,
}

impl SyncConflictResolution {
    fn as_str(&self) -> &'static str {
        match self {
            SyncConflictResolution::KeepLocal => "keep_local",
            SyncConflictResolution::Ignore => "ignore",
            SyncConflictResolution::AcceptRemote => "accept_remote",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncConflictRow {
    pub operation_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub table_name: String,
    pub resource_type: String,
    pub resource_id: String,
    pub action: String,
    pub source_node_id: String,
    pub error_kind: String,
    pub error_message: String,
    pub status: String,
    pub created_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
    pub resolution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConflictResolveResult {
    pub operation_id: String,
    pub status: String,
    pub resolution: String,
    pub rows_affected: u64,
}

#[derive(Debug, Error)]
pub enum StorageSqlError {
    #[error("manifest does not declare [storage] sql = true")]
    NotDeclared,
    #[error("DDL blocked at runtime — use migrations")]
    DdlBlocked,
    #[error("query is not read-only")]
    NotReadOnly,
    #[error("internal TentaFlow table is not writable by addons")]
    InternalTableBlocked,
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("sql syntax error")]
    SqlSyntax,
    #[error("sql constraint violation")]
    SqlConstraint,
    /// A causal-ordering gap, not a data conflict: the replicated write could not
    /// be applied yet because the row it depends on does not exist. Two shapes:
    /// a FOREIGN KEY violation (missing parent row) or an UPDATE that matched no
    /// row (the INSERT that creates the target has not been replicated yet). The
    /// inbox must keep the entry retryable so a later drain applies it once the
    /// prerequisite arrives — never recorded as a sync conflict.
    #[error("replicated write deferred (missing prerequisite row): {0}")]
    OrderingGap(String),
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
            StorageSqlError::InternalTableBlocked => AbiError::Permission,
            StorageSqlError::InvalidParams(_) => AbiError::Operation,
            StorageSqlError::SqlSyntax => AbiError::SqlSyntax,
            StorageSqlError::SqlConstraint => AbiError::SqlConstraint,
            StorageSqlError::OrderingGap(_) => AbiError::Operation,
            StorageSqlError::Timeout => AbiError::Timeout,
            StorageSqlError::Internal(_) => AbiError::Operation,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            StorageSqlError::NotDeclared => "not_declared",
            StorageSqlError::DdlBlocked => "ddl_blocked",
            StorageSqlError::NotReadOnly => "not_write_statement",
            StorageSqlError::InternalTableBlocked => "internal_table_blocked",
            StorageSqlError::InvalidParams(_) => "invalid_params",
            StorageSqlError::SqlSyntax => "sql_syntax",
            StorageSqlError::SqlConstraint => "sql_constraint",
            StorageSqlError::OrderingGap(_) => "ordering_gap",
            StorageSqlError::Timeout => "timeout",
            StorageSqlError::Internal(_) => "internal",
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

fn insert_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r#"(?i)^(?:INSERT(?:\s+OR\s+\w+)?\s+INTO|REPLACE\s+INTO)\s+["`\[]?([A-Za-z_][A-Za-z0-9_]*)["`\]]?"#)
            .expect("insert regex stale poprawny")
    })
}

fn update_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r#"(?i)^UPDATE\s+["`\[]?([A-Za-z_][A-Za-z0-9_]*)["`\]]?"#)
            .expect("update regex stale poprawny")
    })
}

fn delete_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r#"(?i)^DELETE\s+FROM\s+["`\[]?([A-Za-z_][A-Za-z0-9_]*)["`\]]?"#)
            .expect("delete regex stale poprawny")
    })
}

fn select_table_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r#"(?i)\bFROM\s+["`\[]?([A-Za-z_][A-Za-z0-9_]*)["`\]]?"#)
            .expect("select table regex stale poprawny")
    })
}

fn identifier_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("identifier regex stale poprawny")
    })
}

pub fn resource_type_for_query(query: &str) -> Option<String> {
    select_table_regex()
        .captures(query.trim())
        .and_then(|captures| captures.get(1))
        .map(|table| table.as_str().to_string())
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

pub fn parse_write_action_and_table(
    query: &str,
) -> Result<(SqlWriteAction, String), StorageSqlError> {
    let q = strip_leading_noise(query);
    if let Some(caps) = insert_regex().captures(q) {
        return Ok((SqlWriteAction::Insert, caps[1].to_string()));
    }
    if let Some(caps) = update_regex().captures(q) {
        return Ok((SqlWriteAction::Update, caps[1].to_string()));
    }
    if let Some(caps) = delete_regex().captures(q) {
        return Ok((SqlWriteAction::Delete, caps[1].to_string()));
    }
    Err(StorageSqlError::NotReadOnly)
}

fn reject_internal_table(table_name: &str) -> Result<(), StorageSqlError> {
    if table_name.starts_with("__tentaflow_") {
        Err(StorageSqlError::InternalTableBlocked)
    } else {
        Ok(())
    }
}

fn quote_identifier(identifier: &str) -> Result<String, StorageSqlError> {
    if identifier_regex().is_match(identifier) {
        Ok(format!("\"{identifier}\""))
    } else {
        Err(StorageSqlError::Internal(
            "invalid sqlite identifier".to_string(),
        ))
    }
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
                Err(StorageSqlError::InvalidParams(
                    "unknown object shape".into(),
                ))
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

/// Classifies a SQLite error raised while applying a REPLICATED write. Unlike
/// `map_sqlite_error` (addon-driven local writes, where every constraint is a
/// real error to surface), a FOREIGN KEY violation here means the parent row has
/// not been replicated yet — a causal-ordering gap that must stay retryable, not
/// a data conflict. Every other constraint (UNIQUE / PRIMARY KEY / CHECK /
/// NOT NULL) is a genuine conflict in the replicated payload and stays terminal.
fn map_replicated_sqlite_error(e: &rusqlite::Error) -> StorageSqlError {
    if let rusqlite::Error::SqliteFailure(code, _) = e {
        if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY {
            return StorageSqlError::OrderingGap(format!("foreign key not yet present: {e}"));
        }
    }
    map_sqlite_error(e)
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
    actor_user_id: Option<String>,
) -> Result<(u64, i64), StorageSqlError> {
    if is_ddl(query) {
        return Err(StorageSqlError::DdlBlocked);
    }
    let (action, table_name) = parse_write_action_and_table(query)?;
    reject_internal_table(&table_name)?;
    let pool = acquire_pool(org_id, addon_id)?;
    let sqlite_params: Vec<SqliteValue> = params
        .iter()
        .map(json_to_sqlite_value)
        .collect::<Result<_, _>>()?;
    let mut conn = pool.get().map_err(abi_to_storage)?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);
    let bound: Vec<&dyn rusqlite::ToSql> = sqlite_params
        .iter()
        .map(|v| v as &dyn rusqlite::ToSql)
        .collect();
    let tx = conn.transaction().map_err(|e| map_sqlite_error(&e))?;
    let rows = tx
        .execute(query, rusqlite::params_from_iter(bound.iter().copied()))
        .map_err(|e| {
            warn!("storage_sql_exec: {}", e);
            map_sqlite_error(&e)
        })?;
    let last_id = tx.last_insert_rowid();
    let capture = build_capture(
        org_id,
        addon_id,
        &table_name,
        action,
        query,
        params,
        rows as u64,
        last_id,
        actor_user_id,
    )?;
    insert_capture(&tx, &capture)?;
    tx.commit().map_err(|e| map_sqlite_error(&e))?;
    publish_capture(&conn, &capture);
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
    actor_user_id: Option<String>,
) -> Result<i64, StorageSqlError> {
    for (q, _) in statements {
        if is_ddl(q) {
            return Err(StorageSqlError::DdlBlocked);
        }
        let (_, table_name) = parse_write_action_and_table(q)?;
        reject_internal_table(&table_name)?;
    }
    let pool = acquire_pool(org_id, addon_id)?;
    let mut conn = pool.get().map_err(abi_to_storage)?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);
    // BEGIN IMMEDIATE (nie domyslny DEFERRED): bierze write-lock juz na starcie tx, wiec
    // dwie rownolegle transakcje tej instancji serializuja sie OD POCZATKU, a nie dopiero
    // na pierwszym zapisie. To jest fundament exactly-once aktywacji w reconcile_schemas:
    // warunkowany enqueue (INSERT ... WHERE EXISTS active=0) i warunkowany flip
    // (UPDATE ... WHERE active=0) drugiego reconcile widza juz active=1 i nie powtarzaja
    // materializacji. Bez IMMEDIATE oba reconcile czytalyby active=0 przed kolizja.
    let mut tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| map_sqlite_error(&e))?;
    tx.set_drop_behavior(rusqlite::DropBehavior::Rollback);
    let mut total: i64 = 0;
    let mut captures = Vec::with_capacity(statements.len());
    for (q, params_json_values) in statements {
        let (action, table_name) = parse_write_action_and_table(q)?;
        let params: Vec<SqliteValue> = params_json_values
            .iter()
            .map(json_to_sqlite_value)
            .collect::<Result<_, _>>()?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let n = tx
            .execute(q, rusqlite::params_from_iter(bound.iter().copied()))
            .map_err(|e| map_sqlite_error(&e))?;
        total += n as i64;
        let last_id = tx.last_insert_rowid();
        let capture = build_capture(
            org_id,
            addon_id,
            &table_name,
            action,
            q,
            params_json_values,
            n as u64,
            last_id,
            actor_user_id.clone(),
        )?;
        insert_capture(&tx, &capture)?;
        captures.push(capture);
    }
    tx.commit().map_err(|e| map_sqlite_error(&e))?;
    for capture in captures {
        publish_capture(&conn, &capture);
    }
    Ok(total)
}

pub fn apply_replicated_write(
    capture: &SqlWriteCapture,
    operation_id: OperationId,
) -> Result<u64, StorageSqlError> {
    apply_replicated_write_with_resolution(capture, operation_id, false)
}

fn apply_replicated_write_with_resolution(
    capture: &SqlWriteCapture,
    operation_id: OperationId,
    accept_remote: bool,
) -> Result<u64, StorageSqlError> {
    if is_ddl(&capture.query) {
        return Err(StorageSqlError::DdlBlocked);
    }
    let (action, table_name) = parse_write_action_and_table(&capture.query)?;
    if action.as_str() != capture.action.as_str() || table_name != capture.table_name {
        return Err(StorageSqlError::Internal(
            "replicated capture metadata mismatch".to_string(),
        ));
    }
    reject_internal_table(&table_name)?;
    open_addon_db(&capture.org_id, &capture.addon_id).map_err(abi_to_storage)?;
    let pool = acquire_pool(&capture.org_id, &capture.addon_id)?;
    let sqlite_params: Vec<SqliteValue> = capture
        .params
        .iter()
        .map(json_to_sqlite_value)
        .collect::<Result<_, _>>()?;
    let mut conn = pool.get().map_err(abi_to_storage)?;
    let _timeout = QueryTimeoutGuard::new(&conn, QUERY_TIMEOUT_MS);
    let bound: Vec<&dyn rusqlite::ToSql> = sqlite_params
        .iter()
        .map(|v| v as &dyn rusqlite::ToSql)
        .collect();
    let tx = conn.transaction().map_err(|e| map_sqlite_error(&e))?;
    let operation_id_hex = operation_id.to_hex();
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO __tentaflow_sync_applied (operation_id, applied_at_ms) \
             VALUES (?1, ?2)",
            rusqlite::params![operation_id_hex, sync_runtime::now_ms()],
        )
        .map_err(|e| map_sqlite_error(&e))?;
    if inserted == 0 {
        tx.commit().map_err(|e| map_sqlite_error(&e))?;
        return Ok(0);
    }
    if accept_remote && matches!(capture.action, SqlWriteAction::Insert) {
        delete_existing_primary_key(&tx, capture)?;
    }
    let rows = tx
        .execute(
            &capture.query,
            rusqlite::params_from_iter(bound.iter().copied()),
        )
        .map_err(|e| map_replicated_sqlite_error(&e))?;
    // An UPDATE that matched no row means the INSERT creating the target has not
    // been replicated yet — a causal-ordering gap, deferred (not a conflict).
    // A DELETE matching no row is an idempotent no-op success: the row is already
    // absent, so the delete's intent ("ensure this row is gone") is satisfied and
    // there is nothing to wait for. Manual `accept_remote` resolution is exempt:
    // the operator explicitly chose to apply this payload, so a no-op UPDATE there
    // must surface as such rather than re-defer.
    if !accept_remote && rows == 0 && matches!(capture.action, SqlWriteAction::Update) {
        return Err(StorageSqlError::OrderingGap(format!(
            "update target row not present yet: {}/{}",
            capture.table_name, capture.resource_id
        )));
    }
    tx.commit().map_err(|e| map_sqlite_error(&e))?;
    Ok(rows as u64)
}

fn delete_existing_primary_key(
    tx: &rusqlite::Transaction<'_>,
    capture: &SqlWriteCapture,
) -> Result<(), StorageSqlError> {
    let Some(primary_key) = primary_key_column(tx, &capture.table_name)? else {
        return Err(StorageSqlError::Internal(format!(
            "table {} has no primary key for accept_remote",
            capture.table_name
        )));
    };
    let table = quote_identifier(&capture.table_name)?;
    let column = quote_identifier(&primary_key)?;
    let sql = format!("DELETE FROM {table} WHERE {column} = ?1");
    let primary_key_value = match capture.params.first() {
        Some(value) => json_to_sqlite_value(value)?,
        None => SqliteValue::Text(capture.resource_id.clone()),
    };
    tx.execute(&sql, rusqlite::params![primary_key_value])
        .map_err(|e| map_sqlite_error(&e))?;
    Ok(())
}

fn primary_key_column(
    tx: &rusqlite::Transaction<'_>,
    table_name: &str,
) -> Result<Option<String>, StorageSqlError> {
    let table = quote_identifier(table_name)?;
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = tx.prepare(&pragma).map_err(|e| map_sqlite_error(&e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
        })
        .map_err(|e| map_sqlite_error(&e))?;
    for row in rows {
        let (name, pk) = row.map_err(|e| map_sqlite_error(&e))?;
        if pk > 0 {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

pub fn record_sync_conflict(
    capture: &SqlWriteCapture,
    operation_id: OperationId,
    source_node_id: &str,
    error: &StorageSqlError,
) -> Result<(), StorageSqlError> {
    open_addon_db(&capture.org_id, &capture.addon_id).map_err(abi_to_storage)?;
    let pool = acquire_pool(&capture.org_id, &capture.addon_id)?;
    let conn = pool.get().map_err(abi_to_storage)?;
    let capture_json = serde_json::to_string(capture)
        .map_err(|e| StorageSqlError::Internal(format!("conflict capture: {e}")))?;
    conn.execute(
        "INSERT OR IGNORE INTO __tentaflow_sync_conflicts \
         (operation_id, org_id, addon_id, table_name, resource_type, resource_id, action, \
          source_node_id, error_kind, error_message, capture_json, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            operation_id.to_hex(),
            &capture.org_id,
            &capture.addon_id,
            &capture.table_name,
            &capture.resource_type,
            &capture.resource_id,
            capture.action.as_str(),
            source_node_id,
            error.kind(),
            error.to_string(),
            capture_json,
            sync_runtime::now_ms()
        ],
    )
    .map_err(|e| map_sqlite_error(&e))?;
    Ok(())
}

pub fn list_sync_conflicts(
    org_id: &str,
    addon_id: &str,
    status: Option<&str>,
    limit: usize,
) -> Result<Vec<SyncConflictRow>, StorageSqlError> {
    open_addon_db(org_id, addon_id).map_err(abi_to_storage)?;
    let pool = acquire_pool(org_id, addon_id)?;
    let conn = pool.get().map_err(abi_to_storage)?;
    let status_filter = status.unwrap_or("open");
    let mut stmt = conn
        .prepare(
            "SELECT operation_id, org_id, addon_id, table_name, resource_type, resource_id, \
                    action, source_node_id, error_kind, error_message, status, created_at_ms, \
                    resolved_at_ms, resolution \
             FROM __tentaflow_sync_conflicts \
             WHERE status = ?1 \
             ORDER BY created_at_ms ASC \
             LIMIT ?2",
        )
        .map_err(|e| map_sqlite_error(&e))?;
    let rows = stmt
        .query_map(rusqlite::params![status_filter, limit as i64], |row| {
            Ok(SyncConflictRow {
                operation_id: row.get(0)?,
                org_id: row.get(1)?,
                addon_id: row.get(2)?,
                table_name: row.get(3)?,
                resource_type: row.get(4)?,
                resource_id: row.get(5)?,
                action: row.get(6)?,
                source_node_id: row.get(7)?,
                error_kind: row.get(8)?,
                error_message: row.get(9)?,
                status: row.get(10)?,
                created_at_ms: row.get(11)?,
                resolved_at_ms: row.get(12)?,
                resolution: row.get(13)?,
            })
        })
        .map_err(|e| map_sqlite_error(&e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| map_sqlite_error(&e))?;
    Ok(rows)
}

pub fn resolve_sync_conflict(
    org_id: &str,
    addon_id: &str,
    operation_id: OperationId,
    resolution: SyncConflictResolution,
) -> Result<SyncConflictResolveResult, StorageSqlError> {
    open_addon_db(org_id, addon_id).map_err(abi_to_storage)?;
    let pool = acquire_pool(org_id, addon_id)?;
    let conn = pool.get().map_err(abi_to_storage)?;
    let operation_id_hex = operation_id.to_hex();
    let conflict = load_open_conflict(&conn, &operation_id_hex)?;
    let mut rows_affected = 0;
    let status = match resolution {
        SyncConflictResolution::KeepLocal | SyncConflictResolution::Ignore => "ignored",
        SyncConflictResolution::AcceptRemote => {
            rows_affected =
                apply_replicated_write_with_resolution(&conflict.capture, operation_id, true)?;
            "resolved"
        }
    };
    conn.execute(
        "UPDATE __tentaflow_sync_conflicts \
         SET status = ?2, resolved_at_ms = ?3, resolution = ?4 \
         WHERE operation_id = ?1 AND status = 'open'",
        rusqlite::params![
            operation_id_hex,
            status,
            sync_runtime::now_ms(),
            resolution.as_str()
        ],
    )
    .map_err(|e| map_sqlite_error(&e))?;
    Ok(SyncConflictResolveResult {
        operation_id: operation_id.to_hex(),
        status: status.to_string(),
        resolution: resolution.as_str().to_string(),
        rows_affected,
    })
}

struct OpenConflict {
    capture: SqlWriteCapture,
}

fn load_open_conflict(
    conn: &rusqlite::Connection,
    operation_id: &str,
) -> Result<OpenConflict, StorageSqlError> {
    conn.query_row(
        "SELECT capture_json FROM __tentaflow_sync_conflicts \
         WHERE operation_id = ?1 AND status = 'open'",
        rusqlite::params![operation_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| map_sqlite_error(&e))?
    .map(|capture_json| {
        serde_json::from_str::<SqlWriteCapture>(&capture_json)
            .map(|capture| OpenConflict { capture })
    })
    .transpose()
    .map_err(|e| StorageSqlError::Internal(format!("conflict capture: {e}")))?
    .ok_or_else(|| StorageSqlError::Internal("open conflict not found".to_string()))
}

pub fn drain_installed_sql_captures(
    db: &DbPool,
    limit_per_addon: usize,
) -> Result<usize, StorageSqlError> {
    let addons = repository::list_addons(db)
        .map_err(|e| StorageSqlError::Internal(format!("list addons: {e}")))?;
    let mut drained = 0usize;
    for addon in addons {
        if !addon.is_enabled {
            continue;
        }
        let manifest = parse_manifest_toml(&addon.manifest_json)
            .map_err(|e| StorageSqlError::Internal(format!("parse addon manifest: {e}")))?;
        let uses_sql = manifest
            .storage
            .as_ref()
            .map(|storage| storage.sql)
            .unwrap_or(false);
        if !uses_sql {
            continue;
        }
        open_addon_db("org-default", &addon.addon_id).map_err(abi_to_storage)?;
        drained +=
            drain_pending_captures_for_addon("org-default", &addon.addon_id, limit_per_addon)?;
    }
    Ok(drained)
}

pub fn drain_pending_captures_for_addon(
    org_id: &str,
    addon_id: &str,
    limit: usize,
) -> Result<usize, StorageSqlError> {
    let pool = acquire_pool(org_id, addon_id)?;
    drain_pending_captures(&pool, limit)
}

fn drain_pending_captures(pool: &AddonDbPool, limit: usize) -> Result<usize, StorageSqlError> {
    let conn = pool.get().map_err(abi_to_storage)?;
    let captures = load_pending_captures(&conn, limit)?;
    let mut drained = 0usize;
    for row in captures {
        let result = if let Some(operation_id) = row.operation_id.as_deref() {
            OperationId::from_hex(operation_id).and_then(|op_id| {
                sync_runtime::record_sql_capture_outbox_only(row.capture.clone(), op_id)
            })
        } else {
            sync_runtime::record_sql_capture(row.capture.clone())
        };
        match result {
            Ok(Some(record)) => {
                mark_capture_status(
                    &conn,
                    &row.capture.capture_id,
                    "ledgered",
                    None,
                    Some(record.op_id),
                );
                drained += 1;
            }
            Ok(None) => break,
            Err(e) => mark_capture_status(
                &conn,
                &row.capture.capture_id,
                "error",
                Some(&e.to_string()),
                None,
            ),
        }
    }
    Ok(drained)
}

struct PendingCaptureRow {
    capture: SqlWriteCapture,
    operation_id: Option<String>,
}

fn load_pending_captures(
    conn: &rusqlite::Connection,
    limit: usize,
) -> Result<Vec<PendingCaptureRow>, StorageSqlError> {
    let mut stmt = conn
        .prepare(
            "SELECT capture_id, org_id, addon_id, table_name, action, resource_type, resource_id, \
             query, params_json, rows_affected, last_insert_id, actor_user_id, created_at_ms, \
             operation_id \
             FROM __tentaflow_sync_captures \
             WHERE status IN ('pending','error') \
             ORDER BY created_at_ms ASC \
             LIMIT ?1",
        )
        .map_err(|e| map_sqlite_error(&e))?;
    let rows = stmt
        .query_map(rusqlite::params![limit as i64], |row| {
            let action: String = row.get(4)?;
            let params_json: String = row.get(8)?;
            let params = serde_json::from_str::<Vec<JsonValue>>(&params_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            let action = SqlWriteAction::from_str(&action).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(PendingCaptureRow {
                capture: SqlWriteCapture {
                    capture_id: row.get(0)?,
                    org_id: row.get(1)?,
                    addon_id: row.get(2)?,
                    table_name: row.get(3)?,
                    action,
                    resource_type: row.get(5)?,
                    resource_id: row.get(6)?,
                    query: row.get(7)?,
                    params,
                    rows_affected: row.get::<_, i64>(9)? as u64,
                    last_insert_id: row.get(10)?,
                    actor_user_id: row.get(11)?,
                    created_at_ms: row.get(12)?,
                },
                operation_id: row.get(13)?,
            })
        })
        .map_err(|e| map_sqlite_error(&e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| map_sqlite_error(&e))?;
    Ok(rows)
}

fn build_capture(
    org_id: &str,
    addon_id: &str,
    table_name: &str,
    action: SqlWriteAction,
    query: &str,
    params: &[JsonValue],
    rows_affected: u64,
    last_insert_id: i64,
    actor_user_id: Option<String>,
) -> Result<SqlWriteCapture, StorageSqlError> {
    let created_at_ms = sync_runtime::now_ms();
    let params_json = serde_json::to_vec(params)
        .map_err(|e| StorageSqlError::Internal(format!("capture params: {e}")))?;
    let resource_id = match action {
        SqlWriteAction::Insert if last_insert_id > 0 => last_insert_id.to_string(),
        _ => query_hash_short(&format!(
            "{}:{}:{}:{}",
            table_name,
            action.as_str(),
            query,
            hex::encode(sha256(&params_json))
        )),
    };
    let capture_id = query_hash_short(&format!(
        "{}:{}:{}:{}:{}:{}",
        org_id, addon_id, table_name, resource_id, created_at_ms, rows_affected
    ));
    Ok(SqlWriteCapture {
        capture_id,
        org_id: org_id.to_string(),
        addon_id: addon_id.to_string(),
        table_name: table_name.to_string(),
        action,
        resource_type: table_name.to_string(),
        resource_id,
        query: query.to_string(),
        params: params.to_vec(),
        rows_affected,
        last_insert_id,
        actor_user_id,
        created_at_ms,
    })
}

fn insert_capture(
    tx: &rusqlite::Transaction<'_>,
    capture: &SqlWriteCapture,
) -> Result<(), StorageSqlError> {
    let params_json = serde_json::to_string(&capture.params)
        .map_err(|e| StorageSqlError::Internal(format!("capture params: {e}")))?;
    tx.execute(
        "INSERT INTO __tentaflow_sync_captures \
         (capture_id, org_id, addon_id, table_name, action, resource_type, resource_id, query, \
          params_json, rows_affected, last_insert_id, actor_user_id, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            &capture.capture_id,
            &capture.org_id,
            &capture.addon_id,
            &capture.table_name,
            capture.action.as_str(),
            &capture.resource_type,
            &capture.resource_id,
            &capture.query,
            params_json,
            capture.rows_affected as i64,
            capture.last_insert_id,
            capture.actor_user_id,
            capture.created_at_ms
        ],
    )
    .map_err(|e| map_sqlite_error(&e))?;
    Ok(())
}

fn publish_capture(conn: &rusqlite::Connection, capture: &SqlWriteCapture) {
    match sync_runtime::record_sql_capture(capture.clone()) {
        Ok(Some(record)) => mark_capture_status(
            conn,
            &capture.capture_id,
            "ledgered",
            None,
            Some(record.op_id),
        ),
        Ok(None) => {}
        Err(e) => mark_capture_status(
            conn,
            &capture.capture_id,
            "error",
            Some(&e.to_string()),
            None,
        ),
    }
}

fn mark_capture_status(
    conn: &rusqlite::Connection,
    capture_id: &str,
    status: &str,
    error_message: Option<&str>,
    operation_id: Option<OperationId>,
) {
    let ledgered_at_ms = if status == "ledgered" {
        Some(sync_runtime::now_ms())
    } else {
        None
    };
    let operation_id = operation_id.map(|op_id| op_id.to_hex());
    if let Err(e) = conn.execute(
        "UPDATE __tentaflow_sync_captures \
         SET status = ?2, error_message = ?3, ledgered_at_ms = ?4, \
             operation_id = COALESCE(?5, operation_id) \
         WHERE capture_id = ?1",
        rusqlite::params![
            capture_id,
            status,
            error_message,
            ledgered_at_ms,
            operation_id
        ],
    ) {
        warn!(
            "storage_sql_exec: nie udalo sie oznaczyc capture {}: {}",
            capture_id, e
        );
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
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

    fn with_tmp_home<F: FnOnce()>(f: F) {
        let _guard = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        // Also pin TENTAFLOW_HOME: tentaflow_home() prefers the repo's live
        // .runtime/ over HOME, so HOME alone would not isolate the test.
        let prev_tf = std::env::var_os("TENTAFLOW_HOME");
        std::env::set_var("TENTAFLOW_HOME", tmp.path());
        f();
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(p) = prev_tf {
            std::env::set_var("TENTAFLOW_HOME", p);
        } else {
            std::env::remove_var("TENTAFLOW_HOME");
        }
    }

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

    #[test]
    fn exec_for_addon_persists_sync_capture_with_write() {
        with_tmp_home(|| {
            let pool = crate::addon::storage_sql::open_addon_db("org-default", "sync-capture-test")
                .expect("open addon db");
            {
                let conn = pool.get().expect("conn");
                conn.execute(
                    "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                    [],
                )
                .expect("create table");
            }

            let (rows, last_id) = exec_for_addon(
                "org-default",
                "sync-capture-test",
                "INSERT INTO contacts (name) VALUES (?1)",
                &[JsonValue::String("Jan".to_string())],
                Some("7".to_string()),
            )
            .expect("exec");

            assert_eq!(rows, 1);
            assert_eq!(last_id, 1);

            let conn = pool.get().expect("conn");
            let row = conn
                .query_row(
                    "SELECT table_name, action, resource_type, resource_id, actor_user_id, status \
                     FROM __tentaflow_sync_captures",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    },
                )
                .expect("capture row");

            assert_eq!(row.0, "contacts");
            assert_eq!(row.1, "insert");
            assert_eq!(row.2, "contacts");
            assert_eq!(row.3, "1");
            assert_eq!(row.4, "7");
            assert_eq!(row.5, "pending");
        });
    }

    #[test]
    fn drain_without_runtime_keeps_pending_capture() {
        with_tmp_home(|| {
            let pool = crate::addon::storage_sql::open_addon_db("org-default", "drain-test")
                .expect("open addon db");
            {
                let conn = pool.get().expect("conn");
                conn.execute(
                    "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                    [],
                )
                .expect("create table");
            }

            exec_for_addon(
                "org-default",
                "drain-test",
                "INSERT INTO contacts (name) VALUES (?1)",
                &[JsonValue::String("Anna".to_string())],
                Some("9".to_string()),
            )
            .expect("exec");

            let drained =
                drain_pending_captures_for_addon("org-default", "drain-test", 100).expect("drain");
            assert_eq!(drained, 0);

            let conn = pool.get().expect("conn");
            let status: String = conn
                .query_row("SELECT status FROM __tentaflow_sync_captures", [], |row| {
                    row.get(0)
                })
                .expect("capture status");
            assert_eq!(status, "pending");
        });
    }

    #[test]
    fn apply_replicated_write_executes_without_capture_loop() {
        with_tmp_home(|| {
            let pool =
                crate::addon::storage_sql::open_addon_db("org-default", "replicated-apply-test")
                    .expect("open addon db");
            {
                let conn = pool.get().expect("conn");
                conn.execute(
                    "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                    [],
                )
                .expect("create table");
            }

            let capture = SqlWriteCapture {
                capture_id: "remote-capture-1".to_string(),
                org_id: "org-default".to_string(),
                addon_id: "replicated-apply-test".to_string(),
                table_name: "contacts".to_string(),
                action: SqlWriteAction::Insert,
                resource_type: "contacts".to_string(),
                resource_id: "1".to_string(),
                query: "INSERT INTO contacts (id, name) VALUES (?1, ?2)".to_string(),
                params: vec![JsonValue::from(1), JsonValue::String("Ewa".to_string())],
                rows_affected: 1,
                last_insert_id: 1,
                actor_user_id: Some("11".to_string()),
                created_at_ms: sync_runtime::now_ms(),
            };

            let operation_id = OperationId::from_hash([3; 32]);
            let rows = apply_replicated_write(&capture, operation_id).expect("apply");
            assert_eq!(rows, 1);
            let rows = apply_replicated_write(&capture, operation_id).expect("apply again");
            assert_eq!(rows, 0);

            let conn = pool.get().expect("conn");
            let name: String = conn
                .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .expect("contact");
            assert_eq!(name, "Ewa");
            let capture_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM __tentaflow_sync_captures",
                    [],
                    |row| row.get(0),
                )
                .expect("capture count");
            assert_eq!(capture_count, 0);
            let applied_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM __tentaflow_sync_applied", [], |row| {
                    row.get(0)
                })
                .expect("applied count");
            assert_eq!(applied_count, 1);
        });
    }

    #[test]
    fn record_sync_conflict_persists_open_conflict() {
        with_tmp_home(|| {
            let pool =
                crate::addon::storage_sql::open_addon_db("org-default", "conflict-record-test")
                    .expect("open addon db");
            let capture = SqlWriteCapture {
                capture_id: "remote-capture-conflict".to_string(),
                org_id: "org-default".to_string(),
                addon_id: "conflict-record-test".to_string(),
                table_name: "contacts".to_string(),
                action: SqlWriteAction::Insert,
                resource_type: "contacts".to_string(),
                resource_id: "1".to_string(),
                query: "INSERT INTO contacts (id, name) VALUES (?1, ?2)".to_string(),
                params: vec![JsonValue::from(1), JsonValue::String("Ewa".to_string())],
                rows_affected: 1,
                last_insert_id: 1,
                actor_user_id: Some("11".to_string()),
                created_at_ms: sync_runtime::now_ms(),
            };
            let operation_id = OperationId::from_hash([4; 32]);

            record_sync_conflict(
                &capture,
                operation_id,
                "node_b",
                &StorageSqlError::SqlConstraint,
            )
            .expect("record conflict");
            record_sync_conflict(
                &capture,
                operation_id,
                "node_b",
                &StorageSqlError::SqlConstraint,
            )
            .expect("record conflict again");

            let conn = pool.get().expect("conn");
            let row = conn
                .query_row(
                    "SELECT operation_id, source_node_id, error_kind, status, COUNT(*) OVER () \
                     FROM __tentaflow_sync_conflicts",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .expect("conflict row");

            assert_eq!(row.0, operation_id.to_hex());
            assert_eq!(row.1, "node_b");
            assert_eq!(row.2, "sql_constraint");
            assert_eq!(row.3, "open");
            assert_eq!(row.4, 1);
        });
    }

    #[test]
    fn list_sync_conflicts_returns_open_rows() {
        with_tmp_home(|| {
            let addon_id = "conflict-list-test";
            crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            let capture = conflict_capture(addon_id, "Ewa");
            let operation_id = OperationId::from_hash([5; 32]);
            record_sync_conflict(
                &capture,
                operation_id,
                "node_b",
                &StorageSqlError::SqlConstraint,
            )
            .expect("record conflict");

            let conflicts =
                list_sync_conflicts("org-default", addon_id, Some("open"), 10).expect("list");

            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].operation_id, operation_id.to_hex());
        });
    }

    #[test]
    fn resolve_sync_conflict_keep_local_marks_ignored() {
        with_tmp_home(|| {
            let addon_id = "conflict-keep-local-test";
            crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            let capture = conflict_capture(addon_id, "Ewa");
            let operation_id = OperationId::from_hash([6; 32]);
            record_sync_conflict(
                &capture,
                operation_id,
                "node_b",
                &StorageSqlError::SqlConstraint,
            )
            .expect("record conflict");

            let result = resolve_sync_conflict(
                "org-default",
                addon_id,
                operation_id,
                SyncConflictResolution::KeepLocal,
            )
            .expect("resolve");

            assert_eq!(result.status, "ignored");
            assert_eq!(result.resolution, "keep_local");
        });
    }

    #[test]
    fn resolve_sync_conflict_accept_remote_replaces_existing_insert() {
        with_tmp_home(|| {
            let addon_id = "conflict-accept-remote-test";
            let pool = crate::addon::storage_sql::open_addon_db("org-default", addon_id)
                .expect("open addon db");
            {
                let conn = pool.get().expect("conn");
                conn.execute(
                    "CREATE TABLE contacts (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
                    [],
                )
                .expect("create table");
                conn.execute("INSERT INTO contacts (id, name) VALUES (1, 'Local')", [])
                    .expect("insert local");
            }
            let capture = conflict_capture(addon_id, "Remote");
            let operation_id = OperationId::from_hash([7; 32]);
            record_sync_conflict(
                &capture,
                operation_id,
                "node_b",
                &StorageSqlError::SqlConstraint,
            )
            .expect("record conflict");

            let result = resolve_sync_conflict(
                "org-default",
                addon_id,
                operation_id,
                SyncConflictResolution::AcceptRemote,
            )
            .expect("resolve");

            assert_eq!(result.status, "resolved");
            let conn = pool.get().expect("conn");
            let name: String = conn
                .query_row("SELECT name FROM contacts WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .expect("name");
            assert_eq!(name, "Remote");
        });
    }

    #[test]
    fn write_to_internal_capture_table_is_blocked() {
        let err = parse_write_action_and_table(
            "UPDATE __tentaflow_sync_captures SET status = 'ledgered'",
        )
        .and_then(|(_, table)| reject_internal_table(&table))
        .expect_err("internal table must be blocked");

        assert!(matches!(err, StorageSqlError::InternalTableBlocked));
        assert!(matches!(err.as_abi(), AbiError::Permission));
    }

    fn conflict_capture(addon_id: &str, name: &str) -> SqlWriteCapture {
        SqlWriteCapture {
            capture_id: format!("remote-capture-conflict-{addon_id}-{name}"),
            org_id: "org-default".to_string(),
            addon_id: addon_id.to_string(),
            table_name: "contacts".to_string(),
            action: SqlWriteAction::Insert,
            resource_type: "contacts".to_string(),
            resource_id: "1".to_string(),
            query: "INSERT INTO contacts (id, name) VALUES (?1, ?2)".to_string(),
            params: vec![JsonValue::from(1), JsonValue::String(name.to_string())],
            rows_affected: 1,
            last_insert_id: 1,
            actor_user_id: Some("11".to_string()),
            created_at_ms: sync_runtime::now_ms(),
        }
    }
}
