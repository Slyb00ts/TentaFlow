// =============================================================================
// File: addons/sdk-showcase/src/lib.rs
// Purpose: consolidated test/showcase addon. Exercises host functions
//          (storage, log, permissions, fuel metering, SQL, camera, recording,
//          vector), exposes the 'uppercase' flow block, runs a background
//          service tick, and renders a CBOR UI panel that showcases every
//          component of the SDK catalog plus live state / storage demos.
// =============================================================================

mod catalog;

use tentaflow_addon_sdk::prelude::*;

use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::ui::a11y::EventKind;
use tentaflow_sdk_spec::protocol::ui::actions::Button;
use tentaflow_sdk_spec::protocol::ui::bind::{BindRef, PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, HandlerMap};
use tentaflow_sdk_spec::protocol::ui::data::{Heading, Text};
use tentaflow_sdk_spec::protocol::ui::handler::{FailurePolicy, Handler};
use tentaflow_sdk_spec::protocol::ui::inline::NavTab;
use tentaflow_sdk_spec::protocol::ui::layout::{NavTabs, Stack};
use tentaflow_sdk_spec::protocol::ui::molecules::Inspector;
use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
use tentaflow_sdk_spec::protocol::ui::slot::{
    CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
};
use tentaflow_sdk_spec::protocol::ui::slot_msg::SlotContent;
use tentaflow_sdk_spec::protocol::ui::state::StatePatch;
use tentaflow_sdk_spec::protocol::ui::tokens::{
    ButtonSize, ButtonVariant, Density, FlexAlign, NavTabsVariant, Spacing, TextStyle, Tone,
};
use tentaflow_sdk_spec::protocol::ui::ui_payload::UiPayload;
use tentaflow_sdk_spec::protocol::value::Value as CborValue;

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
}

// =============================================================================
// Constants & mutable state
// =============================================================================

const ADDON_ID: &str = "sdk-showcase";
const PANEL_ID: &str = "main";
const SLOT_ID: &str = "content";
const DEFAULT_TAB: &str = "live";
const VECTOR_NAMESPACE: &str = "showcase";

static mut COUNTER: u64 = 0;
static mut STATE_REVISION: u64 = 0;
static mut PANEL_EPOCH: u64 = 1;
static mut ACTIVE_TAB: Option<String> = None;

fn panel_epoch() -> u64 {
    unsafe { PANEL_EPOCH }
}

fn active_tab() -> String {
    unsafe {
        #[allow(static_mut_refs)]
        ACTIVE_TAB.clone().unwrap_or_else(|| DEFAULT_TAB.to_string())
    }
}

fn set_active_tab(tab: &str) {
    unsafe {
        ACTIVE_TAB = Some(tab.to_string());
    }
}

// =============================================================================
// Lifecycle exports
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("sdk-showcase installed");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("sdk-showcase started");
    // The shell is NOT rendered here: on_start does not receive the
    // host-assigned panel epoch, so a shell emitted now would carry the default
    // epoch and be rejected on any session whose epoch advanced past 1. The
    // host calls on_panel_open (with the authoritative epoch) on every open,
    // including cold starts, so the shell is rendered there exactly once.
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("sdk-showcase stopped");
    0
}

/// Panel reopen handler — the host assigns a fresh epoch and resets the
/// expected state revision to 0 on every PanelOpen, so the addon must adopt
/// the new epoch, restart its own revision counter and re-register the shell.
#[no_mangle]
pub extern "C" fn on_panel_open(panel_id_ptr: i32, panel_id_len: i32, epoch: i64) -> i32 {
    let panel_id = read_string(panel_id_ptr, panel_id_len);
    if panel_id != PANEL_ID {
        log::warn(&format!("on_panel_open: unknown panel '{}'", panel_id));
        return 0;
    }
    unsafe {
        PANEL_EPOCH = epoch as u64;
        STATE_REVISION = 0;
        COUNTER = 0;
        ACTIVE_TAB = None;
    }
    send_panel_shell();
    send_tab_content(&active_tab());
    0
}

#[no_mangle]
pub extern "C" fn on_event(event_ptr: i32, event_len: i32) -> i32 {
    let event_json = read_string(event_ptr, event_len);
    log::info(&format!("sdk-showcase on_event: {}", event_json));
    0
}

