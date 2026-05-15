// =============================================================================
// File: addons/camera-test-addon/src/lib.rs
// Purpose: M1.W6 Chunk D — exercises camera_* host functions through a real
//          WASM guest. The integration test (tests/camera_integration_e2e.rs)
//          dispatches one of three tools via on_request:
//            - "run_lifecycle"      camera_add -> health -> snapshot -> remove
//            - "run_path_traversal" camera_add with a hostile URL
//            - "run_no_write_probe" camera_add; surfaces Permission denial
//                                   when cameras.write is not granted
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Lifecycle hooks (no-op — the test driver only calls on_request)
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

// =============================================================================
// on_request — tool dispatcher (input/output JSON, same ABI as test-addon)
// =============================================================================

#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = read_string(input_ptr, input_len);
    let request: Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            let err = json!({"ok": false, "error": format!("parse: {}", e)});
            return write_response(out_ptr, out_cap, out_len_ptr, &err);
        }
    };

    let tool_name = request.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    let result = match tool_name {
        "run_lifecycle" => run_lifecycle(&params),
        "run_path_traversal" => run_path_traversal(&params),
        "run_no_write_probe" => run_no_write_probe(&params),
        _ => json!({"ok": false, "error": format!("unknown tool: {}", tool_name)}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Tool: run_lifecycle — camera_add -> health -> snapshot -> remove
// =============================================================================

fn run_lifecycle(params: &Value) -> Value {
    let sample_path = match params.get("sample_path").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({"ok": false, "error": "missing sample_path"}),
    };

    // 1. camera_add.
    let spec = CameraAddSpec {
        display_name: "lifecycle test cam".to_string(),
        vendor: "fake_file".to_string(),
        url: sample_path,
        target_fps: 30,
        resolution: None,
        retention_class: "C".to_string(),
        profile: "default".to_string(),
    };
    let added = match camera_add(&spec) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "stage": "camera_add", "abi_error": e.as_i32()}),
    };
    let camera_id = added.camera_id.clone();

    // 2. camera_health — supervisor exposes the session immediately. We do not
    //    require any frames to have flowed yet; the host always returns a
    //    valid CameraHealthInfo as long as the session is registered.
    let health = match camera_health(&camera_id) {
        Ok(v) => v,
        Err(e) => {
            // Best-effort cleanup; ignore the result.
            let _ = camera_remove(&camera_id);
            return json!({
                "ok": false, "stage": "camera_health",
                "abi_error": e.as_i32(), "camera_id": camera_id,
            });
        }
    };

    // 3. camera_snapshot — may return Operation if no frame has arrived yet.
    //    We treat that as a soft failure (still a valid lifecycle path) and
    //    only assert presence in the dedicated snapshot test variant.
    let snap_result = camera_snapshot(&camera_id);
    let (snap_ok, snap_len, snap_width, snap_height, snap_abi) = match &snap_result {
        Ok(s) => (true, s.data.len() as u64, s.width, s.height, 0),
        Err(e) => (false, 0u64, 0u32, 0u32, e.as_i32()),
    };

    // 4. camera_remove.
    if let Err(e) = camera_remove(&camera_id) {
        return json!({
            "ok": false, "stage": "camera_remove",
            "abi_error": e.as_i32(), "camera_id": camera_id,
        });
    }

    json!({
        "ok": true,
        "camera_id": camera_id,
        "status_after_add": added.status,
        "health_status": health.status,
        "snapshot_ok": snap_ok,
        "snapshot_len": snap_len,
        "snapshot_width": snap_width,
        "snapshot_height": snap_height,
        "snapshot_abi_error": snap_abi,
    })
}

// =============================================================================
// Tool: run_path_traversal — camera_add with hostile URL
// =============================================================================

fn run_path_traversal(params: &Value) -> Value {
    let bad_url = match params.get("bad_url").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({"ok": false, "error": "missing bad_url"}),
    };

    let spec = CameraAddSpec {
        display_name: "traversal probe".to_string(),
        vendor: "fake_file".to_string(),
        url: bad_url.clone(),
        target_fps: 30,
        resolution: None,
        retention_class: "C".to_string(),
        profile: "default".to_string(),
    };

    match camera_add(&spec) {
        Ok(res) => {
            // Should never happen — defence in depth in the host MUST reject
            // any non-regular-file resolution. If it slipped through, attempt
            // to clean up and report failure.
            let _ = camera_remove(&res.camera_id);
            json!({
                "ok": false,
                "error": "camera_add unexpectedly succeeded for hostile URL",
                "camera_id": res.camera_id,
            })
        }
        Err(e) => json!({
            "ok": true,
            "rejected": true,
            "abi_error": e.as_i32(),
            "bad_url": bad_url,
        }),
    }
}

// =============================================================================
// Tool: run_no_write_probe — surfaces Permission denial cleanly
// =============================================================================

fn run_no_write_probe(params: &Value) -> Value {
    let sample_path = params
        .get("sample_path")
        .and_then(|v| v.as_str())
        .unwrap_or("/tmp/nonexistent.mp4")
        .to_string();

    let spec = CameraAddSpec {
        display_name: "no-write probe".to_string(),
        vendor: "fake_file".to_string(),
        url: sample_path,
        target_fps: 30,
        resolution: None,
        retention_class: "C".to_string(),
        profile: "default".to_string(),
    };

    match camera_add(&spec) {
        Ok(res) => {
            let _ = camera_remove(&res.camera_id);
            json!({
                "ok": true,
                "granted": true,
                "camera_id": res.camera_id,
            })
        }
        Err(AbiError::Permission) => json!({
            "ok": true,
            "granted": false,
            "abi_error": AbiError::Permission.as_i32(),
        }),
        Err(e) => json!({
            "ok": false,
            "unexpected_abi_error": e.as_i32(),
        }),
    }
}

// =============================================================================
// Helpers — JSON response writer (same pattern as test-addon)
// =============================================================================

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    let written = write_string(out_ptr, out_cap, &response_str);
    if written < 0 {
        return 2;
    }
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);
    0
}
