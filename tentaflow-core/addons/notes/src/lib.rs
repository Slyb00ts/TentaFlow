// =============================================================================
// File: addons/notes/src/lib.rs
// Purpose: Notes addon (WASM) entry points and action dispatch. Notes CRUD
//          with ACL shares lives in db.rs; the three-column panel
//          (list | editor | links) is rendered in ui.rs via the ui_v1 catalog;
//          the auto-graph pipeline (chunk + embed, entity extraction, note
//          links, entity merge, graph outbox) lives in analysis.rs.
// =============================================================================

mod analysis;
mod db;
mod ui;
mod ui_graph;

use serde_json::{json, Value};
use tentaflow_addon_sdk::{log, read_string, write_string};

// Raw binding: the SDK's typed get_current_user expects a numeric user_id,
// but the host returns {"id": <string>, ...} — parse the JSON directly.
#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn user_get_current(out_ptr: i32, out_cap: i32, out_len_ptr: i32) -> i32;
}

/// Id of the user this pooled instance currently acts for (the host binds the
/// instance to the opening/acting user before each lifecycle call).
fn current_user_id() -> Option<String> {
    let mut buffer = vec![0u8; 16 * 1024];
    let mut out_len: i32 = 0;
    let rc = unsafe {
        user_get_current(
            buffer.as_mut_ptr() as i32,
            buffer.len() as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if rc != 0 || out_len <= 0 {
        return None;
    }
    let parsed: Value = serde_json::from_slice(&buffer[..out_len as usize]).ok()?;
    match &parsed["id"] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

// =============================================================================
// Lifecycle exports
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("notes: installed");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    // The shell is NOT rendered here: on_start never receives the
    // host-assigned panel epoch. on_panel_open (called on every open,
    // including cold starts) is the single canonical render entry.
    log::info("notes: started");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("notes: stopped");
    0
}

#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

/// Panel (re)open: adopt the authoritative epoch, resolve the opening user and
/// render the shell plus all three column slots.
#[no_mangle]
pub extern "C" fn on_panel_open(panel_id_ptr: i32, panel_id_len: i32, epoch: i64) -> i32 {
    let panel_id = read_string(panel_id_ptr, panel_id_len);
    if panel_id != ui::PANEL_ID {
        log::warn(&format!("notes: on_panel_open unknown panel '{panel_id}'"));
        return 0;
    }
    // Identity FIRST: reset_for_open zeroes the per-(user, epoch) revision in
    // the host KV, and the key needs the acting user.
    let user_id = current_user_id().unwrap_or_default();
    ui::set_session_user(Some(&user_id));
    ui::reset_for_open(epoch as u64);
    ui::render_full(&user_id);
    0
}

// =============================================================================
// Tool / UI action dispatcher
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
            let error = json!({"ok": false, "error": format!("request parse error: {e}")});
            return write_response(out_ptr, out_cap, out_len_ptr, &error);
        }
    };

    let tool_name = request.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    // UI actions arrive as tool "ui.<panel>.<action>". The per-call user id and
    // the host-validated panel epoch must be adopted BEFORE the action runs —
    // pooled instances may carry another session's statics.
    if let Some(action_id) = tool_name.strip_prefix("ui.main.") {
        ui::set_session_user(request.get("user_id").and_then(|v| v.as_str()));
        if let Some(epoch) = params.get("__panel_epoch").and_then(|v| v.as_u64()) {
            ui::adopt_action_epoch(epoch);
        }
        let result = ui::handle_ui_action(action_id, &params);
        return write_response(out_ptr, out_cap, out_len_ptr, &result);
    }

    let result = match tool_name {
        // Queue worker: callable by the Admin Scheduler (interval job) and
        // manually. Batch of 5 per invocation; the 3 s debounce applies.
        "analyze_pending" => {
            let processed = analysis::process_queue(5);
            json!({"ok": true, "processed": processed})
        }
        other => json!({"ok": false, "error": format!("unknown tool: {other}")}),
    };
    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

/// Writes the JSON response into the caller-provided output buffer.
fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    let written = write_string(out_ptr, out_cap, &response);
    if written < 0 {
        log::error("notes: output buffer too small for response");
        return 2;
    }
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);
    0
}
