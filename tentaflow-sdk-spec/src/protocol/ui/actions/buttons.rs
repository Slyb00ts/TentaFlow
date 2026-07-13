// =============================================================================
// File: protocol/ui/action/buttons.rs — Button/IconButton/ButtonGroup/LinkButton/Link/Fab (catalog §6)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::IconRef;
use super::super::tokens::{
    ButtonGroupOrientation, ButtonSize, ButtonVariant, Density, FabPosition, FabSize,
    LinkUnderline, Tone,
};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_ref_tag_decode,
    ensure_ref_tag_encode, ensure_tag, missing_field, unknown_field, IntoComponentError,
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
// 0x0401 — Button
// -----------------------------------------------------------------------------

/// Standard button (catalog §6 0x0401).
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub variant: ButtonVariant,
    pub tone: Tone,
    pub label: BindRef,
    pub icon_leading: Option<IconRef>,
    pub icon_trailing: Option<IconRef>,
    pub size: ButtonSize,
    pub full_width: bool,
    pub disabled: Option<BindRef>,
    pub loading: Option<BindRef>,
    pub density: Density,
}

impl Button {
    pub const TAG: u16 = 0x0401;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(10);
        e.push((0, encode_to_value(&self.variant)?));
        e.push((1, encode_to_value(&self.tone)?));
        e.push((2, encode_to_value(&self.label)?));
        if let Some(v) = &self.icon_leading {
            e.push((3, encode_to_value(v)?));
        }
        if let Some(v) = &self.icon_trailing {
            e.push((4, encode_to_value(v)?));
        }
        e.push((5, encode_to_value(&self.size)?));
        e.push((6, encode_to_value(&self.full_width)?));
        if let Some(v) = &self.disabled {
            e.push((7, encode_to_value(v)?));
        }
        if let Some(v) = &self.loading {
            e.push((8, encode_to_value(v)?));
        }
        e.push((9, encode_to_value(&self.density)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Button")?;
        ensure_no_duplicate_keys("Button", &c.fields.0)?;
        let mut variant = None;
        let mut tone = None;
        let mut label = None;
        let mut icon_leading = None;
        let mut icon_trailing = None;
        let mut size = None;
        let mut full_width = None;
        let mut disabled = None;
        let mut loading = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => tone = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => icon_leading = Some(decode_from_value(v)?),
                4 => icon_trailing = Some(decode_from_value(v)?),
                5 => size = Some(decode_from_value(v)?),
                6 => full_width = Some(decode_from_value(v)?),
                7 => disabled = Some(decode_from_value(v)?),
                8 => loading = Some(decode_from_value(v)?),
                9 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Button", *other)),
            }
        }
        Ok(Button {
            variant: variant.ok_or_else(|| missing_field("Button", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("Button", "tone"))?,
            label: label.ok_or_else(|| missing_field("Button", "label"))?,
            icon_leading,
            icon_trailing,
            size: size.ok_or_else(|| missing_field("Button", "size"))?,
            full_width: full_width.ok_or_else(|| missing_field("Button", "full_width"))?,
            disabled,
            loading,
            density: density.ok_or_else(|| missing_field("Button", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0402 — IconButton
// -----------------------------------------------------------------------------

/// Button rendered as icon only (catalog §6 0x0402).
#[derive(Debug, Clone, PartialEq)]
pub struct IconButton {
    pub icon: IconRef,
    pub variant: ButtonVariant,
    pub tone: Tone,
    pub size: ButtonSize,
    pub aria_label: String,
    pub disabled: Option<BindRef>,
    pub loading: Option<BindRef>,
}

impl IconButton {
    pub const TAG: u16 = 0x0402;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.icon)?));
        e.push((1, encode_to_value(&self.variant)?));
        e.push((2, encode_to_value(&self.tone)?));
        e.push((3, encode_to_value(&self.size)?));
        e.push((4, encode_to_value(&self.aria_label)?));
        if let Some(v) = &self.disabled {
            e.push((5, encode_to_value(v)?));
        }
        if let Some(v) = &self.loading {
            e.push((6, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "IconButton")?;
        ensure_no_duplicate_keys("IconButton", &c.fields.0)?;
        let mut icon = None;
        let mut variant = None;
        let mut tone = None;
        let mut size = None;
        let mut aria_label = None;
        let mut disabled = None;
        let mut loading = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => icon = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => size = Some(decode_from_value(v)?),
                4 => aria_label = Some(decode_from_value(v)?),
                5 => disabled = Some(decode_from_value(v)?),
                6 => loading = Some(decode_from_value(v)?),
                other => return Err(unknown_field("IconButton", *other)),
            }
        }
        Ok(IconButton {
            icon: icon.ok_or_else(|| missing_field("IconButton", "icon"))?,
            variant: variant.ok_or_else(|| missing_field("IconButton", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("IconButton", "tone"))?,
            size: size.ok_or_else(|| missing_field("IconButton", "size"))?,
            aria_label: aria_label.ok_or_else(|| missing_field("IconButton", "aria_label"))?,
            disabled,
            loading,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0403 — ButtonGroup
// -----------------------------------------------------------------------------

/// Grouped buttons sharing style (catalog §6 0x0403).
#[derive(Debug, Clone, PartialEq)]
pub struct ButtonGroup {
    pub buttons: Vec<Component>,
    pub orientation: ButtonGroupOrientation,
    pub attached: bool,
}

impl ButtonGroup {
    pub const TAG: u16 = 0x0403;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        for b in &self.buttons {
            ensure_ref_tag_encode(b.tag, Button::TAG, "ButtonGroup", "buttons")?;
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(3);
        e.push((0, encode_to_value(&self.buttons)?));
        e.push((1, encode_to_value(&self.orientation)?));
        e.push((2, encode_to_value(&self.attached)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ButtonGroup")?;
        ensure_no_duplicate_keys("ButtonGroup", &c.fields.0)?;
        let mut buttons: Option<Vec<Component>> = None;
        let mut orientation = None;
        let mut attached = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => buttons = Some(decode_from_value(v)?),
                1 => orientation = Some(decode_from_value(v)?),
                2 => attached = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ButtonGroup", *other)),
            }
        }
        let buttons = buttons.ok_or_else(|| missing_field("ButtonGroup", "buttons"))?;
        for b in &buttons {
            ensure_ref_tag_decode(b.tag, Button::TAG, "ButtonGroup", "buttons")?;
        }
        Ok(ButtonGroup {
            buttons,
            orientation: orientation.ok_or_else(|| missing_field("ButtonGroup", "orientation"))?,
            attached: attached.ok_or_else(|| missing_field("ButtonGroup", "attached"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0404 — LinkButton
// -----------------------------------------------------------------------------

/// Link-styled button (catalog §6 0x0404).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkButton {
    pub label: BindRef,
    pub icon_leading: Option<IconRef>,
    pub icon_trailing: Option<IconRef>,
    pub tone: Tone,
    pub underline: LinkUnderline,
}

impl LinkButton {
    pub const TAG: u16 = 0x0404;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.label)?));
        if let Some(v) = &self.icon_leading {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.icon_trailing {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.tone)?));
        e.push((4, encode_to_value(&self.underline)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "LinkButton")?;
        ensure_no_duplicate_keys("LinkButton", &c.fields.0)?;
        let mut label = None;
        let mut icon_leading = None;
        let mut icon_trailing = None;
        let mut tone = None;
        let mut underline = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => label = Some(decode_from_value(v)?),
                1 => icon_leading = Some(decode_from_value(v)?),
                2 => icon_trailing = Some(decode_from_value(v)?),
                3 => tone = Some(decode_from_value(v)?),
                4 => underline = Some(decode_from_value(v)?),
                other => return Err(unknown_field("LinkButton", *other)),
            }
        }
        Ok(LinkButton {
            label: label.ok_or_else(|| missing_field("LinkButton", "label"))?,
            icon_leading,
            icon_trailing,
            tone: tone.ok_or_else(|| missing_field("LinkButton", "tone"))?,
            underline: underline.ok_or_else(|| missing_field("LinkButton", "underline"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0405 — Link
// -----------------------------------------------------------------------------

/// Standard text link (catalog §6 0x0405). No raw `href`; navigation flows through handlers.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub label: BindRef,
    pub underline: LinkUnderline,
    pub tone: Tone,
    pub leading_icon: Option<IconRef>,
    pub trailing_icon: Option<IconRef>,
}

impl Link {
    pub const TAG: u16 = 0x0405;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.label)?));
        e.push((1, encode_to_value(&self.underline)?));
        e.push((2, encode_to_value(&self.tone)?));
        if let Some(v) = &self.leading_icon {
            e.push((3, encode_to_value(v)?));
        }
        if let Some(v) = &self.trailing_icon {
            e.push((4, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Link")?;
        ensure_no_duplicate_keys("Link", &c.fields.0)?;
        let mut label = None;
        let mut underline = None;
        let mut tone = None;
        let mut leading_icon = None;
        let mut trailing_icon = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => label = Some(decode_from_value(v)?),
                1 => underline = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => leading_icon = Some(decode_from_value(v)?),
                4 => trailing_icon = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Link", *other)),
            }
        }
        Ok(Link {
            label: label.ok_or_else(|| missing_field("Link", "label"))?,
            underline: underline.ok_or_else(|| missing_field("Link", "underline"))?,
            tone: tone.ok_or_else(|| missing_field("Link", "tone"))?,
            leading_icon,
            trailing_icon,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x040C — Fab
// -----------------------------------------------------------------------------

/// Floating action button (catalog §6 0x040C).
#[derive(Debug, Clone, PartialEq)]
pub struct Fab {
    pub icon: IconRef,
    pub tone: Tone,
    pub size: FabSize,
    pub position: FabPosition,
    pub label: Option<BindRef>,
}

impl Fab {
    pub const TAG: u16 = 0x040C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.icon)?));
        e.push((1, encode_to_value(&self.tone)?));
        e.push((2, encode_to_value(&self.size)?));
        e.push((3, encode_to_value(&self.position)?));
        if let Some(v) = &self.label {
            e.push((4, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Fab")?;
        ensure_no_duplicate_keys("Fab", &c.fields.0)?;
        let mut icon = None;
        let mut tone = None;
        let mut size = None;
        let mut position = None;
        let mut label = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => icon = Some(decode_from_value(v)?),
                1 => tone = Some(decode_from_value(v)?),
                2 => size = Some(decode_from_value(v)?),
                3 => position = Some(decode_from_value(v)?),
                4 => label = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Fab", *other)),
            }
        }
        Ok(Fab {
            icon: icon.ok_or_else(|| missing_field("Fab", "icon"))?,
            tone: tone.ok_or_else(|| missing_field("Fab", "tone"))?,
            size: size.ok_or_else(|| missing_field("Fab", "size"))?,
            position: position.ok_or_else(|| missing_field("Fab", "position"))?,
            label,
        })
    }
}
