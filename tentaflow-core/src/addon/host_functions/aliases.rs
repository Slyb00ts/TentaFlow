// =============================================================================
// File: addon/host_functions/aliases.rs
// Purpose: Readonly host functions for alias inspection (alias_get_v1,
//          alias_list_owned_v1). Alias create/deactivate is lifecycle-only —
//          performed at install/uninstall time by `addon::install_manifest_aliases`,
//          not by addon-callable ABI. Each call is permission-gated
//          (`alias.read`), audited as risk_class=A, payload-size enforced,
//          and validates alias id format via repository::validate_alias_id.
// =============================================================================

#![allow(clippy::too_many_arguments)]

use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use tracing::warn;

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_string, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::validate_alias_id;
use crate::db::DbPool;

// =============================================================================
// Permission constant
// =============================================================================

/// Readonly alias inspection requires the `alias.read` permission. Lifecycle
/// (create/deactivate) is no longer addon-callable — it happens implicitly
/// during install/uninstall driven by the manifest.
const PERM_ALIAS_READ: &str = "alias.read";

// =============================================================================
// Internal logic — alias_get
// =============================================================================

/// Output schema for both `alias_get_v1` (single object) and
/// `alias_list_owned_v1` (array elements). Usage stats may be stripped to
/// `None`/`0` when the caller is not the owner — see `build_alias_info`
/// for the visibility rules.
#[derive(Debug, Serialize)]
struct AliasInfoOut {
    id: String,
    /// "addon:<id>" or "manual" or null when no owner row is present.
    owner: Option<String>,
    current_target: String,
    fallback_targets: Vec<String>,
    strategy: String,
    is_active: bool,
    last_used_target: Option<String>,
    last_used_at: Option<i64>,
    calls_24h: u64,
    fallback_calls_24h: u64,
}

/// Joins `model_aliases` + `model_alias_owners` + `alias_calls` aggregates.
/// Returns `Err(AbiError::NotFound)` if the alias row does not exist.
fn do_alias_get(
    db: &DbPool,
    alias_id: &str,
    caller_addon_id: &str,
) -> Result<AliasInfoOut, AbiError> {
    enforce_payload_size(alias_id.len(), PayloadKind::SqlCombined)?;
    validate_alias_id(alias_id).map_err(|_| AbiError::Operation)?;
    let conn = db.lock().map_err(|_| AbiError::Operation)?;
    let row: Option<AliasCoreRow> = conn
        .query_row(
            "SELECT a.id, a.alias, a.target_model, a.fallback_targets, a.strategy, a.is_active, \
                    o.owner_type, o.owner_id \
             FROM model_aliases a \
             LEFT JOIN model_alias_owners o ON o.alias_id = a.id \
             WHERE a.alias = ?1",
            rusqlite::params![alias_id],
            |r| {
                Ok(AliasCoreRow {
                    alias_id: r.get(0)?,
                    alias: r.get(1)?,
                    target_model: r.get(2)?,
                    fallback_targets: r.get(3)?,
                    strategy: r.get(4)?,
                    is_active: r.get::<_, i64>(5)? != 0,
                    owner_type: r.get(6)?,
                    owner_id: r.get(7)?,
                })
            },
        )
        .ok();
    let row = row.ok_or(AbiError::NotFound)?;
    build_alias_info(&conn, row, caller_addon_id)
}

struct AliasCoreRow {
    alias_id: i64,
    alias: String,
    target_model: String,
    fallback_targets: Option<String>,
    strategy: Option<String>,
    is_active: bool,
    owner_type: Option<String>,
    owner_id: Option<String>,
}

