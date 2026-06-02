// =============================================================================
// Plik: addon/host_functions/sql.rs
// Opis: Cienki wrapper WASM-ABI nad `addon::storage_sql_exec`. Wrapper czyta
//       guest memory, sprawdza uprawnienia (`sql.read`/`sql.write`), enforce
//       payload limit (4 MB), woła pure-async exec/query/transaction,
//       zapisuje wynik do output bufera i emituje audit row (risk_class A).
//       Pure SQL execution + DDL guard + watchdog timeout zyje w
//       `storage_sql_exec` — operatory flow_runtime wolają to samo bez WASM.
// Uprawnienia: `sql.read` (sql_query/sql_query_one), `sql.write` (sql_exec/sql_transaction).
//              Manifest musi deklarowac [storage] sql=true; bez tego ABI fail-closed.
// =============================================================================

#![allow(clippy::too_many_arguments)]

use serde_json::{json, Value as JsonValue};

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_string, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::addon::storage_sql_exec::{
    exec_for_addon, is_ddl, parse_write_action_and_table, query_for_addon, query_hash_short,
    query_one_for_addon, resource_type_for_query, transaction_for_addon, StorageSqlError,
};
use crate::audit::RiskClass;
use crate::services::storage_proxy::{
    remote_sql_exec, remote_sql_query, DEFAULT_STORAGE_PROXY_TIMEOUT,
};
use tentaflow_protocol::mesh::{StorageProxyRequestKind, StorageProxyRequestPayload};

// =============================================================================
// sql_exec_v1
// =============================================================================

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
    if !addon_has_sql_declared(caller.data()) {
        return AbiError::Permission.as_i32();
    }

    let params: Vec<JsonValue> = match parse_params_json(&params_json) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());

    let (_, table_name) = match parse_write_action_and_table(&query) {
        Ok(parsed) => parsed,
        Err(e) => return e.as_abi().as_i32(),
    };
    if let Some((authority_node_id, iroh)) =
        central_storage_target(caller.data(), &org_id, &addon_id, &table_name, "")
    {
        let request = StorageProxyRequestPayload {
            request_id: String::new(),
            from_node_id: iroh.node_id(),
            org_id: org_id.clone(),
            addon_id: addon_id.clone(),
            resource_type: table_name,
            resource_id: String::new(),
            actor_user_id: caller.data().user_id.clone(),
            kind: StorageProxyRequestKind::SqlExec {
                query: query.clone(),
                params: params
                    .iter()
                    .map(crate::services::storage_proxy::json_to_wire)
                    .collect(),
            },
        };
        match run_async(remote_sql_exec(
            iroh.as_ref(),
            &authority_node_id,
            request,
            DEFAULT_STORAGE_PROXY_TIMEOUT,
        )) {
            Ok((rows_affected, last_insert_id)) => {
                audit_log_with_risk(
                    caller.data(),
                    "sql.exec",
                    Some("sql"),
                    Some(&query_hash_short(&query)),
                    RiskClass::A,
                    None,
                    None,
                    "ok",
                    Some("central_authority"),
                );
                let response = json!({
                    "rows_affected": rows_affected,
                    "last_insert_id": last_insert_id,
                });
                let bytes = match serde_json::to_vec(&response) {
                    Ok(b) => b,
                    Err(_) => return AbiError::Operation.as_i32(),
                };
                return write_output_with_retry_semantics(
                    &memory,
                    &mut caller,
                    &bytes,
                    out_ptr,
                    out_cap,
                    out_len_ptr,
                );
            }
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
                    Some(&e.to_string()),
                );
                return proxy_error_to_abi(&e).as_i32();
            }
        }
    }

    let result = exec_for_addon(
        &org_id,
        &addon_id,
        &query,
        &params,
        caller.data().user_id.clone(),
    );
    match result {
        Ok((rows_affected, last_insert_id)) => {
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
            let response = json!({
                "rows_affected": rows_affected,
                "last_insert_id": last_insert_id,
            });
            let bytes = match serde_json::to_vec(&response) {
                Ok(b) => b,
                Err(_) => return AbiError::Operation.as_i32(),
            };
            write_output_with_retry_semantics(
                &memory,
                &mut caller,
                &bytes,
                out_ptr,
                out_cap,
                out_len_ptr,
            )
        }
        Err(e) => {
            audit_log_with_risk(
                caller.data(),
                "sql.exec",
                Some("sql"),
                Some(&query_hash_short(&query)),
                RiskClass::A,
                None,
                None,
                if matches!(e, StorageSqlError::DdlBlocked) {
                    "denied"
                } else {
                    "error"
                },
                Some(&format!("{}", e)),
            );
            e.as_abi().as_i32()
        }
    }
}

