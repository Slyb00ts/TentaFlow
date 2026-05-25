// =============================================================================
// File: addons/test-app-addon/src/lib.rs
// E2E test addon — validates full CBOR UI pipeline with state management,
// service tick, 3 actions (refresh, increment, submit_form), event publishing,
// notifications, and a flow block (uppercase).
// =============================================================================

use tentaflow_sdk_spec::protocol::ui::bind::{BindRef, PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, FieldMap};
use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
use tentaflow_sdk_spec::protocol::ui::slot::{
    CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
};
use tentaflow_sdk_spec::protocol::ui::slot_msg::SlotContent;
use tentaflow_sdk_spec::protocol::ui::state::StatePatch;
use tentaflow_sdk_spec::protocol::ui::ui_payload::UiPayload;
use tentaflow_sdk_spec::protocol::value::Value;

// =============================================================================
// Host function imports
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
    #[link_name = "log_info"]
    fn host_log_info(msg_ptr: i32, msg_len: i32) -> i32;
    #[link_name = "log_warn"]
    fn host_log_warn(msg_ptr: i32, msg_len: i32) -> i32;
    #[link_name = "log_error"]
    fn host_log_error(msg_ptr: i32, msg_len: i32) -> i32;
    fn store_get(key_ptr: i32, key_len: i32, out_ptr: i32, out_cap: i32) -> i32;
    fn store_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32) -> i32;
    fn event_publish(
        event_type_ptr: i32,
        event_type_len: i32,
        payload_ptr: i32,
        payload_len: i32,
    ) -> i32;
    fn ui_notify(
        title_ptr: i32,
        title_len: i32,
        body_ptr: i32,
        body_len: i32,
        level_ptr: i32,
        level_len: i32,
    ) -> i32;
}

// =============================================================================
// Constants
// =============================================================================

const ADDON_ID: &str = "test-app-addon";
const PANEL_ID: &str = "main";
const PANEL_EPOCH: u64 = 1;
const SLOT_ID: &str = "content";

static mut STATE_REVISION: u64 = 0;

// =============================================================================
// Lifecycle exports
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log_info("test-app-addon installed");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log_info("test-app-addon started — sending PanelShell + SlotContent");

    let counter = read_counter();

    let shell = PanelShell {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: PANEL_EPOCH,
        layout: Component {
            tag: 0x0103, // Stack
            id: "root".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        },
        slots: vec![SlotDecl {
            id: SLOT_ID.into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Always,
            max_payload_bytes: None,
        }],
        initial_state: vec![StateEntry {
            path: counter_path(),
            value: Value::U64(counter as u64),
        }],
        initial_commands: vec![],
    };

    send_ui(&UiPayload::PanelShell(shell));
    send_content_slot(counter);
    0
}

#[no_mangle]
pub extern "C" fn on_tick(_timestamp_ms: i64) -> i32 {
    let counter = read_counter() + 1;
    write_counter(counter);
    log_info(&format!("on_tick: counter -> {}", counter));

    send_state_patch(counter);
    send_content_slot(counter);
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log_info("test-app-addon stopped");
    0
}

#[no_mangle]
pub extern "C" fn on_event(event_ptr: i32, event_len: i32) -> i32 {
    let event_json = unsafe { read_guest_str(event_ptr, event_len) };
    log_info(&format!("on_event: {}", event_json));
    0
}

// =============================================================================
// on_request — UI actions + flow block dispatch
// =============================================================================

#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = unsafe { read_guest_str(input_ptr, input_len) };

    let tool = extract_json_str(&input_json, "tool");
    let params_raw = extract_json_field(&input_json, "params");

    let response = if let Some(action) = tool.strip_prefix("ui.main.") {
        handle_action(action, &params_raw)
    } else if tool.starts_with("block.uppercase") {
        handle_flow_block_uppercase(&params_raw)
    } else {
        format!(r#"{{"error":"unknown tool '{}'"}}"#, tool)
    };

    write_response(out_ptr, out_cap, out_len_ptr, &response);
    0
}

// =============================================================================
// Action handlers
// =============================================================================