/// Background service tick — increments the persisted tick counter and pushes
/// a StatePatch so the bound Text in the Live tab updates reactively.
#[no_mangle]
pub extern "C" fn on_tick(_timestamp_ms: i64) -> i32 {
    let counter = read_tick_counter() + 1;
    write_tick_counter(counter);
    send_state_patch(vec![PatchOp {
        path: state_path("tick_counter"),
        op: PatchOpKind::Set {
            value: CborValue::U64(counter),
        },
    }]);
    0
}

// =============================================================================
// on_request — tool / UI action / flow block dispatcher
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
/// Input JSON: {"tool": "name", "params": {...}, "user_id": ...}
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
            let error = json!({"ok": false, "error": format!("request parse error: {}", e)});
            return write_response(out_ptr, out_cap, out_len_ptr, &error);
        }
    };

    let tool_name = request.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    let result = if tool_name.starts_with("ui.") {
        let action_id = tool_name.rsplit('.').next().unwrap_or("");
        handle_ui_action(action_id, &params)
    } else if let Some(block_type) = tool_name.strip_prefix("block.") {
        handle_flow_block(block_type, &params)
    } else {
        match tool_name {
            "echo" => handle_echo(&params),
            "test_storage" => handle_test_storage(&params),
            "test_permissions" => handle_test_permissions(&params),
            "test_log" => handle_test_log(),
            "test_crash" => handle_test_crash(),
            "run_sql_suite" => run_sql_suite(),
            "run_vector_suite" => run_vector_suite(),
            "run_lifecycle" => run_lifecycle(&params),
            "run_path_traversal" => run_path_traversal(&params),
            "run_no_write_probe" => run_no_write_probe(&params),
            "run_recording_lifecycle" => run_recording_lifecycle(&params),
            "run_recording_save_segment" => run_recording_save_segment(&params),
            "run_frame_url_basic" => run_frame_url_basic(&params),
            _ => json!({"ok": false, "error": format!("unknown tool: {}", tool_name)}),
        }
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Basic host-function tools (echo / storage / permissions / log / fuel)
// =============================================================================

/// Echo — returns the provided text.
fn handle_echo(params: &Value) -> Value {
    let text = params["text"].as_str().unwrap_or("empty");
    json!({"ok": true, "data": {"echo": text}})
}

/// Storage round-trip — writes a value and reads it back.
fn handle_test_storage(params: &Value) -> Value {
    let key = params["key"].as_str().unwrap_or("test_key");
    let value = params["value"].as_str().unwrap_or("test_value");

    if let Err(e) = store_set(key, value) {
        return json!({"ok": false, "error": format!("store_set error: {}", e)});
    }

    match store_get(key) {
        Ok(Some(read_val)) => {
            let matches = value == read_val.as_str();
            json!({
                "ok": true,
                "data": {
                    "written": value,
                    "read": read_val,
                    "match": matches
                }
            })
        }
        Ok(None) => json!({"ok": false, "error": "store_get returned None"}),
        Err(e) => json!({"ok": false, "error": format!("store_get error: {}", e)}),
    }
}

/// Permission probe — storage requires the storage permission, so a successful
/// store_set proves the grant.
fn handle_test_permissions(params: &Value) -> Value {
    let perm = params["permission"].as_str().unwrap_or("test_read");
    let storage_ok = store_set("_perm_check", "1").is_ok();

    json!({
        "ok": true,
        "data": {
            "permission": perm,
            "storage_access": storage_ok,
            "checked": true
        }
    })
}

/// Emits one log line per level.
fn handle_test_log() -> Value {
    log::info("test log info message");
    log::warn("test log warn message");
    log::error("test log error message");
    json!({"ok": true, "data": {"logged": true}})
}

/// Infinite loop — must be stopped by fuel metering. black_box prevents the
/// compiler from optimising the loop away.
fn handle_test_crash() -> Value {
    let mut i: u64 = 0;
    loop {
        i = i.wrapping_add(1);
        core::hint::black_box(i);
        if i == 0 {
            break;
        }
    }
    json!({"ok": true})
}

// =============================================================================
// SQL suite — repeatable host-function scenario over the per-addon SQLite
// =============================================================================

/// Full SQL host-function scenario:
///   1. DELETE cleanup (repeatable runs against the UNIQUE name column)
///   2. sql_exec INSERT with bind params (SQL injection protection)
///   3. sql_query SELECT — returns the inserted rows
///   4. sql_query_one SELECT with WHERE — first row or null
///   5. sql_transaction batch INSERT + UPDATE — atomic
///   6. DDL probe via sql_exec — must fail with AbiError::Permission
fn run_sql_suite() -> Value {
    let cleanup = match sql_exec("DELETE FROM items", &[]) {
        Ok(res) => json!({"ok": true, "rows_affected": res.rows_affected}),
        Err(code) => return json!({"ok": false, "stage": "cleanup", "abi_error": code.as_i32()}),
    };

    let insert = match sql_exec(
        "INSERT INTO items (name, qty, created_at) VALUES (?, ?, ?)",
        &[
            SqlValue::String("alpha'; DROP TABLE items;--".to_string()),
            SqlValue::I64(3),
            SqlValue::I64(1715515200),
        ],
    ) {
        Ok(res) => json!({"ok": true, "rows_affected": res.rows_affected, "last_insert_id": res.last_insert_id}),
        Err(code) => return json!({"ok": false, "stage": "insert", "abi_error": code.as_i32()}),
    };

    if let Err(code) = sql_exec(
        "INSERT INTO items (name, qty, created_at) VALUES (?, ?, ?)",
        &[
            SqlValue::String("beta".to_string()),
            SqlValue::I64(5),
            SqlValue::I64(1715515210),
        ],
    ) {
        return json!({"ok": false, "stage": "insert_beta", "abi_error": code.as_i32()});
    }

    let query = match sql_query("SELECT id, name, qty FROM items ORDER BY id", &[]) {
        Ok(rows) => json!({"ok": true, "rows": rows.len()}),
        Err(code) => return json!({"ok": false, "stage": "query", "abi_error": code.as_i32()}),
    };

    let query_one = match sql_query_one(
        "SELECT id, qty FROM items WHERE name = ?",
        &[SqlValue::String("beta".to_string())],
    ) {
        Ok(Some(row)) => json!({
            "ok": true,
            "id": row.first().and_then(|v| v.as_i64()),
            "qty": row.get(1).and_then(|v| v.as_i64()),
        }),
        Ok(None) => json!({"ok": false, "error": "no row"}),
        Err(code) => return json!({"ok": false, "stage": "query_one", "abi_error": code.as_i32()}),
    };

    let stmts: Vec<(&str, Vec<SqlValue>)> = vec![
        (
            "INSERT INTO items (name, qty, created_at) VALUES (?, ?, ?)",
            vec![
                SqlValue::String("gamma".to_string()),
                SqlValue::I64(7),
                SqlValue::I64(1715515220),
            ],
        ),
        (
            "UPDATE items SET qty = ? WHERE name = ?",
            vec![SqlValue::I64(99), SqlValue::String("beta".to_string())],
        ),
    ];
    let stmts_ref: Vec<(&str, &[SqlValue])> =
        stmts.iter().map(|(q, p)| (*q, p.as_slice())).collect();
    let transaction = match sql_transaction(&stmts_ref) {
        Ok(total) => json!({"ok": true, "rows_affected_total": total}),
        Err(code) => return json!({"ok": false, "stage": "transaction", "abi_error": code.as_i32()}),
    };

    let ddl_block = match sql_exec("DROP TABLE items", &[]) {
        Ok(_) => json!({"ok": false, "error": "DROP TABLE passed (must be blocked)"}),
        Err(AbiError::Permission) => json!({"ok": true, "blocked": true}),
        Err(code) => json!({"ok": false, "unexpected_abi_error": code.as_i32()}),
    };

    let all_ok = [&cleanup, &insert, &query, &query_one, &transaction, &ddl_block]
        .iter()
        .all(|step| step["ok"] == true);

    json!({
        "ok": all_ok,
        "cleanup": cleanup,
        "insert": insert,
        "query": query,
        "query_one": query_one,
        "transaction": transaction,
        "ddl_block": ddl_block,
    })
}

// =============================================================================
// Vector suite — upsert / search / delete in the 'showcase' namespace
// =============================================================================

fn run_vector_suite() -> Value {
    let vectors: [(u64, [f32; 8]); 3] = [
        (1, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        (2, [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        (3, [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ];

    let mut count_after_upsert = 0u64;
    for (ref_id, vector) in &vectors {
        match vector_upsert(VECTOR_NAMESPACE, *ref_id, vector, &[]) {
            Ok(count) => count_after_upsert = count,
            Err(code) => {
                return json!({
                    "ok": false, "stage": "upsert",
                    "ref_id": ref_id, "abi_error": code.as_i32(),
                });
            }
        }
    }

    let query = [0.9_f32, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let hits = match vector_search(VECTOR_NAMESPACE, &query, 2, None, None, &[]) {
        Ok(hits) => hits,
        Err(code) => return json!({"ok": false, "stage": "search", "abi_error": code.as_i32()}),
    };
    let hits_json: Vec<Value> = hits
        .iter()
        .map(|h| json!({"ref_id": h.ref_id, "score": h.score}))
        .collect();
    let best_is_ref1 = hits.first().map(|h| h.ref_id == 1).unwrap_or(false);

    let removed = match vector_delete(VECTOR_NAMESPACE, 3) {
        Ok(removed) => removed,
        Err(code) => return json!({"ok": false, "stage": "delete", "abi_error": code.as_i32()}),
    };

    json!({
        "ok": best_is_ref1 && removed,
        "count_after_upsert": count_after_upsert,
        "hits": hits_json,
        "best_is_ref1": best_is_ref1,
        "deleted_ref3": removed,
    })
}

// =============================================================================
// Camera tools — camera_* / stream_* / recording_* host functions
// =============================================================================

fn camera_spec(display_name: &str, url: String) -> CameraAddSpec {
    CameraAddSpec {
        display_name: display_name.to_string(),
        vendor: "fake_file".to_string(),
        url,
        target_fps: 30,
        analysis_fps: 10,
        resolution: None,
        retention_class: "C".to_string(),
        profile: "default".to_string(),
        credentials_b64: None,
        onvif_profile_token: None,
    }
}

/// camera_add -> camera_health -> camera_snapshot -> camera_remove.
fn run_lifecycle(params: &Value) -> Value {
    let sample_path = match params.get("sample_path").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({"ok": false, "error": "missing sample_path"}),
    };

    let added = match camera_add(&camera_spec("lifecycle test cam", sample_path)) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "stage": "camera_add", "abi_error": e.as_i32()}),
    };
    let camera_id = added.camera_id.clone();

    // The supervisor exposes the session immediately; no frames are required
    // for a valid CameraHealthInfo.
    let health = match camera_health(&camera_id) {
        Ok(v) => v,
        Err(e) => {
            let _ = camera_remove(&camera_id);
            return json!({
                "ok": false, "stage": "camera_health",
                "abi_error": e.as_i32(), "camera_id": camera_id,
            });
        }
    };

    // Snapshot may return Operation when no frame has arrived yet — treated
    // as a soft failure (still a valid lifecycle path).
    let snap_result = camera_snapshot(&camera_id);
    let (snap_ok, snap_len, snap_width, snap_height, snap_abi) = match &snap_result {
        Ok(s) => (true, s.data.len() as u64, s.width, s.height, 0),
        Err(e) => (false, 0u64, 0u32, 0u32, e.as_i32()),
    };

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

/// camera_add with a hostile URL — the host MUST reject any
/// non-regular-file resolution.
fn run_path_traversal(params: &Value) -> Value {
    let bad_url = match params.get("bad_url").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({"ok": false, "error": "missing bad_url"}),
    };

    match camera_add(&camera_spec("traversal probe", bad_url.clone())) {
        Ok(res) => {
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

/// Attempts camera_add; surfaces Permission denial cleanly when cameras.write
/// is not granted.
fn run_no_write_probe(params: &Value) -> Value {
    let sample_path = params
        .get("sample_path")
        .and_then(|v| v.as_str())
        .unwrap_or("/tmp/nonexistent.mp4")
        .to_string();

    match camera_add(&camera_spec("no-write probe", sample_path)) {
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

/// Subscribe + poll a single frame_ref for the recording tools.
fn await_frame_ref(camera_id: &str, max_polls: u32, timeout_ms: u64) -> Result<String, String> {
    let target = format!("camera:{}", camera_id);
    let stream_id = stream_subscribe(&target, Some(30))
        .map_err(|e| format!("stream_subscribe abi={}", e.as_i32()))?;
    let mut last_err = String::from("no frames");
    for _ in 0..max_polls {
        match stream_next(&stream_id, timeout_ms) {
            Ok(StreamNextMessage::Frame(meta)) => {
                let _ = stream_close(&stream_id);
                return Ok(meta.frame_ref);
            }
            Ok(StreamNextMessage::Timeout) => {
                last_err = "stream_next timeout".to_string();
                continue;
            }
            Ok(StreamNextMessage::Drop { .. }) => continue,
            Ok(StreamNextMessage::CameraOffline { reason }) => {
                let _ = stream_close(&stream_id);
                return Err(format!("camera_offline: {}", reason));
            }
            Ok(StreamNextMessage::StreamClosed) => {
                return Err("stream_closed before frame".into());
            }
            Err(e) => {
                last_err = format!("stream_next abi={}", e.as_i32());
                continue;
            }
        }
    }
    let _ = stream_close(&stream_id);
    Err(last_err)
}

/// camera_add -> frame -> save_snapshot -> signed URL -> purge -> remove.
fn run_recording_lifecycle(params: &Value) -> Value {
    let sample_path = match params.get("sample_path").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({"ok": false, "error": "missing sample_path"}),
    };
    let added = match camera_add(&camera_spec("recording lifecycle cam", sample_path)) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "stage": "camera_add", "abi_error": e.as_i32()}),
    };
    let camera_id = added.camera_id.clone();

    let frame_ref = match await_frame_ref(&camera_id, 20, 250) {
        Ok(r) => r,
        Err(e) => {
            let _ = camera_remove(&camera_id);
            return json!({"ok": false, "stage": "await_frame_ref", "reason": e, "camera_id": camera_id});
        }
    };

    let snap = match recording_save_snapshot(&camera_id, &frame_ref, Some("C")) {
        Ok(v) => v,
        Err(e) => {
            let _ = camera_remove(&camera_id);
            return json!({"ok": false, "stage": "recording_save_snapshot", "abi_error": e.as_i32(), "camera_id": camera_id, "frame_ref": frame_ref});
        }
    };
    let recording_ref = snap.recording_ref.clone();

    let url = match recording_get_url(&recording_ref, 120) {
        Ok(v) => v,
        Err(e) => {
            let _ = recording_purge(&recording_ref);
            let _ = camera_remove(&camera_id);
            return json!({"ok": false, "stage": "recording_get_url", "abi_error": e.as_i32(), "recording_ref": recording_ref});
        }
    };

    let purged_abi = match recording_purge(&recording_ref) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    };

    if let Err(e) = camera_remove(&camera_id) {
        return json!({"ok": false, "stage": "camera_remove", "abi_error": e.as_i32(), "camera_id": camera_id});
    }

    json!({
        "ok": true,
        "camera_id": camera_id,
        "frame_ref": frame_ref,
        "recording_ref": recording_ref,
        "file_size_bytes": snap.file_size_bytes,
        "hash_sha256": snap.hash_sha256,
        "url": url.url,
        "expires_unix_ms": url.expires_unix_ms,
        "purge_abi_error": purged_abi,
    })
}

/// camera_add -> save_segment (1s) -> purge -> remove.
fn run_recording_save_segment(params: &Value) -> Value {
    let sample_path = match params.get("sample_path").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({"ok": false, "error": "missing sample_path"}),
    };
    let added = match camera_add(&camera_spec("recording segment cam", sample_path)) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "stage": "camera_add", "abi_error": e.as_i32()}),
    };
    let camera_id = added.camera_id.clone();

    let seg = match recording_save_segment(&camera_id, 1, Some("C")) {
        Ok(v) => v,
        Err(e) => {
            let _ = camera_remove(&camera_id);
            return json!({"ok": false, "stage": "recording_save_segment", "abi_error": e.as_i32(), "camera_id": camera_id});
        }
    };
    let recording_ref = seg.recording_ref.clone();

    let purged_abi = match recording_purge(&recording_ref) {
        Ok(()) => 0,
        Err(e) => e.as_i32(),
    };

    if let Err(e) = camera_remove(&camera_id) {
        return json!({"ok": false, "stage": "camera_remove", "abi_error": e.as_i32(), "camera_id": camera_id});
    }

    json!({
        "ok": true,
        "camera_id": camera_id,
        "recording_ref": recording_ref,
        "file_size_bytes": seg.file_size_bytes,
        "duration_ms": seg.duration_ms,
        "hash_sha256": seg.hash_sha256,
        "purge_abi_error": purged_abi,
    })
}