// =============================================================================
// sql_query_v1
// =============================================================================

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
    sql_query_inner(
        &mut caller,
        query_ptr,
        query_len,
        params_json_ptr,
        params_json_len,
        out_ptr,
        out_cap,
        out_len_ptr,
        "sql.query",
        false,
    )
}

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
    sql_query_inner(
        &mut caller,
        query_ptr,
        query_len,
        params_json_ptr,
        params_json_len,
        out_ptr,
        out_cap,
        out_len_ptr,
        "sql.query_one",
        true,
    )
}

fn sql_query_inner(
    caller: &mut WasmCaller<'_, AddonState>,
    query_ptr: i32,
    query_len: i32,
    params_json_ptr: i32,
    params_json_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
    action: &'static str,
    one: bool,
) -> i32 {
    let memory = match get_memory(caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let query = match read_guest_string(&memory, caller, query_ptr, query_len) {
        Some(s) => s.to_string(),
        None => return AbiError::Operation.as_i32(),
    };
    let params_json = if params_json_len > 0 {
        match read_guest_string(&memory, caller, params_json_ptr, params_json_len) {
            Some(s) => s.to_string(),
            None => return AbiError::Operation.as_i32(),
        }
    } else {
        String::new()
    };

    if enforce_payload_size(query.len() + params_json.len(), PayloadKind::SqlCombined).is_err() {
        audit_log_with_risk(
            caller.data(),
            action,
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
            action,
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
    if !addon_has_sql_declared(caller.data()) {
        return AbiError::Permission.as_i32();
    }

    let params: Vec<JsonValue> = match parse_params_json(&params_json) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let resource_type = resource_type_for_query(&query).unwrap_or_default();
    if let Some((authority_node_id, iroh)) =
        central_storage_target(caller.data(), &org_id, &addon_id, &resource_type, "")
    {
        let request = StorageProxyRequestPayload {
            request_id: String::new(),
            from_node_id: iroh.node_id(),
            org_id: org_id.clone(),
            addon_id: addon_id.clone(),
            resource_type,
            resource_id: String::new(),
            actor_user_id: caller.data().user_id.clone(),
            kind: StorageProxyRequestKind::SqlQuery {
                query: query.clone(),
                params: params
                    .iter()
                    .map(crate::services::storage_proxy::json_to_wire)
                    .collect(),
                one,
                limit: None,
            },
        };
        match run_async(remote_sql_query(
            iroh.as_ref(),
            &authority_node_id,
            request,
            DEFAULT_STORAGE_PROXY_TIMEOUT,
        )) {
            Ok(response) => {
                audit_log_with_risk(
                    caller.data(),
                    action,
                    Some("sql"),
                    Some(&query_hash_short(&query)),
                    RiskClass::A,
                    None,
                    None,
                    "ok",
                    Some("central_authority"),
                );
                let bytes = match serde_json::to_vec(&response) {
                    Ok(b) => b,
                    Err(_) => return AbiError::Operation.as_i32(),
                };
                return write_output_with_retry_semantics(
                    &memory,
                    caller,
                    &bytes,
                    out_ptr,
                    out_cap,
                    out_len_ptr,
                );
            }
            Err(e) => {
                audit_log_with_risk(
                    caller.data(),
                    action,
                    Some("sql"),
                    Some(&query_hash_short(&query)),
                    RiskClass::A,
                    None,
                    None,
                    "error",
                    Some(&e.to_string()),
                );
                return proxy_error_to_abi(&e).as_i32();
            }
        }
    }
    let result = if one {
        query_one_for_addon(&org_id, &addon_id, &query, &params)
    } else {
        query_for_addon(&org_id, &addon_id, &query, &params, None)
    };
    match result {
        Ok(response) => {
            audit_log_with_risk(
                caller.data(),
                action,
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
            write_output_with_retry_semantics(
                &memory,
                caller,
                &bytes,
                out_ptr,
                out_cap,
                out_len_ptr,
            )
        }
        Err(e) => {
            let outcome = if matches!(
                e,
                StorageSqlError::NotReadOnly | StorageSqlError::DdlBlocked
            ) {
                "denied"
            } else {
                "error"
            };
            audit_log_with_risk(
                caller.data(),
                action,
                Some("sql"),
                Some(&query_hash_short(&query)),
                RiskClass::A,
                None,
                None,
                outcome,
                Some(&format!("{}", e)),
            );
            e.as_abi().as_i32()
        }
    }
}

// =============================================================================
// sql_transaction_v1
// =============================================================================

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
    if !addon_has_sql_declared(caller.data()) {
        return AbiError::Permission.as_i32();
    }

    let payload: JsonValue = match serde_json::from_str(&statements_json) {
        Ok(v) => v,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    let stmts = match payload.get("statements").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return AbiError::Operation.as_i32(),
    };

    // Pre-walidacja DDL na poziomie wrappera — daje czytelniejszy audit
    // (action='sql.transaction' result='denied' przed wejsciem do dispatchu).
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

    let mut prepared: Vec<(String, Vec<JsonValue>)> = Vec::with_capacity(stmts.len());
    for s in stmts {
        let q = match s.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return AbiError::Operation.as_i32(),
        };
        let params_val = s
            .get("params")
            .cloned()
            .unwrap_or(JsonValue::Array(Vec::new()));
        let params: Vec<JsonValue> = match params_val.as_array().cloned() {
            Some(a) => a,
            None => return AbiError::Operation.as_i32(),
        };
        prepared.push((q, params));
    }

    let addon_id = caller.data().addon_id.clone();
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    match transaction_for_addon(&org_id, &addon_id, &prepared, caller.data().user_id.clone()) {
        Ok(total) => {
            audit_log_with_risk(
                caller.data(),
                "sql.transaction",
                Some("sql"),
                Some(&query_hash_short(&statements_json)),
                RiskClass::A,
                None,
                None,
                "ok",
                Some(&format!("statements={}", prepared.len())),
            );
            let response = json!({ "rows_affected_total": total });
            let bytes = match serde_json::to_vec(&response) {
                Ok(b) => b,
                Err(_) => return AbiError::Operation.as_i32(),
            };
            write_output_with_retry_semantics(
                &memory,
                &mut caller,
                &bytes,
                out_ptr,
                out_cap,
                out_len_ptr,
            )
        }
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
                Some(&format!("abi_error={}", e.as_abi().as_i32())),
            );
            e.as_abi().as_i32()
        }
    }
}

