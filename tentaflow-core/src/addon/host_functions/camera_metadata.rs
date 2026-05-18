// =============================================================================
// File: addon/host_functions/camera_metadata.rs — ONVIF analytics metadata
// host functions (F2 P6.b).
// =============================================================================
//
// Three host functions:
//   * camera_metadata_subscribe_v1   — register an addon-side metadata
//                                       subscriber. Spawns (or refcounts) the
//                                       per-camera PullPoint task.
//   * camera_metadata_unsubscribe_v1 — release the subscriber; cancels the
//                                       pull task when the last addon
//                                       releases the camera.
//   * camera_metadata_poll_v1        — long-poll for the next batch of
//                                       MetadataFrames. WASM addons cannot
//                                       be invoked spontaneously by the host
//                                       (no host->guest callback channel),
//                                       so the addon drives delivery by
//                                       polling.
//
// Permission: `camera.metadata`. Risk class B for subscribe/unsubscribe
// (mutate supervisor state) and risk class C for poll (read-only drain).
// Org isolation: enforced via `get_camera_for_addon`, which restricts the
// query by `(owner_addon_id, org_id, removed_at IS NULL)`.

#![cfg(feature = "camera")]
#![allow(clippy::too_many_arguments)]

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{
    audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller,
};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::get_camera_for_addon;
use crate::services::camera_ingest::credentials::credentials_cipher;
use crate::services::camera_ingest::metadata_bus::{
    metadata_bus, MetadataMessage, MetadataStreamId, MetadataSubscriber, NextOutcome,
};
use crate::services::camera_ingest::metadata_supervisor::{
    MetadataPullSupervisor, SupervisorError,
};
use crate::services::camera_ingest::onvif_media::OnvifCredentials;

// =============================================================================
// Permission + tuning constants
// =============================================================================

const PERM_CAMERA_METADATA: &str = "camera.metadata";

/// Maximum addon-supplied poll timeout. Capped to keep a stuck addon from
/// pinning the host fuel budget for arbitrary durations. The bus subscriber
/// channel has its own 64-message capacity; an idle addon should still wake
/// at least every `MAX_POLL_TIMEOUT_MS` to drain it.
const MAX_POLL_TIMEOUT_MS: u32 = 30_000;

/// Maximum items the host returns per poll. Mirrors the supervisor's
/// `PULL_MAX_MESSAGES = 100` so a single poll can drain at most one device
/// batch.
const MAX_POLL_ITEMS: u32 = 100;

// =============================================================================
// Per-addon active-subscription registry
// =============================================================================
//
// Once an addon subscribes we hand it an opaque `subscription_id` (the
// metadata bus `MetadataStreamId`). The follow-up `poll` calls and the
// `unsubscribe` call need to map that id back to the live
// `MetadataSubscriber`. The subscriber is `!Send`-friendly but holds a
// tokio mpsc receiver; we keep it inside a process-wide DashMap keyed by
// the same id. The map intentionally outlives any single addon Store —
// an addon may be re-instantiated (cold reload, upgrade) and resume polling
// against the same id, although the supervisor refcount machinery does not
// rely on that.

use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::OnceLock;

struct ActiveSubscription {
    /// The bus subscriber, gated behind a `Mutex` so concurrent polls (an
    /// addon caller bug) serialise instead of corrupting the mpsc channel.
    subscriber: Mutex<MetadataSubscriber>,
    /// Owning addon id at registration time. Used to gate `poll` /
    /// `unsubscribe` against cross-addon hijack: even if an addon learns
    /// the opaque id of a sibling addon's subscription, the gate denies.
    addon_id: String,
    /// camera_id captured at subscription time — needed on unsubscribe to
    /// drop the supervisor refcount once the bus row is gone.
    camera_id: String,
}

fn active_registry() -> &'static DashMap<String, Arc<ActiveSubscription>> {
    static REG: OnceLock<DashMap<String, Arc<ActiveSubscription>>> = OnceLock::new();
    REG.get_or_init(DashMap::new)
}

