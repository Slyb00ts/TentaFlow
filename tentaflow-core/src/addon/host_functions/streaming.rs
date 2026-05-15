// ============ File: streaming.rs — Streaming host functions (M1.W7 F1a TentaVision) ============
//
// Three host functions bridging the `services::streaming::StreamingBus` to WASM:
//   - `stream_subscribe_v1` — register a new subscriber against a camera
//   - `stream_next_v1`      — bounded-await poll for the next message
//   - `stream_close_v1`     — drop subscriber + unsubscribe
//
// Frame bytes are never inlined in `stream_next` output — the addon only sees
// `frame_ref`+metadata, and the actual byte payload moves to a service via the
// `service_call_v1` PickupToken flow. This keeps a 30 fps × 1080p stream from
// crushing the host↔guest copy path.

#![cfg(feature = "camera")]
#![allow(clippy::too_many_arguments)]

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use super::abi_helpers::{enforce_payload_size, write_output_with_retry_semantics, PayloadKind};
use super::{audit_log_with_risk, check_permission, get_memory, read_guest_bytes, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::get_camera_for_addon;
use crate::services::streaming::{StreamFilter, StreamMessage, StreamSubscriber};
use crate::services::{streaming_bus};

// =============================================================================
// Permission
// =============================================================================

const PERM_STREAMS_SUBSCRIBE: &str = "streams.subscribe";

// =============================================================================
// Per-addon subscriber registry
// =============================================================================
//
// `stream_subscribe_v1` returns a `stream_id` to the addon; later `stream_next`
// / `stream_close` calls must look the subscriber back up. The registry keeps
// the `StreamSubscriber` alive (drop closes the channel) and is keyed by the
// pair (addon_id, stream_id) so two addons cannot collide.

type RegistryKey = (String, String);

struct SubscriberSlot {
    /// Camera id is duplicated here so future eager `unsubscribe(bus, …)`
    /// paths do not have to re-query DB. Today the lazy reap path covers it,
    /// so `#[allow(dead_code)]` until M3 wires explicit unsubscribe.
    #[allow(dead_code)]
    camera_id: String,
    /// `Arc<tokio::sync::Mutex<…>>` — `stream_next` is sync from the host side
    /// (it goes through `run_async`) but mutates the subscriber's receiver, so
    /// concurrent calls on the same stream_id must serialize.
    subscriber: Arc<tokio::sync::Mutex<StreamSubscriber>>,
}

static SUBSCRIBERS: OnceLock<DashMap<RegistryKey, SubscriberSlot>> = OnceLock::new();

fn subscribers() -> &'static DashMap<RegistryKey, SubscriberSlot> {
    SUBSCRIBERS.get_or_init(DashMap::new)
}

fn run_async<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

// =============================================================================
// Input / output payloads (TOML)
// =============================================================================

#[derive(Debug, Deserialize)]
struct SubscribeInput {
    /// `camera:<camera_id>` for F1a. F1b will add `service:<…>`.
    target: String,
    #[serde(default)]
    filter: Option<SubscribeFilter>,
}

#[derive(Debug, Deserialize, Default)]
struct SubscribeFilter {
    max_fps: Option<u32>,
    #[serde(default)]
    skip_frames: u32,
}

#[derive(Debug, Serialize)]
struct SubscribeOutput {
    stream_id: String,
}

#[derive(Debug, Deserialize)]
struct NextInput {
    stream_id: String,
    timeout_ms: u64,
}

#[derive(Debug, Serialize)]
struct NextFrameOutput<'a> {
    r#type: &'static str,
    frame_ref: &'a str,
    camera_id: &'a str,
    width: u32,
    height: u32,
    pixel_format: &'a str,
    timestamp_unix_ms: u64,
}

#[derive(Debug, Serialize)]
struct NextDropOutput {
    r#type: &'static str,
    count: u64,
}

#[derive(Debug, Serialize)]
struct NextCameraOfflineOutput<'a> {
    r#type: &'static str,
    reason: &'a str,
}

#[derive(Debug, Serialize)]
struct NextTimeoutOutput {
    r#type: &'static str,
}

#[derive(Debug, Deserialize)]
struct CloseInput {
    stream_id: String,
}

#[derive(Debug, Serialize)]
struct CloseOutput {
    closed: bool,
}

// =============================================================================
// Helpers
// =============================================================================

const MAX_TIMEOUT_MS: u64 = 5_000;

