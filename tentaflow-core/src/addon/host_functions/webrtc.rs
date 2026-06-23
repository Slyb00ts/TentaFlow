// =============================================================================
// File: addon/host_functions/webrtc.rs
// Purpose: Generic webrtc.* host ABI — exposes the vendor-agnostic
//          tentaflow-hardware WebRtcChannel to WASM addons as a dumb pipe. The
//          addon drives signaling (offer out / answer in) and the data channel
//          (send / poll-drain); the host owns the native peer. Robot-specific
//          logic (signaling crypto, commands, safety) lives in the addon.
// =============================================================================

#![allow(clippy::too_many_arguments)]

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use dashmap::DashMap;
use tokio::sync::mpsc;

use tentaflow_hardware::webrtc::{
    ChannelState, DcMessage, KeepaliveConfig, WebRtcChannel, WebRtcConfig,
};
use tentaflow_sdk_spec::{
    WebRtcCloseInput, WebRtcConnectInput, WebRtcConnectOutput, WebRtcDrainInput,
    WebRtcDrainOutputRef, WebRtcMessage, WebRtcSendInput, WebRtcSetAnswerInput, WebRtcStateInput,
    WebRtcStateOutput, WebRtcStatusOutput,
};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;

const PERM_WEBRTC_CONNECT: &str = "webrtc.connect";

const MAX_CHANNELS_PER_ADDON: usize = 8;
const MAX_CHANNELS_GLOBAL: usize = 64;
const MAX_GATHER_TIMEOUT_MS: u64 = 15_000;
const MAX_INBOUND_CAPACITY: usize = 8_192;
const MAX_MESSAGES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
// Raw-byte budget for one drain batch. base64 (~4/3) + CBOR overhead must stay
// under the ServiceCall 8 MiB output cap so a staged batch always encodes and
// the retry-safe path never wedges on PayloadTooLarge.
const MAX_DRAIN_BYTES: usize = 3 * 1024 * 1024;

/// Per-channel host state. `ops` serializes mutating calls on one channel_id and
/// stages drained messages so a CBOR `OutputBufferTooSmall` retry never loses
/// them (the addon repeats the call and gets the same staged batch).
struct ChannelEntry {
    chan: WebRtcChannel,
    ops: Mutex<DrainPending>,
    /// camera_id bound to this channel's video via `webrtc_register_camera_v1`,
    /// so close/cleanup also tears the backed camera down (no leak).
    bound_camera: Mutex<Option<String>>,
}

#[derive(Default)]
struct DrainPending {
    pending: Vec<WebRtcMessage>,
    // Response metadata frozen at stage time, so an OutputBufferTooSmall retry
    // re-encodes byte-identical output (queue depth / drop count / closed flag
    // can otherwise drift between the first attempt and the retry).
    dropped_count: u64,
    queue_len: u32,
    closed: bool,
}

// Nested registry: addon_id → channel_id → entry. Hot-path lookups hash two
// `&str` keys with zero allocation (no `(String, String)` key materialization).
type AddonChannels = DashMap<String, Arc<ChannelEntry>>;

static CHANNELS: OnceLock<DashMap<String, AddonChannels>> = OnceLock::new();

fn channels() -> &'static DashMap<String, AddonChannels> {
    CHANNELS.get_or_init(DashMap::new)
}

fn run_async<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

fn channel_id_valid(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("webrtc_") {
        rest.len() == 36
            && rest.chars().enumerate().all(|(i, c)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    c == '-'
                } else {
                    c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
                }
            })
    } else {
        false
    }
}

fn audit(state: &AddonState, action: &str, resource: Option<&str>, result: &str, reason: Option<&str>) {
    audit_log_with_risk(
        state,
        action,
        Some("webrtc"),
        resource,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
}

fn to_message(m: DcMessage) -> WebRtcMessage {
    match m {
        DcMessage::Text(s) => WebRtcMessage {
            is_text: true,
            data_b64: B64.encode(s.as_bytes()),
        },
        DcMessage::Binary(b) => WebRtcMessage {
            is_text: false,
            data_b64: B64.encode(b),
        },
    }
}

fn lookup(addon_id: &str, channel_id: &str) -> Option<Arc<ChannelEntry>> {
    channels()
        .get(addon_id)
        .and_then(|inner| inner.get(channel_id).map(|e| e.clone()))
}

/// Poison-safe per-channel lock — serializes set_answer/send/drain/close on one
/// channel so concurrent host calls can't race the staging buffer or teardown.
fn lock_ops(entry: &ChannelEntry) -> std::sync::MutexGuard<'_, DrainPending> {
    entry.ops.lock().unwrap_or_else(|p| p.into_inner())
}

