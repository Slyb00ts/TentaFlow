// =============================================================================
// File: protocol/ui/mod.rs — UI channel typed primitives (catalog §1)
// Purpose: semantic tokens, ValueFormat, StatePath/BindRef/BindSpec,
// Accessibility, Visibility and EventKind. Concrete components (§2–§7) and
// Handler/LocalAction recursion land in later chunks.
// See docs/ADDON_UI_COMPONENT_CATALOG_v1.md.
// =============================================================================

pub mod a11y;
pub mod action;
pub mod actions;
pub mod bind;
pub mod command;
pub mod component;
pub mod data;
pub mod error_code;
pub mod event;
pub mod feedback;
pub mod form;
pub mod handler;
pub mod icon_name;
pub mod inline;
pub mod layout;
pub mod molecules;
pub mod panel;
pub mod patch;
pub mod schema;
pub mod slot;
pub mod slot_msg;
pub mod specialized;
pub mod state;
pub mod tokens;
pub mod typed_field;
pub mod ui_payload;
pub mod validation;
pub mod value_format;
