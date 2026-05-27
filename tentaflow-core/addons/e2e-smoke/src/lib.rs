// =============================================================================
// File: addons/e2e-smoke/src/lib.rs
// E2E smoke test addon — minimal CBOR UI pipeline validation.
// Emits PanelShell on start, handles "increment" action with StatePatch.
// =============================================================================

use tentaflow_sdk_spec::protocol::ui::bind::{PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, FieldMap};
use tentaflow_sdk_spec::protocol::ui::panel::PanelShell;
use tentaflow_sdk_spec::protocol::ui::patch::{PatchOp, PatchOpKind};
use tentaflow_sdk_spec::protocol::ui::slot::{
    CachePolicy, SlotDecl, SlotDefault, SlotSemantics, SlotVisibility, StateEntry,
};
use tentaflow_sdk_spec::protocol::ui::state::StatePatch;
use tentaflow_sdk_spec::protocol::ui::ui_payload::UiPayload;
use tentaflow_sdk_spec::protocol::value::Value;

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
}

fn send_ui(payload: &UiPayload) -> i32 {
    let mut buf = Vec::with_capacity(256);
    minicbor::encode(payload, &mut buf).unwrap();
    unsafe { ui_render_cbor(buf.as_ptr() as i32, buf.len() as i32) }
}

static mut COUNTER: u64 = 0;
static mut STATE_REVISION: u64 = 0;

fn counter_path() -> StatePath {
    StatePath::new(vec![PathSegment::Key("counter".into())])
}

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    let shell = PanelShell {
        addon_id: "e2e-smoke".into(),
        panel_id: "main".into(),
        panel_epoch: 1,
        layout: Component {
            tag: 0x0201,
            id: "root".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        },
        slots: vec![SlotDecl {
            id: "content".into(),
            semantics: SlotSemantics::MainContent,
            default_state: SlotDefault::Loading,
            cache_policy: CachePolicy::None,
            visibility: SlotVisibility::Always,
            max_payload_bytes: None,
        }],
        initial_state: vec![StateEntry {
            path: counter_path(),
            value: Value::U64(0),
        }],
        initial_commands: vec![],
    };

    send_ui(&UiPayload::PanelShell(shell));
    0
}

#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    _out_ptr: i32,
    _out_cap: i32,
    _out_len_ptr: i32,
) -> i32 {
    let input_bytes =
        unsafe { core::slice::from_raw_parts(input_ptr as *const u8, input_len as usize) };

    let input_str = match core::str::from_utf8(input_bytes) {
        Ok(s) => s,
        Err(_) => return 1,
    };

    if !input_str.contains("ui.main.increment") {
        return 0;
    }

    unsafe {
        COUNTER += 1;
        STATE_REVISION += 1;
    }

    let (counter, rev) = unsafe { (COUNTER, STATE_REVISION) };

    let patch = StatePatch {
        addon_id: "e2e-smoke".into(),
        panel_id: "main".into(),
        panel_epoch: 1,
        base_revision: rev - 1,
        new_revision: rev,
        ops: vec![PatchOp {
            path: counter_path(),
            op: PatchOpKind::Set {
                value: Value::U64(counter),
            },
        }],
    };

    send_ui(&UiPayload::StatePatch(patch));
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    0
}

// Guest memory allocator export for wasmtime host.
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
