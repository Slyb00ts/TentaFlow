// =============================================================================
// File: addons/go2/src/lib.rs
// Unitree Go2 robot-control addon. Owns ALL Go2 logic: LAN signaling (con_notify
// / con_ing via http_raw + the shared `protocol` crypto), the generic webrtc.*
// channel, the data-channel validation handshake, sport commands, continuous
// battery+RTT telemetry, a durable e-stop gate, and camera registration. Core
// stays a dumb pipe. Connection lives across service ticks (SQL state machine).
// =============================================================================

extern crate alloc;

mod db;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use serde_json::{json, Value as JsonValue};

use tentaflow_hardware::unitree::go2::protocol;
use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::ui::{
    actions::Button as ButtonComp,
    bind::BindRef,
    data::{Heading as HeadingComp, Text as TextComp},
    inline::*,
    layout::SectionCard,
    tokens::*,
};
use tentaflow_sdk_spec::{
    CameraGrantInput, CameraGrantOut, Component, FailurePolicy, Handler, HandlerMap, PanelShell,
    RobotActionWire, RobotControlResponseWire, RobotDispatchInput, StateEntry, UiPayload, Value,
    WebRtcCloseInput, WebRtcConnectInput, WebRtcConnectOutput, WebRtcDrainInput, WebRtcDrainOutput,
    WebRtcRegisterCameraInput, WebRtcRegisterCameraOutput, WebRtcSendInput, WebRtcSetAnswerInput,
    WebRtcStateInput, WebRtcStateOutput, WebRtcStatusOutput,
};

// The vision addon that consumes the robot camera. go2 grants it read access on
// the backed camera so it appears in TentaVision without relaxing tenant
// isolation for any other addon (least-privilege: one specific grantee, 'read').
const VISION_ADDON_ID: &str = "tentavision";

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const ADDON_ID: &str = "go2";
const PANEL_ID: &str = "overview";
// The robot IP is provided per-install via the `ip` connection_param and read
// from addon_config at runtime — there is intentionally NO hardcoded default.
const IP_CONFIG_KEY: &str = "ip";
const LATENCY_ALERT_MS: i64 = 500;
const BATTERY_ALERT_PCT: i64 = 20;
// Watchdogs (seconds). Validation must complete promptly; an online connection
// that stops advancing telemetry (persistent drain/state errors) is declared dead.
const CONNECT_TIMEOUT_SECS: i64 = 20;
const VALIDATION_TIMEOUT_SECS: i64 = 20;
const ONLINE_STALE_SECS: i64 = 12;

static PANEL_EPOCH: AtomicU64 = AtomicU64::new(1);
static REQ_ID: AtomicU64 = AtomicU64::new(1);

// Sport command api_ids (normal mode) — only safe motions exposed (no Air-locked
// flips / handstand).
const SPORT_DAMP: u32 = 1001;
const SPORT_STOP_MOVE: u32 = 1003;
const SPORT_STAND_UP: u32 = 1004;
const SPORT_STAND_DOWN: u32 = 1005;
const SPORT_RECOVERY_STAND: u32 = 1006;
const SPORT_MOVE: u32 = 1008;
const SPORT_SIT: u32 = 1009;
const SPORT_STRETCH: u32 = 1017;
const SPORT_HELLO: u32 = 1016;

// =============================================================================
// Host imports
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
    fn event_publish(et_ptr: i32, et_len: i32, p_ptr: i32, p_len: i32) -> i32;
    fn log_info(msg_ptr: i32, msg_len: i32) -> i32;
    fn log_warn(msg_ptr: i32, msg_len: i32) -> i32;
    fn http_raw_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn webrtc_connect_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn webrtc_set_answer_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn webrtc_state_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn webrtc_send_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn webrtc_drain_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn webrtc_close_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn webrtc_register_camera_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn camera_grant_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn robot_dispatch_v1(in_ptr: i32, in_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
    fn config_get_v1(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
}

/// Reads an install-time connection param from `addon_config` (scoped to this
/// instance). Returns None when the key is absent/empty — there is NO default.
fn config_get(key: &str) -> Option<String> {
    let mut cap = 256usize;
    loop {
        let mut buf = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            config_get_v1(
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
        if ret != 0 {
            return None;
        }
        if out_len <= 0 || out_len as usize > cap {
            return None;
        }
        let s = String::from_utf8_lossy(&buf[..out_len as usize]).into_owned();
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(trimmed.to_string());
    }
}

// =============================================================================
// ABI error + memory helpers
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiError {
    Permission,
    NotFound,
    Operation,
    OutputBufferTooSmall,
    Other(i32),
}

impl AbiError {
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Permission,
            2 => Self::NotFound,
            5 => Self::Operation,
            6 => Self::OutputBufferTooSmall,
            other => Self::Other(other),
        }
    }
}

impl core::fmt::Display for AbiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self)
    }
}

mod log {
    use super::*;
    pub fn info(m: &str) {
        unsafe { log_info(m.as_ptr() as i32, m.len() as i32); }
    }
    pub fn warn(m: &str) {
        unsafe { log_warn(m.as_ptr() as i32, m.len() as i32); }
    }
}

fn read_string(ptr: i32, len: i32) -> String {
    if ptr <= 0 || len <= 0 {
        return String::new();
    }
    let slice = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8_lossy(slice).into_owned()
}

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, body: &str) -> i32 {
    let bytes = body.as_bytes();
    unsafe {
        *(out_len_ptr as *mut i32) = bytes.len() as i32;
    }
    if bytes.len() > out_cap as usize {
        return 6; // OutputBufferTooSmall — host retries with a larger buffer.
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr as *mut u8, bytes.len());
    }
    0
}