fn audit(state: &AddonState, action: &str, resource_id: Option<&str>, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        action,
        Some("stream"),
        resource_id,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
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
    let bytes = read_guest_bytes(memory, caller, input_ptr, input_len).ok_or(AbiError::Operation)?;
    std::str::from_utf8(bytes).map(|s| s.to_string()).map_err(|_| AbiError::Operation)
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
    write_output_with_retry_semantics(memory, caller, serialized.as_bytes(), out_ptr, out_cap, out_len_ptr)
}

/// Parses `camera:<camera_id>` — returns the trailing camera id. F1a only
/// understands the `camera:` prefix; anything else maps to `Operation`.
fn parse_target(target: &str) -> Result<&str, AbiError> {
    target.strip_prefix("camera:").ok_or(AbiError::Operation)
}

fn stream_id_valid(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("stream_") {
        rest.len() == 36
            && rest.chars().enumerate().all(|(i, c)| {
                let dash_pos = matches!(i, 8 | 13 | 18 | 23);
                if dash_pos {
                    c == '-'
                } else {
                    c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
                }
            })
    } else {
        false
    }
}

// =============================================================================
// Host function: stream_subscribe_v1
// =============================================================================

pub fn stream_subscribe_v1(
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
            audit(caller.data(), "stream.subscribe", None, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_STREAMS_SUBSCRIBE, None) {
        audit(caller.data(), "stream.subscribe", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: SubscribeInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "stream.subscribe", None, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    let camera_id = match parse_target(&input.target) {
        Ok(c) => c.to_string(),
        Err(e) => {
            audit(caller.data(), "stream.subscribe", None, "denied", Some("invalid_target"));
            return e.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    // Ownership enforcement — F1a forbids cross-addon subscribes. Result mapped
    // to `NotFound` so an addon cannot enumerate cameras owned by peers.
    match get_camera_for_addon(&db, &addon_id, &camera_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(caller.data(), "stream.subscribe", Some(&camera_id), "denied", Some("not_found_or_not_owned"));
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(caller.data(), "stream.subscribe", Some(&camera_id), "error", Some("db_error"));
            return AbiError::Operation.as_i32();
        }
    }

    let filter = match input.filter {
        Some(f) => StreamFilter { max_fps: f.max_fps, skip_frames: f.skip_frames },
        None => StreamFilter::default(),
    };
    let sub = streaming_bus().subscribe(&camera_id, filter);
    let stream_id = sub.stream_id.to_string();
    subscribers().insert(
        (addon_id.clone(), stream_id.clone()),
        SubscriberSlot {
            camera_id: camera_id.clone(),
            subscriber: Arc::new(tokio::sync::Mutex::new(sub)),
        },
    );

    audit(caller.data(), "stream.subscribe", Some(&camera_id), "ok", Some(&format!("stream_id={}", stream_id)));
    let out = SubscribeOutput { stream_id };
    write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
}

// =============================================================================
// Host function: stream_next_v1
// =============================================================================

pub fn stream_next_v1(
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
            audit(caller.data(), "stream.next", None, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_STREAMS_SUBSCRIBE, None) {
        audit(caller.data(), "stream.next", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: NextInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "stream.next", None, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !stream_id_valid(&input.stream_id) {
        audit(caller.data(), "stream.next", None, "denied", Some("stream_id_invalid"));
        return AbiError::Operation.as_i32();
    }
    let timeout = Duration::from_millis(input.timeout_ms.min(MAX_TIMEOUT_MS));

    let addon_id = caller.data().addon_id.clone();
    let sub_arc = match subscribers().get(&(addon_id.clone(), input.stream_id.clone())) {
        Some(slot) => slot.subscriber.clone(),
        None => {
            audit(caller.data(), "stream.next", Some(&input.stream_id), "denied", Some("stream_not_found"));
            return AbiError::StreamNotFound.as_i32();
        }
    };

    let msg = run_async(async move {
        let mut guard = sub_arc.lock().await;
        guard.next(timeout).await
    });

    match msg {
        Some(StreamMessage::Frame { frame_ref, metadata }) => {
            let pf = match metadata.pixel_format {
                crate::services::frame_storage::FramePixelFormat::Rgb24 => "rgb24",
            };
            let out = NextFrameOutput {
                r#type: "frame",
                frame_ref: frame_ref.as_str(),
                camera_id: &metadata.camera_id,
                width: metadata.width,
                height: metadata.height,
                pixel_format: pf,
                timestamp_unix_ms: metadata.timestamp_unix_ms,
            };
            audit(caller.data(), "stream.next", Some(&input.stream_id), "ok", Some("frame"));
            write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
        Some(StreamMessage::Drop { count }) => {
            let out = NextDropOutput { r#type: "drop", count };
            audit(caller.data(), "stream.next", Some(&input.stream_id), "ok", Some(&format!("drop={}", count)));
            write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
        Some(StreamMessage::CameraOffline { reason }) => {
            // Camera left the bus — also evict the subscriber slot so future
            // calls fail fast with StreamNotFound rather than parking forever.
            subscribers().remove(&(addon_id, input.stream_id.clone()));
            let out = NextCameraOfflineOutput { r#type: "camera_offline", reason: &reason };
            audit(caller.data(), "stream.next", Some(&input.stream_id), "ok", Some("camera_offline"));
            write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
        None => {
            // Could be timeout (channel still open) or channel closed. We
            // cannot cheaply distinguish here — report as `timeout` so the
            // addon can decide to retry; the next call after a real close
            // will return `StreamNotFound` via the SUBSCRIBERS lookup if the
            // entry has been removed elsewhere.
            let out = NextTimeoutOutput { r#type: "timeout" };
            audit(caller.data(), "stream.next", Some(&input.stream_id), "ok", Some("timeout"));
            write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr)
        }
    }
}

// =============================================================================
// Host function: stream_close_v1
// =============================================================================

pub fn stream_close_v1(
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
            audit(caller.data(), "stream.close", None, "error",
                Some(if e == AbiError::PayloadTooLarge { "payload_too_large" } else { "input_read_failed" }));
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_STREAMS_SUBSCRIBE, None) {
        audit(caller.data(), "stream.close", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: CloseInput = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "stream.close", None, "error", Some("invalid_toml"));
            return AbiError::Operation.as_i32();
        }
    };
    if !stream_id_valid(&input.stream_id) {
        audit(caller.data(), "stream.close", None, "denied", Some("stream_id_invalid"));
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let key = (addon_id, input.stream_id.clone());
    // Dropping the slot drops the `Arc<Mutex<StreamSubscriber>>` which closes
    // the receiver; the next `broadcast` from the bus side will see `Closed`
    // on `try_send` and prune the dead entry. We do not have the original
    // `StreamId` here so the explicit `unsubscribe(camera_id, &sid)` path is
    // skipped — the lazy reap path covers it.
    if subscribers().remove(&key).is_some() {
        audit(caller.data(), "stream.close", Some(&input.stream_id), "ok", None);
        let out = CloseOutput { closed: true };
        return write_toml_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr);
    }
    audit(caller.data(), "stream.close", Some(&input.stream_id), "denied", Some("stream_not_found"));
    AbiError::StreamNotFound.as_i32()
}

// =============================================================================
// Test helpers (hidden) — let integration tests poke the registry
// =============================================================================

#[doc(hidden)]
pub mod test_api {
    use super::*;

    #[doc(hidden)]
    pub fn registry_len() -> usize {
        subscribers().len()
    }

    #[doc(hidden)]
    pub fn registry_contains(addon_id: &str, stream_id: &str) -> bool {
        subscribers().contains_key(&(addon_id.to_string(), stream_id.to_string()))
    }

    #[doc(hidden)]
    pub fn registry_clear() {
        subscribers().clear();
    }

    #[doc(hidden)]
    pub fn stream_id_valid_for_test(s: &str) -> bool {
        super::stream_id_valid(s)
    }

    /// Direct subscribe entry that skips the wasmtime caller — used by
    /// integration tests to build a subscriber slot without standing up a
    /// full instance.
    #[doc(hidden)]
    pub fn subscribe_for_test(addon_id: &str, camera_id: &str) -> String {
        let sub = streaming_bus().subscribe(camera_id, StreamFilter::default());
        let stream_id = sub.stream_id.to_string();
        subscribers().insert(
            (addon_id.to_string(), stream_id.clone()),
            SubscriberSlot {
                camera_id: camera_id.to_string(),
                subscriber: Arc::new(tokio::sync::Mutex::new(sub)),
            },
        );
        stream_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parse_camera_prefix() {
        assert_eq!(parse_target("camera:cam_1").unwrap(), "cam_1");
        assert!(parse_target("service:foo").is_err());
        assert!(parse_target("nope").is_err());
    }

    #[test]
    fn stream_id_format_validator() {
        let s = format!("stream_{}", uuid::Uuid::new_v4());
        assert!(stream_id_valid(&s));
        assert!(!stream_id_valid("stream_"));
        assert!(!stream_id_valid("xxx"));
        // Uppercase hex must be rejected.
        let bad = "stream_AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA";
        assert!(!stream_id_valid(bad));
    }
}
