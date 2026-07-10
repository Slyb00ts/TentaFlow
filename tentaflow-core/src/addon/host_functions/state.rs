// =============================================================================
// File: addon/host_functions/state.rs — host ABI for the shared AddonStateStore
// Purpose: expose the host-side in-memory state store (A1/A2) to WASM addons so
//          every instance (service / pooled / ephemeral) of the SAME addon
//          reads and writes one fast shared view instead of round-tripping
//          SQLite. All access is scoped to `caller.data().addon_id` ONLY — there
//          is no cross-addon read/write path here.
// Permissions: "state.read" (get/list) and "state.write" (set/delete).
//          Fail-closed: a missing permission blocks the call before the store is
//          touched. The CR-006 system-call bypass (is_system_call &&
//          user_id.is_none()) lets a service instance with the declared perms use
//          these functions on behalf of no specific user.
// =============================================================================

use tentaflow_sdk_spec::{StateEntryMeta, StateListOutput, StateSetInput};

use super::super::errors::AbiError;
use super::super::state_store::{AddonStateStore, StateStoreError, Tier, MAX_VALUE_BYTES};
use super::abi_helpers::{write_output_with_retry_semantics, PayloadKind};
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log, check_permission, get_memory, read_guest_string, AddonState, WasmCaller};

const PERM_READ: &str = "state.read";
const PERM_WRITE: &str = "state.write";

/// Maximum key length accepted across all state host functions (matches the
/// storage/config key cap). A key longer than this is rejected before the store
/// is touched.
const MAX_KEY_LENGTH: usize = 1024;

/// State-specific input ceiling for `state_set_v1`. A single set carries at most
/// one value (`MAX_VALUE_BYTES`) + one key (`MAX_KEY_LENGTH`) plus a small CBOR
/// map framing allowance. We intentionally do NOT reuse the 8 MiB
/// `PayloadKind::ServiceCall` ceiling here: a guest must not be able to push an
/// 8 MiB body through the state path (the store caps the value at 1 MiB anyway,
/// so anything bigger is wasted decode work and a needless DoS surface).
const STATE_SET_MAX_INPUT_BYTES: usize = MAX_VALUE_BYTES + MAX_KEY_LENGTH + 4 * 1024;

/// Maximum number of entries `state_list_v1` returns in one call. A shard may
/// hold up to `MAX_ENTRIES_PER_ADDON` (50k) keys; cloning + encoding all of them
/// would build a multi-megabyte response. The list is clipped to this many
/// entries and `truncated` is set so the addon can narrow its prefix.
const STATE_LIST_MAX_ENTRIES: usize = 1000;

/// Estimated byte budget for the encoded `StateListOutput`, kept well under the
/// 8 MiB `ServiceCall` ceiling. With `STATE_LIST_MAX_ENTRIES` (1000) and the
/// per-entry overhead this bounds a realistic response to a few hundred KiB.
const STATE_LIST_MAX_BYTES: usize = 1024 * 1024;

/// Wire encoding of `Tier` carried by `StateSetInput::tier` /
/// `StateEntryMeta::tier`. Kept here next to the only translation site so the
/// host never leaks the internal `Tier` enum across the ABI.
const TIER_EPHEMERAL: u8 = tentaflow_sdk_spec::STATE_TIER_EPHEMERAL;
const TIER_DURABLE: u8 = tentaflow_sdk_spec::STATE_TIER_DURABLE;

fn tier_to_wire(tier: Tier) -> u8 {
    match tier {
        Tier::Ephemeral => TIER_EPHEMERAL,
        Tier::Durable => TIER_DURABLE,
    }
}

fn tier_from_wire(raw: u8) -> Option<Tier> {
    match raw {
        TIER_EPHEMERAL => Some(Tier::Ephemeral),
        TIER_DURABLE => Some(Tier::Durable),
        _ => None,
    }
}

/// Maps a store error to the canonical ABI code: an oversized value is a
/// payload-size failure, an over-quota write is a quota failure.
fn store_error_to_abi(err: StateStoreError) -> AbiError {
    match err {
        StateStoreError::ValueTooLarge => AbiError::PayloadTooLarge,
        StateStoreError::AddonQuotaExceeded => AbiError::QuotaExceeded,
    }
}

// =============================================================================
// state_get_v1 — read one value from the calling addon's shared state
// =============================================================================