fn publish_event(event_type: &str, payload: JsonValue) {
    let p = payload.to_string();
    unsafe {
        event_publish(
            event_type.as_ptr() as i32, event_type.len() as i32,
            p.as_ptr() as i32, p.len() as i32,
        );
    }
}

// =============================================================================
// CBOR host-call helper (for webrtc_* — typed sdk-spec structs)
// =============================================================================

// Single-threaded WASM: reusable CBOR encode + host-output buffers, never freed
// between calls. These host fns never re-enter the addon, so the per-call borrows
// can't nest.
std::thread_local! {
    static CBOR_IN: core::cell::RefCell<Vec<u8>> = core::cell::RefCell::new(Vec::with_capacity(4096));
    static CBOR_OUT: core::cell::RefCell<Vec<u8>> = core::cell::RefCell::new(Vec::with_capacity(16384));
    // Reusable base64-decode scratch for the per-tick drain loop (zero alloc/msg).
    static DECODE_BUF: core::cell::RefCell<Vec<u8>> = core::cell::RefCell::new(Vec::with_capacity(4096));
}

fn call_cbor_in_out<I, O>(
    input: &I,
    host_fn: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
) -> Result<O, AbiError>
where
    I: minicbor::Encode<()>,
    O: for<'b> minicbor::Decode<'b, ()>,
{
    CBOR_IN.with(|in_cell| {
        let mut input_bytes = in_cell.borrow_mut();
        input_bytes.clear();
        minicbor::encode(input, &mut *input_bytes).map_err(|_| AbiError::Operation)?;
        CBOR_OUT.with(|out_cell| {
            let mut out = out_cell.borrow_mut();
            let mut cap = out.capacity().max(16384);
            loop {
                out.clear();
                out.reserve(cap);
                // SAFETY: capacity >= cap; host fills [..out_len], only that prefix is decoded.
                unsafe { out.set_len(cap); }
                let mut out_len: i32 = 0;
                let ret = unsafe {
                    host_fn(
                        input_bytes.as_ptr() as i32, input_bytes.len() as i32,
                        out.as_mut_ptr() as i32, cap as i32,
                        &mut out_len as *mut i32 as i32,
                    )
                };
                if ret == 6 {
                    let want = if out_len > 0 { out_len as usize } else { 0 };
                    cap = want.max(cap.saturating_mul(2));
                    continue;
                }
                if ret != 0 {
                    return Err(AbiError::from_code(ret));
                }
                if out_len < 0 || out_len as usize > cap {
                    return Err(AbiError::Operation);
                }
                let n = out_len as usize;
                return minicbor::decode(&out[..n]).map_err(|_| AbiError::Operation);
            }
        })
    })
}

/// Raw HTTP/1.0 POST via the host (reqwest fails on the robot's embedded server).
fn http_raw(url: &str, content_type: &str, body: &str) -> Result<(u16, String), String> {
    let req = json!({ "url": url, "content_type": content_type, "body": body }).to_string();
    let mut cap = 65536usize;
    loop {
        let mut out = vec![0u8; cap];
        let mut out_len: i32 = 0;
        let ret = unsafe {
            http_raw_v1(
                req.as_ptr() as i32, req.len() as i32,
                out.as_mut_ptr() as i32, out.len() as i32,
                &mut out_len as *mut i32 as i32,
            )
        };
        if ret == 6 {
            let want = if out_len > 0 { out_len as usize } else { 0 };
            cap = want.max(cap.saturating_mul(2));
            continue;
        }
        if ret != 0 {
            return Err(alloc::format!("http_raw abi error {ret}"));
        }
        if out_len < 0 || out_len as usize > cap {
            return Err("http_raw bad out_len".to_string());
        }
        out.truncate(out_len as usize);
        let v: JsonValue = serde_json::from_slice(&out).map_err(|_| "bad http_raw json".to_string())?;
        let status = v.get("status").and_then(JsonValue::as_u64).unwrap_or(0) as u16;
        let body = v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
        return Ok((status, body));
    }
}

// =============================================================================
// webrtc.* wrappers
// =============================================================================

fn wc_send_text(channel_id: &str, text: &str) -> Result<(), AbiError> {
    let input = WebRtcSendInput {
        channel_id: channel_id.into(),
        is_text: true,
        data_b64: B64.encode(text.as_bytes()),
    };
    let _: WebRtcStatusOutput = call_cbor_in_out(&input, webrtc_send_v1)?;
    Ok(())
}

fn wc_state(channel_id: &str) -> Result<WebRtcStateOutput, AbiError> {
    call_cbor_in_out(&WebRtcStateInput { channel_id: channel_id.into() }, webrtc_state_v1)
}

fn wc_drain(channel_id: &str, max: u32) -> Result<WebRtcDrainOutput, AbiError> {
    call_cbor_in_out(
        &WebRtcDrainInput { channel_id: channel_id.into(), max_messages: max },
        webrtc_drain_v1,
    )
}

fn wc_close(channel_id: &str) {
    let _: Result<WebRtcStatusOutput, _> =
        call_cbor_in_out(&WebRtcCloseInput { channel_id: channel_id.into() }, webrtc_close_v1);
}

