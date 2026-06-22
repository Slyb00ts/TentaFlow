// =============================================================================
// File: addons/phone/src/lib.rs — mobile-device sensor-robot addon (WASM).
// Opis: cienki orkiestrator. Natywna warstwa telefonu (Swift/Kotlin) przechwytuje
//       czujniki i buforuje kanoniczne próbki w kolejce hosta (MobileSensorQueue);
//       ten addon co tick OPRÓŻNIA kolejkę przez host-fn `mobile_sensor_drain_v1`,
//       która sprawdza uprawnienia per-czujnik (sensor.imu/gps/baro, lidar.publish)
//       i podaje próbki do silnika fuzji (ESKF) oraz wspólnej mapy — kluczowane
//       addon_id == robot_id (ten sam, którym addon ogłasza się jako [robot]).
//       Cała logika czujników żyje w rdzeniu; addon to brama instalacji + tożsamość
//       robota + uprawnienia.
// =============================================================================

use serde_json::{json, Value as JsonValue};

// Host imports (module "tentaflow").
#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn log_info(ptr: i32, len: i32) -> i32;
    /// Drain the native sensor queue into the fusion engine + shared map, gated by
    /// THIS addon's per-sensor permissions. Returns the number of samples consumed.
    fn mobile_sensor_drain_v1() -> i32;
}

fn info(msg: &str) {
    unsafe {
        let _ = log_info(msg.as_ptr() as i32, msg.len() as i32);
    }
}

// ----- WASM memory ABI (host allocates request bytes, reads response bytes) -----

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let mut buf = Vec::<u8>::with_capacity(size.max(0) as usize);
    let ptr = buf.as_mut_ptr() as i32;
    core::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    if ptr != 0 && size > 0 {
        unsafe {
            drop(Vec::from_raw_parts(ptr as *mut u8, 0, size as usize));
        }
    }
}

fn read_string(ptr: i32, len: i32) -> String {
    if ptr == 0 || len <= 0 {
        return String::new();
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Copy `body` into the host-provided out buffer; write the length to `out_len_ptr`.
/// Returns 0 on success, or the required length if the buffer is too small.
fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, body: &str) -> i32 {
    let bytes = body.as_bytes();
    if (bytes.len() as i32) > out_cap {
        unsafe {
            *(out_len_ptr as *mut i32) = bytes.len() as i32;
        }
        return bytes.len() as i32;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr as *mut u8, bytes.len());
        *(out_len_ptr as *mut i32) = bytes.len() as i32;
    }
    0
}

// ----- Lifecycle -----

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    info("phone: addon started — draining native sensors into the fusion engine");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_event(_ptr: i32, _len: i32) -> i32 {
    0
}

/// Each tick: drain whatever the native layer captured since the last tick into the
/// fusion engine + shared map (the host fn permission-checks each sensor kind).
#[no_mangle]
pub extern "C" fn on_tick(_ts_ms: i64) -> i32 {
    unsafe {
        let _ = mobile_sensor_drain_v1();
    }
    0
}

// ----- Tool dispatch (robot status) -----

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
    let tool = req.get("tool").and_then(|t| t.as_str()).unwrap_or("");
    let response = handle(tool);
    write_response(out_ptr, out_cap, out_len_ptr, &response.to_string())
}

fn handle(tool: &str) -> JsonValue {
    match tool {
        // Robot registry status: the phone is online whenever this addon runs (it runs
        // ONLY on the physical device). The actual per-sensor availability is enforced
        // at the drain (permissions) + natively (OS grant); `capabilities` is the
        // advertised superset so the Robots app shows the right tiles.
        "status" => json!({
            "status": "online",
            "kind": "phone",
            "capabilities": ["pose", "imu", "gnss", "lidar", "camera"],
            "actions_meta": []
        }),
        _ => json!({ "error": "unknown tool" }),
    }
}
