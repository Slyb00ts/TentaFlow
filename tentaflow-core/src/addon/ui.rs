// === File: tentaflow-core/src/addon/ui.rs — re-export of typed UI primitives ===
//
// The schema lives in the standalone `tentaflow-ui-schema` crate so it can
// be shared with `tentaflow-addon-sdk` (guest WASM). Host-side validators
// (`parse_and_validate_ui_json`, `validate_and_normalize_component`) and
// every type used to live under `addon::ui::*`; downstream callers still
// import them through that path via this re-export.

pub use tentaflow_ui_schema::*;