/// camera_add -> frame -> frame_url(60s) -> remove.
fn run_frame_url_basic(params: &Value) -> Value {
    let sample_path = match params.get("sample_path").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return json!({"ok": false, "error": "missing sample_path"}),
    };
    let added = match camera_add(&camera_spec("frame_url cam", sample_path)) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "stage": "camera_add", "abi_error": e.as_i32()}),
    };
    let camera_id = added.camera_id.clone();

    let frame_ref = match await_frame_ref(&camera_id, 20, 250) {
        Ok(r) => r,
        Err(e) => {
            let _ = camera_remove(&camera_id);
            return json!({"ok": false, "stage": "await_frame_ref", "reason": e, "camera_id": camera_id});
        }
    };

    let url = match frame_url(&frame_ref, 60) {
        Ok(v) => v,
        Err(e) => {
            let _ = camera_remove(&camera_id);
            return json!({"ok": false, "stage": "frame_url", "abi_error": e.as_i32(), "frame_ref": frame_ref});
        }
    };

    if let Err(e) = camera_remove(&camera_id) {
        return json!({"ok": false, "stage": "camera_remove", "abi_error": e.as_i32(), "camera_id": camera_id});
    }

    json!({
        "ok": true,
        "camera_id": camera_id,
        "frame_ref": frame_ref,
        "url": url.url,
        "expires_unix_ms": url.expires_unix_ms,
    })
}