/// Grant the vision addon read access to the robot camera so it shows up in
/// TentaVision. go2 owns the camera, so the host's owner-gated grant accepts it.
/// Best-effort: a failure (e.g. vision addon not installed) is logged, not fatal.
fn grant_vision_camera(camera_id: &str) {
    if camera_id.is_empty() {
        return;
    }
    let input = CameraGrantInput {
        camera_id: camera_id.into(),
        grantee_addon_id: VISION_ADDON_ID.into(),
        level: "read".into(),
    };
    match call_cbor_in_out::<_, CameraGrantOut>(&input, camera_grant_v1) {
        Ok(o) if o.ok => log::info("go2: camera shared with tentavision"),
        Ok(_) => log::warn("go2: camera grant returned not-ok"),
        Err(e) => log::warn(&alloc::format!("go2: camera grant failed: {e}")),
    }
}

// =============================================================================
// Sport commands + safety gate
// =============================================================================

fn build_sport(api_id: u32, parameter: &str) -> String {
    let id = REQ_ID.fetch_add(1, Ordering::Relaxed);
    json!({
        "type": "req",
        "topic": "rt/api/sport/request",
        "data": { "header": { "identity": { "id": id, "api_id": api_id } }, "parameter": parameter },
    })
    .to_string()
}

fn subscribe_msg(topic: &str) -> String {
    json!({ "type": "subscribe", "topic": topic }).to_string()
}

/// Map a local sport command `(api_id, parameter)` to the vendor-agnostic
/// `RobotActionWire` used for cross-node dispatch. `parameter` is the go2 move
/// JSON (`{"x","y","z"}`) for SPORT_MOVE; ignored otherwise. Returns `None` for
/// an api_id with no remote-control equivalent (so the caller keeps it local).
fn sport_to_action(api_id: u32, parameter: &str) -> Option<RobotActionWire> {
    Some(match api_id {
        SPORT_MOVE => {
            let p: JsonValue = serde_json::from_str(parameter).unwrap_or(JsonValue::Null);
            let axis = |k: &str| p.get(k).and_then(JsonValue::as_f64).unwrap_or(0.0);
            RobotActionWire::move_to(axis("x"), axis("y"), axis("z"))
        }
        SPORT_STOP_MOVE | SPORT_DAMP => RobotActionWire::simple("stop"),
        SPORT_STAND_UP => RobotActionWire::simple("stand_up"),
        SPORT_STAND_DOWN => RobotActionWire::simple("stand_down"),
        SPORT_RECOVERY_STAND => RobotActionWire::simple("recovery_stand"),
        SPORT_SIT => RobotActionWire::simple("sit"),
        SPORT_HELLO => RobotActionWire::simple("hello"),
        SPORT_STRETCH => RobotActionWire::simple("stretch"),
        _ => return None,
    })
}

/// Route a `RobotActionWire` to the owning node via the host. The host resolves
/// the owner (this node if local, else a single remote mesh node) and runs the
/// shared dispatch router. A robot-level refusal is a successful call carrying
/// `rejected`; only an ABI failure surfaces as `Err`.
fn robot_dispatch(action: RobotActionWire) -> Result<RobotControlResponseWire, AbiError> {
    let input = RobotDispatchInput {
        robot_id: ADDON_ID.into(),
        action,
    };
    call_cbor_in_out(&input, robot_dispatch_v1)
}

/// Render a host `RobotControlResponseWire` as the addon's JSON result shape.
fn dispatch_result_json(resp: RobotControlResponseWire) -> JsonValue {
    if resp.ok {
        match resp.result_json {
            Some(s) => serde_json::from_str(&s).unwrap_or(json!({ "status": "sent" })),
            None => json!({ "status": "sent" }),
        }
    } else if let Some(reason) = resp.rejected {
        json!({ "error": alloc::format!("robot dispatch rejected: {reason}") })
    } else {
        json!({ "error": resp.error.unwrap_or_else(|| "robot dispatch failed".into()) })
    }
}

/// Send a sport command with the e-stop + online gates. StopMove/Damp bypass the
/// e-stop gate (they ARE the stop). When the robot is NOT online on THIS node it
/// is owned by another mesh node: route the equivalent `RobotAction` through the
/// host dispatcher instead of failing — the owner re-checks trust, permission and
/// safety before actuating.
fn send_sport_gated(api_id: u32, parameter: &str) -> JsonValue {
    let robot = match db::get_robot() {
        Ok(r) => r,
        Err(e) => return json!({ "error": alloc::format!("db: {e}") }),
    };
    let is_stop = api_id == SPORT_STOP_MOVE || api_id == SPORT_DAMP;
    if robot.status != "online" || robot.channel_id.is_empty() {
        // Not locally online → robot lives on another node. The local e-stop
        // latch is local-only; cross-node motion is governed by the owner, so we
        // gate non-stop actions on the local latch here too (defense in depth).
        if robot.estop_active && !is_stop {
            return json!({ "error": "e-stop active — reset it first" });
        }
        return match sport_to_action(api_id, parameter) {
            Some(action) => match robot_dispatch(action) {
                Ok(resp) => dispatch_result_json(resp),
                Err(e) => json!({ "error": alloc::format!("dispatch: {e}") }),
            },
            None => json!({ "error": "robot not online" }),
        };
    }
    if robot.estop_active && !is_stop {
        return json!({ "error": "e-stop active — reset it first" });
    }
    match wc_send_text(&robot.channel_id, &build_sport(api_id, parameter)) {
        Ok(()) => json!({ "status": "sent" }),
        Err(e) => json!({ "error": alloc::format!("send: {e}") }),
    }
}

