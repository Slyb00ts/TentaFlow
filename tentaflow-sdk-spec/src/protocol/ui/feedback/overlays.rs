// =============================================================================
// File: protocol/ui/feedback/overlays.rs — Modal/Drawer/Popover/Sheet (catalog §7)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::tokens::{DrawerSide, DrawerSize, ModalSize, PopoverPlacement, SheetDetent};
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
// 0x0509 — Modal
// -----------------------------------------------------------------------------

/// Modal dialog (catalog §7 0x0509).
#[derive(Debug, Clone, PartialEq)]
pub struct Modal {
    pub title: BindRef,
    pub subtitle: Option<BindRef>,
    pub body_slot: String,
    pub footer_slot: Option<String>,
    pub size: ModalSize,
    pub dismissible: bool,
    pub prevent_scroll: bool,
    pub closable: bool,
}

impl Modal {
    pub const TAG: u16 = 0x0509;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(8);
        e.push((0, encode_to_value(&self.title)?));
        if let Some(v) = &self.subtitle {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.body_slot)?));
        if let Some(v) = &self.footer_slot {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.size)?));
        e.push((5, encode_to_value(&self.dismissible)?));
        e.push((6, encode_to_value(&self.prevent_scroll)?));
        e.push((7, encode_to_value(&self.closable)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Modal")?;
        ensure_no_duplicate_keys("Modal", &c.fields.0)?;
        let mut title = None;
        let mut subtitle = None;
        let mut body_slot = None;
        let mut footer_slot = None;
        let mut size = None;
        let mut dismissible = None;
        let mut prevent_scroll = None;
        let mut closable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => subtitle = Some(decode_from_value(v)?),
                2 => body_slot = Some(decode_from_value(v)?),
                3 => footer_slot = Some(decode_from_value(v)?),
                4 => size = Some(decode_from_value(v)?),
                5 => dismissible = Some(decode_from_value(v)?),
                6 => prevent_scroll = Some(decode_from_value(v)?),
                7 => closable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Modal", *other)),
            }
        }
        Ok(Modal {
            title: title.ok_or_else(|| missing_field("Modal", "title"))?,
            subtitle,
            body_slot: body_slot.ok_or_else(|| missing_field("Modal", "body_slot"))?,
            footer_slot,
            size: size.ok_or_else(|| missing_field("Modal", "size"))?,
            dismissible: dismissible.ok_or_else(|| missing_field("Modal", "dismissible"))?,
            prevent_scroll: prevent_scroll
                .ok_or_else(|| missing_field("Modal", "prevent_scroll"))?,
            closable: closable.ok_or_else(|| missing_field("Modal", "closable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x050A — Drawer
// -----------------------------------------------------------------------------

/// Side drawer (catalog §7 0x050A).
#[derive(Debug, Clone, PartialEq)]
pub struct Drawer {
    pub side: DrawerSide,
    pub size: DrawerSize,
    pub title: Option<BindRef>,
    pub body_slot: String,
    pub footer_slot: Option<String>,
    pub dismissible: bool,
}

impl Drawer {
    pub const TAG: u16 = 0x050A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.side)?));
        e.push((1, encode_to_value(&self.size)?));
        if let Some(v) = &self.title {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.body_slot)?));
        if let Some(v) = &self.footer_slot {
            e.push((4, encode_to_value(v)?));
        }
        e.push((5, encode_to_value(&self.dismissible)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Drawer")?;
        ensure_no_duplicate_keys("Drawer", &c.fields.0)?;
        let mut side = None;
        let mut size = None;
        let mut title = None;
        let mut body_slot = None;
        let mut footer_slot = None;
        let mut dismissible = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => side = Some(decode_from_value(v)?),
                1 => size = Some(decode_from_value(v)?),
                2 => title = Some(decode_from_value(v)?),
                3 => body_slot = Some(decode_from_value(v)?),
                4 => footer_slot = Some(decode_from_value(v)?),
                5 => dismissible = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Drawer", *other)),
            }
        }
        Ok(Drawer {
            side: side.ok_or_else(|| missing_field("Drawer", "side"))?,
            size: size.ok_or_else(|| missing_field("Drawer", "size"))?,
            title,
            body_slot: body_slot.ok_or_else(|| missing_field("Drawer", "body_slot"))?,
            footer_slot,
            dismissible: dismissible.ok_or_else(|| missing_field("Drawer", "dismissible"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x050B — Popover
// -----------------------------------------------------------------------------

/// Anchored floating panel (catalog §7 0x050B).
#[derive(Debug, Clone, PartialEq)]
pub struct Popover {
    pub anchor_id: String,
    pub body_slot: String,
    pub placement: PopoverPlacement,
    pub dismissible: bool,
    pub arrow: bool,
}

impl Popover {
    pub const TAG: u16 = 0x050B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.anchor_id)?));
        e.push((1, encode_to_value(&self.body_slot)?));
        e.push((2, encode_to_value(&self.placement)?));
        e.push((3, encode_to_value(&self.dismissible)?));
        e.push((4, encode_to_value(&self.arrow)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Popover")?;
        ensure_no_duplicate_keys("Popover", &c.fields.0)?;
        let mut anchor_id = None;
        let mut body_slot = None;
        let mut placement = None;
        let mut dismissible = None;
        let mut arrow = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => anchor_id = Some(decode_from_value(v)?),
                1 => body_slot = Some(decode_from_value(v)?),
                2 => placement = Some(decode_from_value(v)?),
                3 => dismissible = Some(decode_from_value(v)?),
                4 => arrow = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Popover", *other)),
            }
        }
        Ok(Popover {
            anchor_id: anchor_id.ok_or_else(|| missing_field("Popover", "anchor_id"))?,
            body_slot: body_slot.ok_or_else(|| missing_field("Popover", "body_slot"))?,
            placement: placement.ok_or_else(|| missing_field("Popover", "placement"))?,
            dismissible: dismissible.ok_or_else(|| missing_field("Popover", "dismissible"))?,
            arrow: arrow.ok_or_else(|| missing_field("Popover", "arrow"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x050C — Sheet
// -----------------------------------------------------------------------------

/// Bottom sheet (catalog §7 0x050C).
#[derive(Debug, Clone, PartialEq)]
pub struct Sheet {
    pub title: Option<BindRef>,
    pub body_slot: String,
    pub footer_slot: Option<String>,
    pub detents: Vec<SheetDetent>,
    pub current_detent: Option<BindRef>,
    pub dismissible: bool,
}

impl Sheet {
    pub const TAG: u16 = 0x050C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        if let Some(v) = &self.title {
            e.push((0, encode_to_value(v)?));
        }
        e.push((1, encode_to_value(&self.body_slot)?));
        if let Some(v) = &self.footer_slot {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.detents)?));
        if let Some(v) = &self.current_detent {
            e.push((4, encode_to_value(v)?));
        }
        e.push((5, encode_to_value(&self.dismissible)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Sheet")?;
        ensure_no_duplicate_keys("Sheet", &c.fields.0)?;
        let mut title = None;
        let mut body_slot = None;
        let mut footer_slot = None;
        let mut detents = None;
        let mut current_detent = None;
        let mut dismissible = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => body_slot = Some(decode_from_value(v)?),
                2 => footer_slot = Some(decode_from_value(v)?),
                3 => detents = Some(decode_from_value(v)?),
                4 => current_detent = Some(decode_from_value(v)?),
                5 => dismissible = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Sheet", *other)),
            }
        }
        Ok(Sheet {
            title,
            body_slot: body_slot.ok_or_else(|| missing_field("Sheet", "body_slot"))?,
            footer_slot,
            detents: detents.ok_or_else(|| missing_field("Sheet", "detents"))?,
            current_detent,
            dismissible: dismissible.ok_or_else(|| missing_field("Sheet", "dismissible"))?,
        })
    }
}
