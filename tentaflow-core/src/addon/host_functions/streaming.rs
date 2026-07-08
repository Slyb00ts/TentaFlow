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

use tentaflow_sdk_spec::{
    StreamCloseInput, StreamCloseOutput, StreamNextInput, StreamNextOutput, StreamSubscribeInput,
    StreamSubscribeOutput,
};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::db::repository::get_camera_for_addon;
use crate::services::streaming::{
    NextOutcome, StreamFilter, StreamId, StreamMessage, StreamSubscriber,
};
use crate::services::streaming_bus;

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
    /// Camera id + bus stream id are duplicated here so the RAII `Drop` guard
    /// can `unsubscribe` from the bus without re-querying the DB or locking
    /// the subscriber.
    camera_id: String,
    stream_id: StreamId,
    /// `Arc<tokio::sync::Mutex<…>>` — `stream_next` is sync from the host side
    /// (it goes through `run_async`) but mutates the subscriber's receiver, so
    /// concurrent calls on the same stream_id must serialize.
    subscriber: Arc<tokio::sync::Mutex<StreamSubscriber>>,
}

impl Drop for SubscriberSlot {
    /// RAII cleanup — fires on EVERY registry removal (explicit `stream_close`,
    /// `CameraOffline`/`Closed` eviction in `stream_next`, quota-clear tests).
    /// Removing the bus entry here — rather than relying on the lazy
    /// broadcast-only reap — guarantees `list_subscribers` shrinks promptly
    /// when a viewer disconnects, so the RTSP session tick detaches its
    /// expensive on-demand RGB convert branch instead of running it forever.
    fn drop(&mut self) {
        streaming_bus().unsubscribe(&self.camera_id, &self.stream_id);
    }
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
// Helpers
// =============================================================================

const MAX_TIMEOUT_MS: u64 = 5_000;

/// Per-addon ceiling on simultaneous stream subscriptions. F1a value picked
/// so a 32-camera quota plus a couple of analyses per camera fits, while
/// still blocking pathological loops that mint thousands of subs.
const MAX_STREAMS_PER_ADDON: usize = 16;

/// Global ceiling across every addon. Defence against a single compromised
/// addon plus accumulated leakage from other addons.
const MAX_STREAMS_GLOBAL: usize = 256;

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
        Some("stream"),
        resource_id,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
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
    if !check_permission(caller.data(), PERM_STREAMS_SUBSCRIBE, None) {
        audit(
            caller.data(),
            "stream.subscribe",
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: StreamSubscribeInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "stream.subscribe",
                None,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    let camera_id = match parse_target(&input.target) {
        Ok(c) => c.to_string(),
        Err(e) => {
            audit(
                caller.data(),
                "stream.subscribe",
                None,
                "denied",
                Some("invalid_target"),
            );
            return e.as_i32();
        }
    };

    let addon_id = caller.data().addon_id.clone();
    let db = caller.data().db.clone();

    // Ownership enforcement — F1a forbids cross-addon subscribes. Result mapped
    // to `NotFound` so an addon cannot enumerate cameras owned by peers.
    match get_camera_for_addon(&db, &addon_id, &camera_id, caller.data().org_id.as_deref()) {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit(
                caller.data(),
                "stream.subscribe",
                Some(&camera_id),
                "denied",
                Some("not_found_or_not_owned"),
            );
            return AbiError::NotFound.as_i32();
        }
        Err(_) => {
            audit(
                caller.data(),
                "stream.subscribe",
                Some(&camera_id),
                "error",
                Some("db_error"),
            );
            return AbiError::Operation.as_i32();
        }
    }

    // Quota enforcement BEFORE we allocate a new subscriber on the bus —
    // otherwise the channel + drop counter outlive the reject path.
    let registry = subscribers();
    if registry.len() >= MAX_STREAMS_GLOBAL {
        audit(
            caller.data(),
            "stream.subscribe",
            Some(&camera_id),
            "denied",
            Some("streams_quota_global"),
        );
        return AbiError::QuotaExceeded.as_i32();
    }
    let per_addon = registry.iter().filter(|e| e.key().0 == addon_id).count();
    if per_addon >= MAX_STREAMS_PER_ADDON {
        audit(
            caller.data(),
            "stream.subscribe",
            Some(&camera_id),
            "denied",
            Some("streams_quota_per_addon"),
        );
        return AbiError::QuotaExceeded.as_i32();
    }

    let filter = match input.filter {
        Some(f) => StreamFilter {
            max_fps: f.max_fps,
            skip_frames: f.skip_frames_or_default(),
        },
        None => StreamFilter::default(),
    };
    let sub = streaming_bus().subscribe(&camera_id, filter);
    let stream_id_typed = sub.stream_id.clone();
    let stream_id = stream_id_typed.to_string();
    registry.insert(
        (addon_id.clone(), stream_id.clone()),
        SubscriberSlot {
            camera_id: camera_id.clone(),
            stream_id: stream_id_typed,
            subscriber: Arc::new(tokio::sync::Mutex::new(sub)),
        },
    );

    audit(
        caller.data(),
        "stream.subscribe",
        Some(&camera_id),
        "ok",
        Some(&format!("stream_id={}", stream_id)),
    );
    let out = StreamSubscribeOutput { stream_id };
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
    if !check_permission(caller.data(), PERM_STREAMS_SUBSCRIBE, None) {
        audit(
            caller.data(),
            "stream.next",
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: StreamNextInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "stream.next",
                None,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !stream_id_valid(&input.stream_id) {
        audit(
            caller.data(),
            "stream.next",
            None,
            "denied",
            Some("stream_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    let timeout = Duration::from_millis(input.timeout_ms.min(MAX_TIMEOUT_MS));

    let addon_id = caller.data().addon_id.clone();
    let sub_arc = match subscribers().get(&(addon_id.clone(), input.stream_id.clone())) {
        Some(slot) => slot.subscriber.clone(),
        None => {
            audit(
                caller.data(),
                "stream.next",
                Some(&input.stream_id),
                "denied",
                Some("stream_not_found"),
            );
            return AbiError::StreamNotFound.as_i32();
        }
    };

    let msg = run_async(async move {
        let mut guard = sub_arc.lock().await;
        guard.next(timeout).await
    });

    match msg {
        NextOutcome::Message(StreamMessage::Frame {
            frame_ref,
            metadata,
        }) => {
            let pf = match metadata.pixel_format {
                crate::services::frame_storage::FramePixelFormat::Rgb24 => "rgb24",
                crate::services::frame_storage::FramePixelFormat::Nv12 => "nv12",
            };
            let out = StreamNextOutput::frame(
                frame_ref.as_str().to_string(),
                metadata.camera_id.clone(),
                metadata.width,
                metadata.height,
                pf.to_string(),
                metadata.timestamp_unix_ms,
            );
            audit(
                caller.data(),
                "stream.next",
                Some(&input.stream_id),
                "ok",
                Some("frame"),
            );
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
        NextOutcome::Message(StreamMessage::Drop { count }) => {
            let out = StreamNextOutput::drop(count);
            audit(
                caller.data(),
                "stream.next",
                Some(&input.stream_id),
                "ok",
                Some(&format!("drop={}", count)),
            );
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
        NextOutcome::Message(StreamMessage::CameraOffline { reason }) => {
            // Camera left the bus — evict the subscriber slot so future
            // calls fail fast with StreamNotFound.
            subscribers().remove(&(addon_id, input.stream_id.clone()));
            let out = StreamNextOutput::camera_offline(reason.clone());
            audit(
                caller.data(),
                "stream.next",
                Some(&input.stream_id),
                "ok",
                Some("camera_offline"),
            );
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
        NextOutcome::Closed => {
            // Channel closed without an explicit CameraOffline (subscriber
            // dropped, supervisor exit). Evict registry entry and surface a
            // distinct `stream_closed` type so the addon stops polling.
            subscribers().remove(&(addon_id, input.stream_id.clone()));
            let out = StreamNextOutput::stream_closed();
            audit(
                caller.data(),
                "stream.next",
                Some(&input.stream_id),
                "ok",
                Some("stream_closed"),
            );
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
        NextOutcome::Timeout => {
            let out = StreamNextOutput::timeout();
            audit(
                caller.data(),
                "stream.next",
                Some(&input.stream_id),
                "ok",
                Some("timeout"),
            );
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
    if !check_permission(caller.data(), PERM_STREAMS_SUBSCRIBE, None) {
        audit(
            caller.data(),
            "stream.close",
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    let input: StreamCloseInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "stream.close",
                None,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !stream_id_valid(&input.stream_id) {
        audit(
            caller.data(),
            "stream.close",
            None,
            "denied",
            Some("stream_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let key = (addon_id, input.stream_id.clone());
    // Removing the slot drops it; its `Drop` guard eagerly calls
    // `streaming_bus().unsubscribe(camera_id, stream_id)`, so the bus entry
    // disappears immediately (no dependence on the next broadcast) and the
    // `Arc<Mutex<StreamSubscriber>>` receiver closes with it.
    if subscribers().remove(&key).is_some() {
        audit(
            caller.data(),
            "stream.close",
            Some(&input.stream_id),
            "ok",
            None,
        );
        let out = StreamCloseOutput { closed: true };
        return write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        );
    }
    audit(
        caller.data(),
        "stream.close",
        Some(&input.stream_id),
        "denied",
        Some("stream_not_found"),
    );
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
    pub fn max_streams_per_addon() -> usize {
        super::MAX_STREAMS_PER_ADDON
    }

    #[doc(hidden)]
    pub fn max_streams_global() -> usize {
        super::MAX_STREAMS_GLOBAL
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
        let stream_id_typed = sub.stream_id.clone();
        let stream_id = stream_id_typed.to_string();
        subscribers().insert(
            (addon_id.to_string(), stream_id.clone()),
            SubscriberSlot {
                camera_id: camera_id.to_string(),
                stream_id: stream_id_typed,
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
    fn per_addon_quota_threshold_holds() {
        // Pre-fill the global registry with slots owned by a unique addon up
        // to the per-addon cap, then verify that count() reflects the limit
        // and would block a subsequent subscribe attempt.
        let addon = format!("addon-quota-{}", uuid::Uuid::new_v4());
        let cam = format!("cam-quota-{}", uuid::Uuid::new_v4());
        for _ in 0..MAX_STREAMS_PER_ADDON {
            test_api::subscribe_for_test(&addon, &cam);
        }
        let per_addon = subscribers().iter().filter(|e| e.key().0 == addon).count();
        assert_eq!(per_addon, MAX_STREAMS_PER_ADDON);
        // Any further subscribe for the same addon would exceed the cap.
        assert!(per_addon >= MAX_STREAMS_PER_ADDON);
    }

    #[test]
    fn slot_drop_unsubscribes_from_bus() {
        // subscribe_for_test registers a bus subscriber AND a registry slot.
        // Removing the slot must fire the RAII `Drop` guard and clear the bus
        // entry immediately — with NO broadcast — so `list_subscribers` (which
        // gates the on-demand RGB convert branch) shrinks the moment the
        // viewer's slot goes away.
        let addon = format!("addon-drop-{}", uuid::Uuid::new_v4());
        let cam = format!("cam-drop-{}", uuid::Uuid::new_v4());
        let sid = test_api::subscribe_for_test(&addon, &cam);
        assert_eq!(streaming_bus().list_subscribers(&cam).len(), 1);
        // Explicit close path: removing the slot drops the guard.
        assert!(subscribers().remove(&(addon, sid)).is_some());
        assert!(
            streaming_bus().list_subscribers(&cam).is_empty(),
            "bus entry must be gone after slot drop without a broadcast"
        );
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
