// =============================================================================
// File: protocol/ui/feedback/gates.rs — GateScreen/ConfirmationDialog (catalog §7)
// =============================================================================

use super::super::super::value::Value;
use super::super::actions::Button;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::IconRef;
use super::super::tokens::{GateVariant, Tone};
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
// 0x050D — GateScreen
// -----------------------------------------------------------------------------

/// Full-screen permission/auth gate (catalog §7 0x050D).
#[derive(Debug, Clone, PartialEq)]
pub struct GateScreen {
    pub icon: IconRef,
    pub title: BindRef,
    pub message: BindRef,
    pub actions: Vec<Component>,
    pub variant: GateVariant,
}

impl GateScreen {
    pub const TAG: u16 = 0x050D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        for b in &self.actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "GateScreen", "actions")?;
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.icon)?));
        e.push((1, encode_to_value(&self.title)?));
        e.push((2, encode_to_value(&self.message)?));
        e.push((3, encode_to_value(&self.actions)?));
        e.push((4, encode_to_value(&self.variant)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "GateScreen")?;
        ensure_no_duplicate_keys("GateScreen", &c.fields.0)?;
        let mut icon = None;
        let mut title = None;
        let mut message = None;
        let mut actions: Option<Vec<Component>> = None;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => icon = Some(decode_from_value(v)?),
                1 => title = Some(decode_from_value(v)?),
                2 => message = Some(decode_from_value(v)?),
                3 => actions = Some(decode_from_value(v)?),
                4 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("GateScreen", *other)),
            }
        }
        let actions = actions.ok_or_else(|| missing_field("GateScreen", "actions"))?;
        for b in &actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "GateScreen", "actions")?;
        }
        Ok(GateScreen {
            icon: icon.ok_or_else(|| missing_field("GateScreen", "icon"))?,
            title: title.ok_or_else(|| missing_field("GateScreen", "title"))?,
            message: message.ok_or_else(|| missing_field("GateScreen", "message"))?,
            actions,
            variant: variant.ok_or_else(|| missing_field("GateScreen", "variant"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x050E — ConfirmationDialog
// -----------------------------------------------------------------------------

/// Specialised confirmation modal (catalog §7 0x050E).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmationDialog {
    pub title: BindRef,
    pub message: BindRef,
    pub icon: Option<IconRef>,
    pub tone: Tone,
    pub confirm_label: BindRef,
    pub cancel_label: BindRef,
    pub destructive: bool,
    pub require_typing: Option<String>,
}

impl ConfirmationDialog {
    pub const TAG: u16 = 0x050E;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(8);
        e.push((0, encode_to_value(&self.title)?));
        e.push((1, encode_to_value(&self.message)?));
        if let Some(v) = &self.icon {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.tone)?));
        e.push((4, encode_to_value(&self.confirm_label)?));
        e.push((5, encode_to_value(&self.cancel_label)?));
        e.push((6, encode_to_value(&self.destructive)?));
        if let Some(v) = &self.require_typing {
            e.push((7, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ConfirmationDialog")?;
        ensure_no_duplicate_keys("ConfirmationDialog", &c.fields.0)?;
        let mut title = None;
        let mut message = None;
        let mut icon = None;
        let mut tone = None;
        let mut confirm_label = None;
        let mut cancel_label = None;
        let mut destructive = None;
        let mut require_typing = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => message = Some(decode_from_value(v)?),
                2 => icon = Some(decode_from_value(v)?),
                3 => tone = Some(decode_from_value(v)?),
                4 => confirm_label = Some(decode_from_value(v)?),
                5 => cancel_label = Some(decode_from_value(v)?),
                6 => destructive = Some(decode_from_value(v)?),
                7 => require_typing = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ConfirmationDialog", *other)),
            }
        }
        Ok(ConfirmationDialog {
            title: title.ok_or_else(|| missing_field("ConfirmationDialog", "title"))?,
            message: message.ok_or_else(|| missing_field("ConfirmationDialog", "message"))?,
            icon,
            tone: tone.ok_or_else(|| missing_field("ConfirmationDialog", "tone"))?,
            confirm_label: confirm_label
                .ok_or_else(|| missing_field("ConfirmationDialog", "confirm_label"))?,
            cancel_label: cancel_label
                .ok_or_else(|| missing_field("ConfirmationDialog", "cancel_label"))?,
            destructive: destructive
                .ok_or_else(|| missing_field("ConfirmationDialog", "destructive"))?,
            require_typing,
        })
    }
}
