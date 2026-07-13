// =============================================================================
// File: protocol/ui/layout/atomic.rs — Divider/Spacer/Tooltip (catalog §3)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::tokens::{DividerOrientation, DividerVariant, DrawerSide, SpacerAxis};
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
// Divider
// -----------------------------------------------------------------------------
/// Horizontal/vertical line (catalog §3 0x0108).
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    pub orientation: DividerOrientation,
    pub variant: DividerVariant,
    pub spacing: super::super::tokens::Spacing,
    pub label: Option<BindRef>,
}

impl Divider {
    pub const TAG: u16 = 0x0108;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.orientation)?));
        entries.push((1, encode_to_value(&self.variant)?));
        entries.push((2, encode_to_value(&self.spacing)?));
        if let Some(l) = &self.label {
            entries.push((3, encode_to_value(l)?));
        }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Divider")?;
        ensure_no_duplicate_keys("Divider", &c.fields.0)?;
        let mut orientation = None;
        let mut variant = None;
        let mut spacing = None;
        let mut label = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => orientation = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => spacing = Some(decode_from_value(v)?),
                3 => label = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Divider", *other)),
            }
        }
        Ok(Divider {
            orientation: orientation.ok_or_else(|| missing_field("Divider", "orientation"))?,
            variant: variant.ok_or_else(|| missing_field("Divider", "variant"))?,
            spacing: spacing.ok_or_else(|| missing_field("Divider", "spacing"))?,
            label,
        })
    }
}

// -----------------------------------------------------------------------------
// Spacer
// -----------------------------------------------------------------------------
/// Empty layout space (catalog §3 0x0109).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacer {
    pub size: super::super::tokens::Spacing,
    pub axis: SpacerAxis,
}

impl Spacer {
    pub const TAG: u16 = 0x0109;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(2);
        entries.push((0, encode_to_value(&self.size)?));
        entries.push((1, encode_to_value(&self.axis)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Spacer")?;
        ensure_no_duplicate_keys("Spacer", &c.fields.0)?;
        let mut size = None;
        let mut axis = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => size = Some(decode_from_value(v)?),
                1 => axis = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Spacer", *other)),
            }
        }
        Ok(Spacer {
            size: size.ok_or_else(|| missing_field("Spacer", "size"))?,
            axis: axis.ok_or_else(|| missing_field("Spacer", "axis"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Tooltip
// -----------------------------------------------------------------------------
/// Hover/focus popup with short description (catalog §3 0x010F).
#[derive(Debug, Clone, PartialEq)]
pub struct Tooltip {
    pub child: Component,
    pub content: BindRef,
    pub side: DrawerSide,
    pub max_width_px: u16,
}

impl Tooltip {
    pub const TAG: u16 = 0x010F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.child)?));
        entries.push((1, encode_to_value(&self.content)?));
        entries.push((2, encode_to_value(&self.side)?));
        entries.push((3, encode_to_value(&self.max_width_px)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Tooltip")?;
        ensure_no_duplicate_keys("Tooltip", &c.fields.0)?;
        let mut child = None;
        let mut content = None;
        let mut side = None;
        let mut max_width_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => child = Some(decode_from_value(v)?),
                1 => content = Some(decode_from_value(v)?),
                2 => side = Some(decode_from_value(v)?),
                3 => max_width_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Tooltip", *other)),
            }
        }
        Ok(Tooltip {
            child: child.ok_or_else(|| missing_field("Tooltip", "child"))?,
            content: content.ok_or_else(|| missing_field("Tooltip", "content"))?,
            side: side.ok_or_else(|| missing_field("Tooltip", "side"))?,
            max_width_px: max_width_px.ok_or_else(|| missing_field("Tooltip", "max_width_px"))?,
        })
    }
}
