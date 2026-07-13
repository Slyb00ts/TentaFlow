// =============================================================================
// File: protocol/ui/feedback/loading.rs — Skeleton/Spinner/LoadingBar (catalog §7)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::DimensionToken;
use super::super::tokens::{SkeletonVariant, SpinnerSize, SpinnerVariant, Tone};
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
// 0x0506 — Skeleton
// -----------------------------------------------------------------------------

/// Loading placeholder (catalog §7 0x0506).
#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    pub variant: SkeletonVariant,
    pub width: Option<DimensionToken>,
    pub height: Option<DimensionToken>,
    pub animate: bool,
    pub lines: u8,
}

impl Skeleton {
    pub const TAG: u16 = 0x0506;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.variant)?));
        if let Some(v) = &self.width {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.height {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.animate)?));
        e.push((4, encode_to_value(&self.lines)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Skeleton")?;
        ensure_no_duplicate_keys("Skeleton", &c.fields.0)?;
        let mut variant = None;
        let mut width = None;
        let mut height = None;
        let mut animate = None;
        let mut lines = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => width = Some(decode_from_value(v)?),
                2 => height = Some(decode_from_value(v)?),
                3 => animate = Some(decode_from_value(v)?),
                4 => lines = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Skeleton", *other)),
            }
        }
        Ok(Skeleton {
            variant: variant.ok_or_else(|| missing_field("Skeleton", "variant"))?,
            width,
            height,
            animate: animate.ok_or_else(|| missing_field("Skeleton", "animate"))?,
            lines: lines.ok_or_else(|| missing_field("Skeleton", "lines"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0507 — Spinner
// -----------------------------------------------------------------------------

/// Loading spinner (catalog §7 0x0507).
#[derive(Debug, Clone, PartialEq)]
pub struct Spinner {
    pub size: SpinnerSize,
    pub tone: Tone,
    pub label: Option<BindRef>,
    pub variant: SpinnerVariant,
}

impl Spinner {
    pub const TAG: u16 = 0x0507;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.size)?));
        e.push((1, encode_to_value(&self.tone)?));
        if let Some(v) = &self.label {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.variant)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Spinner")?;
        ensure_no_duplicate_keys("Spinner", &c.fields.0)?;
        let mut size = None;
        let mut tone = None;
        let mut label = None;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => size = Some(decode_from_value(v)?),
                1 => tone = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Spinner", *other)),
            }
        }
        Ok(Spinner {
            size: size.ok_or_else(|| missing_field("Spinner", "size"))?,
            tone: tone.ok_or_else(|| missing_field("Spinner", "tone"))?,
            label,
            variant: variant.ok_or_else(|| missing_field("Spinner", "variant"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0508 — LoadingBar
// -----------------------------------------------------------------------------

/// Top-of-page progress indicator (catalog §7 0x0508).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadingBar {
    pub visible: BindRef,
    pub progress: Option<BindRef>,
    pub tone: Tone,
}

impl LoadingBar {
    pub const TAG: u16 = 0x0508;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(3);
        e.push((0, encode_to_value(&self.visible)?));
        if let Some(v) = &self.progress {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.tone)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "LoadingBar")?;
        ensure_no_duplicate_keys("LoadingBar", &c.fields.0)?;
        let mut visible = None;
        let mut progress = None;
        let mut tone = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => visible = Some(decode_from_value(v)?),
                1 => progress = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                other => return Err(unknown_field("LoadingBar", *other)),
            }
        }
        Ok(LoadingBar {
            visible: visible.ok_or_else(|| missing_field("LoadingBar", "visible"))?,
            progress,
            tone: tone.ok_or_else(|| missing_field("LoadingBar", "tone"))?,
        })
    }
}