/// Find `needle` in `hay` starting at `from` (plain substring scan).
fn find_sub(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() || needle.len() > hay.len() - from {
        return None;
    }
    let last = hay.len() - needle.len();
    let first = needle[0];
    let mut i = from;
    while i <= last {
        if hay[i] == first && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Parse the integer battery SOC out of raw lowstate JSON bytes WITHOUT building
/// a serde_json::Value tree. Scans for the `"soc"` key, then parses the JSON
/// integer after the colon. Lowstate is small control text; a scalar scan is the
/// right tool here (SIMD/memchr is unwarranted for these tiny buffers).
fn parse_soc(bytes: &[u8]) -> Option<i64> {
    const KEY: &[u8] = b"\"soc\"";
    let mut from = 0;
    while let Some(p) = find_sub(bytes, KEY, from) {
        let mut j = p + KEY.len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b':' {
            j += 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let neg = j < bytes.len() && bytes[j] == b'-';
            if neg {
                j += 1;
            }
            let start = j;
            let mut val: i64 = 0;
            let mut overflow = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                match val
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(i64::from(bytes[j] - b'0')))
                {
                    Some(v) => val = v,
                    None => {
                        overflow = true;
                        break;
                    }
                }
                j += 1;
            }
            if !overflow && j > start {
                return Some(if neg { -val } else { val });
            }
        }
        from = p + KEY.len();
    }
    None
}

// =============================================================================
// Connection state machine
// =============================================================================

fn do_connect() -> JsonValue {
    log::info("go2: do_connect entered");
    let robot = db::get_robot().unwrap_or_default();
    // The configured IP (install-time `ip` connection_param) is the single source
    // of truth. No default — an unconfigured instance refuses to connect.
    let ip = match config_get(IP_CONFIG_KEY) {
        Some(ip) => ip,
        None => {
            log::warn("go2: no IP configured — cannot connect");
            let _ = db::set_offline("error", "no IP configured");
            return json!({ "error": "no IP configured" });
        }
    };
    log::info(&alloc::format!("go2: do_connect ip={ip} status={}", robot.status));
    if db::ensure_robot(&ip).is_err() {
        log::warn("go2: ensure_robot failed");
        return json!({ "error": "db init failed" });
    }
    // CAS: only one connect in flight.
    match db::try_begin_connect() {
        Ok(true) => log::info("go2: try_begin_connect won (status->connecting)"),
        Ok(false) => {
            log::warn("go2: try_begin_connect lost (already connecting/online)");
            return json!({ "error": "already connecting or online" });
        }
        Err(e) => {
            log::warn(&alloc::format!("go2: try_begin_connect db err: {e}"));
            return json!({ "error": alloc::format!("db: {e}") });
        }
    }

    let connect_in = WebRtcConnectInput {
        data_channel_label: "data".into(),
        want_video: true,
        disable_mdns: true,
        gather_timeout_ms: 8000,
        inbound_capacity: 2048,
        keepalive_text: Some(protocol::HEARTBEAT_TEXT.into()),
        keepalive_interval_ms: 1000,
        keepalive_marker: Some(protocol::HEARTBEAT_MARKER.into()),
    };
    let out: WebRtcConnectOutput = match call_cbor_in_out(&connect_in, webrtc_connect_v1) {
        Ok(o) => o,
        Err(e) => {
            log::warn(&alloc::format!("go2: webrtc_connect_v1 failed: {e}"));
            let _ = db::set_offline("error", &alloc::format!("webrtc_connect: {e}"));
            return json!({ "error": alloc::format!("webrtc_connect: {e}") });
        }
    };
    let channel_id = out.channel_id;
    log::info(&alloc::format!("go2: webrtc channel created id={channel_id}, offer {} bytes", out.offer_sdp.len()));

    // Signaling: con_notify → con_ing (raw HTTP/1.0; reqwest fails on the robot).
    let result = (|| -> Result<String, String> {
        log::info("go2: con_notify POST...");
        let (st, body) = http_raw(&alloc::format!("http://{ip}:9991/con_notify"), "", "")?;
        log::info(&alloc::format!("go2: con_notify http {st}, {} bytes", body.len()));
        if st != 200 {
            return Err(alloc::format!("con_notify http {st}"));
        }
        let identity = protocol::parse_con_notify(&body).map_err(|e| alloc::format!("con_notify: {e}"))?;
        let key = protocol::gen_session_key();
        let (path, ci_body) =
            protocol::build_con_ing(&identity, &key, &out.offer_sdp).map_err(|e| alloc::format!("build con_ing: {e}"))?;
        log::info(&alloc::format!("go2: con_ing POST path={path}..."));
        let (st2, ans) = http_raw(
            &alloc::format!("http://{ip}:9991/{path}"),
            "application/x-www-form-urlencoded",
            &ci_body,
        )?;
        log::info(&alloc::format!("go2: con_ing http {st2}, {} bytes", ans.len()));
        if st2 != 200 {
            return Err(alloc::format!("con_ing http {st2}"));
        }
        protocol::parse_con_ing_answer(&ans, &key).map_err(|e| alloc::format!("answer: {e}"))
    })();

    let answer_sdp = match result {
        Ok(a) => a,
        Err(e) => {
            log::warn(&alloc::format!("go2: signaling failed: {e}"));
            wc_close(&channel_id);
            let _ = db::set_offline("error", &e);
            return json!({ "error": e });
        }
    };
    log::info(&alloc::format!("go2: answer received {} bytes, set_answer...", answer_sdp.len()));
    let set_ans = WebRtcSetAnswerInput { channel_id: channel_id.clone(), answer_sdp };
    if let Err(e) = call_cbor_in_out::<_, WebRtcStatusOutput>(&set_ans, webrtc_set_answer_v1) {
        wc_close(&channel_id);
        let _ = db::set_offline("error", &alloc::format!("set_answer: {e}"));
        return json!({ "error": alloc::format!("set_answer: {e}") });
    }
    // CAS connecting -> validating. If a disconnect raced (we lost), the fresh
    // channel is orphaned — close it so we don't leak a peer connection.
    match db::set_channel(&channel_id) {
        Ok(true) => json!({ "status": "connecting", "channel_id": channel_id }),
        Ok(false) => {
            wc_close(&channel_id);
            json!({ "error": "connect cancelled" })
        }
        Err(e) => {
            wc_close(&channel_id);
            let _ = db::set_offline("error", &alloc::format!("set_channel: {e}"));
            json!({ "error": alloc::format!("set_channel: {e}") })
        }
    }
}

