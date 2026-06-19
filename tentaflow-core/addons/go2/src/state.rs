// =============================================================================
// File: addons/go2/src/state.rs
// Host-side shared state store access for the go2 addon. Wraps the `state.*` host
// functions (state_get/set/delete) exposed by the core AddonStateStore so live
// telemetry, lidar status and the connection-status mirror are visible across ALL
// instances of this addon (the service instance that drains the WebRTC stream AND
// the ephemeral pooled workers that serve go2.status / go2.lidar_* tool calls).
// This replaces the per-worker thread_local + the per-addon `robot_live` SQLite
// round-trip on the live hot path.
// =============================================================================

extern crate alloc;

use alloc::string::String;
#[cfg(not(test))]
use alloc::vec;
use alloc::vec::Vec;

#[cfg(not(test))]
use tentaflow_sdk_spec::protocol::state::StateSetInput;
use tentaflow_sdk_spec::protocol::state::{STATE_TIER_DURABLE, STATE_TIER_EPHEMERAL};

use crate::AbiError;

// Stable keys this addon stores. Each instance scopes by addon_id host-side, so
// these need no per-instance namespacing — a single Go2 robot per install.
/// Latest structured telemetry snapshot (the exact `telemetry_json()` bytes).
pub const KEY_TELEMETRY: &str = "live:telemetry";
/// Small LiDAR availability metadata (the `lidar_status_json()` bytes).
pub const KEY_LIDAR_STATUS: &str = "live:lidar_status";
/// Connection-status mirror for fast cross-worker / host-side reads (advertise).
pub const KEY_STATUS: &str = "live:status";
/// Operator LiDAR enable intent. Durable so it survives a restart, exactly like
/// the old `robot_live.lidar_enabled` column did.
pub const KEY_LIDAR_ENABLED: &str = "lidar:enabled";

#[cfg(not(test))]
#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn state_get_v1(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn state_set_v1(in_ptr: i32, in_len: i32) -> i32;
    fn state_delete_v1(key_ptr: i32, key_len: i32) -> i32;
}

/// Read a raw value for `key`. `Ok(None)` for an absent key (NotFound), `Err` for
/// any real ABI failure so a caller never confuses "no value yet" with "read
/// failed" (the latter must NOT be interpreted as a missing/false live state).
#[cfg(not(test))]
pub fn get(key: &str) -> Result<Option<Vec<u8>>, AbiError> {
    let mut cap = 4096usize;
    loop {
        let mut buf = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            state_get_v1(
                key.as_ptr() as i32,
                key.len() as i32,
                buf.as_mut_ptr() as i32,
                cap as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        if ret == 6 {
            let want = if out_len > 0 { out_len as usize } else { 0 };
            cap = want.max(cap.saturating_mul(2));
            continue;
        }
        if ret == 2 {
            return Ok(None);
        }
        if ret != 0 {
            return Err(AbiError::from_code(ret));
        }
        if out_len < 0 || out_len as usize > cap {
            return Err(AbiError::Operation);
        }
        buf.truncate(out_len as usize);
        return Ok(Some(buf));
    }
}

/// Write `value` under `key` at the requested tier.
#[cfg(not(test))]
fn set(key: &str, value: Vec<u8>, tier: u8) -> Result<(), AbiError> {
    let input = StateSetInput {
        key: key.into(),
        value,
        tier,
    };
    let mut bytes = Vec::with_capacity(input.value.len() + key.len() + 16);
    minicbor::encode(&input, &mut bytes).map_err(|_| AbiError::Operation)?;
    let ret = unsafe { state_set_v1(bytes.as_ptr() as i32, bytes.len() as i32) };
    if ret != 0 {
        return Err(AbiError::from_code(ret));
    }
    Ok(())
}

/// Delete `key`. Absent (0) and deleted (1) are both success; only a real ABI
/// failure surfaces as `Err`.
#[cfg(not(test))]
pub fn delete(key: &str) -> Result<(), AbiError> {
    let ret = unsafe { state_delete_v1(key.as_ptr() as i32, key.len() as i32) };
    if ret == 0 || ret == 1 {
        return Ok(());
    }
    Err(AbiError::from_code(ret))
}

/// Store an ephemeral (RAM-only) value — telemetry / lidar status / status mirror.
pub fn set_ephemeral(key: &str, value: Vec<u8>) -> Result<(), AbiError> {
    set(key, value, STATE_TIER_EPHEMERAL)
}

/// Store a durable (restart-surviving) value — the LiDAR enable intent.
pub fn set_durable(key: &str, value: Vec<u8>) -> Result<(), AbiError> {
    set(key, value, STATE_TIER_DURABLE)
}

/// Read a stored UTF-8 string value, `Ok(None)` when the key is absent.
pub fn get_string(key: &str) -> Result<Option<String>, AbiError> {
    match get(key)? {
        Some(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        None => Ok(None),
    }
}

// =============================================================================
// In-memory test backend
//
// Native tests can't round-trip the host state ABI (it passes guest pointers as
// i32, which truncates 64-bit stack addresses → SIGSEGV — same constraint the SQL
// path has). Under `#[cfg(test)]` the get/set/delete entry points route to this
// per-thread map, so the addon's cross-worker live-state tests exercise the real
// telemetry/lidar key round-trips without the host. The actual store semantics
// (tier handling, quotas, permission gating) are covered by the host-side
// state_store / state host-fn tests in tentaflow-core.
// =============================================================================

#[cfg(test)]
mod test_backend {
    use super::*;
    use alloc::collections::BTreeMap;
    use core::cell::RefCell;

    std::thread_local! {
        static STORE: RefCell<BTreeMap<String, Vec<u8>>> = RefCell::new(BTreeMap::new());
        // When set, the next get() returns an AbiError so a test can exercise
        // read-error propagation (e.g. the lidar-enabled intent read).
        static FAIL_NEXT_GET: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    }

    pub fn reset() {
        STORE.with(|c| c.borrow_mut().clear());
        FAIL_NEXT_GET.with(|c| c.set(false));
    }

    pub fn fail_next_get() {
        FAIL_NEXT_GET.with(|c| c.set(true));
    }

    pub fn get(key: &str) -> Result<Option<Vec<u8>>, AbiError> {
        if FAIL_NEXT_GET.with(|c| c.replace(false)) {
            return Err(AbiError::Operation);
        }
        Ok(STORE.with(|c| c.borrow().get(key).cloned()))
    }

    pub fn set(key: &str, value: Vec<u8>) -> Result<(), AbiError> {
        STORE.with(|c| {
            c.borrow_mut().insert(key.into(), value);
        });
        Ok(())
    }

    pub fn delete(key: &str) -> Result<(), AbiError> {
        STORE.with(|c| {
            c.borrow_mut().remove(key);
        });
        Ok(())
    }
}

#[cfg(test)]
pub fn get(key: &str) -> Result<Option<Vec<u8>>, AbiError> {
    test_backend::get(key)
}

#[cfg(test)]
fn set(key: &str, value: Vec<u8>, _tier: u8) -> Result<(), AbiError> {
    test_backend::set(key, value)
}

#[cfg(test)]
pub fn delete(key: &str) -> Result<(), AbiError> {
    test_backend::delete(key)
}

/// Reset the in-memory test state store between tests.
#[cfg(test)]
pub fn test_reset() {
    test_backend::reset();
}

/// Arm a one-shot read failure on the next `get` (test-only) so callers can
/// assert read-error propagation rather than a false default.
#[cfg(test)]
pub fn test_fail_next_get() {
    test_backend::fail_next_get();
}
