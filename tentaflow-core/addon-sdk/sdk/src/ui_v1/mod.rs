// =============================================================================
// File: ui_v1/mod.rs — typed UI catalog v1 client (canonical CBOR over
// the `ui_render_cbor` host function)
//
// Rust addons build panels from the typed catalog structs re-exported in
// [`components_g`] (generated — see ./scripts/gen-rust.sh), wrap them in a
// spec `UiPayload` (PanelShell / SlotContent / StatePatch / …) and send the
// canonical CBOR bytes to the host with [`render`]. Encoding goes through
// `tentaflow-sdk-spec`'s own minicbor encoders, so the wire bytes are
// identical to what the host validator and the JS sdk-runtime decode.
// =============================================================================

pub mod components_g;

pub use components_g::*;

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    /// ABI: (cbor_ptr, cbor_len) -> 0 on success, negative AbiError code
    /// otherwise. Matches `host_functions/ui.rs::ui_render_cbor`.
    fn ui_render_cbor(cbor_ptr: i32, cbor_len: i32) -> i32;
}

/// Failure of [`render`]: guest-side CBOR encoding or host-side rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiRenderError {
    Encode(String),
    Host(i32),
}

impl core::fmt::Display for UiRenderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "UI payload CBOR encoding failed: {e}"),
            Self::Host(rc) => write!(f, "ui_render_cbor rejected payload (rc={rc})"),
        }
    }
}

impl std::error::Error for UiRenderError {}

/// Sends one UI-channel payload to the host, which validates the canonical
/// CBOR and forwards it to connected sessions. Requires the `ui` permission
/// in the addon manifest.
pub fn render(payload: &UiPayload) -> Result<(), UiRenderError> {
    let bytes =
        minicbor::to_vec(payload).map_err(|e| UiRenderError::Encode(e.to_string()))?;
    let rc = unsafe { ui_render_cbor(bytes.as_ptr() as i32, bytes.len() as i32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(UiRenderError::Host(rc))
    }
}

/// One-event handler map: backend action with no static params and the
/// default `toast` failure policy.
pub fn backend(kind: EventKind, action_id: impl Into<String>) -> HandlerMap {
    HandlerMap(vec![(
        kind,
        Handler::Backend {
            action_id: action_id.into(),
            params: CborMap::default(),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )])
}

/// One-event handler map: backend action carrying a single static
/// `{"key": <key>}` param, so one action id can serve many fields.
pub fn backend_kv(
    kind: EventKind,
    action_id: impl Into<String>,
    key: impl Into<String>,
) -> HandlerMap {
    HandlerMap(vec![(
        kind,
        Handler::Backend {
            action_id: action_id.into(),
            params: CborMap(vec![("key".to_string(), Value::Text(key.into()))]),
            optimistic: None,
            on_failure: FailurePolicy::Toast,
        },
    )])
}

/// One-event handler map running a client-side [`LocalAction`].
pub fn local(kind: EventKind, action: LocalAction) -> HandlerMap {
    HandlerMap(vec![(kind, Handler::Local(action))])
}

/// Dotted key path into panel state (`"user.name"` → `[Key("user"), Key("name")]`).
/// Array indices need explicit [`PathSegment::Index`] via [`StatePath::new`].
pub fn state_path(dotted: &str) -> StatePath {
    StatePath::new(
        dotted
            .split('.')
            .filter(|s| !s.is_empty())
            .map(|s| PathSegment::Key(s.to_string()))
            .collect(),
    )
}

/// Literal text bind (the most common `BindRef` shape).
pub fn lit(text: impl Into<String>) -> BindRef {
    BindRef::Literal(Value::Text(text.into()))
}

/// Literal bind carrying an arbitrary CBOR value.
pub fn lit_value(value: Value) -> BindRef {
    BindRef::Literal(value)
}

/// State-bound bind for a dotted key path.
pub fn bound(dotted: &str) -> BindRef {
    BindRef::Bound(state_path(dotted))
}
