// =============================================================================
// File: addon/host_functions/directory.rs
// Read-only directory views for addons: users, groups, roles and the calling
// instance's organization. Backs sharing UIs (pick a person / group / org)
// without exposing credentials, SSO subjects or role permission lists.
//
// All four host functions:
//   * require the "directory.read" permission (fail-closed, audit per outcome);
//   * are output-only (no input payload) — the host derives the scope from
//     the instance's AddonState (org_id, defaulting to `org-default` for
//     system/boot starts, same convention as sql.rs / vector.rs);
//   * return CBOR structs from `tentaflow-sdk-spec::directory`.
//
// Org membership is materialized in `org_memberships` — the startup seed
// guarantees every active `user_accounts` row has a membership row in
// `org-default` (see db/seed.rs), so a plain JOIN is the correct scoping.
// =============================================================================

use std::collections::HashMap;

use tentaflow_sdk_spec::{
    DirectoryGroupOut, DirectoryGroupsOutput, DirectoryOrgOutput, DirectoryRoleOut,
    DirectoryRolesOutput, DirectoryUserOut, DirectoryUsersOutput,
};

use super::abi_helpers::PayloadKind;
use super::cbor_io::write_cbor_capped;
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::DbPool;

/// Required permission for all directory host functions. Risk class B —
/// read-only access to ordinary personal data (names, e-mail addresses).
pub const PERM_DIRECTORY_READ: &str = "directory.read";

// ---------------------------------------------------------------------------
// ABI shells
// ---------------------------------------------------------------------------

pub fn directory_users_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_DIRECTORY_READ, None) {
        audit_directory(
            caller.data(),
            "directory.users",
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let org_id = caller_org_id(caller.data());
    let out = match list_org_users(&caller.data().db, &org_id) {
        Ok(v) => v,
        Err(e) => {
            audit_directory(caller.data(), "directory.users", "error", Some("db_error"));
            return e.as_i32();
        }
    };
    audit_directory(caller.data(), "directory.users", "ok", None);
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

pub fn directory_groups_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_DIRECTORY_READ, None) {
        audit_directory(
            caller.data(),
            "directory.groups",
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let org_id = caller_org_id(caller.data());
    let out = match list_org_groups(&caller.data().db, &org_id) {
        Ok(v) => v,
        Err(e) => {
            audit_directory(caller.data(), "directory.groups", "error", Some("db_error"));
            return e.as_i32();
        }
    };
    audit_directory(caller.data(), "directory.groups", "ok", None);
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

pub fn directory_roles_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_DIRECTORY_READ, None) {
        audit_directory(
            caller.data(),
            "directory.roles",
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let out = match list_roles(&caller.data().db) {
        Ok(v) => v,
        Err(e) => {
            audit_directory(caller.data(), "directory.roles", "error", Some("db_error"));
            return e.as_i32();
        }
    };
    audit_directory(caller.data(), "directory.roles", "ok", None);
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

pub fn directory_org_v1(
    mut caller: WasmCaller<'_, AddonState>,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    if !check_permission(caller.data(), PERM_DIRECTORY_READ, None) {
        audit_directory(
            caller.data(),
            "directory.org",
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let org_id = caller_org_id(caller.data());
    let out = match get_org(&caller.data().db, &org_id) {
        Ok(v) => v,
        Err(e) => {
            let reason = if e == AbiError::NotFound {
                "org_not_found"
            } else {
                "db_error"
            };
            audit_directory(caller.data(), "directory.org", "error", Some(reason));
            return e.as_i32();
        }
    };
    audit_directory(caller.data(), "directory.org", "ok", None);
    write_cbor_capped(
        &memory,
        &mut caller,
        &out,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

// ---------------------------------------------------------------------------
// Query layer (shared with the integration tests via `test_api`)
// ---------------------------------------------------------------------------

/// Org scope for the call — `None` means a system/boot instance, which the
/// whole host-fn layer treats as `org-default` (same rule as sql.rs et al.).
fn caller_org_id(state: &AddonState) -> String {
    state
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string())
}

/// Active members of `org_id` with their group IDs. Credential columns
/// (`password_hash`, `sso_*`) are never selected.
fn list_org_users(db: &DbPool, org_id: &str) -> Result<DirectoryUsersOutput, AbiError> {
    let conn = db.read().map_err(|_| AbiError::Operation)?;

    // One pass over group_members for the whole org instead of a per-user
    // subquery — the users list is the hot shape for share pickers.
    let mut groups_by_user: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT gm.user_id, gm.group_id FROM group_members gm \
                 JOIN user_accounts u ON u.id = gm.user_id AND u.is_active = 1 \
                 JOIN org_memberships m ON m.user_id = u.id AND m.org_id = ?1 \
                 ORDER BY gm.group_id",
            )
            .map_err(|_| AbiError::Operation)?;
        let rows = stmt
            .query_map(rusqlite::params![org_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| AbiError::Operation)?;
        for row in rows {
            let (user_id, group_id) = row.map_err(|_| AbiError::Operation)?;
            groups_by_user.entry(user_id).or_default().push(group_id);
        }
    }

    let mut stmt = conn
        .prepare(
            "SELECT u.id, u.username, u.display_name, u.email, u.role FROM user_accounts u \
             JOIN org_memberships m ON m.user_id = u.id \
             WHERE m.org_id = ?1 AND u.is_active = 1 \
             ORDER BY u.username",
        )
        .map_err(|_| AbiError::Operation)?;
    let rows = stmt
        .query_map(rusqlite::params![org_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|_| AbiError::Operation)?;

    let mut users = Vec::new();
    for row in rows {
        let (id, username, display_name, email, role) = row.map_err(|_| AbiError::Operation)?;
        let groups = groups_by_user.remove(&id).unwrap_or_default();
        users.push(DirectoryUserOut {
            id,
            username,
            display_name,
            email,
            groups,
            is_active: true,
            role,
        });
    }
    Ok(DirectoryUsersOutput { users })
}

/// Groups visible to `org_id`, with a member count restricted to its active
/// users. `user_groups` has no org column — groups are platform-global — so
/// org scoping is membership-based: a group is returned ONLY when at least
/// one active member of the caller's org belongs to it (otherwise a tenant
/// would see other tenants' group names/descriptions with a zero count).
fn list_org_groups(db: &DbPool, org_id: &str) -> Result<DirectoryGroupsOutput, AbiError> {
    let conn = db.read().map_err(|_| AbiError::Operation)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, description, org_members FROM ( \
                 SELECT g.id AS id, g.name AS name, \
                        IFNULL(g.description, '') AS description, \
                        (SELECT COUNT(*) FROM group_members gm \
                         JOIN user_accounts u ON u.id = gm.user_id AND u.is_active = 1 \
                         JOIN org_memberships m ON m.user_id = u.id AND m.org_id = ?1 \
                         WHERE gm.group_id = g.id) AS org_members \
                 FROM user_groups g) \
             WHERE org_members > 0 ORDER BY name",
        )
        .map_err(|_| AbiError::Operation)?;
    let rows = stmt
        .query_map(rusqlite::params![org_id], |row| {
            Ok(DirectoryGroupOut {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                member_count: row.get::<_, i64>(3)?.max(0) as u64,
            })
        })
        .map_err(|_| AbiError::Operation)?;
    let groups = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AbiError::Operation)?;
    Ok(DirectoryGroupsOutput { groups })
}

/// All RBAC roles (preseed + custom). Permission lists stay host-side.
fn list_roles(db: &DbPool) -> Result<DirectoryRolesOutput, AbiError> {
    let conn = db.read().map_err(|_| AbiError::Operation)?;
    let mut stmt = conn
        .prepare("SELECT role_id, name FROM roles ORDER BY name")
        .map_err(|_| AbiError::Operation)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DirectoryRoleOut {
                role_id: row.get(0)?,
                name: row.get(1)?,
            })
        })
        .map_err(|_| AbiError::Operation)?;
    let roles = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AbiError::Operation)?;
    Ok(DirectoryRolesOutput { roles })
}

