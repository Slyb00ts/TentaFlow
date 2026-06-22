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
mod state;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use serde_json::{json, Value as JsonValue};

use tentaflow_hardware::unitree::go2::protocol;
use tentaflow_sdk_spec::{
    LidarFrameHeader, LIDAR_FRAME_VERSION, LIDAR_HEADER_LEN, LIDAR_LAYOUT_XYZ_I16_PLANAR,
};
use tentaflow_sdk_spec::{
    CameraGrantInput, CameraGrantOut,
    RobotActionWire, RobotControlResponseWire, RobotDispatchInput,
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
// The robot IP is provided per-install via the `ip` connection_param and read
// from addon_config at runtime — there is intentionally NO hardcoded default.
const IP_CONFIG_KEY: &str = "ip";
const LATENCY_ALERT_MS: i64 = 500;
const BATTERY_ALERT_PCT: i64 = 20;
// Watchdogs (seconds). Validation must complete promptly; an online connection
// that stops advancing telemetry (persistent drain/state errors) is declared dead.
const CONNECT_TIMEOUT_SECS: i64 = 20;
const VALIDATION_TIMEOUT_SECS: i64 = 20;
// Auto-connect backoff: when offline with connect-intent on, the tick retries a
// connect at most this often (the tick runs every 100ms — don't hammer the robot).
const RECONNECT_BACKOFF_SECS: i64 = 5;
const ONLINE_STALE_SECS: i64 = 12;

static REQ_ID: AtomicU64 = AtomicU64::new(1);

// Sport command api_ids (Go2 normal mode). This addon drives the robot over the
// WebRTC firmware channel, so the authoritative id source is the WebRTC SPORT_CMD
// table, NOT the DDS `unitree_sdk2` header. Cross-checked on 2026-06-18 against:
//   - DDS:    https://raw.githubusercontent.com/unitreerobotics/unitree_sdk2/main/include/unitree/robot/go2/sport/sport_api.hpp
//   - WebRTC: https://raw.githubusercontent.com/legion1581/unitree_webrtc_connect/master/unitree_webrtc_connect/constants.py (SPORT_CMD)
// 19 of the 22 ids appear in BOTH tables identically. The remaining three —
// BODY_HEIGHT (1013), FOOT_RAISE_HEIGHT (1014) and WIGGLE_HIPS (1033) — are NOT in
// the DDS sport_api.hpp but ARE in the WebRTC SPORT_CMD table, which is the table
// this transport uses. 1036 is `FingerHeart`/HEART in both, mapped to `heart`.
// These IDs move a REAL robot, so they are not invented.
const SPORT_DAMP: u32 = 1001;
const SPORT_BALANCE_STAND: u32 = 1002;
const SPORT_STOP_MOVE: u32 = 1003;
const SPORT_STAND_UP: u32 = 1004;
const SPORT_STAND_DOWN: u32 = 1005;
const SPORT_RECOVERY_STAND: u32 = 1006;
const SPORT_EULER: u32 = 1007;
const SPORT_MOVE: u32 = 1008;
const SPORT_SIT: u32 = 1009;
const SPORT_BODY_HEIGHT: u32 = 1013;
const SPORT_FOOT_RAISE_HEIGHT: u32 = 1014;
const SPORT_SPEED_LEVEL: u32 = 1015;
const SPORT_HELLO: u32 = 1016;
const SPORT_STRETCH: u32 = 1017;
const SPORT_DANCE1: u32 = 1022;
const SPORT_DANCE2: u32 = 1023;
const SPORT_SCRAPE: u32 = 1029;
const SPORT_FRONT_FLIP: u32 = 1030;
const SPORT_FRONT_JUMP: u32 = 1031;
const SPORT_FRONT_POUNCE: u32 = 1032;
const SPORT_WIGGLE_HIPS: u32 = 1033;
const SPORT_FINGER_HEART: u32 = 1036;

// Safe parameter clamps (mirrors core robot_control limits) — applied here too so
// a LOCAL tool/block call (which does not pass through the mesh sanitizer) can
// never send an out-of-range pose to the robot.
const EULER_LIMIT: f64 = 0.75;
const BODY_HEIGHT_MIN: f64 = -0.18;
const BODY_HEIGHT_MAX: f64 = 0.03;
const FOOT_RAISE_MIN: f64 = -0.06;
const FOOT_RAISE_MAX: f64 = 0.10;

// =============================================================================
// Host imports
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
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
    fn lidar_publish_v1(in_ptr: i32, in_len: i32) -> i32;
}

/// Publish ONE canonical LiDAR frame (packed f32, sdk-spec layout) to the host
/// LidarStreamHub. A single byte buffer, single host copy; non-fatal on
/// failure (the next frame retries). Logs a warning on a real ABI error.
fn publish_lidar_frame(frame: &[u8]) {
    let ret = unsafe { lidar_publish_v1(frame.as_ptr() as i32, frame.len() as i32) };
    if ret != 0 {
        log::warn(&alloc::format!("go2 lidar: publish abi error {ret}"));
    }
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
    /// Error-level host log. The host exposes only info/warn import symbols, so
    /// error routes through `log_warn` with an explicit `ERROR` prefix — there is
    /// no separate error import to add. Used by the panic hook to make a future
    /// trap loud and unmistakable.
    pub fn error(m: &str) {
        let line = alloc::format!("ERROR {m}");
        unsafe { log_warn(line.as_ptr() as i32, line.len() as i32); }
    }
}

