// =============================================================================
// File: protocol/ui/panel.rs — panel lifecycle messages (§6.2)
// Purpose: typed PanelOpen + PanelOpenContext (with Viewport), PanelShell,
// PanelReady, PanelError, PanelClose (with CloseReason), PanelReset.
// =============================================================================

use minicbor::{Decode, Encode};

use super::command::Command;
use super::component::Component;
use super::error_code::ErrorCode;
use super::slot::{SlotDecl, StateEntry};

/// Client viewport geometry as observed at PanelOpen.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Viewport {
    #[n(0)]
    pub width_px: u32,
    #[n(1)]
    pub height_px: u32,
    #[n(2)]
    pub density: f64,
}

/// Panel-open context enriched by core (assigned_epoch is core-controlled).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct PanelOpenContext {
    #[n(0)]
    pub user_id: String,
    #[n(1)]
    pub locale: String,
    #[n(2)]
    pub theme: String,
    #[n(3)]
    pub viewport: Viewport,
    #[n(4)]
    pub deep_link: Option<String>,
    #[n(5)]
    pub prefers_reduced_motion: bool,
    #[n(6)]
    pub prefers_high_contrast: bool,
    /// Set by core, NOT by frontend. Addon MUST echo it back in PanelShell.
    #[n(7)]
    pub assigned_epoch: u64,
}

/// `PanelOpen` (0x0101). Frontend→Core; Core enriches with assigned_epoch
/// before dispatching to addon.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct PanelOpen {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub ctx: PanelOpenContext,
}

/// `PanelShell` (0x0102). Addon→Core→Frontend.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct PanelShell {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub layout: Component,
    #[n(4)]
    pub slots: Vec<SlotDecl>,
    #[n(5)]
    pub initial_state: Vec<StateEntry>,
    #[n(6)]
    pub initial_commands: Vec<Command>,
}

/// `PanelReady` (0x0103). Frontend→Core.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PanelReady {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub first_paint_ms: u32,
}

/// `PanelError` (0x0104). Core→Frontend.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PanelError {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub code: ErrorCode,
    /// Developer-facing message ≤ 256 chars. UI renders i18n by `code`.
    #[n(3)]
    pub message: String,
}

string_enum! {
    /// Reason a panel session is ending.
    pub enum CloseReason {
        UserNavigated = "user_navigated",
        ConnectionDropped = "connection_dropped",
        AddonUnloaded = "addon_unloaded",
        ServerInitiated = "server_initiated",
    }
}

/// `PanelClose` (0x0105). Frontend→Core→Addon.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PanelClose {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub reason: CloseReason,
}

/// `PanelReset` (0x0106). Core→Frontend. Always core-initiated.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct PanelReset {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    /// MUST be strictly greater than the panel's current epoch.
    #[n(2)]
    pub new_panel_epoch: u64,
    #[n(3)]
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{PathSegment, StatePath};
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::value::Value;

    fn rt<T>(v: T)
    where
        T: minicbor::Encode<()> + for<'b> minicbor::Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut b1 = Vec::new();
        minicbor::encode(&v, &mut b1).unwrap();
        let d: T = minicbor::decode(&b1).unwrap();
        assert_eq!(d, v);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    fn empty_component() -> Component {
        Component {
            tag: 0x0001,
            id: "root".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    #[test]
    fn panel_open_roundtrip() {
        rt(PanelOpen {
            addon_id: "tentavision".into(),
            panel_id: "cameras".into(),
            ctx: PanelOpenContext {
                user_id: "u-1".into(),
                locale: "pl-PL".into(),
                theme: "dark".into(),
                viewport: Viewport {
                    width_px: 1920,
                    height_px: 1080,
                    density: 1.5,
                },
                deep_link: None,
                prefers_reduced_motion: false,
                prefers_high_contrast: false,
                assigned_epoch: 7,
            },
        });
    }

    #[test]
    fn panel_shell_roundtrip() {
        rt(PanelShell {
            addon_id: "x".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            layout: empty_component(),
            slots: vec![],
            initial_state: vec![StateEntry {
                path: StatePath::new(vec![PathSegment::Key("k".into())]),
                value: Value::U64(0),
            }],
            initial_commands: vec![],
        });
    }

    #[test]
    fn panel_ready_roundtrip() {
        rt(PanelReady {
            addon_id: "x".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            first_paint_ms: 42,
        });
    }

    #[test]
    fn panel_error_roundtrip() {
        rt(PanelError {
            addon_id: "x".into(),
            panel_id: "p".into(),
            code: ErrorCode::AddonTimeout,
            message: "addon did not respond in 2000ms".into(),
        });
    }

    #[test]
    fn panel_close_roundtrip() {
        rt(PanelClose {
            addon_id: "x".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            reason: CloseReason::UserNavigated,
        });
    }

    #[test]
    fn panel_reset_roundtrip() {
        rt(PanelReset {
            addon_id: "x".into(),
            panel_id: "p".into(),
            new_panel_epoch: 2,
            reason: "addon-crashed".into(),
        });
    }
}