fn do_disconnect() -> JsonValue {
    if let Ok(robot) = db::get_robot() {
        if !robot.channel_id.is_empty() {
            wc_close(&robot.channel_id);
        }
    }
    let _ = db::set_offline("offline", "");
    json!({ "status": "offline" })
}

fn do_estop() -> JsonValue {
    let _ = db::set_estop(true);
    let mut remote_warning: Option<String> = None;
    if let Ok(robot) = db::get_robot() {
        if robot.status == "online" && !robot.channel_id.is_empty() {
            let _ = wc_send_text(&robot.channel_id, &build_sport(SPORT_STOP_MOVE, ""));
            let _ = wc_send_text(&robot.channel_id, &build_sport(SPORT_DAMP, ""));
        } else {
            // Robot owned by another node — an e-stop must ALWAYS reach it. Route a
            // stop (e-stop class: bypasses the addon robot.control gate, never
            // blocked by latch/dedup) to the owner. Surface any failure so the UI
            // never claims the robot stopped when the command did not get through.
            match robot_dispatch(RobotActionWire::simple("stop")) {
                Ok(resp) if resp.ok => {}
                Ok(resp) => {
                    remote_warning =
                        Some(resp.rejected.or(resp.error).unwrap_or_else(|| "rejected".into()));
                }
                Err(e) => remote_warning = Some(alloc::format!("{e}")),
            }
        }
    }
    publish_event("go2.estop", json!({ "active": true }));
    match remote_warning {
        Some(w) => json!({
            "status": "estop_active",
            "warning": alloc::format!("e-stop latched locally but did NOT reach the robot: {w}")
        }),
        None => json!({ "status": "estop_active" }),
    }
}

fn do_reset_estop() -> JsonValue {
    let _ = db::set_estop(false);
    publish_event("go2.estop", json!({ "active": false }));
    json!({ "status": "estop_cleared" })
}