/// Builds the `AliasInfoOut` for a fetched row plus per-alias usage stats
/// from `alias_calls`. Visibility rules for usage stats:
/// - Manual aliases (`owner_type='manual'`): stats are public — they belong
///   to the operator-managed registry and are surfaced in M16 dashboards.
/// - Addon-owned aliases: stats are visible only to the owning addon. Other
///   callers see the metadata (id/owner/current_target/strategy/is_active)
///   but `last_used_*` is nulled and the 24h counters are zeroed. This
///   keeps cross-addon usage patterns private even when both addons hold
///   the `alias.read` permission.
fn build_alias_info(
    conn: &rusqlite::Connection,
    row: AliasCoreRow,
    caller_addon_id: &str,
) -> Result<AliasInfoOut, AbiError> {
    let fallback_targets: Vec<String> = match row.fallback_targets.as_deref() {
        None | Some("") => Vec::new(),
        Some(s) => serde_json::from_str(s).map_err(|_| AbiError::Operation)?,
    };

    let owner = match (row.owner_type.as_deref(), row.owner_id.as_deref()) {
        (Some("addon"), Some(id)) => Some(format!("addon:{}", id)),
        (Some("manual"), _) => Some("manual".to_string()),
        _ => None,
    };

    let stats_visible = match (row.owner_type.as_deref(), row.owner_id.as_deref()) {
        (Some("manual"), _) => true,
        (Some("addon"), Some(id)) => id == caller_addon_id,
        _ => false,
    };

    let (last_used_target, last_used_at, calls_24h, fallback_calls_24h) = if stats_visible {
        let last: Option<(String, i64)> = conn
            .query_row(
                "SELECT target_used, ts FROM alias_calls \
                 WHERE alias_id = ?1 \
                 ORDER BY ts DESC LIMIT 1",
                rusqlite::params![row.alias_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok();

        let (c24, f24): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN fallback_used = 1 THEN 1 ELSE 0 END), 0) \
                 FROM alias_calls \
                 WHERE alias_id = ?1 AND ts > (CAST(strftime('%s','now') AS INTEGER) - 86400)",
                rusqlite::params![row.alias_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap_or((0, 0));
        (
            last.as_ref().map(|(t, _)| t.clone()),
            last.as_ref().map(|(_, ts)| *ts),
            c24.max(0) as u64,
            f24.max(0) as u64,
        )
    } else {
        (None, None, 0u64, 0u64)
    };

    Ok(AliasInfoOut {
        id: row.alias,
        owner,
        current_target: row.target_model,
        fallback_targets,
        strategy: row.strategy.unwrap_or_else(|| "first_available".to_string()),
        is_active: row.is_active,
        last_used_target,
        last_used_at,
        calls_24h,
        fallback_calls_24h,
    })
}

// =============================================================================
// Internal logic — alias_list_owned
// =============================================================================

fn do_alias_list_owned(
    db: &DbPool,
    caller_addon_id: &str,
) -> Result<Vec<AliasInfoOut>, AbiError> {
    let conn = db.lock().map_err(|_| AbiError::Operation)?;
    let mut stmt = conn
        .prepare(
            "SELECT a.id, a.alias, a.target_model, a.fallback_targets, a.strategy, a.is_active, \
                    o.owner_type, o.owner_id \
             FROM model_aliases a \
             JOIN model_alias_owners o ON o.alias_id = a.id \
             WHERE o.owner_type = 'addon' AND o.owner_id = ?1 \
             ORDER BY a.alias",
        )
        .map_err(|_| AbiError::Operation)?;
    let rows = stmt
        .query_map(rusqlite::params![caller_addon_id], |r| {
            Ok(AliasCoreRow {
                alias_id: r.get(0)?,
                alias: r.get(1)?,
                target_model: r.get(2)?,
                fallback_targets: r.get(3)?,
                strategy: r.get(4)?,
                is_active: r.get::<_, i64>(5)? != 0,
                owner_type: r.get(6)?,
                owner_id: r.get(7)?,
            })
        })
        .map_err(|_| AbiError::Operation)?;
    let collected: Vec<AliasCoreRow> = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            warn!("alias_list_owned: failed to collect alias rows: {}", e);
            AbiError::Operation
        })?;
    let mut out = Vec::with_capacity(collected.len());
    for row in collected {
        out.push(build_alias_info(&conn, row, caller_addon_id)?);
    }
    Ok(out)
}

// =============================================================================
// Audit helper — single point so action strings stay consistent
// =============================================================================

fn audit(
    state: &AddonState,
    action: &str,
    resource_id: Option<&str>,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("alias"),
        resource_id,
        RiskClass::A,
        None,
        None,
        result,
        reason,
    );
}

/// Maps an `AbiError` produced by `do_alias_*` to the (result, reason) pair
/// that is recorded in the audit log. `validation_failed` distinguishes
/// operation errors that came from caller-side validation (alias id, payload
/// format) from those that came from the DB layer.
fn audit_outcome_for_error(e: AbiError, validation_failed: bool) -> (&'static str, &'static str) {
    match e {
        AbiError::Permission => ("denied", "missing_permission_or_ownership"),
        AbiError::Conflict => ("denied", "ownership_or_chain_conflict"),
        AbiError::PayloadTooLarge => ("denied", "payload_too_large"),
        AbiError::NotFound => ("denied", "alias_not_found"),
        AbiError::Operation if validation_failed => ("denied", "invalid_payload_or_format"),
        AbiError::Operation => ("error", "db_or_encode_error"),
        _ => ("error", "unexpected_error"),
    }
}

// =============================================================================
// Host function: alias_get_v1
// =============================================================================

