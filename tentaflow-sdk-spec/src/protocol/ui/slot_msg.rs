// =============================================================================
// File: protocol/ui/slot_msg.rs — slot mutation wire messages (§6.3)
// Purpose: SlotContent (0x0110), SlotClear (0x0111), SlotShow (0x0112),
// SlotHide (0x0113). SlotContent replaces slot fragment + optional atomic
// state_overlay.
// =============================================================================

use minicbor::{Decode, Encode};

use super::component::Component;
use super::slot::StateEntry;

/// `SlotContent` (0x0110). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SlotContent {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub slot_id: String,
    #[n(4)]
    pub fragment: Component,
    /// Atomic state mutations applied in the same wire frame as `fragment`.
    /// Caller MUST sort by canonical encoded StatePath bytes; we do not re-sort here.
    #[n(5)]
    pub state_overlay: Option<Vec<StateEntry>>,
}

/// `SlotClear` (0x0111). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct SlotClear {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub slot_id: String,
}

/// `SlotShow` (0x0112). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct SlotShow {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub slot_id: String,
}

/// `SlotHide` (0x0113). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct SlotHide {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub slot_id: String,
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
    }

    fn empty_component() -> Component {
        Component {
            tag: 0x0001,
            id: "x".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    #[test]
    fn slot_content_roundtrip_with_overlay() {
        rt(SlotContent {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "main".into(),
            fragment: empty_component(),
            state_overlay: Some(vec![StateEntry {
                path: StatePath::new(vec![PathSegment::Key("count".into())]),
                value: Value::U64(7),
            }]),
        });
    }

    #[test]
    fn slot_content_roundtrip_without_overlay() {
        rt(SlotContent {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "main".into(),
            fragment: empty_component(),
            state_overlay: None,
        });
    }

    #[test]
    fn slot_clear_show_hide_roundtrip() {
        rt(SlotClear {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "drawer".into(),
        });
        rt(SlotShow {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "modal".into(),
        });
        rt(SlotHide {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            slot_id: "modal".into(),
        });
    }
}