/// Tick: drive validation, then continuous telemetry.
fn tick() {
    let robot = match db::get_robot() {
        Ok(r) => r,
        Err(_) => return,
    };
    match robot.status.as_str() {
        "connecting" => {
            // Stuck-connect watchdog: do_connect normally flips connecting ->
            // validating within one request. If the addon trapped / was killed /
            // a host call wedged after try_begin_connect, the row would block all
            // future connects forever — release it.
            if db::now_secs() - robot.last_update > CONNECT_TIMEOUT_SECS {
                if !robot.channel_id.is_empty() {
                    wc_close(&robot.channel_id);
                }
                let _ = db::set_offline("error", "connect timeout");
                publish_event("go2.offline", json!({ "reason": "connect timeout" }));
            }
        }
        "validating" => {
            if robot.channel_id.is_empty() {
                let _ = db::set_offline("error", "no channel");
                return;
            }
            // Validation watchdog: a stuck handshake (incl. persistent drain
            // errors below, which just retry) is bounded here.
            if db::now_secs() - robot.last_update > VALIDATION_TIMEOUT_SECS {
                wc_close(&robot.channel_id);
                let _ = db::set_offline("error", "validation timeout");
                publish_event("go2.offline", json!({ "reason": "validation timeout" }));
                return;
            }
            let drained = match wc_drain(&robot.channel_id, 32) {
                Ok(d) => d,
                Err(_) => return,
            };
            if drained.closed {
                let _ = db::set_offline("error", "channel closed during validation");
                return;
            }
            for msg in drained.messages {
                if !msg.is_text {
                    continue;
                }
                let raw = B64.decode(msg.data_b64.as_bytes()).unwrap_or_default();
                let text = String::from_utf8_lossy(&raw);
                if let Ok(v) = serde_json::from_str::<JsonValue>(&text) {
                    if v.get("type").and_then(|t| t.as_str()) == Some("validation") {
                        let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                        if data == "Validation Ok." {
                            // Bind camera, subscribe telemetry, go online.
                            let reg = WebRtcRegisterCameraInput {
                                channel_id: robot.channel_id.clone(),
                                display_name: "Unitree Go2".into(),
                                target_fps: 25,
                                analysis_fps: 5,
                            };
                            let cam_id = match call_cbor_in_out::<_, WebRtcRegisterCameraOutput>(&reg, webrtc_register_camera_v1) {
                                Ok(o) => o.camera_id,
                                Err(e) => {
                                    log::warn(&alloc::format!("go2: register_camera failed: {e}"));
                                    String::new()
                                }
                            };
                            // CAS validating -> online on THIS channel. If a
                            // disconnect/reconnect raced, close the now-orphan
                            // channel + its camera registration.
                            match db::set_online(&robot.channel_id, &cam_id) {
                                Ok(true) => {
                                    let _ = wc_send_text(&robot.channel_id, &subscribe_msg("rt/lf/lowstate"));
                                    let _ = wc_send_text(&robot.channel_id, &subscribe_msg("rt/sportmodestate"));
                                    // Go2 only starts publishing the camera RTP after this
                                    // app-level command; the recvonly transceiver alone is silent.
                                    if !cam_id.is_empty() {
                                        let _ = wc_send_text(
                                            &robot.channel_id,
                                            &json!({ "type": "vid", "topic": "", "data": "on" }).to_string(),
                                        );
                                    }
                                    grant_vision_camera(&cam_id);
                                    publish_event("go2.online", json!({ "camera_id": cam_id }));
                                    log::info("go2: online");
                                }
                                _ => {
                                    wc_close(&robot.channel_id);
                                    log::warn("go2: validation won a race it lost — channel closed");
                                }
                            }
                        } else {
                            let resp = protocol::validation_response(data);
                            let _ = wc_send_text(
                                &robot.channel_id,
                                &json!({ "type": "validation", "topic": "", "data": resp }).to_string(),
                            );
                        }
                    }
                }
            }
        }
        "online" => {
            if robot.channel_id.is_empty() {
                let _ = db::set_offline("error", "no channel");
                return;
            }
            // Liveness watchdog keyed on REAL telemetry receipt: lowstate streams
            // continuously, so if no fresh lowstate has arrived within the window
            // the link is dead (a stalled-but-open data channel, or persistent
            // drain failure). last_telemetry advances ONLY on actual lowstate.
            if db::now_secs() - robot.last_telemetry > ONLINE_STALE_SECS {
                wc_close(&robot.channel_id);
                let _ = db::set_offline("error", "telemetry stalled");
                publish_event("go2.offline", json!({ "reason": "telemetry stalled" }));
                return;
            }
            db::bump_tick();
            let tick_n = robot.tick_count + 1;
            let drained = match wc_drain(&robot.channel_id, 64) {
                Ok(d) => d,
                Err(_) => return,
            };
            if drained.closed {
                let _ = db::set_offline("error", "channel closed");
                publish_event("go2.offline", json!({ "reason": "channel closed" }));
                return;
            }
            let mut battery = robot.battery_pct;
            let mut got_telemetry = false;
            DECODE_BUF.with(|cell| {
                let mut dec = cell.borrow_mut();
                for msg in &drained.messages {
                    if !msg.is_text {
                        continue;
                    }
                    let src = msg.data_b64.as_bytes();
                    if dec.len() < src.len() {
                        dec.resize(src.len(), 0);
                    }
                    let n = match B64.decode_slice(src, &mut dec[..]) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    let raw = &dec[..n];
                    // Only lowstate carries battery; gate on the topic substring,
                    // then pull the integer soc with a zero-alloc byte scan.
                    if find_sub(raw, b"lowstate", 0).is_some() {
                        if let Some(soc) = parse_soc(raw) {
                            battery = soc;
                            got_telemetry = true;
                        }
                    }
                }
            });
            // Liveness: advance last_telemetry EVERY tick a lowstate actually
            // arrived (not throttled), so the watchdog tracks real receipt
            // regardless of which tick the throttled publish lands on.
            if got_telemetry {
                let _ = db::record_lowstate(battery);
            }
            // Throttle RTT poll + publish to ~1s (every 5 ticks @200ms).
            if tick_n % 5 == 0 {
                let mut rtt = robot.rtt_ms;
                if let Ok(st) = wc_state(&robot.channel_id) {
                    if st.peer_state == "failed" || st.peer_state == "closed" {
                        let _ = db::set_offline("error", &st.peer_state);
                        publish_event("go2.offline", json!({ "reason": st.peer_state }));
                        return;
                    }
                    if let Some(r) = st.rtt_ms {
                        rtt = r as i64;
                        let _ = db::set_rtt(rtt);
                    }
                }
                publish_event("go2.telemetry", json!({ "battery_pct": battery, "rtt_ms": rtt }));
                if battery >= 0 && battery < BATTERY_ALERT_PCT {
                    publish_event("go2.battery_low", json!({ "battery_pct": battery }));
                }
                if rtt > LATENCY_ALERT_MS {
                    publish_event("go2.latency_high", json!({ "rtt_ms": rtt }));
                }
            }
        }
        _ => {}
    }
}

// =============================================================================
// UI
// =============================================================================

fn next_id() -> String {
    static C: AtomicU64 = AtomicU64::new(0);
    alloc::format!("c{}", C.fetch_add(1, Ordering::Relaxed))
}

fn lit(s: &str) -> BindRef {
    BindRef::Literal(Value::Text(s.into()))
}

fn text(content: &str) -> Component {
    TextComp {
        content: lit(content),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
    }
    .into_component(next_id())
    .expect("Text")
}

fn heading(level: u8, content: &str) -> Component {
    HeadingComp { content: lit(content), level, tone: None, align: None }
        .into_component(next_id())
        .expect("Heading")
}

