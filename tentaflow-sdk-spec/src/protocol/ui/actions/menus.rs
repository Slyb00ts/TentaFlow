// =============================================================================
// File: protocol/ui/action/menus.rs — MenuButton/Menu (catalog §6)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{IconRef, MenuItem};
use super::super::tokens::{ButtonVariant, MenuPlacement};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component {
        tag,
        id: id.into(),
        fields: FieldMap(fields),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    }
}

// -----------------------------------------------------------------------------
// 0x0406 — MenuButton
// -----------------------------------------------------------------------------

/// Button with dropdown menu (catalog §6 0x0406).
#[derive(Debug, Clone, PartialEq)]
pub struct MenuButton {
    pub trigger_label: Option<BindRef>,
    pub trigger_icon: Option<IconRef>,
    pub trigger_variant: ButtonVariant,
    pub items: Vec<MenuItem>,
    pub placement: MenuPlacement,
}

impl MenuButton {
    pub const TAG: u16 = 0x0406;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        if let Some(v) = &self.trigger_label {
            e.push((0, encode_to_value(v)?));
        }
        if let Some(v) = &self.trigger_icon {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.trigger_variant)?));
        e.push((3, encode_to_value(&self.items)?));
        e.push((4, encode_to_value(&self.placement)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "MenuButton")?;
        ensure_no_duplicate_keys("MenuButton", &c.fields.0)?;
        let mut trigger_label = None;
        let mut trigger_icon = None;
        let mut trigger_variant = None;
        let mut items = None;
        let mut placement = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => trigger_label = Some(decode_from_value(v)?),
                1 => trigger_icon = Some(decode_from_value(v)?),
                2 => trigger_variant = Some(decode_from_value(v)?),
                3 => items = Some(decode_from_value(v)?),
                4 => placement = Some(decode_from_value(v)?),
                other => return Err(unknown_field("MenuButton", *other)),
            }
        }
        Ok(MenuButton {
            trigger_label,
            trigger_icon,
            trigger_variant: trigger_variant
                .ok_or_else(|| missing_field("MenuButton", "trigger_variant"))?,
            items: items.ok_or_else(|| missing_field("MenuButton", "items"))?,
            placement: placement.ok_or_else(|| missing_field("MenuButton", "placement"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0407 — Menu
// -----------------------------------------------------------------------------

/// Standalone menu (catalog §6 0x0407).
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub items: Vec<MenuItem>,
    pub search: bool,
}

impl Menu {
    pub const TAG: u16 = 0x0407;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(2);
        e.push((0, encode_to_value(&self.items)?));
        e.push((1, encode_to_value(&self.search)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Menu")?;
        ensure_no_duplicate_keys("Menu", &c.fields.0)?;
        let mut items = None;
        let mut search = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => search = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Menu", *other)),
            }
        }
        Ok(Menu {
            items: items.ok_or_else(|| missing_field("Menu", "items"))?,
            search: search.ok_or_else(|| missing_field("Menu", "search"))?,
        })
    }
}