// =============================================================================
// Pomocnicze
// =============================================================================

fn addon_has_sql_declared(state: &AddonState) -> bool {
    state
        .manifest
        .storage
        .as_ref()
        .map(|s| s.sql)
        .unwrap_or(false)
}

fn parse_params_json(params_json: &str) -> Result<Vec<JsonValue>, i32> {
    if params_json.is_empty() {
        return Ok(Vec::new());
    }
    let v: JsonValue =
        serde_json::from_str(params_json).map_err(|_| AbiError::Operation.as_i32())?;
    let arr = v.as_array().ok_or_else(|| AbiError::Operation.as_i32())?;
    Ok(arr.clone())
}

pub(crate) fn central_storage_target(
    state: &AddonState,
    org_id: &str,
    addon_id: &str,
    resource_type: &str,
    resource_id: &str,
) -> Option<(
    String,
    std::sync::Arc<crate::mesh::iroh_manager::IrohMeshManager>,
)> {
    let policy = crate::db::repository::get_effective_sync_policy(
        &state.db,
        org_id,
        addon_id,
        resource_type,
        resource_id,
    )
    .ok()
    .flatten()?;
    if !policy.is_enabled {
        return None;
    }
    if !policy.mode.is_authority_backed() {
        return None;
    }
    let authority_node_id = policy.authority_node_id?;
    let router = state.router.as_ref()?;
    let iroh = router.mesh_manager()?;
    if authority_node_id == iroh.node_id() {
        return None;
    }
    if policy.mode.materializes_by_permission()
        && crate::db::repository::can_node_receive_sync_resource(
            &state.db,
            &iroh.node_id(),
            org_id,
            addon_id,
            resource_type,
            resource_id,
        )
        .ok()
        .map(|decision| decision.allowed)
        .unwrap_or(false)
    {
        return None;
    }
    Some((authority_node_id, iroh))
}