fn button(label: &str, action: &str, variant: &str) -> Component {
    let v = match variant {
        "primary" => ButtonVariant::Primary,
        "danger" => ButtonVariant::Destructive,
        _ => ButtonVariant::Secondary,
    };
    let mut c = ButtonComp {
        variant: v,
        tone: Tone::Neutral,
        label: lit(label),
        icon_leading: None,
        icon_trailing: None,
        size: ButtonSize::Md,
        full_width: false,
        disabled: None,
        loading: None,
        density: Density::Default,
    }
    .into_component(next_id())
    .expect("Button");
    c.handlers = Some(HandlerMap(vec![(
        tentaflow_sdk_spec::EventKind::Click,
        Handler::Backend {
            action_id: action.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )]));
    c
}

fn card(title: &str, children: Vec<Component>) -> Component {
    SectionCard {
        title: lit(title),
        subtitle: None,
        header_actions: vec![],
        header_divider: false,
        body: children,
        footer: None,
        padding: Spacing::Lg,
        gap: Spacing::Md,
        variant: CardVariant::Outlined,
        radius: RadiusToken::Lg,
        shadow: ShadowToken::Subtle,
        border: BorderToken::Hairline,
        background: BackgroundToken::None,
        accent: None,
    }
    .into_component(next_id())
    .expect("SectionCard")
}

fn render_panel() {
    let robot = db::get_robot().unwrap_or_default();
    let battery = if robot.battery_pct >= 0 {
        alloc::format!("{}%", robot.battery_pct)
    } else {
        "—".into()
    };
    let rtt = if robot.rtt_ms >= 0 {
        alloc::format!("{} ms", robot.rtt_ms)
    } else {
        "—".into()
    };
    // IP pochodzi WYLACZNIE z konfiguracji instalacji (connection-param). Brak
    // fallbacku do starej kolumny robot.ip — niesakonfigurowana instancja pokazuje
    // marker, zeby UI nie sugerowal polaczenia z nieaktualnym/legacy adresem.
    let ip_display = config_get(IP_CONFIG_KEY).unwrap_or_else(|| "(brak konfiguracji)".to_string());
    let status_line = alloc::format!("Status: {}  ·  IP: {}", robot.status, ip_display);
    let estop_line = if robot.estop_active { "E-STOP AKTYWNY".into() } else { "e-stop: wyłączony".to_string() };

    let layout = card(
        "Unitree Go2",
        vec![
            heading(2, "Status"),
            text(&status_line),
            text(&alloc::format!("Bateria: {battery}   ·   Latency (RTT): {rtt}")),
            text(&estop_line),
            heading(3, "Połączenie"),
            button("Połącz", "go2.connect", "primary"),
            button("Rozłącz", "go2.disconnect", "secondary"),
            heading(3, "Bezpieczeństwo"),
            button("STOP (e-stop)", "go2.estop", "danger"),
            button("Reset e-stop", "go2.reset_estop", "secondary"),
            heading(3, "Sterowanie"),
            button("RecoveryStand", "go2.action_recovery", "secondary"),
            button("Hello", "go2.action_hello", "secondary"),
            button("Sit", "go2.action_sit", "secondary"),
            button("Naprzód", "go2.move_fwd", "secondary"),
            button("W tył", "go2.move_back", "secondary"),
            button("Lewo", "go2.move_left", "secondary"),
            button("Prawo", "go2.move_right", "secondary"),
        ],
    );

    let payload = UiPayload::PanelShell(PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: PANEL_EPOCH.load(Ordering::Relaxed),
        layout,
        slots: Vec::<tentaflow_sdk_spec::SlotDecl>::new(),
        initial_state: Vec::<StateEntry>::new(),
        initial_commands: vec![],
    });
    let mut buf = Vec::with_capacity(4096);
    if minicbor::encode(&payload, &mut buf).is_ok() {
        unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32); }
    }
}

// =============================================================================
// Request dispatch
// =============================================================================

fn handle(tool: &str, _params: &JsonValue) -> JsonValue {
    let mv = |vx: f64, vy: f64, vyaw: f64| -> JsonValue {
        let p = json!({ "x": vx, "y": vy, "z": vyaw }).to_string();
        send_sport_gated(SPORT_MOVE, &p)
    };
    match tool {
        "go2.connect" => do_connect(),
        "go2.disconnect" => do_disconnect(),
        "go2.estop" => do_estop(),
        "go2.reset_estop" => do_reset_estop(),
        "go2.action_recovery" => send_sport_gated(SPORT_RECOVERY_STAND, ""),
        "go2.action_hello" => send_sport_gated(SPORT_HELLO, ""),
        "go2.action_sit" => send_sport_gated(SPORT_SIT, ""),
        "go2.action_standup" => send_sport_gated(SPORT_STAND_UP, ""),
        "go2.action_standdown" => send_sport_gated(SPORT_STAND_DOWN, ""),
        "go2.action_stretch" => send_sport_gated(SPORT_STRETCH, ""),
        "go2.move_fwd" => mv(0.3, 0.0, 0.0),
        "go2.move_back" => mv(-0.3, 0.0, 0.0),
        "go2.move_left" => mv(0.0, 0.3, 0.0),
        "go2.move_right" => mv(0.0, -0.3, 0.0),
        "go2.status" => match db::get_robot() {
            Ok(r) => json!({
                "status": r.status, "battery_pct": r.battery_pct, "rtt_ms": r.rtt_ms,
                "estop_active": r.estop_active, "camera_id": r.camera_id,
                // Capabilities the go2 driver exposes. Advertised on the mesh so a
                // controller node can present available actions without owning the
                // addon. Keep in sync with the `go2.action_*` / `go2.move_*` tools.
                "capabilities": [
                    "move", "sit", "stand_up", "stand_down", "recovery_stand",
                    "hello", "stretch", "stop", "camera",
                ],
            }),
            Err(e) => json!({ "error": alloc::format!("{e}") }),
        },
        other => json!({ "error": alloc::format!("unknown tool: {other}") }),
    }
}

