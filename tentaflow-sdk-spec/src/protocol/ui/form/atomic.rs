// =============================================================================
// File: protocol/ui/form/atomic.rs — Toggle/Checkbox/Radio (catalog §5)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::SelectValue;
use super::super::tokens::{CheckboxSize, TogglePosition, ToggleSize, Tone};
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
// 0x030A — Toggle
// -----------------------------------------------------------------------------

/// Switch on/off (catalog §5 0x030A).
#[derive(Debug, Clone, PartialEq)]
pub struct Toggle {
    pub bind_path: StatePath,
    pub label: Option<BindRef>,
    pub hint: Option<BindRef>,
    pub size: ToggleSize,
    pub tone: Tone,
    pub disabled: Option<BindRef>,
    pub label_position: TogglePosition,
}

impl Toggle {
    pub const TAG: u16 = 0x030A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.bind_path)?));
        if let Some(v) = &self.label {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.hint {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.size)?));
        e.push((4, encode_to_value(&self.tone)?));
        if let Some(v) = &self.disabled {
            e.push((5, encode_to_value(v)?));
        }
        e.push((6, encode_to_value(&self.label_position)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Toggle")?;
        ensure_no_duplicate_keys("Toggle", &c.fields.0)?;
        let mut bind_path = None;
        let mut label = None;
        let mut hint = None;
        let mut size = None;
        let mut tone = None;
        let mut disabled = None;
        let mut label_position = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => label = Some(decode_from_value(v)?),
                2 => hint = Some(decode_from_value(v)?),
                3 => size = Some(decode_from_value(v)?),
                4 => tone = Some(decode_from_value(v)?),
                5 => disabled = Some(decode_from_value(v)?),
                6 => label_position = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Toggle", *other)),
            }
        }
        Ok(Toggle {
            bind_path: bind_path.ok_or_else(|| missing_field("Toggle", "bind_path"))?,
            label,
            hint,
            size: size.ok_or_else(|| missing_field("Toggle", "size"))?,
            // §5 0x030A default: tone = Primary.
            tone: tone.unwrap_or(Tone::Primary),
            disabled,
            label_position: label_position
                .ok_or_else(|| missing_field("Toggle", "label_position"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x030B — Checkbox
// -----------------------------------------------------------------------------

/// Standard checkbox (catalog §5 0x030B).
#[derive(Debug, Clone, PartialEq)]
pub struct Checkbox {
    pub bind_path: StatePath,
    pub label: Option<BindRef>,
    pub hint: Option<BindRef>,
    pub indeterminate: Option<BindRef>,
    pub disabled: Option<BindRef>,
    pub size: CheckboxSize,
}

impl Checkbox {
    pub const TAG: u16 = 0x030B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.bind_path)?));
        if let Some(v) = &self.label {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.hint {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.indeterminate {
            e.push((3, encode_to_value(v)?));
        }
        if let Some(v) = &self.disabled {
            e.push((4, encode_to_value(v)?));
        }
        e.push((5, encode_to_value(&self.size)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Checkbox")?;
        ensure_no_duplicate_keys("Checkbox", &c.fields.0)?;
        let mut bind_path = None;
        let mut label = None;
        let mut hint = None;
        let mut indeterminate = None;
        let mut disabled = None;
        let mut size = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => label = Some(decode_from_value(v)?),
                2 => hint = Some(decode_from_value(v)?),
                3 => indeterminate = Some(decode_from_value(v)?),
                4 => disabled = Some(decode_from_value(v)?),
                5 => size = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Checkbox", *other)),
            }
        }
        Ok(Checkbox {
            bind_path: bind_path.ok_or_else(|| missing_field("Checkbox", "bind_path"))?,
            label,
            hint,
            indeterminate,
            disabled,
            size: size.ok_or_else(|| missing_field("Checkbox", "size"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x030C — Radio
// -----------------------------------------------------------------------------

/// Single radio button (catalog §5 0x030C).
#[derive(Debug, Clone, PartialEq)]
pub struct Radio {
    pub bind_path: StatePath,
    pub value: SelectValue,
    pub label: BindRef,
    pub hint: Option<BindRef>,
    pub disabled: Option<BindRef>,
}

impl Radio {
    pub const TAG: u16 = 0x030C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.value)?));
        e.push((2, encode_to_value(&self.label)?));
        if let Some(v) = &self.hint {
            e.push((3, encode_to_value(v)?));
        }
        if let Some(v) = &self.disabled {
            e.push((4, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Radio")?;
        ensure_no_duplicate_keys("Radio", &c.fields.0)?;
        let mut bind_path = None;
        let mut value = None;
        let mut label = None;
        let mut hint = None;
        let mut disabled = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => value = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => hint = Some(decode_from_value(v)?),
                4 => disabled = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Radio", *other)),
            }
        }
        Ok(Radio {
            bind_path: bind_path.ok_or_else(|| missing_field("Radio", "bind_path"))?,
            value: value.ok_or_else(|| missing_field("Radio", "value"))?,
            label: label.ok_or_else(|| missing_field("Radio", "label"))?,
            hint,
            disabled,
        })
    }
}