// =============================================================================
// Flow block
// =============================================================================

/// Flow block 'uppercase' — takes the Text payload, returns it uppercased
/// inside a flow envelope.
fn handle_flow_block(block_type: &str, params: &Value) -> Value {
    match block_type {
        "uppercase" => {
            let text = params
                .get("payload")
                .and_then(|p| p.get("Text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            json!({"payload": {"Text": text.to_uppercase()}})
        }
        _ => json!({"ok": false, "error": format!("unknown flow block: {}", block_type)}),
    }
}

// =============================================================================
// UI actions
// =============================================================================

fn handle_ui_action(action_id: &str, params: &Value) -> Value {
    match action_id {
        "increment" => {
            let counter = unsafe {
                COUNTER += 1;
                COUNTER
            };
            send_state_patch(vec![PatchOp {
                path: state_path("counter"),
                op: PatchOpKind::Set {
                    value: CborValue::U64(counter),
                },
            }]);
            json!({"ok": true, "counter": counter})
        }
        "panel-navigate" => {
            let tab = params
                .get("item_id")
                .or_else(|| params.get("panel_id"))
                .or_else(|| params.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_TAB)
                .to_string();
            set_active_tab(&tab);
            send_tab_content(&tab);
            json!({"ok": true, "tab": tab})
        }
        "refresh" => {
            let tab = active_tab();
            send_tab_content(&tab);
            notify_with_level("SDK Showcase", "Panel refreshed", "info");
            let _ = publish_event("showcase.refresh", json!({"addon": ADDON_ID, "tab": tab}));
            json!({"ok": true})
        }
        "run-kv-demo" => {
            let result = handle_test_storage(&json!({
                "key": "demo_key",
                "value": "demo_value",
            }));
            let summary = if result["ok"] == true {
                format!(
                    "KV round-trip OK: wrote '{}', read '{}'",
                    result["data"]["written"].as_str().unwrap_or(""),
                    result["data"]["read"].as_str().unwrap_or("")
                )
            } else {
                format!("KV round-trip FAILED: {}", result["error"])
            };
            patch_demo_result(&summary);
            result
        }
        "run-sql-demo" => {
            let result = run_sql_suite();
            let summary = if result["ok"] == true {
                format!(
                    "SQL suite OK: {} rows selected, transaction affected {}, DDL blocked: {}",
                    result["query"]["rows"],
                    result["transaction"]["rows_affected_total"],
                    result["ddl_block"]["blocked"]
                )
            } else {
                format!("SQL suite FAILED: {}", result)
            };
            patch_demo_result(&summary);
            result
        }
        "run-vector-demo" => {
            let result = run_vector_suite();
            let summary = if result["ok"] == true {
                format!(
                    "Vector suite OK: {} vectors, best hit ref_id=1: {}, deleted ref 3: {}",
                    result["count_after_upsert"], result["best_is_ref1"], result["deleted_ref3"]
                )
            } else {
                format!("Vector suite FAILED: {}", result)
            };
            patch_demo_result(&summary);
            result
        }
        other => json!({"ok": true, "ignored": other}),
    }
}

fn patch_demo_result(text: &str) {
    send_state_patch(vec![PatchOp {
        path: state_path("demo_result"),
        op: PatchOpKind::Set {
            value: CborValue::Text(text.into()),
        },
    }]);
}

// =============================================================================
// CBOR UI — panel shell, tabs, slot content
// =============================================================================

fn send_ui(payload: &UiPayload) -> i32 {
    let mut buf = Vec::with_capacity(1024);
    minicbor::encode(payload, &mut buf).expect("UiPayload encode");
    unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) }
}