/// ABI: (alias_id_ptr, alias_id_len, out_ptr, out_cap, out_len_ptr) -> i32
///
/// Returns the full `AliasInfo` schema. Read access is allowed for any
/// addon holding `alias.read` regardless of ownership — alias metadata
/// is part of the global registry and is not considered confidential
/// (no PII, no secrets). Usage stats (`last_used_*`, `calls_24h`,
/// `fallback_calls_24h`) are visible only to the owner addon and for
/// manual aliases. Cross-addon callers see metadata with stats zeroed.
pub fn alias_get_v1(
    mut caller: WasmCaller<'_, AddonState>,
    alias_id_ptr: i32,
    alias_id_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    let alias_id = match read_guest_string(&memory, &caller, alias_id_ptr, alias_id_len) {
        Some(s) => s.to_string(),
        None => return AbiError::Operation.as_i32(),
    };

    if !check_permission(caller.data(), PERM_ALIAS_READ, None) {
        audit(
            caller.data(),
            "alias.get",
            Some(&alias_id),
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    if enforce_payload_size(alias_id.len(), PayloadKind::SqlCombined).is_err() {
        audit(
            caller.data(),
            "alias.get",
            Some(&alias_id),
            "denied",
            Some("payload_too_large"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    let info = match do_alias_get(&db, &alias_id, &addon_id) {
        Ok(v) => v,
        Err(e) => {
            let validation_failed = matches!(e, AbiError::NotFound)
                || matches!(e, AbiError::Operation);
            let (result_kind, reason) = audit_outcome_for_error(e, validation_failed);
            audit(caller.data(), "alias.get", Some(&alias_id), result_kind, Some(reason));
            return e.as_i32();
        }
    };

    audit(caller.data(), "alias.get", Some(&alias_id), "ok", None);

    let bytes = match serde_json::to_vec(&info) {
        Ok(b) => b,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    write_output_with_retry_semantics(&memory, &mut caller, &bytes, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: alias_list_owned_v1
// =============================================================================

/// ABI: (out_ptr, out_cap, out_len_ptr) -> i32
///
/// Returns `{"aliases":[AliasInfo,...]}` — only rows where
/// `model_alias_owners.owner_id = caller.addon_id`. Manual or other-addon
/// rows are never visible through this endpoint; admins use M16 UI for
/// global enumeration.
pub fn alias_list_owned_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    if !check_permission(caller.data(), PERM_ALIAS_READ, None) {
        audit(caller.data(), "alias.list_owned", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    let list = match do_alias_list_owned(&db, &addon_id) {
        Ok(v) => v,
        Err(e) => {
            let (result_kind, reason) = audit_outcome_for_error(e, false);
            audit(caller.data(), "alias.list_owned", None, result_kind, Some(reason));
            return e.as_i32();
        }
    };

    audit(caller.data(), "alias.list_owned", None, "ok", None);

    let response = json!({ "aliases": list });
    let bytes = match serde_json::to_vec(&response) {
        Ok(b) => b,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    write_output_with_retry_semantics(&memory, &mut caller, &bytes, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Public test surface — invoked from `tests/alias_host_functions.rs`
// =============================================================================

/// Re-exports the internal logic functions under a stable name so integration
/// tests can drive the full pipeline without constructing a `WasmCaller`.
/// Marked `#[doc(hidden)]` — not part of the addon-facing API.
#[doc(hidden)]
pub mod test_api {
    use super::*;

    #[doc(hidden)]
    pub fn alias_get_internal(
        db: &DbPool,
        alias_id: &str,
        caller_addon_id: &str,
    ) -> Result<JsonValue, AbiError> {
        let info = do_alias_get(db, alias_id, caller_addon_id)?;
        serde_json::to_value(info).map_err(|_| AbiError::Operation)
    }

    #[doc(hidden)]
    pub fn alias_list_owned_internal(
        db: &DbPool,
        caller_addon_id: &str,
    ) -> Result<JsonValue, AbiError> {
        let list = do_alias_list_owned(db, caller_addon_id)?;
        Ok(json!({ "aliases": list }))
    }
}

// =============================================================================
// Unit tests — pure helpers
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_outcome_permission_is_denied() {
        let (r, _) = audit_outcome_for_error(AbiError::Permission, false);
        assert_eq!(r, "denied");
    }

    #[test]
    fn audit_outcome_conflict_is_denied() {
        let (r, _) = audit_outcome_for_error(AbiError::Conflict, false);
        assert_eq!(r, "denied");
    }

    #[test]
    fn audit_outcome_payload_too_large_is_denied() {
        let (r, _) = audit_outcome_for_error(AbiError::PayloadTooLarge, false);
        assert_eq!(r, "denied");
    }

    #[test]
    fn audit_outcome_not_found_is_denied() {
        let (r, _) = audit_outcome_for_error(AbiError::NotFound, false);
        assert_eq!(r, "denied");
    }

    #[test]
    fn audit_outcome_operation_validation_is_denied() {
        let (r, _) = audit_outcome_for_error(AbiError::Operation, true);
        assert_eq!(r, "denied");
    }

    #[test]
    fn audit_outcome_operation_db_is_error() {
        let (r, _) = audit_outcome_for_error(AbiError::Operation, false);
        assert_eq!(r, "error");
    }
}
