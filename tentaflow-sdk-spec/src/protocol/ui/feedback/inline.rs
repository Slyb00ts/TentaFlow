// =============================================================================
// File: protocol/ui/feedback/inline.rs — Alert/Banner/Callout/Toast/Hint/OfflineBanner (catalog §7)
// =============================================================================

use super::super::super::value::Value;
use super::super::actions::Button;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::IconRef;
use super::super::tokens::{AlertVariant, BannerPosition, Tone};
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
// 0x0501 — Alert
// -----------------------------------------------------------------------------

/// Inline alert (catalog §7 0x0501).
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub tone: Tone,
    pub variant: AlertVariant,
    pub icon: Option<IconRef>,
    pub title: Option<BindRef>,
    pub message: BindRef,
    pub actions: Option<Vec<Component>>,
    pub dismissible: bool,
}

impl Alert {
    pub const TAG: u16 = 0x0501;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        if let Some(acts) = &self.actions {
            for b in acts {
                ensure_ref_tag_encode(b.tag, Button::TAG, "Alert", "actions")?;
            }
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.tone)?));
        e.push((1, encode_to_value(&self.variant)?));
        if let Some(v) = &self.icon {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.title {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.message)?));
        if let Some(v) = &self.actions {
            e.push((5, encode_to_value(v)?));
        }
        e.push((6, encode_to_value(&self.dismissible)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Alert")?;
        ensure_no_duplicate_keys("Alert", &c.fields.0)?;
        let mut tone = None;
        let mut variant = None;
        let mut icon = None;
        let mut title = None;
        let mut message = None;
        let mut actions: Option<Vec<Component>> = None;
        let mut dismissible = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => tone = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => icon = Some(decode_from_value(v)?),
                3 => title = Some(decode_from_value(v)?),
                4 => message = Some(decode_from_value(v)?),
                5 => actions = Some(decode_from_value(v)?),
                6 => dismissible = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Alert", *other)),
            }
        }
        if let Some(acts) = &actions {
            for b in acts {
                ensure_ref_tag_decode(b.tag, Button::TAG, "Alert", "actions")?;
            }
        }
        Ok(Alert {
            tone: tone.ok_or_else(|| missing_field("Alert", "tone"))?,
            variant: variant.ok_or_else(|| missing_field("Alert", "variant"))?,
            icon,
            title,
            message: message.ok_or_else(|| missing_field("Alert", "message"))?,
            actions,
            dismissible: dismissible.ok_or_else(|| missing_field("Alert", "dismissible"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0502 — Banner
// -----------------------------------------------------------------------------

/// Full-width banner (catalog §7 0x0502).
#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub tone: Tone,
    pub icon: Option<IconRef>,
    pub message: BindRef,
    pub action: Option<Component>,
    pub dismissible: bool,
    pub position: BannerPosition,
}

impl Banner {
    pub const TAG: u16 = 0x0502;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        if let Some(b) = &self.action {
            ensure_ref_tag_encode(b.tag, Button::TAG, "Banner", "action")?;
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.tone)?));
        if let Some(v) = &self.icon {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.message)?));
        if let Some(v) = &self.action {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.dismissible)?));
        e.push((5, encode_to_value(&self.position)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Banner")?;
        ensure_no_duplicate_keys("Banner", &c.fields.0)?;
        let mut tone = None;
        let mut icon = None;
        let mut message = None;
        let mut action: Option<Component> = None;
        let mut dismissible = None;
        let mut position = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => tone = Some(decode_from_value(v)?),
                1 => icon = Some(decode_from_value(v)?),
                2 => message = Some(decode_from_value(v)?),
                3 => action = Some(decode_from_value(v)?),
                4 => dismissible = Some(decode_from_value(v)?),
                5 => position = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Banner", *other)),
            }
        }
        if let Some(b) = &action {
            ensure_ref_tag_decode(b.tag, Button::TAG, "Banner", "action")?;
        }
        Ok(Banner {
            tone: tone.ok_or_else(|| missing_field("Banner", "tone"))?,
            icon,
            message: message.ok_or_else(|| missing_field("Banner", "message"))?,
            action,
            dismissible: dismissible.ok_or_else(|| missing_field("Banner", "dismissible"))?,
            position: position.ok_or_else(|| missing_field("Banner", "position"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0503 — Callout
// -----------------------------------------------------------------------------

/// Inline note (catalog §7 0x0503).
#[derive(Debug, Clone, PartialEq)]
pub struct Callout {
    pub tone: Tone,
    pub icon: Option<IconRef>,
    pub title: Option<BindRef>,
    pub content: Vec<Component>,
}

impl Callout {
    pub const TAG: u16 = 0x0503;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.tone)?));
        if let Some(v) = &self.icon {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.title {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.content)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Callout")?;
        ensure_no_duplicate_keys("Callout", &c.fields.0)?;
        let mut tone = None;
        let mut icon = None;
        let mut title = None;
        let mut content = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => tone = Some(decode_from_value(v)?),
                1 => icon = Some(decode_from_value(v)?),
                2 => title = Some(decode_from_value(v)?),
                3 => content = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Callout", *other)),
            }
        }
        Ok(Callout {
            tone: tone.ok_or_else(|| missing_field("Callout", "tone"))?,
            icon,
            title,
            content: content.ok_or_else(|| missing_field("Callout", "content"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0504 — Toast
// -----------------------------------------------------------------------------

/// Embedded toast notification (catalog §7 0x0504).
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub tone: Tone,
    pub title: BindRef,
    pub body: Option<BindRef>,
    pub icon: Option<IconRef>,
    pub action_label: Option<String>,
    pub action_id: Option<String>,
}

impl Toast {
    pub const TAG: u16 = 0x0504;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.tone)?));
        e.push((1, encode_to_value(&self.title)?));
        if let Some(v) = &self.body {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.icon {
            e.push((3, encode_to_value(v)?));
        }
        if let Some(v) = &self.action_label {
            e.push((4, encode_to_value(v)?));
        }
        if let Some(v) = &self.action_id {
            e.push((5, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Toast")?;
        ensure_no_duplicate_keys("Toast", &c.fields.0)?;
        let mut tone = None;
        let mut title = None;
        let mut body = None;
        let mut icon = None;
        let mut action_label = None;
        let mut action_id = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => tone = Some(decode_from_value(v)?),
                1 => title = Some(decode_from_value(v)?),
                2 => body = Some(decode_from_value(v)?),
                3 => icon = Some(decode_from_value(v)?),
                4 => action_label = Some(decode_from_value(v)?),
                5 => action_id = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Toast", *other)),
            }
        }
        Ok(Toast {
            tone: tone.ok_or_else(|| missing_field("Toast", "tone"))?,
            title: title.ok_or_else(|| missing_field("Toast", "title"))?,
            body,
            icon,
            action_label,
            action_id,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0505 — Hint
// -----------------------------------------------------------------------------

/// Subtle help text (catalog §7 0x0505).
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    pub content: BindRef,
    pub icon: Option<IconRef>,
    pub tone: Tone,
}

impl Hint {
    pub const TAG: u16 = 0x0505;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(3);
        e.push((0, encode_to_value(&self.content)?));
        if let Some(v) = &self.icon {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.tone)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Hint")?;
        ensure_no_duplicate_keys("Hint", &c.fields.0)?;
        let mut content = None;
        let mut icon = None;
        let mut tone = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => icon = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Hint", *other)),
            }
        }
        Ok(Hint {
            content: content.ok_or_else(|| missing_field("Hint", "content"))?,
            icon,
            tone: tone.ok_or_else(|| missing_field("Hint", "tone"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x050F — OfflineBanner
// -----------------------------------------------------------------------------

/// Specialised offline banner (catalog §7 0x050F).
#[derive(Debug, Clone, PartialEq)]
pub struct OfflineBanner {
    pub message: BindRef,
    pub action_label: Option<BindRef>,
    pub reconnecting: BindRef,
}

impl OfflineBanner {
    pub const TAG: u16 = 0x050F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(3);
        e.push((0, encode_to_value(&self.message)?));
        if let Some(v) = &self.action_label {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.reconnecting)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "OfflineBanner")?;
        ensure_no_duplicate_keys("OfflineBanner", &c.fields.0)?;
        let mut message = None;
        let mut action_label = None;
        let mut reconnecting = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => message = Some(decode_from_value(v)?),
                1 => action_label = Some(decode_from_value(v)?),
                2 => reconnecting = Some(decode_from_value(v)?),
                other => return Err(unknown_field("OfflineBanner", *other)),
            }
        }
        Ok(OfflineBanner {
            message: message.ok_or_else(|| missing_field("OfflineBanner", "message"))?,
            action_label,
            reconnecting: reconnecting
                .ok_or_else(|| missing_field("OfflineBanner", "reconnecting"))?,
        })
    }
}