// =============================================================================
// Wire payloads (TOML)
// =============================================================================

#[derive(Debug, Deserialize)]
struct SubscribeInput {
    camera_id: String,
}

#[derive(Debug, Serialize)]
struct SubscribeOutput {
    subscription_id: String,
    status: &'static str,
}

#[derive(Debug, Deserialize)]
struct UnsubscribeInput {
    subscription_id: String,
}

#[derive(Debug, Serialize)]
struct UnsubscribeOutput {
    unsubscribed: bool,
}

#[derive(Debug, Deserialize)]
struct PollInput {
    subscription_id: String,
    #[serde(default = "default_max_items")]
    max_items: u32,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u32,
}

fn default_max_items() -> u32 {
    10
}

fn default_timeout_ms() -> u32 {
    5_000
}

#[derive(Debug, Serialize)]
struct PollOutput {
    frames: Vec<MetadataFrameOut>,
    /// Set when the bus signalled `CameraOffline` mid-poll. The addon should
    /// stop polling and treat the subscription as terminated.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    camera_offline: bool,
    /// Set when the bus reported dropped frames (backpressure). The count
    /// accumulates across polls until the addon catches up.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    dropped: u64,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

#[derive(Debug, Serialize)]
struct MetadataFrameOut {
    camera_id: String,
    ts_unix_ms: i64,
    items: Vec<MetadataItemOut>,
}

#[derive(Debug, Serialize)]
struct MetadataItemOut {
    class: String,
    confidence: f64,
    /// `[left, top, right, bottom]` in normalised 0..1 device coords.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bbox: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    track_id: Option<String>,
}

// =============================================================================
// Host functions
// =============================================================================

pub fn camera_metadata_subscribe_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.metadata.subscribe", None, "error", Some("input_read_failed"));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERA_METADATA, None) {
        audit(
            caller.data(),
            "camera.metadata.subscribe",
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: SubscribeInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.metadata.subscribe", None, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let org_id = caller.data().org_id.clone();
    let db = caller.data().db.clone();

    // Org isolation + ownership. NotFound covers all three of: foreign
    // addon, foreign org, soft-deleted — addons learn nothing about the
    // existence of cameras outside their tenancy.
    let row = match get_camera_for_addon(&db, &addon_id, &input.camera_id, org_id.as_deref()) {
        Ok(Some(r)) => r,
        Ok(None) => {
            audit(
                caller.data(),
                "camera.metadata.subscribe",
                Some(&input.camera_id),
                "denied",
                Some("not_found_or_not_owned"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "camera.metadata.subscribe",
                Some(&input.camera_id),
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    if !row.metadata_supported {
        audit(
            caller.data(),
            "camera.metadata.subscribe",
            Some(&input.camera_id),
            "denied",
            Some("camera_metadata_not_supported"),
        );
        return AbiError::Operation.as_i32();
    }

    // The events service URL must come from a successful ONVIF discovery —
    // `onvif_url` carries the device-service endpoint, from which we derive
    // the events endpoint by swapping the path. Falling back to the same
    // host as a last resort is intentionally not done: an addon that hits
    // an unknown path would loop on transport errors and burn host fuel.
    let events_url = match row.onvif_url.as_deref().and_then(derive_events_service_url) {
        Some(u) => u,
        None => {
            audit(
                caller.data(),
                "camera.metadata.subscribe",
                Some(&input.camera_id),
                "error",
                Some("events_url_missing"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    // Decrypt credentials. Plaintext lives in `_plain` for the lifetime of
    // the credentials struct and gets dropped at the end of the function.
    let creds_blob = match row.credentials_encrypted.as_deref() {
        Some(b) => b,
        None => {
            audit(
                caller.data(),
                "camera.metadata.subscribe",
                Some(&input.camera_id),
                "error",
                Some("credentials_missing"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let plain = match credentials_cipher().decrypt(creds_blob) {
        Ok(p) => p,
        Err(_) => {
            audit(
                caller.data(),
                "camera.metadata.subscribe",
                Some(&input.camera_id),
                "error",
                Some("credentials_decrypt_failed"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let (username, password) = match plain.split_once(':') {
        Some((u, p)) if !u.is_empty() && !p.is_empty() => (u.to_string(), p.to_string()),
        _ => {
            audit(
                caller.data(),
                "camera.metadata.subscribe",
                Some(&input.camera_id),
                "error",
                Some("credentials_format_invalid"),
            );
            return AbiError::Operation.as_i32();
        }
    };
    let creds = OnvifCredentials { username, password };

    // First subscribe to the bus so the supervisor's first publish race is
    // observable — if the supervisor publishes between create-task and
    // subscribe, the addon would otherwise miss the very first frame.
    let subscriber = metadata_bus().subscribe(&input.camera_id);
    let stream_id = subscriber.stream_id.clone();

    // Then spawn (or refcount) the pull task.
    let supervisor = MetadataPullSupervisor::global();
    let ensure_res = block_in_place_on(supervisor.ensure_pull_task(
        &input.camera_id,
        creds,
        events_url,
    ));
    if let Err(e) = ensure_res {
        // Roll back the bus subscription so the registry is consistent.
        metadata_bus().unsubscribe(&input.camera_id, &stream_id);
        let (abi_err, reason) = match e {
            SupervisorError::AuthFailed => (AbiError::Permission, "onvif_auth_failed"),
            SupervisorError::Transport(_) => (AbiError::CameraUnreachable, "onvif_transport_failure"),
        };
        audit(
            caller.data(),
            "camera.metadata.subscribe",
            Some(&input.camera_id),
            "error",
            Some(reason),
        );
        return abi_err.as_i32();
    }

    // Register the active subscription so subsequent poll / unsubscribe
    // calls can map subscription_id -> subscriber.
    let active = Arc::new(ActiveSubscription {
        subscriber: Mutex::new(subscriber),
        addon_id: addon_id.clone(),
        camera_id: input.camera_id.clone(),
    });
    active_registry().insert(stream_id.as_str().to_string(), active);

    audit(
        caller.data(),
        "camera.metadata.subscribe",
        Some(&input.camera_id),
        "ok",
        None,
    );

    let out = SubscribeOutput {
        subscription_id: stream_id.as_str().to_string(),
        status: "subscribed",
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

pub fn camera_metadata_unsubscribe_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit(caller.data(), "camera.metadata.unsubscribe", None, "error", Some("input_read_failed"));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERA_METADATA, None) {
        audit(
            caller.data(),
            "camera.metadata.unsubscribe",
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: UnsubscribeInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "camera.metadata.unsubscribe", None, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };

    // Peek the active entry to enforce addon ownership BEFORE we atomically
    // remove it. The ownership check must precede the remove so a foreign
    // caller cannot use this host fn to delete a sibling's registration.
    let caller_addon = caller.data().addon_id.clone();
    {
        let snapshot = active_registry().get(&input.subscription_id);
        match snapshot {
            Some(e) => {
                if e.value().addon_id != caller_addon {
                    drop(e);
                    audit_with_risk(
                        caller.data(),
                        "camera.metadata.unsubscribe",
                        Some(&input.subscription_id),
                        RiskClass::B,
                        "denied",
                        Some("cross_addon_unsubscribe"),
                    );
                    return AbiError::NotFound.as_i32();
                }
            }
            None => {
                audit_with_risk(
                    caller.data(),
                    "camera.metadata.unsubscribe",
                    Some(&input.subscription_id),
                    RiskClass::B,
                    "denied",
                    Some("subscription_not_found"),
                );
                let out = UnsubscribeOutput { unsubscribed: false };
                return write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr);
            }
        }
    }

    // Atomic remove — whichever thread wins the `remove` call is the one
    // that drops the bus row and releases the supervisor refcount. Other
    // concurrent unsubscribes for the same id observe `None` and return
    // idempotent `unsubscribed=false` without double-decrementing.
    let active = match active_registry().remove(&input.subscription_id) {
        Some((_k, v)) => v,
        None => {
            // Lost the race to a concurrent unsubscribe. Idempotent return.
            audit_with_risk(
                caller.data(),
                "camera.metadata.unsubscribe",
                Some(&input.subscription_id),
                RiskClass::B,
                "ok",
                Some("already_unsubscribed"),
            );
            let out = UnsubscribeOutput { unsubscribed: false };
            return write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr);
        }
    };

    // Drop the bus subscriber row and await the supervisor refcount drain.
    // We use `release_and_wait` (not the sync `release`) so a subscribe
    // immediately following an unsubscribe sees the previous device-side
    // PullPoint torn down before a fresh CreatePullPointSubscription fires.
    let stream_id = MetadataStreamId::from_str_unchecked(&input.subscription_id);
    let _ = metadata_bus().unsubscribe_by_stream_id(&stream_id);
    let camera_id_for_release = active.camera_id.clone();
    block_in_place_on(async move {
        MetadataPullSupervisor::global()
            .release_and_wait(&camera_id_for_release)
            .await;
    });

    audit_with_risk(
        caller.data(),
        "camera.metadata.unsubscribe",
        Some(&active.camera_id),
        RiskClass::B,
        "ok",
        None,
    );

    let out = UnsubscribeOutput { unsubscribed: true };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

pub fn camera_metadata_poll_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let raw = match read_input_toml(&memory, &caller, input_ptr, input_len) {
        Ok(s) => s,
        Err(e) => {
            audit_with_risk(caller.data(), "camera.metadata.poll", None, RiskClass::C, "error", Some("input_read_failed"));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_CAMERA_METADATA, None) {
        audit_with_risk(caller.data(), "camera.metadata.poll", None, RiskClass::C, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: PollInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit_with_risk(caller.data(), "camera.metadata.poll", None, RiskClass::C, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };

    let timeout_ms = input.timeout_ms.min(MAX_POLL_TIMEOUT_MS);
    let max_items = input.max_items.clamp(1, MAX_POLL_ITEMS) as usize;

    let active = match active_registry().get(&input.subscription_id) {
        Some(e) => e.value().clone(),
        None => {
            audit_with_risk(
                caller.data(),
                "camera.metadata.poll",
                Some(&input.subscription_id),
                RiskClass::C,
                "denied",
                Some("subscription_not_found"),
            );
            return AbiError::StreamNotFound.as_i32();
        }
    };
    let caller_addon = caller.data().addon_id.clone();
    if active.addon_id != caller_addon {
        audit_with_risk(
            caller.data(),
            "camera.metadata.poll",
            Some(&input.subscription_id),
            RiskClass::C,
            "denied",
            Some("cross_addon_poll"),
        );
        return AbiError::StreamNotFound.as_i32();
    }

    // First message: bounded wait. Once we receive anything, drain
    // synchronously up to `max_items` with a 0 ms timeout. This keeps the
    // common case ("device emitted three objects on the same tick") as a
    // single ABI call without re-blocking.
    let mut frames: Vec<MetadataFrameOut> = Vec::new();
    let mut dropped: u64 = 0;
    let mut camera_offline = false;

    let first = {
        let mut sub_guard = active.subscriber.lock();
        block_in_place_on(sub_guard.next(Duration::from_millis(timeout_ms as u64)))
    };
    match first {
        NextOutcome::Timeout => {}
        NextOutcome::Closed => {
            camera_offline = true;
        }
        NextOutcome::Message(m) => {
            apply_message(m, &mut frames, &mut dropped, &mut camera_offline);
            // Drain synchronously.
            while frames.len() < max_items && !camera_offline {
                let mut sub_guard = active.subscriber.lock();
                let next =
                    block_in_place_on(sub_guard.next(Duration::from_millis(0)));
                drop(sub_guard);
                match next {
                    NextOutcome::Timeout | NextOutcome::Closed => {
                        if matches!(next, NextOutcome::Closed) {
                            camera_offline = true;
                        }
                        break;
                    }
                    NextOutcome::Message(m) => {
                        apply_message(m, &mut frames, &mut dropped, &mut camera_offline);
                    }
                }
            }
        }
    }

    audit_with_risk(
        caller.data(),
        "camera.metadata.poll",
        Some(&active.camera_id),
        RiskClass::C,
        "ok",
        None,
    );

    let out = PollOutput {
        frames,
        camera_offline,
        dropped,
    };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Helpers
// =============================================================================

fn apply_message(
    m: MetadataMessage,
    frames: &mut Vec<MetadataFrameOut>,
    dropped: &mut u64,
    camera_offline: &mut bool,
) {
    match m {
        MetadataMessage::Frame(f) => {
            let items = f
                .items
                .into_iter()
                .map(|it| MetadataItemOut {
                    class: it.class,
                    confidence: it.confidence,
                    bbox: it.bbox.map(|b| [b.left, b.top, b.right, b.bottom]),
                    track_id: it.track_id,
                })
                .collect();
            frames.push(MetadataFrameOut {
                camera_id: f.camera_id,
                ts_unix_ms: f.ts_unix,
                items,
            });
        }
        MetadataMessage::Drop { count } => {
            *dropped = dropped.saturating_add(count);
        }
        MetadataMessage::CameraOffline { reason } => {
            warn!("camera.metadata.poll: camera offline ({reason})");
            *camera_offline = true;
        }
    }
}

/// Derive the ONVIF events service URL from the device-service URL. Most
/// camera vendors publish them on the same authority with a fixed path
/// suffix: `/onvif/device_service` -> `/onvif/event_service`. We do a
/// path-segment swap rather than a string replace so a vendor that
/// formats `device_service` differently still routes correctly. Returns
/// `None` on parse failure or when the URL is missing a recognisable
/// suffix — callers map this to ABI_ERR_OPERATION.
fn derive_events_service_url(device_service_url: &str) -> Option<String> {
    let parsed = url::Url::parse(device_service_url).ok()?;
    let mut new_url = parsed.clone();
    let path = parsed.path();
    let new_path = if let Some(stripped) = path.strip_suffix("/device_service") {
        format!("{stripped}/event_service")
    } else if let Some(stripped) = path.strip_suffix("/device") {
        format!("{stripped}/events")
    } else {
        // Generic vendors that already encode the event service path can be
        // detected via the `event` segment — if it's there, keep the URL.
        if path.contains("event") {
            return Some(device_service_url.to_string());
        }
        return None;
    };
    new_url.set_path(&new_path);
    Some(new_url.to_string())
}

fn read_input_toml(
    memory: &super::super::runtime::WasmMemory,
    caller: &WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
) -> Result<String, AbiError> {
    if input_len < 0 {
        return Err(AbiError::Operation);
    }
    if enforce_payload_size(input_len as usize, PayloadKind::ServiceCall).is_err() {
        return Err(AbiError::PayloadTooLarge);
    }
    let bytes =
        read_guest_bytes(memory, caller, input_ptr, input_len).ok_or(AbiError::Operation)?;
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|_| AbiError::Operation)
}

fn write_toml_capped<T: Serialize>(
    memory: &super::super::runtime::WasmMemory,
    caller: &mut WasmCaller<'_, AddonState>,
    value: &T,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let serialized = match toml::to_string(value) {
        Ok(s) => s,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    if enforce_payload_size(serialized.len(), PayloadKind::ServiceCall).is_err() {
        return AbiError::PayloadTooLarge.as_i32();
    }
    write_output_with_retry_semantics(
        memory,
        caller,
        serialized.as_bytes(),
        out_ptr,
        out_cap,
        out_len_ptr,
    )
}

fn audit(state: &AddonState, action: &str, resource_id: Option<&str>, result: &str, reason: Option<&str>) {
    // Default risk class for subscribe/unsubscribe: B (mutates supervisor
    // state). Poll callers use `audit_with_risk` with `RiskClass::C` since
    // a poll is a read-only drain of an existing subscriber's mpsc queue.
    audit_with_risk(state, action, resource_id, RiskClass::B, result, reason);
}

fn audit_with_risk(
    state: &AddonState,
    action: &str,
    resource_id: Option<&str>,
    risk: RiskClass,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("camera"),
        resource_id,
        risk,
        None,
        None,
        result,
        reason,
    );
}

fn block_in_place_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

// =============================================================================
// Test-only API surface
// =============================================================================
//
// The integration tests in `tests/camera_metadata_host_fn.rs` exercise the
// permission gate, the org-isolation guard, the metadata_supported gate,
// and the supervisor refcount semantics WITHOUT spinning up a WASM Store.
// They drive the underlying logic via direct DB inserts + bus publishes.
// The shapes below mirror the wire structs for that purpose.

#[doc(hidden)]
pub mod test_api {
    use super::*;

    /// Mirror of the host-fn permission check + ownership + metadata_supported
    /// gate. Returns the same ABI error codes the WASM caller would see.
    /// Used by tests to validate denials without a live ONVIF endpoint.
    pub fn precheck_subscribe(
        state: &AddonState,
        camera_id: &str,
    ) -> Result<(), AbiError> {
        if !check_permission(state, PERM_CAMERA_METADATA, None) {
            return Err(AbiError::Permission);
        }
        let db = state.db.clone();
        let org_id = state.org_id.clone();
        let row = match get_camera_for_addon(&db, &state.addon_id, camera_id, org_id.as_deref()) {
            Ok(Some(r)) => r,
            Ok(None) => return Err(AbiError::NotFound),
            Err(_) => return Err(AbiError::Operation),
        };
        if !row.metadata_supported {
            return Err(AbiError::Operation);
        }
        Ok(())
    }

    /// Convenience: programmatically register a subscription against the
    /// process-wide registry so the poll/unsubscribe paths can be tested in
    /// isolation. Returns the synthetic subscription_id.
    pub fn register_active_subscription(
        addon_id: &str,
        camera_id: &str,
    ) -> String {
        let subscriber = metadata_bus().subscribe(camera_id);
        let stream_id = subscriber.stream_id.clone();
        let active = Arc::new(ActiveSubscription {
            subscriber: Mutex::new(subscriber),
            addon_id: addon_id.to_string(),
            camera_id: camera_id.to_string(),
        });
        let id = stream_id.as_str().to_string();
        active_registry().insert(id.clone(), active);
        id
    }

    pub fn active_count() -> usize {
        active_registry().len()
    }

    pub fn drop_active_subscription(id: &str) {
        active_registry().remove(id);
    }
}

// `MetadataStreamId` has no public constructor from a borrowed &str — we
// need one to map the addon-supplied subscription_id back into the bus.
// Live in this file (not `metadata_bus.rs`) so the bus API stays minimal.
impl MetadataStreamId {
    pub(crate) fn from_str_unchecked(s: &str) -> Self {
        // The id is opaque on the wire; if the caller hands us garbage,
        // `metadata_bus().unsubscribe_by_stream_id` will simply find no
        // matching entry and return None. We never parse it.
        // Safety: `MetadataStreamId(String)` is the inner shape.
        Self::new_from_raw(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_events_url_swaps_device_service_suffix() {
        assert_eq!(
            derive_events_service_url("http://192.168.1.10/onvif/device_service"),
            Some("http://192.168.1.10/onvif/event_service".to_string())
        );
    }

    #[test]
    fn derive_events_url_handles_short_path() {
        assert_eq!(
            derive_events_service_url("http://cam.local/onvif/device"),
            Some("http://cam.local/onvif/events".to_string())
        );
    }

    #[test]
    fn derive_events_url_keeps_url_with_event_segment() {
        assert_eq!(
            derive_events_service_url("http://cam.local/event_service"),
            Some("http://cam.local/event_service".to_string())
        );
    }

    #[test]
    fn derive_events_url_returns_none_for_unknown_suffix() {
        assert_eq!(
            derive_events_service_url("http://cam.local/api/v1/things"),
            None
        );
    }

    #[test]
    fn derive_events_url_returns_none_for_invalid_url() {
        assert_eq!(derive_events_service_url("not-a-url"), None);
    }
}