/// Take the channel's inbound H.264 byte stream (single consumer). Used by the
/// camera backed-registration path. `None` if no such channel / no video / taken.
pub fn take_channel_video(addon_id: &str, channel_id: &str) -> Option<mpsc::Receiver<bytes::Bytes>> {
    lookup(addon_id, channel_id).and_then(|e| e.chan.take_h264_rx())
}

/// Record the camera_id bound to a channel's video so teardown removes it too.
pub fn bind_camera(addon_id: &str, channel_id: &str, camera_id: &str) {
    if let Some(e) = lookup(addon_id, channel_id) {
        *e.bound_camera.lock().unwrap_or_else(|p| p.into_inner()) = Some(camera_id.to_string());
    }
}

/// Remove the backed camera (if any) bound to an entry being torn down.
#[cfg(feature = "camera")]
fn remove_bound_camera(addon_id: &str, entry: &ChannelEntry) {
    let cam = entry
        .bound_camera
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(cid) = cam {
        crate::addon::host_functions::camera::remove_backed_camera(addon_id, &cid);
    }
}

/// Compute the local IPv4 addresses ICE is allowed to gather host candidates
/// from, mirroring the mesh transport's interface selection so the offer only
/// carries reachable candidates on a multi-homed host.
///
/// Base set: host IPv4s kept by the mesh `AddrFilterSnapshot`
/// (`keep_transport_ip`) — honors a pinned `mesh.bind_ipv4`, else hides
/// docker/link-local/loopback. When the peer IP is known and
/// `prefer_same_subnet` is on, narrow to the SAME /24 as the peer; if that
/// narrowing is non-empty use it, else fall back to the full kept set so a
/// usable IP is never dropped. Empty result = no usable IPv4 (create() then
/// keeps default gathering — fail open, never brick the connect).
fn compute_ice_allowlist(
    db: &crate::db::DbPool,
    peer_ipv4: Option<&str>,
) -> Vec<std::net::Ipv4Addr> {
    use crate::mesh::network_interfaces;

    let snap = network_interfaces::build_addr_filter_snapshot(db);
    let kept: Vec<std::net::Ipv4Addr> = network_interfaces::list_interfaces()
        .into_iter()
        .flat_map(|iface| iface.ipv4_addrs)
        .filter_map(|addr| addr.parse::<std::net::Ipv4Addr>().ok())
        .filter(|ip| snap.keep_transport_ip(*ip))
        .collect();

    let peer = peer_ipv4.and_then(|raw| raw.parse::<std::net::Ipv4Addr>().ok());
    if let Some(peer) = peer {
        if network_interfaces::load_prefer_same_subnet(db) {
            let po = peer.octets();
            let narrowed: Vec<std::net::Ipv4Addr> = kept
                .iter()
                .copied()
                .filter(|ip| {
                    let o = ip.octets();
                    o[0] == po[0] && o[1] == po[1] && o[2] == po[2]
                })
                .collect();
            if !narrowed.is_empty() {
                return narrowed;
            }
        }
    }
    kept
}

// =============================================================================
// webrtc_connect_v1
// =============================================================================