/// Decode a `FlowValue` (`{"kind":"json"|"text","data":...}`) into a number.
fn flow_value_num(fv: &JsonValue) -> Option<f64> {
    match fv.get("kind").and_then(JsonValue::as_str) {
        Some("json") => fv.get("data").and_then(JsonValue::as_f64),
        Some("text") => fv
            .get("data")
            .and_then(JsonValue::as_str)
            .and_then(|s| s.trim().parse::<f64>().ok()),
        _ => None,
    }
}

/// Read a clamped velocity (-1..1) from the flow envelope's `variables[key]`
/// FlowValue. Variables are THE contract for go2.move (vx/vy/vyaw), as declared
/// in blocks.json — there is no payload/config source. Missing/unparseable = 0.
fn block_num(params: &JsonValue, key: &str) -> f64 {
    if let Some(fv) = params.get("variables").and_then(|v| v.get(key)) {
        if let Some(n) = flow_value_num(fv) {
            return n.clamp(-1.0, 1.0);
        }
    }
    0.0
}

/// Flow-block dispatch. Executes the robot action, then returns the INPUT
/// envelope (a valid FlowEnvelope) with the result recorded in `meta.go2` so the
/// flow continues. `params` IS the FlowEnvelope the host sent.
fn handle_block(block_type: &str, params: &JsonValue) -> JsonValue {
    let result = match block_type {
        "go2.stop" => do_estop(),
        "go2.recovery_stand" => send_sport_gated(SPORT_RECOVERY_STAND, ""),
        "go2.stand_up" => send_sport_gated(SPORT_STAND_UP, ""),
        "go2.stand_down" => send_sport_gated(SPORT_STAND_DOWN, ""),
        "go2.sit" => send_sport_gated(SPORT_SIT, ""),
        "go2.hello" => send_sport_gated(SPORT_HELLO, ""),
        "go2.stretch" => send_sport_gated(SPORT_STRETCH, ""),
        "go2.move" => {
            let p = json!({
                "x": block_num(params, "vx"),
                "y": block_num(params, "vy"),
                "z": block_num(params, "vyaw"),
            })
            .to_string();
            send_sport_gated(SPORT_MOVE, &p)
        }
        other => json!({ "error": alloc::format!("unknown block: {other}") }),
    };
    let mut env = params.clone();
    if let Some(obj) = env.as_object_mut() {
        let meta = obj
            .entry("meta")
            .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
        if let Some(m) = meta.as_object_mut() {
            m.insert("go2".into(), result);
        }
    }
    env
}

// =============================================================================
// Entry points
// =============================================================================

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let mut buf: Vec<u8> = Vec::with_capacity(size.max(0) as usize);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr as i32
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    if ptr > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr as *mut u8, 0, size.max(0) as usize);
        }
    }
}

/// Seeds the singleton robot row from the install-time `ip` config. Passing an
/// empty ip when no config exists creates the row in offline state WITHOUT
/// inventing a default (ensure_robot keeps the existing ip on empty input).
fn ensure_robot_from_config() {
    let ip = config_get(IP_CONFIG_KEY).unwrap_or_default();
    let _ = db::ensure_robot(&ip);
}

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    ensure_robot_from_config();
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    ensure_robot_from_config();
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    if let Ok(robot) = db::get_robot() {
        if !robot.channel_id.is_empty() {
            wc_close(&robot.channel_id);
        }
    }
    let _ = db::set_offline("offline", "");
    0
}

#[no_mangle]
pub extern "C" fn on_event(_ptr: i32, _len: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_panel_open(_id_ptr: i32, _id_len: i32, epoch: i64) -> i32 {
    PANEL_EPOCH.store(epoch.max(1) as u64, Ordering::Relaxed);
    render_panel();
    0
}

#[no_mangle]
pub extern "C" fn on_tick(ts_ms: i64) -> i32 {
    // Seed the cached clock from the host timestamp so the tick never issues a
    // SQL roundtrip just to read wall-clock time.
    db::set_now_secs(ts_ms / 1000);
    tick();
    0
}

#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input = read_string(input_ptr, input_len);
    let req: JsonValue = serde_json::from_str(&input).unwrap_or(JsonValue::Null);
    let raw_tool = req.get("tool").and_then(|t| t.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(JsonValue::Null);
    // Panel button actions arrive from the dashboard as `ui.<panel_id>.<action_id>`
    // (ui_channel: format!("ui.{panel}.{action}")). Strip the panel prefix so the
    // declared action_ids (go2.connect, go2.estop, …) route to handle(). Flow
    // blocks come via invoke_block as `block.go2.*` with no ui prefix.
    let tool = raw_tool.strip_prefix("ui.overview.").unwrap_or(raw_tool);
    let response = if let Some(block_type) = tool.strip_prefix("block.") {
        handle_block(block_type, &params)
    } else {
        handle(tool, &params)
    };
    write_response(out_ptr, out_cap, out_len_ptr, &response.to_string())
}