fn handle_action(action: &str, params_raw: &str) -> String {
    log_info(&format!("action '{}' params: {}", action, params_raw));

    match action {
        "refresh" => {
            let counter = read_counter();
            send_content_slot(counter);
            notify("Test App", "Panel refreshed", "info");
            publish("test.refresh", r#"{"addon":"test-app-addon"}"#);
            r#"{"ok":true}"#.into()
        }
        "increment" => {
            let counter = read_counter() + 10;
            write_counter(counter);
            send_state_patch(counter);
            send_content_slot(counter);
            format!(r#"{{"ok":true,"counter":{}}}"#, counter)
        }
        "submit_form" => {
            let username = extract_json_str(params_raw, "username");
            let color = extract_json_str(params_raw, "color");
            log_info(&format!("form submit: username={}, color={}", username, color));
            store_set_str("last_username", &username);
            store_set_str("last_color", &color);
            let counter = read_counter();
            send_content_slot(counter);
            notify("Test App", &format!("Form: {} ({})", username, color), "success");
            format!(r#"{{"ok":true,"echo":{}}}"#, params_raw)
        }
        _ => format!(r#"{{"error":"unknown action '{}'"}}"#, action),
    }
}

fn handle_flow_block_uppercase(params_raw: &str) -> String {
    log_info("flow block uppercase invoked");
    let text = extract_nested_json_str(params_raw, "payload", "Text");
    let upper = text.to_uppercase();
    // Return flow envelope with transformed payload
    format!(r#"{{"payload":{{"Text":"{}"}}}}"#, upper)
}

// =============================================================================
// UI payload builders
// =============================================================================

fn counter_path() -> StatePath {
    StatePath::new(vec![PathSegment::Key("counter".into())])
}

fn send_state_patch(counter: u32) {
    let (base, new) = unsafe {
        let base = STATE_REVISION;
        STATE_REVISION += 1;
        (base, STATE_REVISION)
    };

    let patch = StatePatch {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: PANEL_EPOCH,
        base_revision: base,
        new_revision: new,
        ops: vec![PatchOp {
            path: counter_path(),
            op: PatchOpKind::Set {
                value: Value::U64(counter as u64),
            },
        }],
    };

    send_ui(&UiPayload::StatePatch(patch));
}

fn send_content_slot(counter: u32) {
    // Build a vertical layout with text + buttons as raw Components.
    // Text component (tag 0x0201): field 0 = content (BindRef literal)
    let text_counter = Component {
        tag: 0x0201,
        id: "txt-counter".into(),
        fields: FieldMap(vec![(0, encode_bind_ref_literal(&format!("Counter: {}", counter)))]),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    };

    let text_info = Component {
        tag: 0x0201,
        id: "txt-info".into(),
        fields: FieldMap(vec![(0, encode_bind_ref_literal("Test App — tick increments +1, button +10"))]),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    };

    // Button "Increment +10" (tag 0x0401):
    //   field 0 = variant, field 1 = tone, field 2 = label, field 5 = size,
    //   field 6 = full_width, field 9 = density
    let btn_increment = Component {
        tag: 0x0401,
        id: "btn-increment".into(),
        fields: FieldMap(vec![
            (0, Value::Text("primary".into())),
            (1, Value::Text("neutral".into())),
            (2, encode_bind_ref_literal("Increment +10")),
            (5, Value::Text("md".into())),
            (6, Value::Bool(false)),
            (9, Value::Text("default".into())),
        ]),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    };

    let btn_refresh = Component {
        tag: 0x0401,
        id: "btn-refresh".into(),
        fields: FieldMap(vec![
            (0, Value::Text("secondary".into())),
            (1, Value::Text("neutral".into())),
            (2, encode_bind_ref_literal("Refresh")),
            (5, Value::Text("md".into())),
            (6, Value::Bool(false)),
            (9, Value::Text("default".into())),
        ]),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    };

    // Stack container (0x0103) wrapping all children
    let stack = Component {
        tag: 0x0103,
        id: "content-stack".into(),
        fields: FieldMap(vec![(0, encode_children(&[
            text_info,
            text_counter,
            btn_increment,
            btn_refresh,
        ]))]),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    };

    let slot_content = SlotContent {
        addon_id: ADDON_ID.into(),
        panel_id: PANEL_ID.into(),
        panel_epoch: PANEL_EPOCH,
        slot_id: SLOT_ID.into(),
        fragment: stack,
        state_overlay: None,
    };

    send_ui(&UiPayload::SlotContent(slot_content));
}

// =============================================================================
// CBOR helpers
// =============================================================================

fn send_ui(payload: &UiPayload) -> i32 {
    let mut buf = Vec::with_capacity(512);
    minicbor::encode(payload, &mut buf).unwrap();
    unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) }
}

/// Encode a BindRef::Literal(Value::Text(s)) to a Value representation.
/// BindRef encodes as a CBOR map { "kind": "literal", "value": <cbor> }.
/// We produce this as a nested CBOR blob stored in Value::Bytes.
fn encode_bind_ref_literal(s: &str) -> Value {
    let bind = BindRef::Literal(Value::Text(s.into()));
    let mut buf = Vec::with_capacity(64);
    minicbor::encode(&bind, &mut buf).unwrap();
    // FieldMap values are Value; a BindRef is itself CBOR-encodable so we
    // store the raw CBOR bytes. The host decoder will decode them in place.
    Value::Bytes(buf)
}

/// Encode a Vec<Component> to Value::Bytes (array of CBOR-encoded components).
fn encode_children(children: &[Component]) -> Value {
    let mut buf = Vec::with_capacity(256);
    minicbor::encode(children, &mut buf).unwrap();
    Value::Bytes(buf)
}

// =============================================================================
// Storage helpers
// =============================================================================

fn read_counter() -> u32 {
    let val = store_get_str("tick_counter");
    val.and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn write_counter(v: u32) {
    store_set_str("tick_counter", &v.to_string());
}

fn store_get_str(key: &str) -> Option<String> {
    let mut buf = [0u8; 128];
    let ret = unsafe {
        store_get(
            key.as_ptr() as i32,
            key.len() as i32,
            buf.as_mut_ptr() as i32,
            buf.len() as i32,
        )
    };
    if ret <= 0 {
        return None;
    }
    let slice = &buf[..ret as usize];
    core::str::from_utf8(slice).ok().map(|s| s.to_string())
}

fn store_set_str(key: &str, val: &str) {
    unsafe {
        store_set(
            key.as_ptr() as i32,
            key.len() as i32,
            val.as_ptr() as i32,
            val.len() as i32,
        );
    }
}

// =============================================================================
// Logging / notifications / events
// =============================================================================

fn log_info(msg: &str) {
    unsafe { host_log_info(msg.as_ptr() as i32, msg.len() as i32); }
}

fn notify(title: &str, body: &str, level: &str) {
    unsafe {
        ui_notify(
            title.as_ptr() as i32,
            title.len() as i32,
            body.as_ptr() as i32,
            body.len() as i32,
            level.as_ptr() as i32,
            level.len() as i32,
        );
    }
}

fn publish(event_type: &str, payload_json: &str) {
    unsafe {
        event_publish(
            event_type.as_ptr() as i32,
            event_type.len() as i32,
            payload_json.as_ptr() as i32,
            payload_json.len() as i32,
        );
    }
}

// =============================================================================
// Guest memory helpers
// =============================================================================

unsafe fn read_guest_str(ptr: i32, len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    let slice = core::slice::from_raw_parts(ptr as *const u8, len as usize);
    core::str::from_utf8(slice).unwrap_or("").to_string()
}

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(out_cap as usize);
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr as *mut u8, n);
        let p = out_len_ptr as *mut i32;
        *p = n as i32;
    }
}

// =============================================================================
// Minimal JSON string extraction (no full parser needed for test addon)
// =============================================================================

fn extract_json_str(json: &str, key: &str) -> String {
    // Finds "key":"value" pattern. Good enough for flat test payloads.
    let needle = format!(r#""{}":"#, key);
    if let Some(start) = json.find(&needle) {
        let after = &json[start + needle.len()..];
        if after.starts_with('"') {
            let inner = &after[1..];
            if let Some(end) = inner.find('"') {
                return inner[..end].to_string();
            }
        }
    }
    String::new()
}

fn extract_json_field(json: &str, key: &str) -> String {
    // Extract raw value of "key": <anything> (object, string, number).
    let needle = format!(r#""{}":"#, key);
    if let Some(start) = json.find(&needle) {
        let after = &json[start + needle.len()..];
        // Find balanced end: if starts with '{', find matching '}'
        let trimmed = after.trim_start();
        if trimmed.starts_with('{') {
            let mut depth = 0i32;
            for (i, ch) in trimmed.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return trimmed[..=i].to_string();
                        }
                    }
                    _ => {}
                }
            }
        }
        // Fallback: return up to next comma or closing brace
        let end = trimmed
            .find(|c: char| c == ',' || c == '}')
            .unwrap_or(trimmed.len());
        return trimmed[..end].to_string();
    }
    "{}".into()
}

fn extract_nested_json_str(json: &str, outer_key: &str, inner_key: &str) -> String {
    let outer = extract_json_field(json, outer_key);
    extract_json_str(&outer, inner_key)
}

// =============================================================================
// Guest memory allocator export for wasmtime host
// =============================================================================

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::alloc(layout) as i32 }
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: i32, size: i32) {
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).unwrap();
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) }
}