fn state_path(key: &str) -> StatePath {
    StatePath::new(vec![PathSegment::Key(key.into())])
}

fn lit(text: &str) -> BindRef {
    BindRef::Literal(CborValue::Text(text.into()))
}

fn bound(key: &str) -> BindRef {
    BindRef::Bound(state_path(key))
}

fn nav_tab(id: &str, label: &str) -> NavTab {
    NavTab {
        id: id.into(),
        label: lit(label),
        icon: None,
        badge: None,
        panel_id: None,
        locked: false,
    }
}

fn backend_handler(event: EventKind, action_id: &str) -> (EventKind, Handler) {
    (
        event,
        Handler::Backend {
            action_id: action_id.into(),
            params: CborMap(vec![]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )
}

fn send_panel_shell() {
    let mut nav = NavTabs {
        items: vec![
            nav_tab("live", "Live"),
            nav_tab("molecules", "Molecules"),
            nav_tab("layout", "Layout"),
            nav_tab("data", "Data"),
            nav_tab("form", "Form"),
            nav_tab("action", "Action"),
            nav_tab("feedback", "Feedback"),
            nav_tab("specialized", "Specialized"),
            nav_tab("storage", "SQL / KV / Vector"),
        ],
        active_id: bound("active_tab"),
        variant: NavTabsVariant::Underlined,
        scroll_overflow: true,
    }
    .into_component("nav-tabs")
    .expect("NavTabs encode");
    nav.handlers = Some(HandlerMap(vec![backend_handler(
        EventKind::Select,
        "panel-navigate",
    )]));

    let body = Inspector {
        title: lit("SDK Showcase"),
        content_slot: SLOT_ID.into(),
        actions: vec![],
        tabs: None,
        collapsible: false,
    }
    .into_component("content-host")
    .expect("Inspector encode");

    let layout = Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children: vec![nav, body],
        padding: None,
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component("root")
    .expect("Stack encode");

    let shell = PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        layout,
        slots: vec![SlotDecl {
            id: SLOT_ID.into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Always,
            max_payload_bytes: None,
        }],
        initial_state: vec![
            StateEntry {
                path: state_path("counter"),
                value: CborValue::U64(0),
            },
            StateEntry {
                path: state_path("tick_counter"),
                value: CborValue::U64(read_tick_counter()),
            },
            StateEntry {
                path: state_path("active_tab"),
                value: CborValue::Text(DEFAULT_TAB.into()),
            },
            StateEntry {
                path: state_path("demo_result"),
                value: CborValue::Text("Run a demo to see results here.".into()),
            },
        ],
        initial_commands: vec![],
    };

    send_ui(&UiPayload::PanelShell(shell));
}

fn send_tab_content(tab: &str) {
    let fragment = build_tab_content(tab);

    let mut state_overlay = vec![StateEntry {
        path: state_path("active_tab"),
        value: CborValue::Text(tab.into()),
    }];
    // The Data tab hosts the chart/data-viz catalog samples, which read their
    // plotted points/slices/cells from `["charts", ...]` state paths. Seed that
    // data alongside the fragment so the charts render real curves, not blanks.
    if tab == "data" {
        state_overlay.extend(catalog::chart_state_entries());
    }

    let slot_content = SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        slot_id: SLOT_ID.into(),
        fragment,
        state_overlay: Some(state_overlay),
    };

    send_ui(&UiPayload::SlotContent(slot_content));
}