pub fn webrtc_connect_v1(
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
    if !check_permission(caller.data(), PERM_WEBRTC_CONNECT, None) {
        audit(caller.data(), "webrtc.connect", None, "denied", Some("missing_permission"));
        return AbiError::Permission.as_i32();
    }
    let input: WebRtcConnectInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => {
                audit(caller.data(), "webrtc.connect", None, "error", Some("invalid_payload"));
                return e.as_i32();
            }
        };

    if input.data_channel_label.is_empty() || input.data_channel_label.len() > 64 {
        audit(caller.data(), "webrtc.connect", None, "error", Some("invalid_label"));
        return AbiError::Operation.as_i32();
    }

    let gather_ms = input.gather_timeout_ms.clamp(500, MAX_GATHER_TIMEOUT_MS);
    let cap = (input.inbound_capacity as usize).clamp(16, MAX_INBOUND_CAPACITY);
    let addon_id = caller.data().addon_id.clone();

    let reg = channels();
    let global_count: usize = reg.iter().map(|e| e.value().len()).sum();
    if global_count >= MAX_CHANNELS_GLOBAL {
        audit(caller.data(), "webrtc.connect", None, "denied", Some("quota_global"));
        return AbiError::QuotaExceeded.as_i32();
    }
    if reg.get(&addon_id).map(|i| i.len()).unwrap_or(0) >= MAX_CHANNELS_PER_ADDON {
        audit(caller.data(), "webrtc.connect", None, "denied", Some("quota_per_addon"));
        return AbiError::QuotaExceeded.as_i32();
    }

    let ice_ipv4_allowlist = compute_ice_allowlist(&caller.data().db, input.peer_ipv4.as_deref());
    tracing::info!(
        addon_id = %addon_id,
        count = ice_ipv4_allowlist.len(),
        ips = ?ice_ipv4_allowlist,
        "webrtc: ICE IPv4 allowlist for connect"
    );

    let keepalive = match (input.keepalive_text, input.keepalive_marker) {
        (Some(text), Some(marker)) if input.keepalive_interval_ms > 0 && !text.is_empty() => {
            Some(KeepaliveConfig {
                text,
                interval: Duration::from_millis(input.keepalive_interval_ms.clamp(200, 60_000)),
                response_marker: marker,
            })
        }
        _ => None,
    };
    let cfg = WebRtcConfig {
        data_channel_label: input.data_channel_label.clone(),
        want_video: input.want_video,
        disable_mdns: input.disable_mdns,
        gather_timeout: Duration::from_millis(gather_ms),
        inbound_capacity: cap,
        keepalive,
        ice_ipv4_allowlist,
    };
    let created = run_async(async { WebRtcChannel::create(cfg).await });
    let (chan, offer_sdp) = match created {
        Ok(v) => v,
        Err(_) => {
            audit(caller.data(), "webrtc.connect", None, "error", Some("create_failed"));
            return AbiError::Operation.as_i32();
        }
    };

    let channel_id = format!("webrtc_{}", uuid::Uuid::new_v4());
    reg.entry(addon_id).or_default().insert(
        channel_id.clone(),
        Arc::new(ChannelEntry {
            chan,
            ops: Mutex::new(DrainPending::default()),
            bound_camera: Mutex::new(None),
        }),
    );
    audit(caller.data(), "webrtc.connect", Some(&channel_id), "ok", None);

    let out = WebRtcConnectOutput { channel_id, offer_sdp };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::ServiceCall)
}

// =============================================================================
// webrtc_set_answer_v1
// =============================================================================

pub fn webrtc_set_answer_v1(
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
    if !check_permission(caller.data(), PERM_WEBRTC_CONNECT, None) {
        return AbiError::Permission.as_i32();
    }
    let input: WebRtcSetAnswerInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => return e.as_i32(),
        };
    if !channel_id_valid(&input.channel_id) {
        return AbiError::NotFound.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let entry = match lookup(&addon_id, &input.channel_id) {
        Some(e) => e,
        None => return AbiError::NotFound.as_i32(),
    };

    let _g = lock_ops(&entry);
    let res = run_async(entry.chan.set_answer(input.answer_sdp));
    if res.is_err() {
        audit(caller.data(), "webrtc.set_answer", Some(&input.channel_id), "error", Some("set_answer_failed"));
        return AbiError::Operation.as_i32();
    }
    audit(caller.data(), "webrtc.set_answer", Some(&input.channel_id), "ok", None);
    let out = WebRtcStatusOutput { ok: true };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::ServiceCall)
}

// =============================================================================
// webrtc_state_v1
// =============================================================================

pub fn webrtc_state_v1(
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
    if !check_permission(caller.data(), PERM_WEBRTC_CONNECT, None) {
        return AbiError::Permission.as_i32();
    }
    let input: WebRtcStateInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => return e.as_i32(),
        };
    if !channel_id_valid(&input.channel_id) {
        return AbiError::NotFound.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let entry = match lookup(&addon_id, &input.channel_id) {
        Some(e) => e,
        None => return AbiError::NotFound.as_i32(),
    };

    let queue_len = run_async(async { entry.chan.queue_len().await }) as u32;
    let out = WebRtcStateOutput {
        peer_state: entry.chan.state().as_str().to_string(),
        dc_open: entry.chan.dc_open(),
        dropped_count: entry.chan.dropped_count(),
        queue_len,
        rtt_ms: entry.chan.rtt_ms(),
    };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::ServiceCall)
}

// =============================================================================
// webrtc_send_v1
// =============================================================================

