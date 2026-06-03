// =============================================================================
// Plik: addon/host_functions/sync_acl.rs
// Opis: Host functions dla metadanych ACL zasobow synchronizowanych przez Core.
//       Addony przekazuja binary CBOR payload, a Core zapisuje ACL.
// =============================================================================

use serde::Deserialize;

use super::{audit_log, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller};
use crate::addon::errors::AbiError;

#[derive(Debug, Deserialize)]
struct SyncAclUpsertRequest {
    resource_type: String,
    resource_id: String,
    owner_user_id: Option<String>,
    assigned_user_id: Option<String>,
    department_id: Option<String>,
    manager_user_id: Option<String>,
    visibility_scope: String,
}

#[derive(Debug, Deserialize)]
struct SyncAclDeleteRequest {
    resource_type: String,
    resource_id: String,
}

#[derive(Debug, Deserialize)]
struct SyncShareRequest {
    resource_type: String,
    resource_id: String,
    subject_type: String,
    subject_id: String,
    action: String,
}

pub fn sync_acl_upsert_v1(
    mut caller: WasmCaller<'_, AddonState>,
    payload_ptr: i32,
    payload_len: i32,
) -> i32 {
    let payload = match read_payload::<SyncAclUpsertRequest>(&mut caller, payload_ptr, payload_len)
    {
        Ok(payload) => payload,
        Err(code) => return code,
    };
    if validate_resource(&payload.resource_type, &payload.resource_id).is_err()
        || validate_scope(&payload.visibility_scope).is_err()
    {
        return AbiError::Operation.as_i32();
    }
    if !check_permission(caller.data(), "sync.acl", None) {
        audit_log(
            caller.data(),
            "sync.acl.upsert",
            Some("sync_acl"),
            Some(&payload.resource_id),
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let department_id = payload.department_id.as_deref();
    match crate::db::repository::upsert_sync_resource_acl(
        &caller.data().db,
        &org_id,
        &caller.data().addon_id,
        &payload.resource_type,
        &payload.resource_id,
        payload.owner_user_id.as_deref(),
        payload.assigned_user_id.as_deref(),
        department_id,
        payload.manager_user_id.as_deref(),
        &payload.visibility_scope,
    ) {
        Ok(()) => {
            audit_log(
                caller.data(),
                "sync.acl.upsert",
                Some("sync_acl"),
                Some(&payload.resource_id),
                "ok",
                None,
            );
            AbiError::Ok.as_i32()
        }
        Err(e) => {
            audit_log(
                caller.data(),
                "sync.acl.upsert",
                Some("sync_acl"),
                Some(&payload.resource_id),
                "error",
                Some(&e.to_string()),
            );
            AbiError::Operation.as_i32()
        }
    }
}

pub fn sync_acl_delete_v1(
    mut caller: WasmCaller<'_, AddonState>,
    payload_ptr: i32,
    payload_len: i32,
) -> i32 {
    let payload = match read_payload::<SyncAclDeleteRequest>(&mut caller, payload_ptr, payload_len)
    {
        Ok(payload) => payload,
        Err(code) => return code,
    };
    if validate_resource(&payload.resource_type, &payload.resource_id).is_err() {
        return AbiError::Operation.as_i32();
    }
    if !check_permission(caller.data(), "sync.acl", None) {
        audit_log(
            caller.data(),
            "sync.acl.delete",
            Some("sync_acl"),
            Some(&payload.resource_id),
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }
    let org_id = caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    match crate::db::repository::delete_sync_resource_acl(
        &caller.data().db,
        &org_id,
        &caller.data().addon_id,
        &payload.resource_type,
        &payload.resource_id,
    ) {
        Ok(()) => AbiError::Ok.as_i32(),
        Err(_) => AbiError::Operation.as_i32(),
    }
}

pub fn sync_share_grant_v1(
    mut caller: WasmCaller<'_, AddonState>,
    payload_ptr: i32,
    payload_len: i32,
) -> i32 {
    let payload = match read_payload::<SyncShareRequest>(&mut caller, payload_ptr, payload_len) {
        Ok(payload) => payload,
        Err(code) => return code,
    };
    sync_share_apply(caller.data(), payload, true)
}

pub fn sync_share_revoke_v1(
    mut caller: WasmCaller<'_, AddonState>,
    payload_ptr: i32,
    payload_len: i32,
) -> i32 {
    let payload = match read_payload::<SyncShareRequest>(&mut caller, payload_ptr, payload_len) {
        Ok(payload) => payload,
        Err(code) => return code,
    };
    sync_share_apply(caller.data(), payload, false)
}

fn sync_share_apply(state: &AddonState, payload: SyncShareRequest, grant: bool) -> i32 {
    if validate_resource(&payload.resource_type, &payload.resource_id).is_err()
        || validate_subject(&payload.subject_type, &payload.subject_id).is_err()
        || validate_action(&payload.action).is_err()
    {
        return AbiError::Operation.as_i32();
    }
    if !check_permission(state, "sync.acl", None) {
        audit_log(
            state,
            if grant {
                "sync.share.grant"
            } else {
                "sync.share.revoke"
            },
            Some("sync_acl"),
            Some(&payload.resource_id),
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }
    let org_id = state
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
    let result = if grant {
        crate::db::repository::grant_sync_explicit_share(
            &state.db,
            &org_id,
            &state.addon_id,
            &payload.resource_type,
            &payload.resource_id,
            &payload.subject_type,
            &payload.subject_id,
            &payload.action,
            state.user_id.as_deref(),
        )
    } else {
        crate::db::repository::revoke_sync_explicit_share(
            &state.db,
            &org_id,
            &state.addon_id,
            &payload.resource_type,
            &payload.resource_id,
            &payload.subject_type,
            &payload.subject_id,
            &payload.action,
        )
    };
    match result {
        Ok(()) => AbiError::Ok.as_i32(),
        Err(_) => AbiError::Operation.as_i32(),
    }
}

fn read_payload<T: for<'de> Deserialize<'de>>(
    caller: &mut WasmCaller<'_, AddonState>,
    payload_ptr: i32,
    payload_len: i32,
) -> Result<T, i32> {
    let memory = get_memory(caller).ok_or_else(|| AbiError::Operation.as_i32())?;
    let bytes = read_guest_bytes(&memory, caller, payload_ptr, payload_len)
        .ok_or_else(|| AbiError::Operation.as_i32())?;
    ciborium::de::from_reader(std::io::Cursor::new(bytes)).map_err(|_| AbiError::Operation.as_i32())
}

fn validate_resource(resource_type: &str, resource_id: &str) -> Result<(), ()> {
    if resource_type.is_empty()
        || resource_id.is_empty()
        || resource_type.len() > 128
        || resource_id.len() > 256
    {
        return Err(());
    }
    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), ()> {
    match scope {
        "private" | "own" | "assigned" | "department" | "manager_subtree" | "explicit_share"
        | "all" => Ok(()),
        _ => Err(()),
    }
}

fn validate_subject(subject_type: &str, subject_id: &str) -> Result<(), ()> {
    if subject_id.is_empty() || subject_id.len() > 256 {
        return Err(());
    }
    match subject_type {
        "user" | "node" => Ok(()),
        _ => Err(()),
    }
}

fn validate_action(action: &str) -> Result<(), ()> {
    match action {
        "read" | "write" | "sync_receive" | "admin" => Ok(()),
        _ => Err(()),
    }
}