/// The calling instance's organization row. `NotFound` when the org row does
/// not exist (never fabricated).
fn get_org(db: &DbPool, org_id: &str) -> Result<DirectoryOrgOutput, AbiError> {
    let conn = db.read().map_err(|_| AbiError::Operation)?;
    conn.query_row(
        "SELECT org_id, name, slug FROM organizations WHERE org_id = ?1",
        rusqlite::params![org_id],
        |row| {
            Ok(DirectoryOrgOutput {
                org_id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => AbiError::NotFound,
        _ => AbiError::Operation,
    })
}

fn audit_directory(state: &AddonState, action: &str, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        action,
        Some("directory"),
        None,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
}

// =============================================================================
// Public test surface — invoked from `tests/directory_host_functions.rs`
// =============================================================================

/// Re-exports the query layer under stable names so integration tests can
/// drive org scoping + shapes without standing up a WASM Store. Compiled out
/// of production builds — the raw queries bypass `check_permission` + audit,
/// so they must not be reachable outside `test-support` (the test target sets
/// `required-features = ["test-support"]` in Cargo.toml).
#[cfg(any(test, feature = "test-support"))]
pub mod test_api {
    use super::*;

    #[doc(hidden)]
    pub fn users(db: &DbPool, org_id: &str) -> Result<DirectoryUsersOutput, AbiError> {
        list_org_users(db, org_id)
    }

    #[doc(hidden)]
    pub fn groups(db: &DbPool, org_id: &str) -> Result<DirectoryGroupsOutput, AbiError> {
        list_org_groups(db, org_id)
    }

    #[doc(hidden)]
    pub fn roles(db: &DbPool) -> Result<DirectoryRolesOutput, AbiError> {
        list_roles(db)
    }

    #[doc(hidden)]
    pub fn org(db: &DbPool, org_id: &str) -> Result<DirectoryOrgOutput, AbiError> {
        get_org(db, org_id)
    }
}