fn build_tab_content(tab: &str) -> Component {
    if let Some(section) = catalog::section_for_tab(tab) {
        return catalog::section_stack(tab, section);
    }
    match tab {
        "storage" => storage_tab(),
        _ => live_tab(),
    }
}

fn heading(id: &str, content: &str) -> Component {
    Heading {
        content: lit(content),
        level: 2,
        tone: None,
        align: None,
    }
    .into_component(id)
    .expect("Heading encode")
}

fn body_text(id: &str, content: BindRef) -> Component {
    Text {
        content,
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
        streaming: None,
    }
    .into_component(id)
    .expect("Text encode")
}

fn action_button(id: &str, label: &str, action_id: &str, variant: ButtonVariant) -> Component {
    let mut button = Button {
        variant,
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
    .into_component(id)
    .expect("Button encode");
    button.handlers = Some(HandlerMap(vec![backend_handler(EventKind::Click, action_id)]));
    button
}

/// Live state demo — reactive counter (StatePatch on increment), background
/// tick counter and refresh.
fn live_tab() -> Component {
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children: vec![
            heading("live-heading", "Live state demo"),
            body_text(
                "live-info",
                lit("Increment patches state via the binary CBOR protocol; the service tick advances every 2 s."),
            ),
            body_text("live-counter", bound("counter")),
            body_text("live-tick-counter", bound("tick_counter")),
            action_button("btn-increment", "Increment", "increment", ButtonVariant::Primary),
            action_button("btn-refresh", "Refresh", "refresh", ButtonVariant::Secondary),
        ],
        padding: Some(Spacing::Md),
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component("tab-live")
    .expect("Stack encode")
}

/// SQL / KV / Vector demo — buttons run the storage suites and the bound
/// result text is updated through StatePatch.
fn storage_tab() -> Component {
    Stack {
        gap: Spacing::Md,
        align: FlexAlign::Stretch,
        children: vec![
            heading("storage-heading", "SQL / KV / Vector demos"),
            body_text("storage-result", bound("demo_result")),
            action_button("btn-kv-demo", "Run KV round-trip", "run-kv-demo", ButtonVariant::Primary),
            action_button("btn-sql-demo", "Run SQL suite", "run-sql-demo", ButtonVariant::Primary),
            action_button(
                "btn-vector-demo",
                "Run vector suite",
                "run-vector-demo",
                ButtonVariant::Primary,
            ),
        ],
        padding: Some(Spacing::Md),
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component("tab-storage")
    .expect("Stack encode")
}

// =============================================================================
// State patches & persisted tick counter
// =============================================================================

fn send_state_patch(ops: Vec<PatchOp>) {
    let base = unsafe { STATE_REVISION };
    let new = base + 1;

    let patch = StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: panel_epoch(),
        base_revision: base,
        new_revision: new,
        ops,
    };

    // The host advances its expected revision only when it accepts the patch;
    // advancing locally on rejection would drift the counters apart forever.
    if send_ui(&UiPayload::StatePatch(patch)) == 0 {
        unsafe {
            STATE_REVISION = new;
        }
    }
}

fn read_tick_counter() -> u64 {
    store_get("tick_counter")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn write_tick_counter(v: u64) {
    let _ = store_set("tick_counter", &v.to_string());
}

// =============================================================================
// Response helper — writes the JSON response into the output buffer
// =============================================================================

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };

    let written = write_string(out_ptr, out_cap, &response_str);
    if written < 0 {
        log::error("output buffer too small for response");
        return 2;
    }

    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);

    0
}
