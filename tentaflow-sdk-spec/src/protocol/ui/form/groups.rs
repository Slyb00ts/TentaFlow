// =============================================================================
// File: protocol/ui/form/groups.rs — RadioGroup/RadioCardGroup (catalog §5)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::{RadioCardOption, RadioOption};
use super::super::tokens::{Density, RadioCardVariant, RadioGroupOrientation};
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
// 0x030D — RadioGroup
// -----------------------------------------------------------------------------

/// Group of Radios with shared state (catalog §5 0x030D).
#[derive(Debug, Clone, PartialEq)]
pub struct RadioGroup {
    pub bind_path: StatePath,
    pub options: Vec<RadioOption>,
    pub orientation: RadioGroupOrientation,
    pub label: Option<BindRef>,
    pub density: Density,
}

impl RadioGroup {
    pub const TAG: u16 = 0x030D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.options)?));
        e.push((2, encode_to_value(&self.orientation)?));
        if let Some(v) = &self.label {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.density)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "RadioGroup")?;
        ensure_no_duplicate_keys("RadioGroup", &c.fields.0)?;
        let mut bind_path = None;
        let mut options = None;
        let mut orientation = None;
        let mut label = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => options = Some(decode_from_value(v)?),
                2 => orientation = Some(decode_from_value(v)?),
                3 => label = Some(decode_from_value(v)?),
                4 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("RadioGroup", *other)),
            }
        }
        Ok(RadioGroup {
            bind_path: bind_path.ok_or_else(|| missing_field("RadioGroup", "bind_path"))?,
            options: options.ok_or_else(|| missing_field("RadioGroup", "options"))?,
            orientation: orientation.ok_or_else(|| missing_field("RadioGroup", "orientation"))?,
            label,
            density: density.ok_or_else(|| missing_field("RadioGroup", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x030E — RadioCardGroup
// -----------------------------------------------------------------------------

/// Group of full radio cards with icon/title/description (catalog §5 0x030E).
#[derive(Debug, Clone, PartialEq)]
pub struct RadioCardGroup {
    pub bind_path: StatePath,
    pub options: Vec<RadioCardOption>,
    pub columns: u8,
    pub variant: RadioCardVariant,
}

impl RadioCardGroup {
    pub const TAG: u16 = 0x030E;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.options)?));
        e.push((2, encode_to_value(&self.columns)?));
        e.push((3, encode_to_value(&self.variant)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "RadioCardGroup")?;
        ensure_no_duplicate_keys("RadioCardGroup", &c.fields.0)?;
        let mut bind_path = None;
        let mut options = None;
        let mut columns = None;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => options = Some(decode_from_value(v)?),
                2 => columns = Some(decode_from_value(v)?),
                3 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("RadioCardGroup", *other)),
            }
        }
        Ok(RadioCardGroup {
            bind_path: bind_path.ok_or_else(|| missing_field("RadioCardGroup", "bind_path"))?,
            options: options.ok_or_else(|| missing_field("RadioCardGroup", "options"))?,
            columns: columns.ok_or_else(|| missing_field("RadioCardGroup", "columns"))?,
            variant: variant.ok_or_else(|| missing_field("RadioCardGroup", "variant"))?,
        })
    }
}