pub(crate) fn run_async<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

pub(crate) fn proxy_error_to_abi(
    error: &crate::services::storage_proxy::StorageProxyError,
) -> AbiError {
    match error {
        crate::services::storage_proxy::StorageProxyError::Timeout(_) => AbiError::Timeout,
        crate::services::storage_proxy::StorageProxyError::Remote { code, .. }
            if code == "not_declared"
                || code == "ddl_blocked"
                || code == "not_write_statement"
                || code == "internal_table_blocked"
                || code == "authority_denied" =>
        {
            AbiError::Permission
        }
        crate::services::storage_proxy::StorageProxyError::Remote { code, .. }
            if code == "sql_syntax" =>
        {
            AbiError::SqlSyntax
        }
        crate::services::storage_proxy::StorageProxyError::Remote { code, .. }
            if code == "sql_constraint" =>
        {
            AbiError::SqlConstraint
        }
        _ => AbiError::Operation,
    }
}

// =============================================================================
// Testy jednostkowe — delegujemy do storage_sql_exec dla pure helpers.
// =============================================================================

#[cfg(test)]
mod tests {
    use crate::addon::storage_sql_exec::{
        is_ddl, is_read_only, json_to_sqlite_value, parse_params, query_hash_short,
    };
    use serde_json::Value as JsonValue;

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
        use rusqlite::types::Value as SqliteValue;
        assert!(matches!(
            json_to_sqlite_value(&JsonValue::Null).unwrap(),
            SqliteValue::Null
        ));
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
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn is_ddl_blocks_comment_prefixed_ddl() {
        assert!(is_ddl("-- evil\nCREATE TABLE x (id INTEGER)"));
        assert!(is_ddl("/* evil */ CREATE TABLE x (id INTEGER)"));
        assert!(is_ddl("/* a */ -- b\nDROP TABLE items"));
        assert!(is_ddl("  -- foo\n  /* bar */ALTER TABLE x ADD COLUMN y"));
        assert!(!is_ddl("-- comment\nINSERT INTO x VALUES (1)"));
        assert!(!is_ddl("/* c */ SELECT 1"));
    }

    #[test]
    fn is_read_only_handles_leading_comments() {
        assert!(is_read_only("-- foo\nSELECT 1"));
        assert!(is_read_only(
            "/* foo */ WITH t AS (SELECT 1) SELECT * FROM t"
        ));
        assert!(!is_read_only("/* foo */ INSERT INTO x VALUES (1)"));
    }
}