pub fn webrtc_send_v1(
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
    if !check_permission(caller.data(), PERM_WEBRTC_CONNECT, None) {
        return AbiError::Permission.as_i32();
    }
    let input: WebRtcSendInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => return e.as_i32(),
        };
    if !channel_id_valid(&input.channel_id) {
        return AbiError::NotFound.as_i32();
    }
    let bytes = match B64.decode(input.data_b64.as_bytes()) {
        Ok(b) => b,
        Err(_) => return AbiError::Operation.as_i32(),
    };
    if bytes.len() > MAX_MESSAGE_BYTES {
        return AbiError::PayloadTooLarge.as_i32();
    }
    let addon_id = caller.data().addon_id.clone();
    let entry = match lookup(&addon_id, &input.channel_id) {
        Some(e) => e,
        None => return AbiError::NotFound.as_i32(),
    };

    let _g = lock_ops(&entry);
    let res = run_async(async {
        if input.is_text {
            match String::from_utf8(bytes) {
                Ok(s) => entry.chan.dc_send_text(s).await,
                Err(_) => Err(anyhow::anyhow!("is_text payload is not valid UTF-8")),
            }
        } else {
            entry.chan.dc_send_binary(bytes).await
        }
    });
    if res.is_err() {
        // dc not open / send failed — addon polls state.dc_open and retries.
        return AbiError::Operation.as_i32();
    }
    let out = WebRtcStatusOutput { ok: true };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::ServiceCall)
}

// =============================================================================
// webrtc_drain_v1 — retry-safe via per-channel staging buffer
// =============================================================================

pub fn webrtc_drain_v1(
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
    if !check_permission(caller.data(), PERM_WEBRTC_CONNECT, None) {
        return AbiError::Permission.as_i32();
    }
    let input: WebRtcDrainInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => return e.as_i32(),
        };
    if !channel_id_valid(&input.channel_id) {
        return AbiError::NotFound.as_i32();
    }
    let max = (input.max_messages as usize).clamp(1, MAX_MESSAGES);
    let addon_id = caller.data().addon_id.clone();
    let entry = match lookup(&addon_id, &input.channel_id) {
        Some(e) => e,
        None => return AbiError::NotFound.as_i32(),
    };

    // Hold the per-channel lock across stage → write → clear so no concurrent
    // drain can observe or clear the same staged batch.
    let mut ops = lock_ops(&entry);
    // First attempt drains + freezes the FULL response (messages + metadata) in
    // one inbound-lock acquisition; a retry (staged batch kept) re-encodes the
    // frozen snapshot so the bytes are identical across OutputBufferTooSmall.
    if ops.pending.is_empty() {
        let (msgs, remaining) =
            run_async(entry.chan.dc_drain_budget_with_remaining(max, MAX_DRAIN_BYTES));
        ops.pending = msgs.into_iter().map(to_message).collect();
        ops.dropped_count = entry.chan.dropped_count();
        ops.queue_len = remaining as u32;
        ops.closed = matches!(entry.chan.state(), ChannelState::Closed | ChannelState::Failed);
    }
    // Encode from a BORROW of the frozen staged batch (no deep clone).
    let out = WebRtcDrainOutputRef {
        messages: &ops.pending,
        dropped_count: ops.dropped_count,
        queue_len: ops.queue_len,
        closed: ops.closed,
    };
    let rc = write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::ServiceCall);
    // Consume the staged batch ONLY once it actually reached the guest (rc == 0).
    // On OutputBufferTooSmall the SDK retries with a bigger buffer and gets the
    // same batch; on any other error the batch is kept (never silently lost).
    if rc == 0 {
        ops.pending.clear();
    }
    rc
}

// =============================================================================
// webrtc_close_v1
// =============================================================================

pub fn webrtc_close_v1(
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
    if !check_permission(caller.data(), PERM_WEBRTC_CONNECT, None) {
        return AbiError::Permission.as_i32();
    }
    let input: WebRtcCloseInput =
        match read_input_cbor(&memory, &caller, input_ptr, input_len, PayloadKind::ServiceCall) {
            Ok(v) => v,
            Err(e) => return e.as_i32(),
        };
    let addon_id = caller.data().addon_id.clone();
    let entry = match lookup(&addon_id, &input.channel_id) {
        Some(e) => e,
        None => return AbiError::NotFound.as_i32(),
    };
    {
        // Serialize against in-flight ops, close the peer, then deregister.
        let _g = lock_ops(&entry);
        run_async(async { let _ = entry.chan.close().await; });
        if let Some(inner) = channels().get(&addon_id) {
            inner.remove(&input.channel_id);
        }
    }
    #[cfg(feature = "camera")]
    remove_bound_camera(&addon_id, &entry);
    audit(caller.data(), "webrtc.close", Some(&input.channel_id), "ok", None);
    let out = WebRtcStatusOutput { ok: true };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::ServiceCall)
}

/// Deterministic teardown of every channel owned by an addon — called from
/// `AddonManager::unregister_addon_runtime` on disable / uninstall / unload.
pub fn cleanup_addon_channels(addon_id: &str) {
    let reg = channels();
    let Some((_, inner)) = reg.remove(addon_id) else {
        return;
    };
    for (_, entry) in inner.into_iter() {
        #[cfg(feature = "camera")]
        remove_bound_camera(addon_id, &entry);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = entry.chan.close().await;
            });
        }
    }
}
