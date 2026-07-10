// =============================================================================
// File: protocol/ui/data/progress.rs — ProgressBar/RatingDisplay/Diff (catalog §4)
// =============================================================================

use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::tokens::{
    DiffVariant, ProgressOrientation, ProgressSize, ProgressVariant, RatingPrecision,
    RatingVariant, Tone,
};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::super::value::Value;

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component { tag, id: id.into(), fields: FieldMap(fields), handlers: None, bind: None, a11y: None, visibility: None, test_id: None }
}

// -----------------------------------------------------------------------------
// 0x021D — ProgressBar
// -----------------------------------------------------------------------------

/// Linear progress indicator (catalog §4 0x021D). `max` is `f64` for
/// Value-roundtrip compatibility (default 1.0 applied on decode).
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressBar {
    pub value: BindRef,
    pub max: f64,
    pub variant: ProgressVariant,
    pub tone: Tone,
    pub show_label: bool,
    pub label: Option<BindRef>,
    pub size: ProgressSize,
    /// Fill orientation. Absent on the wire (and `None`) means the default
    /// `Horizontal`; only `Some(Vertical)` is encoded, so existing horizontal
    /// bars keep byte-identical payloads.
    pub orientation: Option<ProgressOrientation>,
}

impl ProgressBar {
    pub const TAG: u16 = 0x021D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(8);
        e.push((0, encode_to_value(&self.value)?));
        e.push((1, encode_to_value(&self.max)?));
        e.push((2, encode_to_value(&self.variant)?));
        e.push((3, encode_to_value(&self.tone)?));
        e.push((4, encode_to_value(&self.show_label)?));
        if let Some(l) = &self.label { e.push((5, encode_to_value(l)?)); }
        e.push((6, encode_to_value(&self.size)?));
        // Wire-compat: emit key 7 ONLY for the non-default vertical orientation.
        if matches!(self.orientation, Some(ProgressOrientation::Vertical)) {
            e.push((7, encode_to_value(&ProgressOrientation::Vertical)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ProgressBar")?;
        ensure_no_duplicate_keys("ProgressBar", &c.fields.0)?;
        let mut value = None;
        let mut max = None;
        let mut variant = None;
        let mut tone = None;
        let mut show_label = None;
        let mut label = None;
        let mut size = None;
        let mut orientation = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => value = Some(decode_from_value(v)?),
                1 => max = Some(decode_from_value(v)?),
                2 => variant = Some(decode_from_value(v)?),
                3 => tone = Some(decode_from_value(v)?),
                4 => show_label = Some(decode_from_value(v)?),
                5 => label = Some(decode_from_value(v)?),
                6 => size = Some(decode_from_value(v)?),
                7 => orientation = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ProgressBar", *other)),
            }
        }
        Ok(ProgressBar {
            value: value.ok_or_else(|| missing_field("ProgressBar", "value"))?,
            // §4 0x021D default: max = 1.0.
            max: max.unwrap_or(1.0),
            variant: variant.ok_or_else(|| missing_field("ProgressBar", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("ProgressBar", "tone"))?,
            show_label: show_label.ok_or_else(|| missing_field("ProgressBar", "show_label"))?,
            label,
            size: size.ok_or_else(|| missing_field("ProgressBar", "size"))?,
            orientation,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x021E — RatingDisplay
// -----------------------------------------------------------------------------

/// Star/heart/numeric rating (catalog §4 0x021E).
#[derive(Debug, Clone, PartialEq)]
pub struct RatingDisplay {
    pub value: BindRef,
    pub max: u8,
    pub variant: RatingVariant,
    pub show_value: bool,
    pub precision: RatingPrecision,
}

impl RatingDisplay {
    pub const TAG: u16 = 0x021E;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.value)?));
        e.push((1, encode_to_value(&self.max)?));
        e.push((2, encode_to_value(&self.variant)?));
        e.push((3, encode_to_value(&self.show_value)?));
        e.push((4, encode_to_value(&self.precision)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "RatingDisplay")?;
        ensure_no_duplicate_keys("RatingDisplay", &c.fields.0)?;
        let mut value = None;
        let mut max = None;
        let mut variant = None;
        let mut show_value = None;
        let mut precision = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => value = Some(decode_from_value(v)?),
                1 => max = Some(decode_from_value(v)?),
                2 => variant = Some(decode_from_value(v)?),
                3 => show_value = Some(decode_from_value(v)?),
                4 => precision = Some(decode_from_value(v)?),
                other => return Err(unknown_field("RatingDisplay", *other)),
            }
        }
        Ok(RatingDisplay {
            value: value.ok_or_else(|| missing_field("RatingDisplay", "value"))?,
            // §4 0x021E default: max = 5.
            max: max.unwrap_or(5),
            variant: variant.ok_or_else(|| missing_field("RatingDisplay", "variant"))?,
            show_value: show_value.ok_or_else(|| missing_field("RatingDisplay", "show_value"))?,
            precision: precision.ok_or_else(|| missing_field("RatingDisplay", "precision"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x021F — Diff
// -----------------------------------------------------------------------------

/// Text diff display (catalog §4 0x021F).
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub before_path: StatePath,
    pub after_path: StatePath,
    pub variant: DiffVariant,
    pub language: Option<String>,
    pub word_wrap: bool,
    pub show_line_numbers: bool,
}

impl Diff {
    pub const TAG: u16 = 0x021F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.before_path)?));
        e.push((1, encode_to_value(&self.after_path)?));
        e.push((2, encode_to_value(&self.variant)?));
        if let Some(l) = &self.language { e.push((3, encode_to_value(l)?)); }
        e.push((4, encode_to_value(&self.word_wrap)?));
        e.push((5, encode_to_value(&self.show_line_numbers)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Diff")?;
        ensure_no_duplicate_keys("Diff", &c.fields.0)?;
        let mut before_path = None;
        let mut after_path = None;
        let mut variant = None;
        let mut language = None;
        let mut word_wrap = None;
        let mut show_line_numbers = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => before_path = Some(decode_from_value(v)?),
                1 => after_path = Some(decode_from_value(v)?),
                2 => variant = Some(decode_from_value(v)?),
                3 => language = Some(decode_from_value(v)?),
                4 => word_wrap = Some(decode_from_value(v)?),
                5 => show_line_numbers = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Diff", *other)),
            }
        }
        Ok(Diff {
            before_path: before_path.ok_or_else(|| missing_field("Diff", "before_path"))?,
            after_path: after_path.ok_or_else(|| missing_field("Diff", "after_path"))?,
            variant: variant.ok_or_else(|| missing_field("Diff", "variant"))?,
            language,
            word_wrap: word_wrap.ok_or_else(|| missing_field("Diff", "word_wrap"))?,
            show_line_numbers: show_line_numbers.ok_or_else(|| missing_field("Diff", "show_line_numbers"))?,
        })
    }
}