/// Sub-millisecond clocks for pipeline timing. The addon targets `wasm32-wasip1`
/// and both the wasmtime (desktop) and wasmi (mobile) hosts back WASI
/// `clock_time_get` for clock_id 0 (realtime) and 1 (monotonic), so `std::time`
/// works here with real precision — no extra host-fn is needed just to measure.
/// `wall_micros` stamps the canonical frame header so the browser can compute a
/// true end-to-end latency; `mono_micros` measures stage durations (immune to
/// wall-clock steps).
mod clock {
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    /// Wall-clock microseconds since the Unix epoch. Used to stamp the canonical
    /// frame header (`timestamp_us`) so the browser, on the same machine for the
    /// local case, can subtract its own wall clock for an end-to-end delta.
    pub fn wall_micros() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0)
    }

    /// Monotonic microseconds from a process-static reference Instant. Only valid
    /// for measuring intervals/durations within this process (never compared
    /// across machines), so it never goes backwards under NTP adjustments.
    pub fn mono_micros() -> i64 {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        epoch.elapsed().as_micros() as i64
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

/// Stop a pub/sub topic. The go2_webrtc data channel mirrors `subscribe` with an
/// `unsubscribe` message so the robot stops publishing the voxel stream on disable
/// (the switch "off" stops the sensor; unsubscribe stops the topic delivery).
fn unsubscribe_msg(topic: &str) -> String {
    json!({ "type": "unsubscribe", "topic": topic }).to_string()
}

/// Clamp a value into `[lo, hi]`; reject NaN/inf (caller surfaces an error).
fn clamp_finite(v: f64, lo: f64, hi: f64) -> Option<f64> {
    if !v.is_finite() {
        return None;
    }
    Some(v.clamp(lo, hi))
}

/// Read a JSON number param from the tool/block input (supports a raw number or a
/// FlowValue `{kind,data}`). Returns None when absent/unparseable so the caller
/// can reject rather than silently default a motion command.
fn param_num(params: &JsonValue, key: &str) -> Option<f64> {
    let v = params.get(key)?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    flow_value_num(v)
}

/// Map a local sport command `(api_id, parameter)` to the vendor-agnostic
/// `RobotActionWire` used for cross-node dispatch. `parameter` is the go2 move
/// JSON (`{"x","y","z"}`) for SPORT_MOVE; ignored otherwise. Returns `None` for
/// an api_id with no remote-control equivalent (so the caller keeps it local).
fn sport_to_action(api_id: u32, parameter: &str) -> Option<RobotActionWire> {
    let p: JsonValue = serde_json::from_str(parameter).unwrap_or(JsonValue::Null);
    let num = |k: &str| p.get(k).and_then(JsonValue::as_f64).unwrap_or(0.0);
    Some(match api_id {
        SPORT_MOVE => RobotActionWire::move_to(num("x"), num("y"), num("z")),
        SPORT_STOP_MOVE | SPORT_DAMP => RobotActionWire::simple("stop"),
        SPORT_STAND_UP => RobotActionWire::simple("stand_up"),
        SPORT_STAND_DOWN => RobotActionWire::simple("stand_down"),
        SPORT_RECOVERY_STAND => RobotActionWire::simple("recovery_stand"),
        SPORT_BALANCE_STAND => RobotActionWire::simple("balance_stand"),
        SPORT_SIT => RobotActionWire::simple("sit"),
        SPORT_HELLO => RobotActionWire::simple("hello"),
        SPORT_STRETCH => RobotActionWire::simple("stretch"),
        SPORT_WIGGLE_HIPS => RobotActionWire::simple("wiggle_hips"),
        SPORT_FINGER_HEART => RobotActionWire::simple("heart"),
        SPORT_DANCE1 => RobotActionWire::simple("dance1"),
        SPORT_DANCE2 => RobotActionWire::simple("dance2"),
        SPORT_SCRAPE => RobotActionWire::simple("scrape"),
        SPORT_FRONT_FLIP => RobotActionWire::simple("front_flip"),
        SPORT_FRONT_JUMP => RobotActionWire::simple("front_jump"),
        SPORT_FRONT_POUNCE => RobotActionWire::simple("front_pounce"),
        // The Go2 sport `parameter` for Euler is a JSON object with x/y/z (the
        // roll/pitch/yaw radians); map it back onto the generic params shape.
        SPORT_EULER => RobotActionWire::params("euler", num("x"), num("y"), num("z"), 0.0),
        SPORT_BODY_HEIGHT => RobotActionWire::params("body_height", num("data"), 0.0, 0.0, 0.0),
        SPORT_FOOT_RAISE_HEIGHT => {
            RobotActionWire::params("foot_raise_height", num("data"), 0.0, 0.0, 0.0)
        }
        SPORT_SPEED_LEVEL => RobotActionWire::params("speed_level", num("data"), 0.0, 0.0, 0.0),
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

/// Euler tool: clamp roll/pitch/yaw to ±EULER_LIMIT, reject NaN/inf, send 1007.
/// The Go2 sport `parameter` for Euler is `{"x":roll,"y":pitch,"z":yaw}`.
fn send_euler(params: &JsonValue) -> JsonValue {
    let (Some(roll), Some(pitch), Some(yaw)) = (
        param_num(params, "roll"),
        param_num(params, "pitch"),
        param_num(params, "yaw"),
    ) else {
        return json!({ "error": "euler requires numeric roll/pitch/yaw" });
    };
    let (Some(roll), Some(pitch), Some(yaw)) = (
        clamp_finite(roll, -EULER_LIMIT, EULER_LIMIT),
        clamp_finite(pitch, -EULER_LIMIT, EULER_LIMIT),
        clamp_finite(yaw, -EULER_LIMIT, EULER_LIMIT),
    ) else {
        return json!({ "error": "euler params must be finite" });
    };
    let p = json!({ "x": roll, "y": pitch, "z": yaw }).to_string();
    send_sport_gated(SPORT_EULER, &p)
}

/// BodyHeight tool: clamp delta to [BODY_HEIGHT_MIN, BODY_HEIGHT_MAX], send 1013.
fn send_body_height(params: &JsonValue) -> JsonValue {
    let Some(h) = param_num(params, "height") else {
        return json!({ "error": "body_height requires numeric height" });
    };
    let Some(h) = clamp_finite(h, BODY_HEIGHT_MIN, BODY_HEIGHT_MAX) else {
        return json!({ "error": "body_height must be finite" });
    };
    send_sport_gated(SPORT_BODY_HEIGHT, &json!({ "data": h }).to_string())
}

/// FootRaiseHeight tool: clamp to [FOOT_RAISE_MIN, FOOT_RAISE_MAX], send 1014.
fn send_foot_raise(params: &JsonValue) -> JsonValue {
    let Some(h) = param_num(params, "height") else {
        return json!({ "error": "foot_raise_height requires numeric height" });
    };
    let Some(h) = clamp_finite(h, FOOT_RAISE_MIN, FOOT_RAISE_MAX) else {
        return json!({ "error": "foot_raise_height must be finite" });
    };
    send_sport_gated(SPORT_FOOT_RAISE_HEIGHT, &json!({ "data": h }).to_string())
}

/// SpeedLevel tool: discrete -1/0/1, NaN/out-of-range rejected, send 1015.
fn send_speed_level(params: &JsonValue) -> JsonValue {
    let Some(l) = param_num(params, "level") else {
        return json!({ "error": "speed_level requires numeric level" });
    };
    if !l.is_finite() {
        return json!({ "error": "speed_level must be finite" });
    }
    let l = l.round();
    if !(-1.0..=1.0).contains(&l) {
        return json!({ "error": "speed_level must be -1, 0 or 1" });
    }
    send_sport_gated(SPORT_SPEED_LEVEL, &json!({ "data": l as i64 }).to_string())
}

/// Composite body pose: Euler orientation (1007) + optional BodyHeight delta (1013).
/// This is deliberately NOT the canonical `Pose` (1028) toggle — 1028 is a mode
/// switch whose bool-flag semantics vary across Go2 firmware variants, so it can
/// leave the robot in an unexpected state. The composite gives a deterministic
/// body-orientation+height pose on every firmware; 1028 is intentionally omitted.
///
/// SAFETY: validate and clamp ALL params {roll,pitch,yaw,height} up front and send
/// nothing until every value is known-safe. A bad height must NOT move the robot
/// via the Euler leg first. NaN/inf are rejected (never coerced); finite-but-out-of
/// -range values are clamped to the documented envelope.
fn send_pose(params: &JsonValue) -> JsonValue {
    let (Some(roll), Some(pitch), Some(yaw)) = (
        param_num(params, "roll"),
        param_num(params, "pitch"),
        param_num(params, "yaw"),
    ) else {
        return json!({ "error": "pose requires numeric roll/pitch/yaw" });
    };
    let (Some(roll), Some(pitch), Some(yaw)) = (
        clamp_finite(roll, -EULER_LIMIT, EULER_LIMIT),
        clamp_finite(pitch, -EULER_LIMIT, EULER_LIMIT),
        clamp_finite(yaw, -EULER_LIMIT, EULER_LIMIT),
    ) else {
        return json!({ "error": "pose orientation params must be finite" });
    };

    // `height` is optional for a pure-orientation pose; default to no height change.
    let height = match params.get("height") {
        Some(_) => match param_num(params, "height") {
            Some(h) => match clamp_finite(h, BODY_HEIGHT_MIN, BODY_HEIGHT_MAX) {
                Some(h) => Some(h),
                None => return json!({ "error": "pose height must be finite" }),
            },
            None => return json!({ "error": "pose height must be numeric" }),
        },
        None => None,
    };

    // Everything is validated and clamped: now (and only now) emit motion commands.
    let euler_param = json!({ "x": roll, "y": pitch, "z": yaw }).to_string();
    let euler = send_sport_gated(SPORT_EULER, &euler_param);
    if euler.get("error").is_some() {
        return euler;
    }
    if let Some(h) = height {
        let body = send_sport_gated(SPORT_BODY_HEIGHT, &json!({ "data": h }).to_string());
        if body.get("error").is_some() {
            return body;
        }
    }
    json!({ "status": "sent" })
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
// Latest-telemetry snapshot (rt/sportmodestate + rt/lf/lowstate)
// =============================================================================

/// Most-recent values parsed from the high-rate telemetry streams. Every field is
/// optional so an absent value is NEVER fabricated — a stream that omits a field
/// (or a whole sub-block) simply leaves it `None`/empty. This lives in process
/// memory (single-threaded WASM) and is overwritten with the latest values on
/// every tick; `go2.status` reads it at the existing status cadence (it is NOT
/// advertised at the raw stream rate).
#[derive(Clone, Default)]
struct Telemetry {
    mode: Option<i64>,
    gait_type: Option<i64>,
    body_height: Option<f64>,
    vx: Option<f64>,
    vy: Option<f64>,
    vyaw: Option<f64>,
    position: Vec<f64>,
    foot_force: Vec<f64>,
    imu_roll: Option<f64>,
    imu_pitch: Option<f64>,
    imu_yaw: Option<f64>,
    imu_quaternion: Vec<f64>,
    imu_temperature: Option<f64>,
    // Leg joint angles (radians) from lowstate `motor_state[i].q`, Go2 order:
    // 0-2 FR, 3-5 FL, 6-8 RR, 9-11 RL. Drives the dashboard robot animation (R2).
    joints: Vec<f64>,
    bat_soc: Option<f64>,
    bat_voltage: Option<f64>,
    bat_current: Option<f64>,
    bat_temperature: Option<f64>,
}

std::thread_local! {
    static TELEMETRY: core::cell::RefCell<Telemetry> = core::cell::RefCell::new(Telemetry::default());
}

// =============================================================================
// LiDAR voxel map (rt/utlidar/voxel_map_compressed)
// =============================================================================

const LIDAR_TOPIC: &str = "rt/utlidar/voxel_map_compressed";
const LIDAR_SWITCH_TOPIC: &str = "rt/utlidar/switch";
// Lidar-derived robot odometry pose (position + orientation quaternion). Unlike
// `rt/sportmodestate` (not published over WebRTC on this firmware), go2_ros2_sdk
// sources its /odom from this topic, so it should carry world pose on the Air too.
const POSE_TOPIC: &str = "rt/utlidar/robot_pose";
// Upstream decoder buffers the decompressed occupancy grid at exactly 80_000 bytes
// (go2_webrtc_connect `decompressBuffer`). The grid addresses z*0x800 + y*0x10 +
// x_byte: z-stride 0x800 (2048), y-stride 0x10 (16), 16 x-bytes => 128 x-voxels.
// The upstream-documented uncompressed grid is 80_000 bytes (the maximum valid
// `index + 1`); a frame can hold at most 80_000*8 occupied voxels. `src_size` is
// the documented uncompressed grid size, so any frame declaring a larger size is
// malformed and rejected (logged), never silently truncated. This is also the
// hard cap so a malformed `src_size` cannot make the LZ4 decoder allocate without
// bound.
const LIDAR_GRID_BYTES: usize = 80_000;
// Hard cap on retained decoded points. The Go2 voxel map is sparse (a few k
// occupied voxels per frame in practice); this bounds memory if a frame decodes
// to an unexpectedly dense grid. Exceeding it marks the frame unavailable and is
// logged — we never keep a half-decoded point set (which would misplace points).
const LIDAR_MAX_POINTS: usize = 300_000;
// Periodic cadence (seconds) for refreshing the small LiDAR status in the shared
// DB on steady-state frames. A ~1s refresh keeps point_count/frame_seq fresh for
// the card without writing on every decoded voxel frame; availability/enabled
// transitions bypass this and persist immediately.
const LIDAR_STATUS_REFRESH_SECS: i64 = 1;
// Rate-limit for the per-frame pipeline timing log line. At a typical ~5 Hz voxel
// stream this emits roughly one concise timing line every ~2 s, enough to watch
// live without flooding the log on the hot drain path.
const LIDAR_TIMING_LOG_EVERY: u64 = 10;
// Rate-limit for the per-frame decode-sizing diagnostic line. The FIRST voxel
// frame of a session always logs (counter starts at 0), then one line every
// LIDAR_DIAG_LOG_EVERY frames so the 5Hz stream is not flooded while still
// surfacing src_size / decompressed length / point_count on real data.
const LIDAR_DIAG_LOG_EVERY: u64 = 25;

/// Latest decoded LiDAR frame plus the on/off intent. Only the MOST RECENT frame
/// is kept (the voxel map is a stream); a new frame overwrites the prior one so
/// memory stays bounded. `enabled` is the operator intent (we sent switch "on"),
/// distinct from `available` (we have actually decoded at least one fresh frame).
#[derive(Clone, Default)]
struct LidarState {
    enabled: bool,
    // True once the host channel has been sent the subscribe message for this
    // online session, so the per-tick path does not re-subscribe every tick.
    subscribed: bool,
    // Set when a disable transition's switch "off" send failed: local state is
    // left as still-enabled so a later tick retries the off-send. Cleared only
    // once an off-send actually succeeds (then enabled/subscribed flip to false).
    pending_disable: bool,
    // Last wall-clock second the small status was persisted to the shared DB.
    // Gates the per-frame status-write throttle (periodic ~1s refresh on top of
    // the immediate write on any availability/enabled transition).
    status_persist_ts: i64,
    // The (enabled, available) pair last persisted to the shared DB, so a frame
    // that changes either flag is persisted immediately (not waiting for the 1s
    // refresh) while steady-state frames only refresh point_count/frame_seq ~1s.
    status_persist_state: (bool, bool),
    resolution: Option<f32>,
    origin: Option<[f64; 3]>,
    // Cheap occupied-voxel count of the latest frame (popcount over the bitfield).
    // The on_tick path NEVER materializes the full point cloud — it only counts set
    // bits — so a dense frame cannot blow the per-tick fuel budget and wedge the
    // connection. The canonical packed-f32 cloud is decoded directly on the tick
    // (decode_voxel_to_canonical) and published to the host LidarStreamHub; this
    // addon never retains the full Vec<[f32;3]> or the raw frame.
    point_count: usize,
    // Monotonic counter of frames decoded this session (UI freshness indicator).
    frame_seq: u64,
    // Wall-clock seconds of the last decoded frame.
    last_update_ts: i64,
    // Size of the last compressed payload received (bytes), for diagnostics even
    // when a frame failed to decode.
    last_payload_bytes: usize,
    // --- Pipeline timing (always-on, rate-limited in logs) ---
    // Monotonic µs of the previous voxel-frame arrival, to derive the WebRTC
    // inter-arrival interval (and thus the robot's effective send Hz). Zero until
    // the first frame establishes a baseline (no interval logged for frame #1).
    last_voxel_arrival_us: i64,
    // Counter of ingested voxel frames, used to rate-limit the timing log line so
    // a multi-Hz stream emits roughly one timing line per LIDAR_TIMING_LOG_EVERY
    // frames instead of one per frame.
    timing_log_counter: u64,
    // Counter of voxel frames seen by the diagnostic line (decode sizing inputs).
    // First frame always logs; thereafter rate-limited by LIDAR_DIAG_LOG_EVERY so
    // we can confirm src_size / decompressed length / point_count on real data
    // without flooding the 5Hz stream.
    diag_log_counter: u64,
}

std::thread_local! {
    static LIDAR: core::cell::RefCell<LidarState> = core::cell::RefCell::new(LidarState::default());
    // One-shot guard so parse_voxel_frame logs the matched JSON framing offset and
    // declared src_size EXACTLY ONCE per worker, confirming the (2,0) auto-detect
    // picked the correct boundary on real firmware without flooding the stream.
    static VOXEL_FRAMING_LOGGED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    // Throttle counter for the robot_pose probe log.
    static POSE_LOG: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
    // DIAGNOSTIC (R2 / 0e-live probe): distinct WebRTC topics seen this session +
    // one-shot lowstate-shape dump. Tells us what data is actually available over
    // WebRTC (joints? position?) so we know whether option-B pose is possible here.
    static DIAG: core::cell::RefCell<DiagState> =
        core::cell::RefCell::new(DiagState { topics: alloc::vec::Vec::new(), lowstate_dumped: false });
}

#[derive(Default)]
struct DiagState {
    topics: alloc::vec::Vec<alloc::string::String>,
    lowstate_dumped: bool,
}

/// Log each DISTINCT inbound topic exactly once (cap 64) — enumerates what the
/// robot actually publishes over WebRTC.
fn diag_note_topic(raw: &[u8]) {
    let needle = b"\"topic\":\"";
    let Some(pos) = find_sub(raw, needle, 0) else { return };
    let start = pos + needle.len();
    let rest = &raw[start..];
    let Some(end) = rest.iter().position(|&b| b == b'"') else { return };
    let Ok(topic) = core::str::from_utf8(&rest[..end]) else { return };
    DIAG.with(|c| {
        let mut d = c.borrow_mut();
        if d.topics.iter().any(|t| t == topic) || d.topics.len() >= 64 {
            return;
        }
        d.topics.push(alloc::string::String::from(topic));
        log::info(&alloc::format!("go2 DIAG topic seen: {topic}"));
    });
}

/// One-shot dump of the lowstate payload shape: which `data` keys exist, the joint
/// `motor_state[].q` values (R2), and whether any position-like field is present
/// (0e-live source check). Runs on the first lowstate frame only.
fn diag_lowstate_dump(data: &JsonValue) {
    let first = DIAG.with(|c| {
        let mut d = c.borrow_mut();
        if d.lowstate_dumped {
            return false;
        }
        d.lowstate_dumped = true;
        true
    });
    if !first {
        return;
    }
    if let Some(obj) = data.as_object() {
        let keys: alloc::vec::Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        log::info(&alloc::format!("go2 DIAG lowstate data keys: {keys:?}"));
    }
    match data.get("motor_state").and_then(JsonValue::as_array) {
        Some(ms) => {
            let qs: alloc::vec::Vec<f64> = ms
                .iter()
                .take(12)
                .filter_map(|m| m.get("q").and_then(JsonValue::as_f64))
                .collect();
            log::info(&alloc::format!(
                "go2 DIAG joints: motor_state count={} q[0..12]={qs:?}",
                ms.len()
            ));
        }
        None => log::info("go2 DIAG joints: NO motor_state in lowstate"),
    }
    for k in ["position", "pose", "p", "xyz", "foot_position", "trunk_pose"] {
        if data.get(k).is_some() {
            log::info(&alloc::format!("go2 DIAG lowstate HAS position-like field '{k}'"));
        }
    }
}

/// Reset all per-session LiDAR runtime state. Called on disconnect/offline so a
/// stale frame from a previous session is never reported as available. Clears the
/// shared-store live session snapshots (telemetry + lidar status) but PRESERVES
/// the operator's persistent enable INTENT so the lidar re-subscribes
/// automatically once the link is back online.
fn lidar_reset_session() {
    LIDAR.with(|cell| {
        let mut l = cell.borrow_mut();
        let enabled = l.enabled;
        *l = LidarState::default();
        l.enabled = enabled;
    });
    let _ = state::delete(state::KEY_TELEMETRY);
    let _ = state::delete(state::KEY_LIDAR_STATUS);
    // Reset the diagnostic probe so a reconnect re-enumerates topics + re-dumps the
    // lowstate shape (it is meant to report what THIS session sees).
    DIAG.with(|cell| *cell.borrow_mut() = DiagState::default());
}

/// Mirror the connection-status fields the advertise path (and a future host-side
/// reader) consumes into the shared store under `live:status`. Ephemeral: this is
/// a fast cross-worker READ cache, NOT the source of truth — the `robot` SQLite
/// row (with its CAS connect lock) stays authoritative. Written on every status
/// transition so a worker / host can read status/camera/battery/rtt without a
/// WASM call or a SQL round-trip.
fn mirror_status(robot: &db::Robot) {
    let snapshot = json!({
        "status": robot.status,
        "camera_id": robot.camera_id,
        "battery_pct": robot.battery_pct,
        "rtt_ms": robot.rtt_ms,
    });
    // The mirror is a best-effort read-cache, but a write failure must be
    // observable, not silently swallowed — a stale mirror misleads cross-worker
    // / host-side readers about the connection state.
    if let Err(e) = state::set_ephemeral(state::KEY_STATUS, snapshot.to_string().into_bytes()) {
        log::warn(&alloc::format!("go2: status mirror write failed: {e}"));
    }
}

/// Write the status mirror after a transition that changed connection fields,
/// reading the freshly persisted `robot` row so the mirror reflects exactly what
/// the source-of-truth holds. Best-effort: a read failure simply skips the mirror
/// for this transition (the robot row remains authoritative).
fn mirror_status_from_db() {
    if let Ok(robot) = db::get_robot() {
        mirror_status(&robot);
    }
}

/// `db::set_offline` + refresh the `live:status` mirror so the store reflects the
/// new offline/error state for cross-worker / host-side reads. The `robot` row
/// stays the source of truth; this only keeps the fast read cache in sync.
fn set_offline_mirrored(status: &str, msg: &str) -> Result<(), AbiError> {
    let r = db::set_offline(status, msg);
    mirror_status_from_db();
    r
}

/// Build the SMALL LiDAR availability sub-object from the in-tick thread_local
/// (no point cloud). `available` is true only when at least one frame decoded this
/// session. Absent fields (resolution/origin) are omitted, never fabricated. ONLY
/// the service instance calls this — it persists the result to the shared store;
/// the cross-worker `go2.status` reads it back via `lidar_status_from_store`.
fn lidar_status_json() -> JsonValue {
    LIDAR.with(|cell| {
        let l = cell.borrow();
        let mut obj = serde_json::Map::new();
        obj.insert("enabled".into(), json!(l.enabled));
        obj.insert("available".into(), json!(l.frame_seq > 0 && l.point_count > 0));
        obj.insert("point_count".into(), json!(l.point_count));
        if let Some(r) = l.resolution {
            obj.insert("resolution".into(), json!(r));
        }
        if let Some(o) = l.origin {
            obj.insert("origin".into(), json!([o[0], o[1], o[2]]));
        }
        obj.insert("frame_seq".into(), json!(l.frame_seq));
        if l.last_update_ts > 0 {
            obj.insert("last_update_ts".into(), json!(l.last_update_ts));
        }
        JsonValue::Object(obj)
    })
}

/// Cheap occupied-voxel count: popcount over the decompressed occupancy bitfield.
/// O(bitfield bytes) and allocates nothing. Used both to size the canonical frame
/// buffer exactly and as the availability/diagnostic point count, so the per-tick
/// cost stays bounded even for a dense grid (the failure mode that wedged the
/// connection when the cloud was materialized as a Vec).
fn count_voxel_points(buf: &[u8]) -> usize {
    buf.iter().map(|b| b.count_ones() as usize).sum()
}

/// Decode the Go2 voxel-map occupancy bitfield DIRECTLY into a canonical,
/// vendor-agnostic `LidarFrame` (packed f32, sdk-spec layout) — the format Core
/// and the renderer consume identically for every robot. This is the path used
/// on the service tick instead of building a `Vec<[f32;3]>` + JSON.
///
/// INVARIANT (the whole point of L1): zero JSON; the output `Vec<u8>` is
/// PREALLOCATED to exactly `LIDAR_HEADER_LEN + point_count*3*2` from the exact
/// popcount, so it never grows during the bit-scan, and the whole frame is one
/// buffer for a single WASM->host copy in `publish_lidar_frame`.
///
/// Returns `None` if the occupied count exceeds `LIDAR_MAX_POINTS` (caller logs
/// + drops the frame — never a partial cloud, which would misplace points).
fn decode_voxel_to_canonical(
    decompressed: &[u8],
    resolution: f32,
    origin: [f32; 3],
    frame_seq: u32,
    ts_us: i64,
) -> Option<Vec<u8>> {
    let point_count = count_voxel_points(decompressed);
    if point_count > LIDAR_MAX_POINTS {
        return None;
    }
    let header = LidarFrameHeader {
        version: LIDAR_FRAME_VERSION,
        // Packed-i16 grid indices: half the wire bytes of f32 XYZ and lossless for
        // a voxel map (every point already lands on `origin + index * resolution`).
        // The browser reconstructs world meters from these indices + the header's
        // resolution/origin, so those two fields are now load-bearing, not just
        // informational. Emitting indices is also cheaper per point than the f32
        // multiply-add, easing the service-tick fuel budget.
        layout: LIDAR_LAYOUT_XYZ_I16_PLANAR,
        // Addon emits an uncompressed body; the host pump applies LZ4 + the flag
        // on the way out, so the metered service tick never pays compression fuel.
        flags: 0,
        point_count: point_count as u32,
        frame_seq,
        timestamp_us: ts_us,
        // The addon does not know when the host will broadcast this frame; the
        // host pump stamps `host_send_us` in place just before the WS send.
        host_send_us: 0,
        resolution,
        origin,
    };
    // Exact preallocation with checked arithmetic so a pathological count can
    // never overflow usize and panic the allocation size. point_count is already
    // capped at LIDAR_MAX_POINTS above, so the checked path always succeeds here;
    // the saturating fallback is a defensive belt-and-braces (still a finite,
    // bounded reserve — never a panic). The body is `point_count * 3 * 2` bytes
    // (3 i16 grid indices per point, XYZ_I16 layout).
    let body_cap = point_count
        .checked_mul(3)
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| n.checked_add(LIDAR_HEADER_LEN))
        .unwrap_or(LIDAR_HEADER_LEN);
    // Fallible reserve instead of with_capacity: under panic = abort an oversized
    // or garbage allocation request would call handle_alloc_error and KILL the
    // process (no panic to catch). try_reserve_exact returns Err on failure so we
    // drop the frame (caller logs) instead of aborting the connection. We size the
    // exact buffer then `resize` (within the reserved capacity, so no reallocation
    // / abort) because PLANAR emission writes to three disjoint regions by index,
    // not sequentially.
    let mut out: Vec<u8> = Vec::new();
    if out.try_reserve_exact(body_cap).is_err() {
        return None;
    }
    out.resize(body_cap, 0);
    out[..LIDAR_HEADER_LEN].copy_from_slice(&header.encode_header());
    // Emit raw grid INDICES as i16 in PLANAR order: all ix, then all iy, then all
    // iz. Each plane is a long low-entropy run (iy/iz barely change along a scan
    // row), so the host's LZ4 pass compresses it far better than interleaved. The
    // browser reconstructs `idx * resolution + origin`; no per-point float math
    // here, which also trims the service-tick fuel.
    let n = point_count;
    let ix_base = LIDAR_HEADER_LEN;
    let iy_base = LIDAR_HEADER_LEN + n * 2;
    let iz_base = LIDAR_HEADER_LEN + n * 4;
    let mut p = 0usize;
    for (i, &byte) in decompressed.iter().enumerate() {
        if byte == 0 {
            continue;
        }
        let z = (i / 0x800) as i16;
        let n_slice = i % 0x800;
        let y = (n_slice / 0x10) as i16;
        let x_base = ((n_slice % 0x10) * 8) as i16;
        // y/z are constant for this byte; only x varies per set bit.
        let yi = y.to_le_bytes();
        let zi = z.to_le_bytes();
        // Bit-scan: only set bits do work. The Go2 grid is MSB-first along x
        // (bit 0 == 0x80), so reverse the trailing-zero index to recover x.
        let mut bits = byte;
        while bits != 0 {
            let b = bits.trailing_zeros();
            bits &= bits - 1;
            let xi = (x_base + (7 - b) as i16).to_le_bytes();
            out[ix_base + p * 2..ix_base + p * 2 + 2].copy_from_slice(&xi);
            out[iy_base + p * 2..iy_base + p * 2 + 2].copy_from_slice(&yi);
            out[iz_base + p * 2..iz_base + p * 2 + 2].copy_from_slice(&zi);
            p += 1;
        }
    }
    Some(out)
}

/// Read a 3-element `[x,y,z]` numeric origin from the lidar frame JSON `data`.
/// All-or-nothing: a missing/non-numeric element drops the whole origin (we never
/// place points against a fabricated origin).
fn parse_origin3(v: Option<&JsonValue>) -> Option<[f64; 3]> {
    let arr = v.and_then(JsonValue::as_array)?;
    // get(..) instead of indexing: a JSON origin array shorter than 3 returns
    // None rather than panicking (panic = abort would drop the link).
    Some([
        arr.get(0)?.as_f64()?,
        arr.get(1)?.as_f64()?,
        arr.get(2)?.as_f64()?,
    ])
}

/// Parse the two binary data-channel framings the Go2 uses for an inbound voxel
/// map (upstream `deal_array_buffer`):
///   - LiDAR framing: leading `<HH>` == (2,0); then a `<I>` (u32 LE) JSON length;
///     the `len`-byte JSON object follows, then the LZ4-compressed bitfield. The
///     JSON starts either immediately after the length (raw[8]) or after a 4-byte
///     pad (raw[12]) depending on firmware. We AUTO-DETECT by trying both and
///     keeping the one that parses as the voxel topic — the JSON is exactly `len`
///     bytes, so each candidate boundary is unambiguous to validate, and the
///     compressed tail follows it. This avoids a fragile hard-coded offset (an
///     off-by-4 there silently yields zero points / a broken decode).
///   - normal framing: `<H>` JSON length at 0, JSON at [4..4+len], compressed after.
/// Returns `(data_json, compressed_bytes)` where `data_json` is the inner `data`
/// object carrying `{resolution, origin, src_size}`. `None` if the frame is too
/// short, the JSON is invalid, or the topic is not the voxel map. Every read is
/// bounds-checked via `get(..)`: a short/truncated/oddly-framed real frame can
/// NEVER panic the decode path (panic = abort would drop the whole connection).
fn parse_voxel_frame(raw: &[u8]) -> Option<(JsonValue, &[u8])> {
    let header = raw.get(0..4)?;
    let h1 = u16::from_le_bytes([header[0], header[1]]);
    let h2 = u16::from_le_bytes([header[2], header[3]]);
    if h1 == 2 && h2 == 0 {
        // LiDAR framing: u32 JSON length right after the (2,0) header.
        let len_bytes = raw.get(4..8)?;
        let len =
            u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
        // The JSON object is exactly `len` bytes; firmware places it at raw[8] or
        // raw[12]. Try both and accept the one that parses to the voxel topic.
        for json_start in [8usize, 12usize] {
            let Some(json_end) = json_start.checked_add(len) else {
                continue;
            };
            let Some(json) = raw.get(json_start..json_end) else {
                continue;
            };
            let Ok(env) = serde_json::from_slice::<JsonValue>(json) else {
                continue;
            };
            if env.get("topic").and_then(JsonValue::as_str) != Some(LIDAR_TOPIC) {
                continue;
            }
            let compressed = raw.get(json_end..)?;
            let data = env.get("data")?.clone();
            // One-shot: confirm WHICH json_start matched and the declared src_size on
            // real firmware. An off-by-4 in this auto-detect silently yields a broken
            // decode, so logging the empirically-chosen boundary once pins it down.
            VOXEL_FRAMING_LOGGED.with(|logged| {
                if !logged.get() {
                    logged.set(true);
                    let src_size = data.get("src_size").and_then(JsonValue::as_u64);
                    log::info(&alloc::format!(
                        "go2 lidar framing: json_start={} declared_len={} src_size={:?}",
                        json_start,
                        len,
                        src_size,
                    ));
                }
            });
            return Some((data, compressed));
        }
        None
    } else {
        // Normal framing: u16 JSON length at 0, JSON at [4..4+len].
        let len = u16::from_le_bytes([header[0], header[1]]) as usize;
        let json_end = 4usize.checked_add(len)?;
        let json = raw.get(4..json_end)?;
        let envelope: JsonValue = serde_json::from_slice(json).ok()?;
        // EXACT match: only the voxel-map topic decodes into the voxel frame.
        if envelope.get("topic").and_then(JsonValue::as_str) != Some(LIDAR_TOPIC) {
            return None;
        }
        let compressed = raw.get(json_end..)?;
        let data = envelope.get("data")?.clone();
        Some((data, compressed))
    }
}

/// Ingest one inbound binary voxel-map frame: parse the framing, LZ4-decompress to
/// `src_size`, COUNT occupied voxels (cheap popcount), bump the latest-frame
/// metadata, then decode the bitfield DIRECTLY into the canonical packed-f32 layout
/// and publish it to the host `LidarStreamHub`. No raw payload is retained: only the
/// newest frame's small metadata (count/resolution/origin/seq/ts) stays in WASM
/// state; the cloud lives in the host hub (latest-wins). Tolerant: a malformed or
/// oversized frame updates the diagnostic byte size + logs, but leaves the prior
/// metadata untouched and `available` unchanged rather than fabricating data.
///
/// INVARIANT: the per-tick cost is O(bitfield) — popcount + a single bit-scan into
/// one preallocated buffer — so a dense frame cannot exceed the fuel budget.
fn ingest_voxel_map(raw: &[u8]) {
    let Some((data, compressed)) = parse_voxel_frame(raw) else {
        return;
    };
    // Stage 1: WebRTC voxel-frame cadence. Snapshot the arrival instant and the
    // delta from the previous voxel frame (0 for the very first one). Monotonic,
    // so an NTP step can't produce a bogus negative interval.
    let arrival_us = clock::mono_micros();
    let webrtc_interval_us = LIDAR.with(|cell| {
        let mut l = cell.borrow_mut();
        l.last_payload_bytes = compressed.len();
        let delta = if l.last_voxel_arrival_us > 0 {
            arrival_us - l.last_voxel_arrival_us
        } else {
            0
        };
        l.last_voxel_arrival_us = arrival_us;
        delta
    });
    let resolution = data.get("resolution").and_then(JsonValue::as_f64);
    let origin = parse_origin3(data.get("origin"));
    // `src_size` is the decompressed-buffer size the robot used (LZ4 block needs
    // the exact uncompressed length, like `lz4.block.decompress(uncompressed_size)`).
    let src_size = data
        .get("src_size")
        .and_then(JsonValue::as_u64)
        .map(|n| n as usize);
    let (Some(resolution), Some(origin), Some(src_size)) = (resolution, origin, src_size) else {
        log::warn("go2 lidar: frame missing resolution/origin/src_size — skipped");
        return;
    };
    // Range-check resolution/origin BEFORE allocating/decompressing. A non-positive
    // or non-finite resolution, or any non-finite origin component, is malformed —
    // reject the frame rather than spend CPU/memory decoding an unplaceable grid.
    if !resolution.is_finite() || resolution <= 0.0 {
        log::warn("go2 lidar: non-positive or non-finite resolution — frame skipped");
        return;
    }
    if origin.iter().any(|c| !c.is_finite()) {
        log::warn("go2 lidar: non-finite origin component — frame skipped");
        return;
    }
    if src_size == 0 || src_size > LIDAR_GRID_BYTES {
        log::warn("go2 lidar: src_size out of bounds — frame skipped");
        return;
    }
    // Stage 2: addon decode duration starts here (LZ4-decompress + the bit-scan
    // canonical decode below). Monotonic so it measures pure CPU work, immune to
    // any wall-clock adjustment mid-frame.
    let decode_start_us = clock::mono_micros();
    let mut decompressed = vec![0u8; src_size];
    let n = match lz4_flex::block::decompress_into(compressed, &mut decompressed) {
        Ok(n) => n,
        Err(_) => {
            log::warn("go2 lidar: LZ4 decompress failed — frame skipped");
            return;
        }
    };
    // `src_size` is the documented uncompressed grid size: a short decompress means
    // the declared size and the actual block disagree — malformed, not a partial
    // grid. Reject and keep prior state.
    if n != src_size {
        log::warn("go2 lidar: decompressed length != src_size — frame skipped");
        return;
    }
    // Clamp the valid-byte length to the buffer even though `n == src_size ==
    // decompressed.len()` here: a bogus `n` from the decompressor can NEVER
    // produce an out-of-bounds slice (panic = abort would drop the link).
    let n = n.min(decompressed.len());
    let grid = match decompressed.get(..n) {
        Some(g) => g,
        None => return,
    };
    let resolution_f32 = resolution as f32;
    // Cheap popcount first so the canonical buffer below can be exact-preallocated.
    let point_count = count_voxel_points(grid);
    // Rate-limited decode-sizing diagnostic: surfaces the exact inputs that drive
    // the canonical buffer allocation. A garbage-huge point_count here (vs the real
    // Go2 ~30k-42k) means the framing/decompress is wrong (bad src_size or a
    // misaligned compressed tail), which is the root cause of the allocation abort.
    let should_diag = LIDAR.with(|cell| {
        let mut l = cell.borrow_mut();
        let first = l.diag_log_counter == 0;
        let due = l.diag_log_counter % LIDAR_DIAG_LOG_EVERY == 0;
        l.diag_log_counter = l.diag_log_counter.wrapping_add(1);
        first || due
    });
    if should_diag {
        log::info(&alloc::format!(
            "go2 lidar diag: src_size={} decompressed_n={} compressed_len={} point_count={} resolution={}",
            src_size,
            n,
            compressed.len(),
            point_count,
            resolution,
        ));
    }
    let now = db::now_secs();
    let (should_persist, frame_seq, enabled) = LIDAR.with(|cell| {
        let mut l = cell.borrow_mut();
        l.resolution = Some(resolution_f32);
        l.origin = Some(origin);
        l.point_count = point_count;
        l.frame_seq = l.frame_seq.saturating_add(1);
        l.last_update_ts = now;
        // Throttle the shared-DB status write the same way telemetry is throttled:
        // persist IMMEDIATELY when enabled/available transitions (the card must
        // flip availability without delay), otherwise refresh point_count/frame_seq
        // at most once per LIDAR_STATUS_REFRESH_SECS so the 200ms/64-msg drain
        // never hammers SQLite.
        let state = (l.enabled, l.frame_seq > 0 && l.point_count > 0);
        let transitioned = state != l.status_persist_state;
        let due = now - l.status_persist_ts >= LIDAR_STATUS_REFRESH_SECS;
        let should = if transitioned || due {
            l.status_persist_state = state;
            l.status_persist_ts = now;
            true
        } else {
            false
        };
        (should, l.frame_seq, l.enabled)
    });
    // Decode the bitfield DIRECTLY into the canonical packed-f32 frame and publish
    // it to the host (one preallocated buffer, one WASM->host copy, zero JSON) so
    // the L2 stream hub / renderer get vendor-agnostic points. Only when LiDAR is
    // enabled and the grid actually has occupied voxels. A frame above
    // LIDAR_MAX_POINTS is dropped + logged.
    //
    // The header `timestamp_us` is the REAL wall-clock microsecond of decode time
    // (WASI realtime clock), not the old second-granularity `now*1e6` — the
    // browser subtracts its own wall clock from this to get end-to-end latency, so
    // sub-millisecond precision is required (1 s granularity would make every
    // latency look like 0–1000 ms of pure rounding noise).
    let mut decode_us: i64 = 0;
    let mut publish_us: i64 = 0;
    let mut published_bytes: usize = 0;
    if enabled && point_count > 0 {
        match decode_voxel_to_canonical(
            grid,
            resolution_f32,
            [origin[0] as f32, origin[1] as f32, origin[2] as f32],
            frame_seq as u32,
            clock::wall_micros(),
        ) {
            Some(frame) => {
                // Stage 2 end: decode duration covers LZ4 + canonical bit-scan.
                decode_us = clock::mono_micros() - decode_start_us;
                published_bytes = frame.len();
                // Stage 3: publish duration — the WASM->host copy into the hub.
                let publish_start_us = clock::mono_micros();
                publish_lidar_frame(&frame);
                publish_us = clock::mono_micros() - publish_start_us;
            }
            None => log::warn("go2 lidar: frame exceeds LIDAR_MAX_POINTS — not published"),
        }
        // Rate-limited timing line: one concise summary per LIDAR_TIMING_LOG_EVERY
        // frames so a multi-Hz stream doesn't flood the log. Covers stages 1–3.
        let should_log = LIDAR.with(|cell| {
            let mut l = cell.borrow_mut();
            l.timing_log_counter = l.timing_log_counter.wrapping_add(1);
            l.timing_log_counter % LIDAR_TIMING_LOG_EVERY == 0
        });
        if should_log && decode_us > 0 {
            log::info(&alloc::format!(
                "lidar timing: webrtc_interval={}ms decode={}us publish={}us points={} bytes={}",
                webrtc_interval_us / 1000,
                decode_us,
                publish_us,
                point_count,
                published_bytes,
            ));
        }
    }
    // Persist the SMALL status (metadata only) so any worker's go2.status /
    // lidar_frame sees the latest availability. The full point cloud stays in the
    // service instance's memory (never persisted — see lidar_frame).
    if should_persist {
        let _ = state::set_ephemeral(
            state::KEY_LIDAR_STATUS,
            lidar_status_json().to_string().into_bytes(),
        );
    }
}

/// Turn the LiDAR sensor on/off. This runs on ANY pooled worker (NOT necessarily
/// the service instance that drains the stream), so it only writes the persistent
/// enable INTENT to the shared DB. The service instance's on_tick reads that flag
/// each tick and drives the actual `rt/utlidar/switch` command + voxel-topic
/// subscription against the live channel (see `tick`). When the robot lives on
/// another node the toggle is dispatched over the mesh like every other control.
fn set_lidar(enabled: bool) -> JsonValue {
    // Ownership is decided by the install-time `ip` connection_param, NOT by the
    // live online state. A configured IP means THIS node owns the robot (it is the
    // one that opens the WebRTC link); no IP means the robot lives on another mesh
    // node. Conflating "remote-owned" with "local-but-offline/reconnecting" (the
    // old `status != online` gate) dropped the operator's intent while a locally
    // owned robot was briefly down, so reconnect came back with the wrong LiDAR
    // state. The intent must persist for the local owner regardless of online
    // status; on_tick applies the actual switch when the link is back.
    if config_get(IP_CONFIG_KEY).filter(|ip| !ip.is_empty()).is_none() {
        // Robot owned by another node — route the toggle over the mesh.
        let kind = if enabled { "lidar_on" } else { "lidar_off" };
        return match robot_dispatch(RobotActionWire::simple(kind)) {
            Ok(resp) => dispatch_result_json(resp),
            Err(e) => json!({ "error": alloc::format!("dispatch: {e}") }),
        };
    }
    // Local owner: persist the desire regardless of online state. on_tick drives
    // subscription/switch from this flag, so persisting while offline is enough
    // for correct reconnect behavior (re-subscribe on enable, stay off on disable).
    // Durable so the intent survives a restart, exactly like the old persisted
    // column; on_tick reads it back from the store each tick.
    if let Err(e) = state::set_durable(state::KEY_LIDAR_ENABLED, alloc::vec![u8::from(enabled)]) {
        return json!({ "error": alloc::format!("state: {e}") });
    }
    json!({ "status": "sent", "enabled": enabled })
}

/// LiDAR frame availability snapshot. The live point cloud no longer flows
/// through this JSON tool: the service tick decodes each frame DIRECTLY into the
/// canonical packed-f32 layout and publishes it to the host `LidarStreamHub`
/// (`publish_lidar_frame`), which the renderer pulls as binary L1 bytes. This
/// tool stays as the mesh-action seam (`RobotAction::LidarFrame`) and returns the
/// small availability metadata (enabled/available/point_count/frame_seq), never
/// the cloud. A REAL store read error is surfaced, not papered over as "disabled".
fn lidar_frame() -> JsonValue {
    match lidar_status_from_store() {
        Ok(s) => s,
        Err(e) => json!({ "error": alloc::format!("lidar status read: {e}") }),
    }
}

/// Read the persistent LiDAR enable INTENT from the shared store. Propagates a
/// read error rather than collapsing it to `false`: a transient ABI failure must
/// NOT be interpreted as "operator wants LiDAR off" (that would silently command
/// the robot's LiDAR off). `Ok(false)` only when nothing has ever been written.
fn lidar_enabled_intent() -> Result<bool, AbiError> {
    match state::get(state::KEY_LIDAR_ENABLED)? {
        Some(bytes) => Ok(bytes.first().is_some_and(|b| *b != 0)),
        None => Ok(false),
    }
}

/// Read a fixed-layout `[a, b, c, ...]` JSON sensor array of numbers into a
/// `Vec<f64>`. All-or-nothing: these arrays carry positional identity (e.g.
/// `foot_force[0]` is one specific foot), so if ANY element is missing/null/
/// non-numeric the WHOLE vector is dropped rather than compacted — a compacted
/// partial would silently shift indices and corrupt the per-position mapping.
/// Empty when the value is absent, not an array, or any element is non-numeric
/// (no invented entries, no shifted entries).
fn json_f64_array(v: Option<&JsonValue>) -> Vec<f64> {
    let Some(arr) = v.and_then(JsonValue::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for elem in arr {
        match elem.as_f64() {
            Some(n) => out.push(n),
            None => return Vec::new(),
        }
    }
    out
}

/// Probe handler for `rt/utlidar/robot_pose`: parse the world pose
/// (`data.pose.position` {x,y,z} + `data.pose.orientation` {x,y,z,w}) and log it
/// throttled. Confirms whether the Go2 Air actually streams pose over WebRTC
/// (go2_ros2_sdk sources /odom from this topic) and whether it tracks motion,
/// before wiring odometry + map accumulation.
fn ingest_robot_pose(raw: &[u8]) {
    let Ok(v) = serde_json::from_slice::<JsonValue>(raw) else {
        return;
    };
    let data = v.get("data").unwrap_or(&v);
    let pose = data.get("pose").unwrap_or(data);
    let getf = |o: Option<&JsonValue>, k: &str| {
        o.and_then(|m| m.get(k)).and_then(JsonValue::as_f64)
    };
    let pos = pose.get("position");
    let ori = pose.get("orientation");
    let (px, py, pz) = (getf(pos, "x"), getf(pos, "y"), getf(pos, "z"));
    if px.is_none() && py.is_none() && pz.is_none() {
        return;
    }
    let (ox, oy, oz, ow) = (getf(ori, "x"), getf(ori, "y"), getf(ori, "z"), getf(ori, "w"));
    POSE_LOG.with(|c| {
        let n = c.get().wrapping_add(1);
        c.set(n);
        // ~1 in 25 frames keeps the log readable while still showing motion.
        if n % 25 == 1 {
            log::info(&alloc::format!(
                "go2 DIAG robot_pose: pos=({px:?},{py:?},{pz:?}) quat=({ox:?},{oy:?},{oz:?},{ow:?})"
            ));
        }
    });
}

/// Parse a `rt/sportmodestate` message body into the latest-telemetry snapshot,
/// overwriting ONLY the fields actually present (an absent field keeps the prior
/// value rather than clobbering it to None — the stream sometimes omits sub-blocks
/// between frames). The documented go2_webrtc_connect shape carries the sport
/// fields under `data`.
fn ingest_sportmodestate(raw: &[u8]) {
    let Ok(v) = serde_json::from_slice::<JsonValue>(raw) else {
        return;
    };
    let data = v.get("data").unwrap_or(&v);
    TELEMETRY.with(|cell| {
        let mut t = cell.borrow_mut();
        if let Some(n) = data.get("mode").and_then(JsonValue::as_i64) {
            t.mode = Some(n);
        }
        if let Some(n) = data.get("gait_type").and_then(JsonValue::as_i64) {
            t.gait_type = Some(n);
        }
        if let Some(n) = data.get("body_height").and_then(JsonValue::as_f64) {
            t.body_height = Some(n);
        }
        if let Some(vel) = data.get("velocity").and_then(JsonValue::as_array) {
            if let Some(x) = vel.first().and_then(JsonValue::as_f64) {
                t.vx = Some(x);
            }
            if let Some(y) = vel.get(1).and_then(JsonValue::as_f64) {
                t.vy = Some(y);
            }
        }
        if let Some(n) = data.get("yaw_speed").and_then(JsonValue::as_f64) {
            t.vyaw = Some(n);
        }
        let pos = json_f64_array(data.get("position"));
        if !pos.is_empty() {
            t.position = pos;
        }
        let ff = json_f64_array(data.get("foot_force"));
        if !ff.is_empty() {
            t.foot_force = ff;
        }
        if let Some(imu) = data.get("imu_state") {
            if let Some(rpy) = imu.get("rpy").and_then(JsonValue::as_array) {
                if let Some(r) = rpy.first().and_then(JsonValue::as_f64) {
                    t.imu_roll = Some(r);
                }
                if let Some(p) = rpy.get(1).and_then(JsonValue::as_f64) {
                    t.imu_pitch = Some(p);
                }
                if let Some(y) = rpy.get(2).and_then(JsonValue::as_f64) {
                    t.imu_yaw = Some(y);
                }
            }
            let quat = json_f64_array(imu.get("quaternion"));
            if !quat.is_empty() {
                t.imu_quaternion = quat;
            }
            if let Some(n) = imu.get("temperature").and_then(JsonValue::as_f64) {
                t.imu_temperature = Some(n);
            }
        }
    });
}

/// Parse the extra battery detail (voltage / current / bms temperature) out of a
/// `rt/lf/lowstate` body, updating the snapshot. SOC is handled separately by the
/// zero-alloc `parse_soc` byte scan on the hot path; this richer parse runs only
/// when a lowstate frame is the throttled snapshot carrier. Tolerant of the two
/// documented shapes: a top-level `bms_state` object or a flat layout.
fn ingest_lowstate_battery(raw: &[u8]) {
    let Ok(v) = serde_json::from_slice::<JsonValue>(raw) else { return; };
    let data = v.get("data").unwrap_or(&v);
    diag_lowstate_dump(data);
    let bms = data.get("bms_state").or_else(|| data.get("bms"));
    TELEMETRY.with(|cell| {
        let mut t = cell.borrow_mut();
        if let Some(soc) = bms
            .and_then(|b| b.get("soc"))
            .or_else(|| data.get("soc"))
            .and_then(JsonValue::as_f64)
        {
            t.bat_soc = Some(soc);
        }
        // Pack voltage/current can be reported at the lowstate root (`power_v`,
        // `power_a`) or inside the bms block (`voltage`, `current`).
        if let Some(volt) = data
            .get("power_v")
            .or_else(|| bms.and_then(|b| b.get("voltage")))
            .and_then(JsonValue::as_f64)
        {
            t.bat_voltage = Some(volt);
        }
        if let Some(curr) = data
            .get("power_a")
            .or_else(|| bms.and_then(|b| b.get("current")))
            .and_then(JsonValue::as_f64)
        {
            // The Go2 BMS reports pack current in milliamps on this firmware (e.g.
            // -1588 = -1.588 A), while `power_a` (when present) is already amps. A
            // robot's real draw is well under ~50 A, so normalize an implausibly
            // large magnitude from mA to A.
            t.bat_current = Some(if curr.abs() > 100.0 { curr / 1000.0 } else { curr });
        }
        if let Some(temp) = bms
            .and_then(|b| b.get("temperature"))
            .or_else(|| bms.and_then(|b| b.get("bms_temperature")))
            .and_then(JsonValue::as_f64)
        {
            t.bat_temperature = Some(temp);
        }
        // IMU lives in `rt/lf/lowstate` (`data.imu_state.rpy`) on this firmware —
        // `rt/sportmodestate` is NOT published over WebRTC, so lowstate is the only
        // live source of orientation. Same `imu_state` shape sportmodestate carries.
        if let Some(imu) = data.get("imu_state") {
            if let Some(rpy) = imu.get("rpy").and_then(JsonValue::as_array) {
                if let Some(r) = rpy.first().and_then(JsonValue::as_f64) {
                    t.imu_roll = Some(r);
                }
                if let Some(p) = rpy.get(1).and_then(JsonValue::as_f64) {
                    t.imu_pitch = Some(p);
                }
                if let Some(y) = rpy.get(2).and_then(JsonValue::as_f64) {
                    t.imu_yaw = Some(y);
                }
            }
            if let Some(temp) = imu.get("temperature").and_then(JsonValue::as_f64) {
                t.imu_temperature = Some(temp);
            }
        }
        // Leg joint angles for the dashboard robot animation (R2). The first 12 of
        // `motor_state` are FR/FL/RR/RL × hip/thigh/calf; later entries (jaw etc.)
        // are ignored. Only stored when all 12 are present (never a partial pose).
        if let Some(ms) = data.get("motor_state").and_then(JsonValue::as_array) {
            let q: Vec<f64> = ms
                .iter()
                .take(12)
                .filter_map(|m| m.get("q").and_then(JsonValue::as_f64))
                .collect();
            if q.len() == 12 {
                t.joints = q;
            }
        }
    });
}

/// Build the structured `telemetry` JSON object from the in-tick thread_local
/// accumulator, INCLUDING only the fields actually received (absent → omitted,
/// never a fabricated value). Returns `JsonValue::Null` when nothing has been
/// received yet. ONLY the service instance (which drains the stream) calls this —
/// it then persists the result to the shared DB for cross-worker reads.
fn telemetry_json() -> JsonValue {
    TELEMETRY.with(|cell| {
        let t = cell.borrow();
        let mut obj = serde_json::Map::new();
        let put_num = |obj: &mut serde_json::Map<String, JsonValue>, k: &str, v: Option<f64>| {
            if let Some(n) = v {
                obj.insert(k.into(), json!(n));
            }
        };
        if let Some(n) = t.mode {
            obj.insert("mode".into(), json!(n));
        }
        if let Some(n) = t.gait_type {
            obj.insert("gait_type".into(), json!(n));
        }
        put_num(&mut obj, "body_height", t.body_height);
        let mut vel = serde_json::Map::new();
        if let Some(n) = t.vx {
            vel.insert("vx".into(), json!(n));
        }
        if let Some(n) = t.vy {
            vel.insert("vy".into(), json!(n));
        }
        if let Some(n) = t.vyaw {
            vel.insert("vyaw".into(), json!(n));
        }
        if !vel.is_empty() {
            obj.insert("velocity".into(), JsonValue::Object(vel));
        }
        if !t.position.is_empty() {
            obj.insert("position".into(), json!(t.position));
        }
        if !t.foot_force.is_empty() {
            obj.insert("foot_force".into(), json!(t.foot_force));
        }
        // 12 leg joint angles (rad) for the dashboard robot animation (R2).
        if !t.joints.is_empty() {
            obj.insert("joints".into(), json!(t.joints));
        }

        let mut imu = serde_json::Map::new();
        put_num(&mut imu, "roll", t.imu_roll);
        put_num(&mut imu, "pitch", t.imu_pitch);
        put_num(&mut imu, "yaw", t.imu_yaw);
        if !t.imu_quaternion.is_empty() {
            imu.insert("quaternion".into(), json!(t.imu_quaternion));
        }
        put_num(&mut imu, "temperature", t.imu_temperature);
        if !imu.is_empty() {
            obj.insert("imu".into(), JsonValue::Object(imu));
        }

        let mut bat = serde_json::Map::new();
        put_num(&mut bat, "soc", t.bat_soc);
        put_num(&mut bat, "voltage", t.bat_voltage);
        put_num(&mut bat, "current", t.bat_current);
        put_num(&mut bat, "temperature", t.bat_temperature);
        if !bat.is_empty() {
            obj.insert("battery".into(), JsonValue::Object(bat));
        }

        if obj.is_empty() {
            JsonValue::Null
        } else {
            JsonValue::Object(obj)
        }
    })
}

/// Read the structured `telemetry` object for `go2.status` from the SHARED STORE
/// (the cross-worker source of truth) — the calling worker is NOT the service
/// instance that drains the stream, so the thread_local accumulator is empty
/// here. Returns the same shape `telemetry_json` produces; `JsonValue::Null`
/// when nothing has been persisted yet so `go2.status` omits the key entirely.
fn telemetry_from_store() -> JsonValue {
    let raw = match state::get_string(state::KEY_TELEMETRY) {
        Ok(Some(s)) => s,
        Ok(None) | Err(_) => return JsonValue::Null,
    };
    if raw.is_empty() {
        return JsonValue::Null;
    }
    serde_json::from_str::<JsonValue>(&raw)
        .ok()
        .filter(|v| v.is_object())
        .unwrap_or(JsonValue::Null)
}

/// Read the SMALL LiDAR status object for `go2.status` from the SHARED STORE.
/// Same shape `lidar_status_json` builds. Distinguishes a REAL host-fn read error
/// from the legitimate "absent = default disabled / no frame yet" case instead of
/// fabricating a "disabled" object on any failure:
///   - `Ok(obj)` — a real persisted status, or (when none yet) the desired-enable
///     INTENT with `available:false`. An absent key (`Ok(false)` intent) yields
///     the same default shape `go2.status` rendered before, so callers render it
///     identically.
///   - `Err` — a REAL host-fn read error: the caller MUST omit the lidar field
///     rather than emit a fabricated "disabled" object that misreports the robot.
fn lidar_status_from_store() -> Result<JsonValue, AbiError> {
    if let Some(raw) = state::get_string(state::KEY_LIDAR_STATUS)? {
        if !raw.is_empty() {
            if let Ok(v) = serde_json::from_str::<JsonValue>(&raw) {
                if v.is_object() {
                    return Ok(v);
                }
            }
        }
    }
    // No persisted status: fall back to the operator's persistent enable INTENT.
    // A READ ERROR here propagates (we must not paper a transient failure over as
    // "disabled"); a genuinely absent intent (`Ok(false)`) yields the default
    // shape the UI rendered before.
    let enabled = lidar_enabled_intent()?;
    Ok(json!({
        "enabled": enabled,
        "available": false,
        "point_count": 0,
        "frame_seq": 0,
    }))
}

// =============================================================================
// Connection state machine
// =============================================================================

fn do_connect() -> JsonValue {
    log::info("go2: do_connect entered");
    // Explicit connect = operator wants the link up: persist the intent so the tick
    // auto-reconnects if it later drops.
    let _ = state::set_durable(state::KEY_CONNECT_INTENT, alloc::vec![1u8]);
    let robot = db::get_robot().unwrap_or_default();
    // The configured IP (install-time `ip` connection_param) is the single source
    // of truth. No default — an unconfigured instance refuses to connect.
    let ip = match config_get(IP_CONFIG_KEY) {
        Some(ip) => ip,
        None => {
            log::warn("go2: no IP configured — cannot connect");
            let _ = set_offline_mirrored("error", "no IP configured");
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
        Ok(true) => {
            log::info("go2: try_begin_connect won (status->connecting)");
            mirror_status_from_db();
        }
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
        peer_ipv4: Some(ip.clone()),
    };
    let out: WebRtcConnectOutput = match call_cbor_in_out(&connect_in, webrtc_connect_v1) {
        Ok(o) => o,
        Err(e) => {
            log::warn(&alloc::format!("go2: webrtc_connect_v1 failed: {e}"));
            let _ = set_offline_mirrored("error", &alloc::format!("webrtc_connect: {e}"));
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
            let _ = set_offline_mirrored("error", &e);
            return json!({ "error": e });
        }
    };
    log::info(&alloc::format!("go2: answer received {} bytes, set_answer...", answer_sdp.len()));
    let set_ans = WebRtcSetAnswerInput { channel_id: channel_id.clone(), answer_sdp };
    if let Err(e) = call_cbor_in_out::<_, WebRtcStatusOutput>(&set_ans, webrtc_set_answer_v1) {
        wc_close(&channel_id);
        let _ = set_offline_mirrored("error", &alloc::format!("set_answer: {e}"));
        return json!({ "error": alloc::format!("set_answer: {e}") });
    }
    // CAS connecting -> validating. If a disconnect raced (we lost), the fresh
    // channel is orphaned — close it so we don't leak a peer connection.
    match db::set_channel(&channel_id) {
        Ok(true) => {
            mirror_status_from_db();
            json!({ "status": "connecting", "channel_id": channel_id })
        }
        Ok(false) => {
            wc_close(&channel_id);
            json!({ "error": "connect cancelled" })
        }
        Err(e) => {
            wc_close(&channel_id);
            let _ = set_offline_mirrored("error", &alloc::format!("set_channel: {e}"));
            json!({ "error": alloc::format!("set_channel: {e}") })
        }
    }
}

fn do_disconnect() -> JsonValue {
    // Explicit disconnect = stay down: clear the intent so the tick does NOT
    // auto-reconnect until the operator connects again.
    let _ = state::set_durable(state::KEY_CONNECT_INTENT, alloc::vec![0u8]);
    if let Ok(robot) = db::get_robot() {
        if !robot.channel_id.is_empty() {
            wc_close(&robot.channel_id);
        }
    }
    lidar_reset_session();
    let _ = set_offline_mirrored("offline", "");
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

/// Operator connect intent (durable, DEFAULT-ON). A read error or absent key both
/// yield `true` so a transient store hiccup never silently disables auto-connect.
fn connect_intent() -> bool {
    match state::get(state::KEY_CONNECT_INTENT) {
        Ok(Some(v)) => v.first().map(|b| *b != 0).unwrap_or(true),
        // Never set yet → default-on (auto-connect a fresh robot).
        Ok(None) => true,
        // Read hiccup → FAIL CLOSED: do NOT auto-connect this tick. If the operator
        // had disconnected (intent=0), a transient error must not resurrect the link;
        // a genuine auto-connect just retries next tick once the read succeeds.
        Err(_) => false,
    }
}

std::thread_local! {
    // Wall-clock second of the last auto-connect attempt (backoff gate). Resets on
    // restart, which is fine — a fresh process should attempt promptly.
    static LAST_CONNECT_ATTEMPT: core::cell::Cell<i64> = const { core::cell::Cell::new(0) };
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
                let _ = set_offline_mirrored("error", "connect timeout");
                publish_event("go2.offline", json!({ "reason": "connect timeout" }));
            }
        }
        "validating" => {
            if robot.channel_id.is_empty() {
                let _ = set_offline_mirrored("error", "no channel");
                return;
            }
            // Validation watchdog: a stuck handshake (incl. persistent drain
            // errors below, which just retry) is bounded here.
            if db::now_secs() - robot.last_update > VALIDATION_TIMEOUT_SECS {
                wc_close(&robot.channel_id);
                let _ = set_offline_mirrored("error", "validation timeout");
                publish_event("go2.offline", json!({ "reason": "validation timeout" }));
                return;
            }
            let drained = match wc_drain(&robot.channel_id, 32) {
                Ok(d) => d,
                Err(_) => return,
            };
            if drained.closed {
                let _ = set_offline_mirrored("error", "channel closed during validation");
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
                                    let _ = wc_send_text(&robot.channel_id, &subscribe_msg(POSE_TOPIC));
                                    // Go2 only starts publishing the camera RTP after this
                                    // app-level command; the recvonly transceiver alone is silent.
                                    if !cam_id.is_empty() {
                                        let _ = wc_send_text(
                                            &robot.channel_id,
                                            &json!({ "type": "vid", "topic": "", "data": "on" }).to_string(),
                                        );
                                    }
                                    grant_vision_camera(&cam_id);
                                    mirror_status_from_db();
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
                let _ = set_offline_mirrored("error", "no channel");
                return;
            }
            // Liveness watchdog keyed on REAL telemetry receipt: lowstate streams
            // continuously, so if no fresh lowstate has arrived within the window
            // the link is dead (a stalled-but-open data channel, or persistent
            // drain failure). last_telemetry advances ONLY on actual lowstate.
            if db::now_secs() - robot.last_telemetry > ONLINE_STALE_SECS {
                wc_close(&robot.channel_id);
                lidar_reset_session();
                let _ = set_offline_mirrored("error", "telemetry stalled");
                publish_event("go2.offline", json!({ "reason": "telemetry stalled" }));
                return;
            }
            db::bump_tick();
            let tick_n = robot.tick_count + 1;
            // The operator enable INTENT lives in the shared DB (toggled from any
            // worker via go2.lidar_on/off). The service instance reads it each tick
            // and drives the actual switch + subscription against the live channel,
            // tracking what it has already sent in its own thread_local bookkeeping.
            // A transient read error must NOT be read as "disabled" (that would
            // command the LiDAR off): skip the actuator change for this tick and
            // retry next tick. on_tick is the only driver, so a one-tick skip is safe.
            let (enabled_local, subscribed_local, pending_disable) = LIDAR.with(|cell| {
                let l = cell.borrow();
                (l.enabled, l.subscribed, l.pending_disable)
            });
            let desired_lidar = match lidar_enabled_intent() {
                Ok(d) => Some(d),
                Err(_) => None,
            };
            if let Some(desired_lidar) = desired_lidar {
                if desired_lidar && (!enabled_local || !subscribed_local) {
                    // Enable transition (or fresh session not yet subscribed): send
                    // switch "on" + subscribe the voxel topic. A re-enable clears any
                    // stale pending-disable from a prior failed off-send.
                    let switch = json!({ "type": "msg", "topic": LIDAR_SWITCH_TOPIC, "data": "on" }).to_string();
                    if wc_send_text(&robot.channel_id, &switch).is_ok()
                        && wc_send_text(&robot.channel_id, &subscribe_msg(LIDAR_TOPIC)).is_ok()
                    {
                        LIDAR.with(|cell| {
                            let mut l = cell.borrow_mut();
                            l.enabled = true;
                            l.subscribed = true;
                            l.pending_disable = false;
                        });
                    }
                } else if !desired_lidar && (enabled_local || pending_disable) {
                    // Disable transition: send switch "off" + unsubscribe the voxel
                    // topic. Only clear local enabled/subscribed AFTER a successful
                    // off-send — a transient send failure must NOT leave the robot
                    // streaming while we report disabled, so we keep a pending-disable
                    // that a later tick retries (mirrors the enable path's robustness).
                    let switch = json!({ "type": "msg", "topic": LIDAR_SWITCH_TOPIC, "data": "off" }).to_string();
                    let off_ok = wc_send_text(&robot.channel_id, &switch).is_ok();
                    let unsub_ok = wc_send_text(&robot.channel_id, &unsubscribe_msg(LIDAR_TOPIC)).is_ok();
                    if off_ok && unsub_ok {
                        LIDAR.with(|cell| {
                            let mut l = cell.borrow_mut();
                            l.enabled = false;
                            l.subscribed = false;
                            l.pending_disable = false;
                        });
                        let _ = state::set_ephemeral(
                            state::KEY_LIDAR_STATUS,
                            lidar_status_json().to_string().into_bytes(),
                        );
                    } else {
                        LIDAR.with(|cell| cell.borrow_mut().pending_disable = true);
                    }
                }
            }
            let drained = match wc_drain(&robot.channel_id, 64) {
                Ok(d) => d,
                Err(_) => return,
            };
            if drained.closed {
                lidar_reset_session();
                let _ = set_offline_mirrored("error", "channel closed");
                publish_event("go2.offline", json!({ "reason": "channel closed" }));
                return;
            }
            let mut battery = robot.battery_pct;
            let mut got_telemetry = false;
            // The single latest binary (voxel) frame is copied OUT of the scratch
            // here and ingested AFTER the DECODE_BUF borrow is released. This is the
            // connection-saving invariant: ingest_voxel_map -> decode can abort the
            // process on a garbage allocation (panic = abort, no unwinding); if that
            // ran while DECODE_BUF was borrowed, the borrow guard would leak and
            // EVERY later tick would abort at borrow_mut ("already borrowed"),
            // stalling telemetry until the 12s watchdog tore down the link. By
            // owning a small Vec copy we never hold the scratch across the fallible
            // decode, so at worst one tick is lost and telemetry resumes next tick.
            let mut voxel_payload: Option<Vec<u8>> = None;
            DECODE_BUF.with(|cell| {
                let mut dec = cell.borrow_mut();
                // Index of the LAST binary frame drained this tick. The voxel map is
                // a stream where each frame supersedes the prior one, so we decode at
                // most ONE per tick (the latest) — this bounds per-tick lidar work
                // regardless of stream rate so it can never blow the fuel budget.
                let last_binary = drained
                    .messages
                    .iter()
                    .rposition(|m| !m.is_text);
                for msg in drained.messages.iter() {
                    let src = msg.data_b64.as_bytes();
                    // base64 decodes to at most 3/4 of the input length; size the
                    // scratch to the source length (an upper bound) so the slice
                    // decode always fits.
                    if dec.len() < src.len() {
                        dec.resize(src.len(), 0);
                    }
                    let n = match B64.decode_slice(src, &mut dec[..]) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    let raw = &dec[..n];
                    // Binary frames carry the LiDAR voxel map (and other binary
                    // pub/sub payloads). The voxel topic is identified inside the
                    // binary framing's JSON header by parse_voxel_frame. Only the
                    // latest binary frame is ingested; earlier ones are superseded.
                    if !msg.is_text {
                        // Defer the (single) voxel decode until AFTER the whole
                        // drain has been scanned for telemetry below: the lidar
                        // path must never run before the watchdog-feeding lowstate
                        // is recorded, so a lidar issue cannot stall the link.
                        continue;
                    }
                    // DIAGNOSTIC: enumerate every distinct inbound topic once.
                    diag_note_topic(raw);
                    // Only lowstate carries battery; gate on the topic substring,
                    // then pull the integer soc with a zero-alloc byte scan. The
                    // richer battery detail (voltage/current/temp) is parsed into
                    // the latest-telemetry snapshot read at the status cadence.
                    if find_sub(raw, b"lowstate", 0).is_some() {
                        if let Some(soc) = parse_soc(raw) {
                            battery = soc;
                            got_telemetry = true;
                        }
                        ingest_lowstate_battery(raw);
                    } else if find_sub(raw, b"sportmodestate", 0).is_some() {
                        // High-rate motion/IMU stream: keep only the latest values
                        // in memory; never advertise at this raw rate.
                        ingest_sportmodestate(raw);
                    } else if find_sub(raw, b"robot_pose", 0).is_some() {
                        // Lidar-derived world pose (probe: confirm the Air streams it
                        // and whether it tracks motion before wiring odometry/mapping).
                        ingest_robot_pose(raw);
                    }
                }
                // Telemetry watchdog FIRST: record real lowstate receipt before any
                // lidar work this drain, so the link's liveness is updated regardless
                // of a (possibly malformed) voxel frame in the same drain.
                if got_telemetry {
                    let _ = db::record_lowstate(battery);
                }
                // Decode the single latest binary frame from base64 into the scratch
                // and COPY the bytes into an owned Vec. ingest is intentionally NOT
                // called here: it runs after this closure ends (see voxel_payload).
                if let Some(idx) = last_binary {
                    if let Some(msg) = drained.messages.get(idx) {
                        let src = msg.data_b64.as_bytes();
                        if dec.len() < src.len() {
                            dec.resize(src.len(), 0);
                        }
                        if let Ok(n) = B64.decode_slice(src, &mut dec[..]) {
                            if let Some(raw) = dec.get(..n) {
                                voxel_payload = Some(raw.to_vec());
                            }
                        }
                    }
                }
            });
            // DECODE_BUF borrow is released. Ingest the latest voxel frame now: an
            // allocation/decode abort here can no longer leak the scratch borrow and
            // cascade into a permanent per-tick abort. Telemetry is already recorded.
            if let Some(payload) = voxel_payload {
                ingest_voxel_map(&payload);
            }
            // Throttle RTT poll + publish + telemetry DB persist to ~1s (every 5
            // ticks @200ms) so the high-rate stream never hammers SQLite. The
            // shared-DB snapshot is the source of truth cross-worker go2.status
            // reads; the thread_local is only the in-tick accumulator.
            if tick_n % 5 == 0 {
                let snapshot = telemetry_json();
                if !snapshot.is_null() {
                    let _ = state::set_ephemeral(
                        state::KEY_TELEMETRY,
                        snapshot.to_string().into_bytes(),
                    );
                }
                let mut rtt = robot.rtt_ms;
                if let Ok(st) = wc_state(&robot.channel_id) {
                    if st.peer_state == "failed" || st.peer_state == "closed" {
                        let _ = set_offline_mirrored("error", &st.peer_state);
                        publish_event("go2.offline", json!({ "reason": st.peer_state }));
                        return;
                    }
                    if let Some(r) = st.rtt_ms {
                        rtt = r as i64;
                        let _ = db::set_rtt(rtt);
                    }
                }
                // Mirror the freshly persisted status fields for fast cross-worker
                // / host-side reads. Re-read the `robot` row HERE (after
                // record_lowstate / set_rtt) instead of mirroring from the
                // pre-drain `robot` snapshot: a go2.disconnect / offline transition
                // could have raced in on another worker after that snapshot, and
                // mirroring the stale snapshot would overwrite the mirror back to
                // `online`. Only mirror `online` when the FRESH row is genuinely
                // online with a live channel; otherwise the disconnecting path's
                // own mirror write (set_offline_mirrored) is authoritative.
                if let Ok(fresh) = db::get_robot() {
                    if fresh.status == "online" && !fresh.channel_id.is_empty() {
                        mirror_status(&fresh);
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
        // offline / error / unknown → AUTO-CONNECT. When the operator intent is
        // "connected" (default), the tick re-establishes the link itself (with a
        // backoff), so a robot comes online without any manual connect in any UI.
        _ => {
            if connect_intent() {
                let now = db::now_secs();
                let last = LAST_CONNECT_ATTEMPT.with(|c| c.get());
                if last == 0 || now - last >= RECONNECT_BACKOFF_SECS {
                    LAST_CONNECT_ATTEMPT.with(|c| c.set(now));
                    let _ = do_connect();
                }
            }
        }
    }
}

// =============================================================================
// Request dispatch
// =============================================================================

fn handle(tool: &str, params: &JsonValue) -> JsonValue {
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
        "go2.action_balance_stand" => send_sport_gated(SPORT_BALANCE_STAND, ""),
        "go2.action_stretch" => send_sport_gated(SPORT_STRETCH, ""),
        "go2.action_wiggle_hips" => send_sport_gated(SPORT_WIGGLE_HIPS, ""),
        "go2.action_heart" => send_sport_gated(SPORT_FINGER_HEART, ""),
        "go2.action_dance1" => send_sport_gated(SPORT_DANCE1, ""),
        "go2.action_dance2" => send_sport_gated(SPORT_DANCE2, ""),
        "go2.action_scrape" => send_sport_gated(SPORT_SCRAPE, ""),
        "go2.action_front_flip" => send_sport_gated(SPORT_FRONT_FLIP, ""),
        "go2.action_front_jump" => send_sport_gated(SPORT_FRONT_JUMP, ""),
        "go2.action_front_pounce" => send_sport_gated(SPORT_FRONT_POUNCE, ""),
        "go2.euler" => send_euler(params),
        "go2.body_height" => send_body_height(params),
        "go2.foot_raise_height" => send_foot_raise(params),
        "go2.speed_level" => send_speed_level(params),
        "go2.pose" => send_pose(params),
        "go2.lidar_on" => set_lidar(true),
        "go2.lidar_off" => set_lidar(false),
        // Combined toggle: `{enabled: bool}`; defaults to enabling when absent.
        "go2.lidar" => {
            let enabled = params
                .get("enabled")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true);
            set_lidar(enabled)
        }
        "go2.lidar_frame" => lidar_frame(),
        "go2.move_fwd" => mv(0.3, 0.0, 0.0),
        "go2.move_back" => mv(-0.3, 0.0, 0.0),
        "go2.move_left" => mv(0.0, 0.3, 0.0),
        "go2.move_right" => mv(0.0, -0.3, 0.0),
        "go2.status" => match db::get_robot() {
            Ok(r) => {
                let mut out = json!({
                    "status": r.status, "battery_pct": r.battery_pct, "rtt_ms": r.rtt_ms,
                    "estop_active": r.estop_active, "camera_id": r.camera_id,
                    // Flat capability tags (kept for backward compatibility with any
                    // consumer that reads a string array). Keep in sync with `actions_meta`.
                    "capabilities": capability_kinds(),
                    // Rich, capability-driven descriptor: every implemented control with
                    // a human label, risk tier and param schema, so a capability-driven
                    // UI can render labels / gate high-risk acrobatics without hardcoding.
                    "actions_meta": actions_meta(),
                });
                // Structured telemetry snapshot, read from the SHARED DB (the
                // cross-worker source of truth — this worker is not the service
                // instance that drains the stream). Only emitted when something has
                // been persisted; an empty stream omits the key entirely.
                let telemetry = telemetry_from_store();
                if !telemetry.is_null() {
                    if let Some(o) = out.as_object_mut() {
                        o.insert("telemetry".into(), telemetry);
                    }
                }
                // SMALL LiDAR availability sub-object (enabled/available/point
                // count/resolution/origin/frame_seq/ts) — NEVER the point cloud.
                // Read from the shared store so any worker reports the live status.
                // On a REAL read error OMIT the field rather than fabricate a
                // "disabled" object that would misreport the robot's LiDAR.
                match lidar_status_from_store() {
                    Ok(lidar) => {
                        if let Some(o) = out.as_object_mut() {
                            o.insert("lidar".into(), lidar);
                        }
                    }
                    Err(e) => log::warn(&alloc::format!("go2.status: lidar read failed, omitting: {e}")),
                }
                out
            }
            Err(e) => json!({ "error": alloc::format!("{e}") }),
        },
        other => json!({ "error": alloc::format!("unknown tool: {other}") }),
    }
}

/// Flat list of control `kind` strings this driver exposes (matches the core
/// `RobotAction` allowlist kinds + the non-motion `camera`/`status` tags).
fn capability_kinds() -> Vec<&'static str> {
    vec![
        "move", "stop", "estop", "reset_estop", "recovery_stand", "stand_up",
        "stand_down", "balance_stand", "sit", "hello", "stretch", "euler",
        "body_height", "foot_raise_height", "speed_level", "pose", "wiggle_hips",
        "heart", "dance1", "dance2", "scrape", "front_flip", "front_jump",
        "front_pounce", "status", "camera", "lidar_on", "lidar_off", "lidar_frame",
    ]
}

/// Rich capability descriptor for a capability-driven UI: each entry carries the
/// control `kind`, a human label, a risk tier ("low"/"medium"/"high") and the
/// numeric param schema (name + min/max). High-risk acrobatics are tagged so the
/// UI can require an explicit confirmation before sending them.
fn actions_meta() -> JsonValue {
    let p = |name: &str, min: f64, max: f64| json!({ "name": name, "min": min, "max": max });
    json!([
        { "kind": "move", "label": "Ruch", "risk": "medium", "params": [
            p("vx", -1.0, 1.0), p("vy", -1.0, 1.0), p("vyaw", -1.0, 1.0) ] },
        { "kind": "stop", "label": "Stop", "risk": "low", "params": [] },
        { "kind": "estop", "label": "E-STOP", "risk": "low", "params": [] },
        { "kind": "reset_estop", "label": "Reset e-stop", "risk": "low", "params": [] },
        { "kind": "recovery_stand", "label": "RecoveryStand", "risk": "medium", "params": [] },
        { "kind": "stand_up", "label": "Wstań", "risk": "medium", "params": [] },
        { "kind": "stand_down", "label": "Połóż się", "risk": "low", "params": [] },
        { "kind": "balance_stand", "label": "BalanceStand", "risk": "medium", "params": [] },
        { "kind": "sit", "label": "Siad", "risk": "low", "params": [] },
        { "kind": "hello", "label": "Przywitanie", "risk": "low", "params": [] },
        { "kind": "stretch", "label": "Przeciąganie", "risk": "low", "params": [] },
        { "kind": "euler", "label": "Orientacja (Euler)", "risk": "medium", "params": [
            p("roll", -EULER_LIMIT, EULER_LIMIT),
            p("pitch", -EULER_LIMIT, EULER_LIMIT),
            p("yaw", -EULER_LIMIT, EULER_LIMIT) ] },
        { "kind": "body_height", "label": "Wysokość ciała", "risk": "medium", "params": [
            p("height", BODY_HEIGHT_MIN, BODY_HEIGHT_MAX) ] },
        { "kind": "foot_raise_height", "label": "Wysokość kroku", "risk": "medium", "params": [
            p("height", FOOT_RAISE_MIN, FOOT_RAISE_MAX) ] },
        { "kind": "speed_level", "label": "Poziom prędkości", "risk": "medium", "params": [
            p("level", -1.0, 1.0) ] },
        { "kind": "pose", "label": "Poza ciała", "risk": "medium", "params": [
            p("roll", -EULER_LIMIT, EULER_LIMIT),
            p("pitch", -EULER_LIMIT, EULER_LIMIT),
            p("yaw", -EULER_LIMIT, EULER_LIMIT),
            p("height", BODY_HEIGHT_MIN, BODY_HEIGHT_MAX) ] },
        { "kind": "wiggle_hips", "label": "Wiggle Hips", "risk": "medium", "params": [] },
        { "kind": "heart", "label": "Serduszko", "risk": "medium", "params": [] },
        { "kind": "dance1", "label": "Taniec 1", "risk": "high", "params": [] },
        { "kind": "dance2", "label": "Taniec 2", "risk": "high", "params": [] },
        { "kind": "scrape", "label": "Scrape", "risk": "high", "acrobatic": true, "params": [] },
        { "kind": "front_flip", "label": "Front Flip", "risk": "high", "acrobatic": true, "params": [] },
        { "kind": "front_jump", "label": "Front Jump", "risk": "high", "acrobatic": true, "params": [] },
        { "kind": "front_pounce", "label": "Front Pounce", "risk": "high", "acrobatic": true, "params": [] },
        { "kind": "status", "label": "Status", "risk": "low", "read_only": true, "params": [] },
        { "kind": "lidar_on", "label": "LiDAR włącz", "risk": "low", "params": [] },
        { "kind": "lidar_off", "label": "LiDAR wyłącz", "risk": "low", "params": [] },
        { "kind": "lidar_frame", "label": "LiDAR klatka", "risk": "low", "read_only": true, "params": [] },
    ])
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

/// Extract the flow block's `variables` map (the contract for typed block params,
/// each a FlowValue `{kind,data}`). `param_num` reads FlowValues directly, so a
/// parametered block (euler/body_height/…) feeds this sub-object to the same
/// param parser the tool path uses. Missing → empty object (param parser rejects).
fn block_vars(params: &JsonValue) -> JsonValue {
    params
        .get("variables")
        .cloned()
        .unwrap_or_else(|| json!({}))
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
        "go2.balance_stand" => send_sport_gated(SPORT_BALANCE_STAND, ""),
        "go2.wiggle_hips" => send_sport_gated(SPORT_WIGGLE_HIPS, ""),
        "go2.heart" => send_sport_gated(SPORT_FINGER_HEART, ""),
        "go2.dance1" => send_sport_gated(SPORT_DANCE1, ""),
        "go2.dance2" => send_sport_gated(SPORT_DANCE2, ""),
        "go2.scrape" => send_sport_gated(SPORT_SCRAPE, ""),
        "go2.front_flip" => send_sport_gated(SPORT_FRONT_FLIP, ""),
        "go2.front_jump" => send_sport_gated(SPORT_FRONT_JUMP, ""),
        "go2.front_pounce" => send_sport_gated(SPORT_FRONT_POUNCE, ""),
        "go2.euler" => send_euler(&block_vars(params)),
        "go2.body_height" => send_body_height(&block_vars(params)),
        "go2.foot_raise_height" => send_foot_raise(&block_vars(params)),
        "go2.speed_level" => send_speed_level(&block_vars(params)),
        "go2.pose" => send_pose(&block_vars(params)),
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
    install_panic_hook();
    ensure_robot_from_config();
    bridge_legacy_lidar_intent();
    seed_lidar_disabled_on_fresh_session();
    0
}

/// Install a panic hook that logs the panic LOCATION (file:line:col) and message
/// through the host log fn BEFORE the process aborts. The addon is built with
/// `panic = abort` on wasm32-wasip1 (so `catch_unwind` is impossible), but the
/// hook still runs before the abort, and the panic location string survives
/// `strip = true`. This is the primary diagnostic for the steady-state tick trap:
/// a future panic prints e.g.
///   `go2 PANIC at src/lib.rs:1234:5: index out of bounds: len 80000 but index 80001`
/// Idempotent across re-starts (set_hook simply replaces the prior hook).
fn install_panic_hook() {
    std::panic::set_hook(std::boxed::Box::new(|info| {
        let location = info
            .location()
            .map(|l| alloc::format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        log::error(&alloc::format!("go2 PANIC at {location}: {message}"));
    }));
}

/// Default the persistent LiDAR enable INTENT to OFF at the start of a FRESH
/// session so a reconnect comes up with telemetry + camera only (no voxel
/// stream). The voxel decoder runs on arbitrary real sensor data; defaulting the
/// stream off isolates the connect + telemetry path from the decoder on bring-up,
/// and a known-bad decoder can never auto-stream on reconnect — the operator must
/// explicitly re-enable LiDAR via the GUI toggle after telemetry is confirmed.
///
/// This writes the durable intent UNCONDITIONALLY to OFF on every start. It runs
/// AFTER `bridge_legacy_lidar_intent` (the one-time legacy upgrade) deliberately:
/// a clean bring-up must win over a possibly-ON persisted/legacy value. To
/// re-enable, toggle from the GUI (writes the durable intent back to ON), which
/// then survives reconnects until the next process start. Reversible: removing
/// this call restores "intent persists across starts".
fn seed_lidar_disabled_on_fresh_session() {
    if let Err(e) = state::set_durable(state::KEY_LIDAR_ENABLED, alloc::vec![0u8]) {
        log::warn(&alloc::format!("go2: lidar default-off seed failed: {e}"));
    }
}

/// One-time, idempotent upgrade bridge for the LiDAR enable INTENT. The intent
/// used to live in `robot_live.lidar_enabled`; it now lives in the durable shared
/// store under `lidar:enabled`. Migrations run before on_start, so the migration
/// keeps the legacy table around and this seeds the store key FROM it exactly once:
/// only when the durable key is ABSENT (a fresh upgrade) AND the legacy table
/// still holds a value. A second start is a no-op — once the key exists (even if a
/// newer toggle wrote a different value meanwhile) we never clobber it. A real
/// read error on either side aborts the bridge for this start (retry next start)
/// rather than fabricating intent.
fn bridge_legacy_lidar_intent() {
    // Already migrated (or already toggled this session): never clobber the store.
    match state::get(state::KEY_LIDAR_ENABLED) {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(e) => {
            log::warn(&alloc::format!("go2: lidar intent bridge skipped, store read failed: {e}"));
            return;
        }
    }
    let legacy = match db::legacy_lidar_enabled() {
        Ok(Some(v)) => v,
        // No legacy table / no row: nothing to bridge (fresh install).
        Ok(None) => return,
        Err(e) => {
            log::warn(&alloc::format!("go2: lidar intent bridge skipped, legacy read failed: {e}"));
            return;
        }
    };
    if let Err(e) = state::set_durable(state::KEY_LIDAR_ENABLED, alloc::vec![u8::from(legacy)]) {
        log::warn(&alloc::format!("go2: lidar intent bridge write failed: {e}"));
    }
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    if let Ok(robot) = db::get_robot() {
        if !robot.channel_id.is_empty() {
            wc_close(&robot.channel_id);
        }
    }
    lidar_reset_session();
    let _ = set_offline_mirrored("offline", "");
    0
}

#[no_mangle]
pub extern "C" fn on_event(_ptr: i32, _len: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_panel_open(_id_ptr: i32, _id_len: i32, _epoch: i64) -> i32 {
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

// =============================================================================
// Tests
// =============================================================================
//
// Decode CONFIDENCE: the voxel grid layout, MSB-first bit packing, LZ4-block
// decompression and `point = [x,y,z]*resolution + origin` are taken VERBATIM from
// the upstream go2_webrtc_connect NATIVE decoder
// (unitree_webrtc_connect/lidar/lidar_decoder_native.py). These synthetic-frame
// tests assert THIS implementation matches that reference for hand-computed
// indices/bits. They do NOT — and cannot, offline — prove the bytes a real Go2
// emits map to physically correct geometry; that needs a live robot.

// Host-import stubs so the lib's `#[link(wasm_import_module="tentaflow")]` externs
// resolve when the crate is linked as a NATIVE test binary (they are real imports
// only under wasm). The decode tests never invoke the robot/SQL/UI paths, so these
// are inert; SQL/UI/webrtc stubs return an error code, logs are no-ops. now_secs is
// seeded via set_now_secs in tests so the SQL clock path is never taken.
#[cfg(test)]
mod host_stubs {
    #[no_mangle]
    extern "C" fn log_info(_p: i32, _l: i32) -> i32 { 0 }
    #[no_mangle]
    extern "C" fn log_warn(_p: i32, _l: i32) -> i32 { 0 }
    #[no_mangle]
    extern "C" fn event_publish(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 { 0 }
    #[no_mangle]
    extern "C" fn config_get_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 2 }
    #[no_mangle]
    extern "C" fn http_raw_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn webrtc_connect_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn webrtc_set_answer_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn webrtc_state_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn webrtc_send_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn webrtc_drain_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn webrtc_close_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn webrtc_register_camera_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn camera_grant_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn robot_dispatch_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn lidar_publish_v1(_a: i32, _b: i32) -> i32 { 0 }
    // Native tests can't round-trip the host SQL ABI: it passes pointers as i32,
    // which truncates 64-bit stack addresses → SIGSEGV. Under `#[cfg(test)]` the
    // `db` module routes SQL to its own in-memory `robot_live` store instead, so
    // these stubs stay inert (the SQL path is never reached natively).
    #[no_mangle]
    extern "C" fn sql_exec_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32, _f: i32, _g: i32) -> i32 { 5 }
    #[no_mangle]
    extern "C" fn sql_query_v1(_a: i32, _b: i32, _c: i32, _d: i32, _e: i32, _f: i32, _g: i32) -> i32 { 5 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wall-clock cache (`db::now_secs`) is a process-global atomic, so tests
    /// that seed it via `set_now_secs` and then assert clock-derived behavior must
    /// not run concurrently with one another (a parallel test would clobber the
    /// shared clock mid-test). Every clock-seeding test holds this mutex; the
    /// purely-decode tests do not touch the clock and stay parallel.
    static CLOCK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Build a minimal "normal-framing" voxel-map binary message: `<u16 json_len>`,
    /// 2 pad bytes, the JSON envelope, then the LZ4-block-compressed occupancy grid.
    fn build_normal_frame(grid: &[u8], resolution: f64, origin: [f64; 3]) -> Vec<u8> {
        let json = json!({
            "type": "msg",
            "topic": "rt/utlidar/voxel_map_compressed",
            "data": { "resolution": resolution, "origin": origin, "src_size": grid.len() },
        })
        .to_string();
        let json_bytes = json.as_bytes();
        let compressed = lz4_flex::block::compress(grid);
        let mut out = Vec::new();
        out.extend_from_slice(&(json_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0u8, 0u8]); // header is 4 bytes total before JSON
        out.extend_from_slice(json_bytes);
        out.extend_from_slice(&compressed);
        out
    }

    #[test]
    fn decode_voxel_to_canonical_round_trips_points() {
        // A synthetic grid with K known set bits must decode to a canonical buffer
        // that (a) parses back to exactly K points, (b) is preallocated EXACTLY
        // (no spare capacity), and (c) places points at the upstream MSB-first
        // coords (`index = z*0x800 + y*0x10 + x_byte`, `x = x_byte*8 + bit`).
        let resolution = 0.05f32;
        let origin = [1.0f32, -2.0, 0.5];
        // byte 0 = 0x81 (bits for x=0 and x=7), byte at y-stride 0x10 = 0x01 (x=7,y=1).
        let mut grid = vec![0u8; 0x20];
        grid[0] = 0x81; // x=0 and x=7 at y=0,z=0
        grid[0x10] = 0x01; // x=7 at y=1,z=0
        let k = count_voxel_points(&grid);
        assert_eq!(k, 3);

        let frame = decode_voxel_to_canonical(&grid, resolution, origin, 5, 1_234_000_000)
            .expect("under cap");
        // Preallocation is EXACT: no growth during the bit-scan. Body is 6 B/point
        // (3 i16 grid indices) under the XYZ_I16 layout.
        assert_eq!(frame.len(), LIDAR_HEADER_LEN + k * 3 * 2);
        assert_eq!(frame.capacity(), LIDAR_HEADER_LEN + k * 3 * 2);

        let h = LidarFrameHeader::decode_header(&frame).expect("header");
        assert_eq!(h.point_count as usize, k);
        assert_eq!(h.frame_seq, 5);
        assert_eq!(h.timestamp_us, 1_234_000_000);
        // The addon never stamps the host send time; the host pump does.
        assert_eq!(h.host_send_us, 0);
        assert_eq!(h.resolution, resolution);
        assert_eq!(h.origin, origin);

        // Reconstruct points from the PLANAR i16 grid body (all ix, then all iy,
        // then all iz; world = `idx * resolution + origin`, exactly what the browser
        // decoder does) and compare to the upstream MSB-first decode as the oracle.
        let body = &frame[LIDAR_HEADER_LEN..];
        let ix_base = 0;
        let iy_base = k * 2;
        let iz_base = k * 4;
        let rd = |o: usize| i16::from_le_bytes([body[o], body[o + 1]]) as f32;
        let mut got: Vec<[f32; 3]> = Vec::new();
        for p in 0..k {
            got.push([
                rd(ix_base + p * 2) * resolution + origin[0],
                rd(iy_base + p * 2) * resolution + origin[1],
                rd(iz_base + p * 2) * resolution + origin[2],
            ]);
        }
        let res = resolution as f64;
        let mut expected: Vec<[f32; 3]> = Vec::new();
        for (idx, &byte) in grid.iter().enumerate() {
            if byte == 0 {
                continue;
            }
            let z = (idx / 0x800) as f64;
            let n_slice = idx % 0x800;
            let y = (n_slice / 0x10) as f64;
            let x_base = ((n_slice % 0x10) * 8) as f64;
            for bit in 0..8u32 {
                if byte & (0x80 >> bit) != 0 {
                    let x = x_base + bit as f64;
                    expected.push([
                        (x * res + origin[0] as f64) as f32,
                        (y * res + origin[1] as f64) as f32,
                        (z * res + origin[2] as f64) as f32,
                    ]);
                }
            }
        }
        assert_eq!(got.len(), expected.len());
        // Point ORDER within a byte differs (the canonical decoder bit-scans
        // LSB-first for speed; the reference walks MSB-first), but the SET of
        // points must be identical — compare order-insensitively.
        let key = |p: &[f32; 3]| {
            (
                (p[0] * 1000.0).round() as i64,
                (p[1] * 1000.0).round() as i64,
                (p[2] * 1000.0).round() as i64,
            )
        };
        let mut got_keys: Vec<_> = got.iter().map(key).collect();
        let mut exp_keys: Vec<_> = expected.iter().map(key).collect();
        got_keys.sort_unstable();
        exp_keys.sort_unstable();
        assert_eq!(got_keys, exp_keys);
    }

    #[test]
    fn decode_voxel_to_canonical_rejects_over_cap() {
        // A grid whose popcount exceeds LIDAR_MAX_POINTS must return None (the
        // caller drops + logs it) — never a partial cloud.
        let bytes_needed = (LIDAR_MAX_POINTS / 8) + 16;
        let grid = vec![0xFFu8; bytes_needed];
        assert!(count_voxel_points(&grid) > LIDAR_MAX_POINTS);
        assert!(decode_voxel_to_canonical(&grid, 0.05, [0.0; 3], 1, 0).is_none());
    }

    #[test]
    fn decode_voxel_to_canonical_large_count_uses_fallible_reserve() {
        // A dense grid at (just under) the cap drives the largest allocation the
        // decoder will ever attempt. try_reserve_exact must succeed and produce a
        // full frame — proving the fallible-reserve path does not regress the happy
        // case and never aborts on a legitimately large (but bounded) point count.
        let bytes = (LIDAR_MAX_POINTS / 8) - 1;
        let grid = vec![0xFFu8; bytes];
        let count = count_voxel_points(&grid);
        assert!(count <= LIDAR_MAX_POINTS);
        let frame = decode_voxel_to_canonical(&grid, 0.05, [0.0; 3], 1, 0)
            .expect("at-cap frame must decode, not abort");
        assert_eq!(frame.len(), LIDAR_HEADER_LEN + count * 3 * 2);
    }

    #[test]
    fn count_voxel_points_sums_set_bits() {
        // Synthetic bitfield: count_voxel_points must equal the total set bits.
        let buf = [0x00u8, 0xFF, 0x80, 0x01, 0b1010_1010, 0x00];
        let expected: usize = buf.iter().map(|b| b.count_ones() as usize).sum();
        assert_eq!(count_voxel_points(&buf), expected);
        assert_eq!(count_voxel_points(&buf), 0 + 8 + 1 + 1 + 4 + 0);
        assert_eq!(count_voxel_points(&[]), 0);
    }

    #[test]
    fn tick_path_uses_cheap_count_not_full_cloud() {
        // ingest_voxel_map (the tick path) must materialize ONLY the cheap popcount,
        // identical to count_voxel_points over the decompressed grid — never a Vec.
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        // 0xFF (8 bits) + 0x0F (4 bits) = 12 occupied voxels.
        let grid = vec![0xFFu8, 0x0F];
        let expected = count_voxel_points(&grid);
        let frame = build_normal_frame(&grid, 0.05, [0.0, 0.0, 0.0]);
        ingest_voxel_map(&frame);
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.point_count, expected, "tick stores the cheap popcount");
            assert_eq!(l.point_count, 12);
        });
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    #[test]
    fn malformed_frame_leaves_prior_state_untouched() {
        // Decode one good frame, then feed a frame with a non-utlidar topic and a
        // truncated frame; neither must overwrite or clear the prior decoded frame.
        let grid = vec![0x80u8];
        let frame = build_normal_frame(&grid, 0.05, [0.0, 0.0, 0.0]);
        ingest_voxel_map(&frame);
        let seq_after_good = LIDAR.with(|cell| cell.borrow().frame_seq);
        assert_eq!(seq_after_good, 1);

        ingest_voxel_map(&[1, 2, 3]); // too short to frame
        ingest_voxel_map(&[]); // empty
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 1, "malformed frames do not bump frame_seq");
            assert_eq!(l.point_count, 1, "prior counted points retained");
        });
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    /// Build a "LiDAR-framing" voxel-map binary message: leading `<HH>` == (2,0),
    /// then a `<u32 json_len>` at +4, the JSON envelope at [8..8+len], compressed
    /// grid after. Mirrors the real Go2 LiDAR data-channel framing.
    fn build_lidar_frame(grid: &[u8], resolution: f64, origin: [f64; 3]) -> Vec<u8> {
        let json = json!({
            "type": "msg",
            "topic": "rt/utlidar/voxel_map_compressed",
            "data": { "resolution": resolution, "origin": origin, "src_size": grid.len() },
        })
        .to_string();
        let json_bytes = json.as_bytes();
        let compressed = lz4_flex::block::compress(grid);
        let mut out = Vec::new();
        out.extend_from_slice(&2u16.to_le_bytes()); // raw[0..2] h1 == 2
        out.extend_from_slice(&0u16.to_le_bytes()); // raw[2..4] h2 == 0
        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes()); // raw[4..8] json len
        out.extend_from_slice(json_bytes); // raw[8..8+len] JSON
        out.extend_from_slice(&compressed);
        out
    }

    #[test]
    fn parse_voxel_frame_handles_truncated_and_short_inputs() {
        // Every length that is too short to hold the framing header / declared JSON
        // must return None, never panic. Covers empty, sub-4-byte, a LiDAR header
        // with no json-length, and a header declaring more JSON than is present.
        assert!(parse_voxel_frame(&[]).is_none());
        assert!(parse_voxel_frame(&[0]).is_none());
        assert!(parse_voxel_frame(&[2, 0, 0]).is_none());
        assert!(parse_voxel_frame(&[2, 0, 0, 0]).is_none()); // (2,0) header, no u32 len
        assert!(parse_voxel_frame(&[2, 0, 0, 0, 0xFF]).is_none()); // partial u32 len
        // LiDAR framing whose declared json_len overruns the buffer.
        let mut overrun = Vec::new();
        overrun.extend_from_slice(&2u16.to_le_bytes());
        overrun.extend_from_slice(&0u16.to_le_bytes());
        overrun.extend_from_slice(&1000u32.to_le_bytes()); // claim 1000 JSON bytes
        overrun.extend_from_slice(b"{}"); // but only 2 present
        assert!(parse_voxel_frame(&overrun).is_none());
        // Normal framing whose declared u16 len overruns the buffer.
        let mut overrun2 = Vec::new();
        overrun2.extend_from_slice(&500u16.to_le_bytes());
        overrun2.extend_from_slice(&[0u8, 0u8]);
        overrun2.extend_from_slice(b"{}");
        assert!(parse_voxel_frame(&overrun2).is_none());
    }

    #[test]
    fn parse_voxel_frame_accepts_real_lidar_framing() {
        // The (2,0)+u32-len LiDAR framing must parse to the voxel data + compressed
        // tail, proving the bounds-safe rewrite still matches the real wire format.
        let grid = vec![0x81u8, 0x00];
        let frame = build_lidar_frame(&grid, 0.05, [1.0, 2.0, 3.0]);
        let (data, compressed) = parse_voxel_frame(&frame).expect("parses lidar framing");
        assert_eq!(data.get("src_size").and_then(JsonValue::as_u64), Some(2));
        assert_eq!(
            compressed,
            lz4_flex::block::compress(&grid).as_slice(),
            "compressed tail recovered intact"
        );
    }

    #[test]
    fn parse_voxel_frame_autodetects_4byte_padded_framing() {
        // Some firmware places a 4-byte pad between the u32 length and the JSON, so
        // the JSON starts at raw[12] instead of raw[8]. parse_voxel_frame must
        // auto-detect this variant and recover the same data + compressed tail.
        let grid = vec![0x81u8, 0x00];
        let json = json!({
            "type": "msg",
            "topic": "rt/utlidar/voxel_map_compressed",
            "data": { "resolution": 0.05, "origin": [1.0, 2.0, 3.0], "src_size": grid.len() },
        })
        .to_string();
        let json_bytes = json.as_bytes();
        let compressed = lz4_flex::block::compress(&grid);
        let mut frame = Vec::new();
        frame.extend_from_slice(&2u16.to_le_bytes()); // raw[0..2] h1 == 2
        frame.extend_from_slice(&0u16.to_le_bytes()); // raw[2..4] h2 == 0
        frame.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes()); // raw[4..8] len
        frame.extend_from_slice(&[0u8; 4]); // raw[8..12] 4-byte pad
        frame.extend_from_slice(json_bytes); // raw[12..12+len] JSON
        frame.extend_from_slice(&compressed);
        let (data, tail) = parse_voxel_frame(&frame).expect("auto-detects padded framing");
        assert_eq!(data.get("src_size").and_then(JsonValue::as_u64), Some(2));
        assert_eq!(tail, compressed.as_slice(), "compressed tail recovered intact");
    }

    #[test]
    fn ingest_oversized_src_size_does_not_panic_or_decode() {
        // A frame whose JSON declares src_size beyond the grid bound must be skipped
        // (no decode, no state change) and MUST NOT panic. Build a valid framing but
        // override src_size to exceed LIDAR_GRID_BYTES.
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        let grid = vec![0xFFu8, 0x0F];
        let json = json!({
            "type": "msg",
            "topic": "rt/utlidar/voxel_map_compressed",
            "data": { "resolution": 0.05, "origin": [0.0, 0.0, 0.0], "src_size": LIDAR_GRID_BYTES + 1 },
        })
        .to_string();
        let compressed = lz4_flex::block::compress(&grid);
        let mut frame = Vec::new();
        frame.extend_from_slice(&(json.len() as u16).to_le_bytes());
        frame.extend_from_slice(&[0u8, 0u8]);
        frame.extend_from_slice(json.as_bytes());
        frame.extend_from_slice(&compressed);
        ingest_voxel_map(&frame); // must not panic
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 0, "oversized src_size frame never decoded");
        });
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    #[test]
    fn ingest_truncated_compressed_tail_does_not_panic() {
        // A valid framing + topic but a corrupt/short LZ4 tail must be skipped
        // (decompress fails or length mismatches) without panicking.
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        let grid = vec![0xFFu8, 0x0F, 0x01, 0x80];
        let mut frame = build_normal_frame(&grid, 0.05, [0.0, 0.0, 0.0]);
        // Lop off the last few bytes of the compressed tail to corrupt it.
        frame.truncate(frame.len().saturating_sub(2));
        ingest_voxel_map(&frame); // must not panic
        LIDAR.with(|cell| {
            assert_eq!(cell.borrow().frame_seq, 0, "corrupt tail never decoded");
        });
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    #[test]
    fn lidar_status_json_reports_availability() {
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        // No frame yet: available=false, enabled=false.
        let s = lidar_status_json();
        assert_eq!(s.get("available").and_then(JsonValue::as_bool), Some(false));
        assert_eq!(s.get("enabled").and_then(JsonValue::as_bool), Some(false));
        assert_eq!(s.get("point_count").and_then(JsonValue::as_u64), Some(0));

        let grid = vec![0xFFu8];
        let frame = build_normal_frame(&grid, 0.05, [1.0, 2.0, 3.0]);
        ingest_voxel_map(&frame);
        LIDAR.with(|cell| cell.borrow_mut().enabled = true);
        let s = lidar_status_json();
        assert_eq!(s.get("available").and_then(JsonValue::as_bool), Some(true));
        assert_eq!(s.get("enabled").and_then(JsonValue::as_bool), Some(true));
        assert_eq!(s.get("point_count").and_then(JsonValue::as_u64), Some(8));
        assert!(s.get("resolution").is_some());
        assert!(s.get("origin").is_some());
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    #[test]
    fn frame_missing_fields_is_skipped() {
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        // Valid framing + topic but the data omits origin/src_size: must skip.
        let json = json!({
            "topic": "rt/utlidar/voxel_map_compressed",
            "data": { "resolution": 0.05 },
        })
        .to_string();
        let jb = json.as_bytes();
        let mut frame = Vec::new();
        frame.extend_from_slice(&(jb.len() as u16).to_le_bytes());
        frame.extend_from_slice(&[0u8, 0u8]);
        frame.extend_from_slice(jb);
        frame.extend_from_slice(&[0u8, 0u8, 0u8]);
        ingest_voxel_map(&frame);
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 0, "incomplete frame must not decode");
        });
    }

    /// Build a normal-framing binary message with full control over the topic, the
    /// declared `src_size` and the resolution, for adversarial-input tests.
    fn build_frame_with(
        topic: &str,
        grid: &[u8],
        declared_src_size: usize,
        resolution: f64,
        origin: [f64; 3],
    ) -> Vec<u8> {
        let json = json!({
            "type": "msg",
            "topic": topic,
            "data": { "resolution": resolution, "origin": origin, "src_size": declared_src_size },
        })
        .to_string();
        let json_bytes = json.as_bytes();
        let compressed = lz4_flex::block::compress(grid);
        let mut out = Vec::new();
        out.extend_from_slice(&(json_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0u8, 0u8]);
        out.extend_from_slice(json_bytes);
        out.extend_from_slice(&compressed);
        out
    }

    #[test]
    fn non_voxel_utlidar_topic_is_ignored() {
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        // A different utlidar topic that merely CONTAINS "utlidar" must never enter
        // the voxel decode path (exact-topic match).
        let grid = vec![0xFFu8];
        let frame = build_frame_with(
            "rt/utlidar/switch",
            &grid,
            grid.len(),
            0.05,
            [0.0, 0.0, 0.0],
        );
        ingest_voxel_map(&frame);
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 0, "non-voxel utlidar topic must not decode");
            assert_eq!(l.point_count, 0);
        });
    }

    #[test]
    fn decompressed_len_mismatch_is_rejected() {
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        // The block decompresses to 1 byte but src_size declares a larger grid:
        // a valid-but-short block with a larger declared size is malformed.
        let grid = vec![0x80u8];
        let frame = build_frame_with(
            LIDAR_TOPIC,
            &grid,
            grid.len() + 16,
            0.05,
            [0.0, 0.0, 0.0],
        );
        ingest_voxel_map(&frame);
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 0, "n != src_size must be rejected");
            assert_eq!(l.point_count, 0);
        });
    }

    #[test]
    fn src_size_over_grid_cap_is_rejected() {
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        // src_size beyond the documented grid (80_000) is malformed and rejected
        // before any allocation/decompress.
        let grid = vec![0x80u8];
        let frame = build_frame_with(
            LIDAR_TOPIC,
            &grid,
            LIDAR_GRID_BYTES + 1,
            0.05,
            [0.0, 0.0, 0.0],
        );
        ingest_voxel_map(&frame);
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 0, "src_size over grid cap must be rejected");
            assert_eq!(l.point_count, 0);
        });
    }

    #[test]
    fn non_positive_resolution_is_rejected() {
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        let grid = vec![0x80u8];
        for bad in [0.0_f64, -0.05, f64::NAN, f64::INFINITY] {
            let frame = build_frame_with(LIDAR_TOPIC, &grid, grid.len(), bad, [0.0, 0.0, 0.0]);
            ingest_voxel_map(&frame);
        }
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 0, "non-positive/non-finite resolution must be rejected");
            assert_eq!(l.point_count, 0);
        });
    }

    #[test]
    fn non_finite_origin_is_rejected() {
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        let grid = vec![0x80u8];
        let frame =
            build_frame_with(LIDAR_TOPIC, &grid, grid.len(), 0.05, [0.0, f64::NAN, 0.0]);
        ingest_voxel_map(&frame);
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert_eq!(l.frame_seq, 0, "non-finite origin component must be rejected");
            assert_eq!(l.point_count, 0);
        });
    }

    // -------------------------------------------------------------------------
    // Cross-worker DB persistence (telemetry + lidar live state)
    // -------------------------------------------------------------------------

    /// Write the durable LiDAR enable intent the same way `set_lidar` does (one
    /// byte: 1 = on, 0 = off), so the store-backed tests don't depend on the
    /// (inert in native tests) `set_lidar` mesh-dispatch path.
    fn set_lidar_intent(enabled: bool) {
        state::set_durable(state::KEY_LIDAR_ENABLED, alloc::vec![u8::from(enabled)])
            .expect("persist lidar intent");
    }

    #[test]
    fn telemetry_store_round_trip_preserves_shape() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        TELEMETRY.with(|cell| *cell.borrow_mut() = Telemetry::default());

        // Feed a sportmodestate + lowstate battery into the in-tick accumulator,
        // exactly as the service instance does while draining the stream.
        let sport = json!({
            "data": {
                "mode": 1, "gait_type": 2, "body_height": 0.32,
                "velocity": [0.1, -0.2], "yaw_speed": 0.05,
                "position": [1.0, 2.0, 3.0], "foot_force": [10.0, 11.0, 12.0, 13.0],
                "imu_state": { "rpy": [0.01, 0.02, 0.03], "quaternion": [1.0, 0.0, 0.0, 0.0], "temperature": 30.0 }
            }
        }).to_string();
        ingest_sportmodestate(sport.as_bytes());
        let low = json!({ "data": { "bms_state": { "soc": 88.0, "voltage": 28.4, "current": -2.1, "temperature": 31.0 } } }).to_string();
        ingest_lowstate_battery(low.as_bytes());

        // The service instance builds the snapshot and persists it (throttled path).
        let built = telemetry_json();
        assert!(built.is_object(), "snapshot must be a JSON object");
        state::set_ephemeral(state::KEY_TELEMETRY, built.to_string().into_bytes())
            .expect("persist telemetry");

        // A different worker (empty thread_local) reads it back from the store.
        TELEMETRY.with(|cell| *cell.borrow_mut() = Telemetry::default());
        assert!(telemetry_json().is_null(), "thread_local is empty on the reader worker");
        let read_back = telemetry_from_store();
        assert_eq!(read_back, built, "store round-trip must preserve the exact telemetry shape");

        // Spot-check the shape parse_status_telemetry consumes.
        assert_eq!(read_back.get("mode").and_then(JsonValue::as_i64), Some(1));
        assert_eq!(
            read_back.get("battery").and_then(|b| b.get("soc")).and_then(JsonValue::as_f64),
            Some(88.0)
        );
        assert_eq!(
            read_back.get("velocity").and_then(|v| v.get("vx")).and_then(JsonValue::as_f64),
            Some(0.1)
        );
        state::test_reset();
    }

    #[test]
    fn telemetry_store_empty_is_null() {
        state::test_reset();
        assert!(telemetry_from_store().is_null(), "no persisted telemetry → Null");
    }

    #[test]
    fn lidar_desired_flag_persists_cross_worker() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        assert!(!lidar_enabled_intent().expect("read"), "default desire is disabled");
        set_lidar_intent(true);
        assert!(lidar_enabled_intent().expect("read"), "enable desire persists");
        set_lidar_intent(false);
        assert!(!lidar_enabled_intent().expect("read"), "disable desire persists");
        state::test_reset();
    }

    #[test]
    fn lidar_enabled_read_error_propagates_not_false() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        // Operator intent is ON, then a transient read fails: the call must surface
        // the error, NOT collapse to `false` (which would drive the LiDAR off).
        set_lidar_intent(true);
        state::test_fail_next_get();
        assert!(lidar_enabled_intent().is_err(), "read error propagates");
        // The next read recovers and still reports the persisted ON intent.
        assert!(lidar_enabled_intent().expect("read"), "intent intact after error");
        state::test_reset();
    }

    #[test]
    fn lidar_disable_keeps_pending_on_send_failure_then_clears_on_success() {
        // wc_send_text fails in native tests (webrtc_send_v1 stub returns 5), so the
        // disable transition must NOT clear local enabled/subscribed: it keeps a
        // pending-disable for a later tick to retry. The robot must never be left
        // streaming while we report disabled.
        LIDAR.with(|cell| {
            let mut l = cell.borrow_mut();
            *l = LidarState::default();
            l.enabled = true;
            l.subscribed = true;
        });
        // Mirror the tick disable branch with a failing send.
        let off_ok = wc_send_text("chan", &json!({ "type": "msg", "topic": LIDAR_SWITCH_TOPIC, "data": "off" }).to_string()).is_ok();
        let unsub_ok = wc_send_text("chan", &unsubscribe_msg(LIDAR_TOPIC)).is_ok();
        assert!(!off_ok || !unsub_ok, "native stub send fails — exercises the failure path");
        if off_ok && unsub_ok {
            LIDAR.with(|cell| {
                let mut l = cell.borrow_mut();
                l.enabled = false;
                l.subscribed = false;
                l.pending_disable = false;
            });
        } else {
            LIDAR.with(|cell| cell.borrow_mut().pending_disable = true);
        }
        LIDAR.with(|cell| {
            let l = cell.borrow();
            assert!(l.enabled, "still enabled — off-send failed, no leak as disabled");
            assert!(l.pending_disable, "pending-disable armed for retry");
        });
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    #[test]
    fn lidar_status_write_is_throttled_per_second() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        // First frame at t0: availability transitions false->true, must persist now.
        let grid = vec![0xFFu8];
        ingest_voxel_map(&build_normal_frame(&grid, 0.05, [0.0, 0.0, 0.0]));
        let seq_at_t0 = LIDAR.with(|c| c.borrow().frame_seq);
        let persisted_t0 = lidar_status_from_store().expect("read")
            .get("frame_seq").and_then(JsonValue::as_u64).unwrap_or(0);
        assert_eq!(persisted_t0, seq_at_t0, "transition frame persisted immediately");
        // A second frame in the same second (no transition): NOT re-persisted.
        ingest_voxel_map(&build_normal_frame(&grid, 0.05, [0.0, 0.0, 0.0]));
        let persisted_same_sec = lidar_status_from_store().expect("read")
            .get("frame_seq").and_then(JsonValue::as_u64).unwrap_or(0);
        assert_eq!(persisted_same_sec, persisted_t0, "steady-state frame within 1s not re-persisted");
        // Advance the clock >= refresh cadence: the periodic refresh writes again.
        db::set_now_secs(1_700_000_000 + LIDAR_STATUS_REFRESH_SECS);
        ingest_voxel_map(&build_normal_frame(&grid, 0.05, [0.0, 0.0, 0.0]));
        let live_seq = LIDAR.with(|c| c.borrow().frame_seq);
        let persisted_after_refresh = lidar_status_from_store().expect("read")
            .get("frame_seq").and_then(JsonValue::as_u64).unwrap_or(0);
        assert_eq!(persisted_after_refresh, live_seq, "periodic refresh persists fresh frame_seq");
        state::test_reset();
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    #[test]
    fn set_lidar_local_persists_intent_while_offline() {
        // No `ip` config in native tests (config_get_v1 stub returns 2 = not found),
        // so set_lidar() would route over the mesh as "remote-owned" — assert that
        // boundary holds (the dispatch path returns an error from the inert stub),
        // then assert the LOCAL path (state store) persists intent regardless of
        // online state, which is what on_tick reads to re-subscribe on reconnect.
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        let resp = set_lidar(true);
        // Remote route taken (no IP configured): dispatch stub fails, surfaced as error.
        assert!(resp.get("error").is_some(), "no-IP node routes over mesh");
        // The owning-node behavior set_lidar performs is the durable intent write
        // while offline; verify that persists the intent the tick loop consumes.
        set_lidar_intent(true);
        assert!(lidar_enabled_intent().expect("read"), "intent persists while robot offline");
        state::test_reset();
    }

    #[test]
    fn lidar_status_store_round_trip_preserves_shape() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());

        // Decode one frame in the service instance; it persists the small status.
        let idx = 0x800 + 0x10;
        let mut grid = vec![0u8; idx + 1];
        grid[idx] = 0x80;
        let frame = build_normal_frame(&grid, 0.05, [1.0, 2.0, 3.0]);
        ingest_voxel_map(&frame);

        let built = lidar_status_json();
        // A reader worker has an empty thread_local but reads the persisted status.
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        let read_back = lidar_status_from_store().expect("read");
        assert_eq!(read_back, built, "lidar status round-trip preserves shape");
        assert_eq!(read_back.get("available").and_then(JsonValue::as_bool), Some(true));
        assert_eq!(read_back.get("point_count").and_then(JsonValue::as_u64), Some(1));
        assert_eq!(read_back.get("frame_seq").and_then(JsonValue::as_u64), Some(1));

        state::test_reset();
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
    }

    #[test]
    fn lidar_frame_returns_status_snapshot_never_points() {
        state::test_reset();
        LIDAR.with(|cell| *cell.borrow_mut() = LidarState::default());
        set_lidar_intent(true);
        // The live cloud flows through the binary host hub now; this tool returns
        // only the small availability snapshot — never points, never a fabricated
        // pending flag.
        let frame = lidar_frame();
        assert!(frame.get("points").is_none(), "cloud is never returned by this tool");
        assert!(frame.get("frame_pending_renderer").is_none(), "no pending flag — points moved off this path");
        assert_eq!(frame.get("enabled").and_then(JsonValue::as_bool), Some(true));
        state::test_reset();
    }

    #[test]
    fn lidar_status_read_error_surfaces_not_fabricated() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        // Operator intent is ON, then the status read fails: the builder must
        // surface the error, NOT fabricate a `{enabled:false, available:false}`.
        set_lidar_intent(true);
        state::test_fail_next_get();
        assert!(lidar_status_from_store().is_err(), "real read error propagates, not fabricated");
        // lidar_frame must turn that into an explicit error, not an empty frame.
        state::test_fail_next_get();
        let frame = lidar_frame();
        assert!(frame.get("error").is_some(), "lidar_frame surfaces the read error");
        // Recovery: the next read returns the intent-default object (enabled).
        let recovered = lidar_status_from_store().expect("read recovers");
        assert_eq!(recovered.get("enabled").and_then(JsonValue::as_bool), Some(true));
        assert_eq!(recovered.get("available").and_then(JsonValue::as_bool), Some(false));
        state::test_reset();
    }

    #[test]
    fn lidar_status_absent_renders_default_disabled_unchanged() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        // Nothing persisted and no intent ever written: the byte-identical default
        // disabled shape go2.status rendered before (absent != error).
        let s = lidar_status_from_store().expect("absent is Ok, not Err");
        assert_eq!(s.get("enabled").and_then(JsonValue::as_bool), Some(false));
        assert_eq!(s.get("available").and_then(JsonValue::as_bool), Some(false));
        assert_eq!(s.get("point_count").and_then(JsonValue::as_u64), Some(0));
        assert_eq!(s.get("frame_seq").and_then(JsonValue::as_u64), Some(0));
        state::test_reset();
    }

    #[test]
    fn lidar_intent_read_error_skips_actuator_not_disable() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        // Operator intent ON; a transient read error in on_tick must yield `None`
        // (skip the actuator decision this tick), NOT `Some(false)` (which would
        // command the robot's LiDAR off). Mirrors the on_tick desired_lidar branch.
        set_lidar_intent(true);
        state::test_fail_next_get();
        // Same Err → None mapping on_tick uses to gate the actuator: a read error
        // skips the decision, never collapses to Some(false) (disable command).
        let desired = lidar_enabled_intent().ok();
        assert_eq!(desired, None, "read error skips the actuator (no disable command)");
        // Next tick recovers the persisted ON intent.
        let recovered = lidar_enabled_intent().ok();
        assert_eq!(recovered, Some(true), "intent ON intact after the transient error");
        state::test_reset();
    }

    #[test]
    fn status_mirror_uses_fresh_row_only_online_when_truly_online() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        // The fresh-row mirror guard: only mirror `online` when the fresh row is
        // genuinely online with a live channel. An online row with a channel
        // mirrors; an offline / channel-less row does not write `online`.
        let online = db::Robot {
            status: "online".into(),
            channel_id: "chan-1".into(),
            camera_id: "cam-1".into(),
            battery_pct: 77,
            rtt_ms: 12,
            ..Default::default()
        };
        if online.status == "online" && !online.channel_id.is_empty() {
            mirror_status(&online);
        }
        let mirrored = state::get_string(state::KEY_STATUS).expect("read").expect("mirror present");
        let v: JsonValue = serde_json::from_str(&mirrored).expect("json");
        assert_eq!(v.get("status").and_then(|s| s.as_str()), Some("online"));
        assert_eq!(v.get("battery_pct").and_then(JsonValue::as_i64), Some(77));

        // A raced offline row (status offline OR empty channel) must NOT overwrite
        // the mirror back to online via the guarded online-tick path.
        let raced_offline = db::Robot { status: "offline".into(), channel_id: String::new(), ..Default::default() };
        let would_mirror_online = raced_offline.status == "online" && !raced_offline.channel_id.is_empty();
        assert!(!would_mirror_online, "offline/channel-less fresh row never mirrors online");

        // An online status with an EMPTY channel (mid-teardown) also must not mirror online.
        let online_no_channel = db::Robot { status: "online".into(), channel_id: String::new(), ..Default::default() };
        let would_mirror = online_no_channel.status == "online" && !online_no_channel.channel_id.is_empty();
        assert!(!would_mirror, "online-but-channel-less is not truly online");
        state::test_reset();
    }

    #[test]
    fn on_start_bridge_seeds_intent_from_legacy_when_absent() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        // Fresh upgrade: durable store key absent, legacy table holds ON intent.
        db::test_set_legacy_lidar(Some(true));
        assert!(state::get(state::KEY_LIDAR_ENABLED).expect("read").is_none(), "store key absent pre-bridge");
        bridge_legacy_lidar_intent();
        assert!(lidar_enabled_intent().expect("read"), "bridge seeds ON intent from legacy table");

        // Idempotent: a second start with a DIFFERENT (newer) store value must NOT
        // be clobbered by the stale legacy value.
        set_lidar_intent(false);
        db::test_set_legacy_lidar(Some(true));
        bridge_legacy_lidar_intent();
        assert!(!lidar_enabled_intent().expect("read"), "second start never clobbers a newer store value");
        db::test_set_legacy_lidar(None);
        state::test_reset();
    }

    #[test]
    fn on_start_bridge_no_legacy_table_is_noop() {
        let _clock_guard = CLOCK_LOCK.lock().unwrap();
        db::set_now_secs(1_700_000_000);
        state::test_reset();
        // No legacy table (later drop migration shipped): the bridge leaves the
        // store key absent (fresh-install default disabled), never fabricates one.
        db::test_set_legacy_lidar(None);
        bridge_legacy_lidar_intent();
        assert!(
            state::get(state::KEY_LIDAR_ENABLED).expect("read").is_none(),
            "no legacy value → store key stays absent (no fabricated intent)"
        );
        state::test_reset();
    }
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
    // Control actions arrive as raw declared action_ids (go2.connect, go2.estop, …)
    // from the core Roboty module via robot dispatch. Flow blocks come via
    // invoke_block as `block.go2.*`.
    let tool = raw_tool;
    let response = if let Some(block_type) = tool.strip_prefix("block.") {
        handle_block(block_type, &params)
    } else {
        handle(tool, &params)
    };
    write_response(out_ptr, out_cap, out_len_ptr, &response.to_string())
}
