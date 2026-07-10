// =============================================================================
// File: protocol/ui/form/range.rs — Slider/RangeSlider/SliderRow (catalog §5)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::SliderMark;
use super::super::tokens::{SliderRowLayout, Tone};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::value_format::ValueFormat;

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
// 0x030F — Slider
// -----------------------------------------------------------------------------

/// Single-handle slider (catalog §5 0x030F).
#[derive(Debug, Clone, PartialEq)]
pub struct Slider {
    pub bind_path: StatePath,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub label: Option<BindRef>,
    pub show_value: bool,
    pub format: Option<ValueFormat>,
    pub marks: Option<Vec<SliderMark>>,
    pub tone: Tone,
}

impl Slider {
    pub const TAG: u16 = 0x030F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(9);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.min)?));
        e.push((2, encode_to_value(&self.max)?));
        e.push((3, encode_to_value(&self.step)?));
        if let Some(v) = &self.label {
            e.push((4, encode_to_value(v)?));
        }
        e.push((5, encode_to_value(&self.show_value)?));
        if let Some(v) = &self.format {
            e.push((6, encode_to_value(v)?));
        }
        if let Some(v) = &self.marks {
            e.push((7, encode_to_value(v)?));
        }
        e.push((8, encode_to_value(&self.tone)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Slider")?;
        ensure_no_duplicate_keys("Slider", &c.fields.0)?;
        let mut bind_path = None;
        let mut min = None;
        let mut max = None;
        let mut step = None;
        let mut label = None;
        let mut show_value = None;
        let mut format = None;
        let mut marks = None;
        let mut tone = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => min = Some(decode_from_value(v)?),
                2 => max = Some(decode_from_value(v)?),
                3 => step = Some(decode_from_value(v)?),
                4 => label = Some(decode_from_value(v)?),
                5 => show_value = Some(decode_from_value(v)?),
                6 => format = Some(decode_from_value(v)?),
                7 => marks = Some(decode_from_value(v)?),
                8 => tone = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Slider", *other)),
            }
        }
        Ok(Slider {
            bind_path: bind_path.ok_or_else(|| missing_field("Slider", "bind_path"))?,
            min: min.ok_or_else(|| missing_field("Slider", "min"))?,
            max: max.ok_or_else(|| missing_field("Slider", "max"))?,
            step: step.ok_or_else(|| missing_field("Slider", "step"))?,
            label,
            show_value: show_value.ok_or_else(|| missing_field("Slider", "show_value"))?,
            format,
            marks,
            tone: tone.ok_or_else(|| missing_field("Slider", "tone"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0310 — RangeSlider
// -----------------------------------------------------------------------------

/// Two-handle slider (catalog §5 0x0310).
#[derive(Debug, Clone, PartialEq)]
pub struct RangeSlider {
    pub bind_path_min: StatePath,
    pub bind_path_max: StatePath,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub label: Option<BindRef>,
    pub show_value: bool,
    pub format: Option<ValueFormat>,
    pub marks: Option<Vec<SliderMark>>,
    pub tone: Tone,
    pub min_separation: f64,
}

impl RangeSlider {
    pub const TAG: u16 = 0x0310;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(11);
        e.push((0, encode_to_value(&self.bind_path_min)?));
        e.push((1, encode_to_value(&self.bind_path_max)?));
        e.push((2, encode_to_value(&self.min)?));
        e.push((3, encode_to_value(&self.max)?));
        e.push((4, encode_to_value(&self.step)?));
        if let Some(v) = &self.label {
            e.push((5, encode_to_value(v)?));
        }
        e.push((6, encode_to_value(&self.show_value)?));
        if let Some(v) = &self.format {
            e.push((7, encode_to_value(v)?));
        }
        if let Some(v) = &self.marks {
            e.push((8, encode_to_value(v)?));
        }
        e.push((9, encode_to_value(&self.tone)?));
        e.push((10, encode_to_value(&self.min_separation)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "RangeSlider")?;
        ensure_no_duplicate_keys("RangeSlider", &c.fields.0)?;
        let mut bind_path_min = None;
        let mut bind_path_max = None;
        let mut min = None;
        let mut max = None;
        let mut step = None;
        let mut label = None;
        let mut show_value = None;
        let mut format = None;
        let mut marks = None;
        let mut tone = None;
        let mut min_separation = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path_min = Some(decode_from_value(v)?),
                1 => bind_path_max = Some(decode_from_value(v)?),
                2 => min = Some(decode_from_value(v)?),
                3 => max = Some(decode_from_value(v)?),
                4 => step = Some(decode_from_value(v)?),
                5 => label = Some(decode_from_value(v)?),
                6 => show_value = Some(decode_from_value(v)?),
                7 => format = Some(decode_from_value(v)?),
                8 => marks = Some(decode_from_value(v)?),
                9 => tone = Some(decode_from_value(v)?),
                10 => min_separation = Some(decode_from_value(v)?),
                other => return Err(unknown_field("RangeSlider", *other)),
            }
        }
        Ok(RangeSlider {
            bind_path_min: bind_path_min
                .ok_or_else(|| missing_field("RangeSlider", "bind_path_min"))?,
            bind_path_max: bind_path_max
                .ok_or_else(|| missing_field("RangeSlider", "bind_path_max"))?,
            min: min.ok_or_else(|| missing_field("RangeSlider", "min"))?,
            max: max.ok_or_else(|| missing_field("RangeSlider", "max"))?,
            step: step.ok_or_else(|| missing_field("RangeSlider", "step"))?,
            label,
            show_value: show_value.ok_or_else(|| missing_field("RangeSlider", "show_value"))?,
            format,
            marks,
            tone: tone.ok_or_else(|| missing_field("RangeSlider", "tone"))?,
            min_separation: min_separation
                .ok_or_else(|| missing_field("RangeSlider", "min_separation"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0311 — SliderRow
// -----------------------------------------------------------------------------

/// Inline slider with label and value display (catalog §5 0x0311).
#[derive(Debug, Clone, PartialEq)]
pub struct SliderRow {
    pub bind_path: StatePath,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub label: BindRef,
    pub format: Option<ValueFormat>,
    pub marks: Option<Vec<SliderMark>>,
    pub tone: Tone,
    pub layout: SliderRowLayout,
}

impl SliderRow {
    pub const TAG: u16 = 0x0311;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(9);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.min)?));
        e.push((2, encode_to_value(&self.max)?));
        e.push((3, encode_to_value(&self.step)?));
        e.push((4, encode_to_value(&self.label)?));
        if let Some(v) = &self.format {
            e.push((5, encode_to_value(v)?));
        }
        if let Some(v) = &self.marks {
            e.push((6, encode_to_value(v)?));
        }
        e.push((7, encode_to_value(&self.tone)?));
        e.push((8, encode_to_value(&self.layout)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "SliderRow")?;
        ensure_no_duplicate_keys("SliderRow", &c.fields.0)?;
        let mut bind_path = None;
        let mut min = None;
        let mut max = None;
        let mut step = None;
        let mut label = None;
        let mut format = None;
        let mut marks = None;
        let mut tone = None;
        let mut layout = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => min = Some(decode_from_value(v)?),
                2 => max = Some(decode_from_value(v)?),
                3 => step = Some(decode_from_value(v)?),
                4 => label = Some(decode_from_value(v)?),
                5 => format = Some(decode_from_value(v)?),
                6 => marks = Some(decode_from_value(v)?),
                7 => tone = Some(decode_from_value(v)?),
                8 => layout = Some(decode_from_value(v)?),
                other => return Err(unknown_field("SliderRow", *other)),
            }
        }
        Ok(SliderRow {
            bind_path: bind_path.ok_or_else(|| missing_field("SliderRow", "bind_path"))?,
            min: min.ok_or_else(|| missing_field("SliderRow", "min"))?,
            max: max.ok_or_else(|| missing_field("SliderRow", "max"))?,
            step: step.ok_or_else(|| missing_field("SliderRow", "step"))?,
            label: label.ok_or_else(|| missing_field("SliderRow", "label"))?,
            format,
            marks,
            tone: tone.ok_or_else(|| missing_field("SliderRow", "tone"))?,
            layout: layout.ok_or_else(|| missing_field("SliderRow", "layout"))?,
        })
    }
}