/// Host function: returns the value for `key` from the calling addon's shard.
///
/// ABI:
/// - key_ptr/key_len: key (UTF-8)
/// - out_ptr/out_cap: buffer for the raw value bytes
/// - out_len_ptr: bytes written (u32 LE) — on too-small, the required size
/// - Returns: `AbiError::Ok`, `AbiError::NotFound` (absent), `OutputBufferTooSmall`
///   (retry with a bigger buffer) or another error code.
///
/// Reads are light (no audit log) — matching the config/storage read convention.
pub fn state_get_v1(
    mut caller: WasmCaller<'_, AddonState>,
    key_ptr: i32,
    key_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    // Reject an oversized key by its declared length BEFORE reading/decoding the
    // guest bytes, so a malicious addon cannot make the host scan/allocate a huge
    // buffer just to fail the length check afterwards.
    if key_len < 0 || key_len as usize > MAX_KEY_LENGTH {
        return AbiError::PayloadTooLarge.as_i32();
    }

    let key = match read_guest_string(&memory, &caller, key_ptr, key_len) {
        Some(s) => s.to_string(),
        None => return AbiError::Operation.as_i32(),
    };
    if key.is_empty() {
        return AbiError::Operation.as_i32();
    }

    if !check_permission(caller.data(), PERM_READ, None) {
        audit_log(
            caller.data(),
            "state.get",
            Some("state"),
            Some(&key),
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    match AddonStateStore::global().get(&addon_id, &key) {
        Some(value) => write_output_with_retry_semantics(
            &memory,
            &mut caller,
            &value,
            out_ptr,
            out_cap,
            out_len_ptr,
        ),
        None => AbiError::NotFound.as_i32(),
    }
}

// =============================================================================
// state_set_v1 — write one value into the calling addon's shared state
// =============================================================================

/// Host function: writes `value` under `key` with the requested `tier`.
///
/// ABI:
/// - in_ptr/in_len: CBOR `StateSetInput { key, value, tier }`
/// - Returns: `AbiError::Ok`, `Permission`, `PayloadTooLarge` (value over the
///   per-value cap), `QuotaExceeded` (over the per-addon cap) or `Operation`
///   (malformed input / bad key / unknown tier).
///
/// Writes are significant — audit-logged like the other mutating host fns.
pub fn state_set_v1(mut caller: WasmCaller<'_, AddonState>, in_ptr: i32, in_len: i32) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    // Permission first: a write without `state.write` is denied before the host
    // decodes (or even bounds-checks) any guest payload.
    if !check_permission(caller.data(), PERM_WRITE, None) {
        audit_log(
            caller.data(),
            "state.set",
            Some("state"),
            None,
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    // State-specific input ceiling BEFORE the CBOR decode: a single set can never
    // legitimately exceed value+key+framing, so reject an oversized body without
    // pushing the full 8 MiB ServiceCall ceiling through this path.
    if in_len < 0 || in_len as usize > STATE_SET_MAX_INPUT_BYTES {
        audit_log(
            caller.data(),
            "state.set",
            Some("state"),
            None,
            "error",
            Some("input exceeds state set cap"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }

    let input: StateSetInput =
        match read_input_cbor(&memory, &caller, in_ptr, in_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => return e.as_i32(),
        };

    if input.key.is_empty() || input.key.len() > MAX_KEY_LENGTH {
        audit_log(
            caller.data(),
            "state.set",
            Some("state"),
            None,
            "error",
            Some("empty or oversized key"),
        );
        return AbiError::Operation.as_i32();
    }

    let tier = match tier_from_wire(input.tier) {
        Some(t) => t,
        None => {
            audit_log(
                caller.data(),
                "state.set",
                Some("state"),
                Some(&input.key),
                "error",
                Some("unknown tier"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    match AddonStateStore::global().set(&addon_id, &input.key, input.value, tier) {
        Ok(()) => {
            audit_log(
                caller.data(),
                "state.set",
                Some("state"),
                Some(&input.key),
                "ok",
                None,
            );
            AbiError::Ok.as_i32()
        }
        Err(e) => {
            let abi = store_error_to_abi(e);
            audit_log(
                caller.data(),
                "state.set",
                Some("state"),
                Some(&input.key),
                "error",
                Some(abi.description()),
            );
            abi.as_i32()
        }
    }
}

// =============================================================================
// state_delete_v1 — remove one key from the calling addon's shared state
// =============================================================================

/// Host function: removes `key`. Returns 1 if it existed, 0 if it was absent
/// (both are non-error outcomes), or a negative-equivalent error code on
/// permission/memory failure.
///
/// ABI:
/// - key_ptr/key_len: key (UTF-8)
/// - Returns: 1 (deleted), 0 (absent), `Permission` or `Operation`.
pub fn state_delete_v1(mut caller: WasmCaller<'_, AddonState>, key_ptr: i32, key_len: i32) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    // Reject an oversized key by its declared length before reading the bytes.
    if key_len < 0 || key_len as usize > MAX_KEY_LENGTH {
        return AbiError::PayloadTooLarge.as_i32();
    }

    let key = match read_guest_string(&memory, &caller, key_ptr, key_len) {
        Some(s) => s.to_string(),
        None => return AbiError::Operation.as_i32(),
    };
    if key.is_empty() {
        return AbiError::Operation.as_i32();
    }

    if !check_permission(caller.data(), PERM_WRITE, None) {
        audit_log(
            caller.data(),
            "state.delete",
            Some("state"),
            Some(&key),
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let deleted = AddonStateStore::global().delete(&addon_id, &key);
    audit_log(
        caller.data(),
        "state.delete",
        Some("state"),
        Some(&key),
        "ok",
        Some(if deleted { "deleted" } else { "absent" }),
    );
    if deleted {
        1
    } else {
        0
    }
}

// =============================================================================
// state_list_v1 — list key metadata under an optional prefix
// =============================================================================

/// Host function: lists `{key, size, tier}` for every key under `prefix` (or all
/// keys when prefix is empty), scoped to the calling addon.
///
/// ABI:
/// - prefix_ptr/prefix_len: prefix (UTF-8); (0,0) lists all keys
/// - out_ptr/out_cap: buffer for CBOR `StateListOutput`
/// - out_len_ptr: bytes written (u32 LE) — on too-small, the required size
/// - Returns: `AbiError::Ok`, `Permission`, `OutputBufferTooSmall` or error.
///
/// Lists are light (no audit log) — matching the storage list-read convention.
pub fn state_list_v1(
    mut caller: WasmCaller<'_, AddonState>,
    prefix_ptr: i32,
    prefix_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };

    // Reject an oversized prefix by its declared length before reading the bytes
    // (the prefix may be empty = list all, but a too-long one is rejected).
    if prefix_len < 0 || prefix_len as usize > MAX_KEY_LENGTH {
        return AbiError::PayloadTooLarge.as_i32();
    }

    let prefix = if prefix_ptr != 0 && prefix_len > 0 {
        match read_guest_string(&memory, &caller, prefix_ptr, prefix_len) {
            Some(s) => Some(s.to_string()),
            None => return AbiError::Operation.as_i32(),
        }
    } else {
        None
    };

    if !check_permission(caller.data(), PERM_READ, None) {
        audit_log(
            caller.data(),
            "state.list",
            Some("state"),
            prefix.as_deref(),
            "denied",
            None,
        );
        return AbiError::Permission.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let (collected, truncated) = AddonStateStore::global().list_bounded(
        &addon_id,
        prefix.as_deref(),
        STATE_LIST_MAX_ENTRIES,
        STATE_LIST_MAX_BYTES,
    );
    let entries = collected
        .into_iter()
        .map(|(key, size, tier)| StateEntryMeta {
            key,
            size: size as u64,
            tier: tier_to_wire(tier),
        })
        .collect();
    let output = StateListOutput { entries, truncated };

    write_cbor_capped(
        &memory,
        &mut caller,
        &output,
        out_ptr,
        out_cap,
        out_len_ptr,
        PayloadKind::ServiceCall,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_wire_roundtrip() {
        assert_eq!(tier_from_wire(TIER_EPHEMERAL), Some(Tier::Ephemeral));
        assert_eq!(tier_from_wire(TIER_DURABLE), Some(Tier::Durable));
        assert_eq!(tier_from_wire(2), None);
        assert_eq!(tier_from_wire(255), None);
        assert_eq!(tier_to_wire(Tier::Ephemeral), TIER_EPHEMERAL);
        assert_eq!(tier_to_wire(Tier::Durable), TIER_DURABLE);
    }

    #[test]
    fn store_error_mapping() {
        assert_eq!(
            store_error_to_abi(StateStoreError::ValueTooLarge),
            AbiError::PayloadTooLarge
        );
        assert_eq!(
            store_error_to_abi(StateStoreError::AddonQuotaExceeded),
            AbiError::QuotaExceeded
        );
    }

    #[test]
    fn state_set_input_cap_under_service_call_ceiling() {
        // The state-specific set ceiling must be far below the 8 MiB ServiceCall
        // ceiling so a guest cannot push a giant body through the state path.
        assert!(STATE_SET_MAX_INPUT_BYTES < 8 * 1024 * 1024);
        // It must still admit a full legal set: max value + max key + framing.
        assert!(STATE_SET_MAX_INPUT_BYTES >= MAX_VALUE_BYTES + MAX_KEY_LENGTH);
    }

    #[test]
    fn state_list_budget_under_service_call_ceiling() {
        // The list byte budget stays well under the ServiceCall ceiling so a
        // bounded list can never exceed the host output cap.
        assert!(STATE_LIST_MAX_BYTES < 8 * 1024 * 1024);
        assert!(STATE_LIST_MAX_ENTRIES > 0);
    }

    // End-to-end DoS-guard behaviour of the bounded list path used by the host
    // function: a full shard is clipped to STATE_LIST_MAX_ENTRIES with truncated
    // set, never materialising all 50k keys.
    #[test]
    fn host_list_path_clips_full_shard() {
        let store = AddonStateStore::new();
        for i in 0..(STATE_LIST_MAX_ENTRIES + 500) {
            store
                .set("addon", &format!("k{i:06}"), b"v".to_vec(), Tier::Ephemeral)
                .unwrap();
        }
        let (entries, truncated) =
            store.list_bounded("addon", None, STATE_LIST_MAX_ENTRIES, STATE_LIST_MAX_BYTES);
        assert_eq!(entries.len(), STATE_LIST_MAX_ENTRIES);
        assert!(truncated);
    }
}
