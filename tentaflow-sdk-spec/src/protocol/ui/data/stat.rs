// =============================================================================
// File: protocol/ui/data/stat.rs — KeyValue/StatCard/Stat (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{Footnote, IconRef, KvItem, Trend};
use super::super::tokens::{Density, KvLayout, StatSize, Tone};
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
// KeyValue
// -----------------------------------------------------------------------------
/// 2-column label:value list (catalog §4 0x0207).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyValue {
    pub items: Vec<KvItem>,
    pub density: Density,
    pub layout: KvLayout,
    pub label_width: Option<super::super::tokens::Spacing>,
}

impl KeyValue {
    pub const TAG: u16 = 0x0207;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.density)?));
        entries.push((2, encode_to_value(&self.layout)?));
        if let Some(lw) = &self.label_width {
            entries.push((3, encode_to_value(lw)?));
        }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "KeyValue")?;
        ensure_no_duplicate_keys("KeyValue", &c.fields.0)?;
        let mut items = None;
        let mut density = None;
        let mut layout = None;
        let mut label_width = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => density = Some(decode_from_value(v)?),
                2 => layout = Some(decode_from_value(v)?),
                3 => label_width = Some(decode_from_value(v)?),
                other => return Err(unknown_field("KeyValue", *other)),
            }
        }
        Ok(KeyValue {
            items: items.unwrap_or_default(),
            density: density.ok_or_else(|| missing_field("KeyValue", "density"))?,
            layout: layout.ok_or_else(|| missing_field("KeyValue", "layout"))?,
            label_width,
        })
    }
}

// -----------------------------------------------------------------------------
// StatCard
// -----------------------------------------------------------------------------
/// Big-number metric card (catalog §4 0x0208). Handler: `"click"` if clickable.
#[derive(Debug, Clone, PartialEq)]
pub struct StatCard {
    pub label: BindRef,
    pub icon: Option<IconRef>,
    pub value: BindRef,
    pub value_suffix: Option<BindRef>,
    pub format: Option<ValueFormat>,
    pub trend: Option<Trend>,
    pub footnote: Option<Footnote>,
    pub accent: Option<Tone>,
    pub clickable: bool,
}

impl StatCard {
    pub const TAG: u16 = 0x0208;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(9);
        entries.push((0, encode_to_value(&self.label)?));
        if let Some(i) = &self.icon {
            entries.push((1, encode_to_value(i)?));
        }
        entries.push((2, encode_to_value(&self.value)?));
        if let Some(s) = &self.value_suffix {
            entries.push((3, encode_to_value(s)?));
        }
        if let Some(f) = &self.format {
            entries.push((4, encode_to_value(f)?));
        }
        if let Some(t) = &self.trend {
            entries.push((5, encode_to_value(t)?));
        }
        if let Some(fn_) = &self.footnote {
            entries.push((6, encode_to_value(fn_)?));
        }
        if let Some(a) = &self.accent {
            entries.push((7, encode_to_value(a)?));
        }
        entries.push((8, encode_to_value(&self.clickable)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "StatCard")?;
        ensure_no_duplicate_keys("StatCard", &c.fields.0)?;
        let mut label = None;
        let mut icon = None;
        let mut value = None;
        let mut value_suffix = None;
        let mut format = None;
        let mut trend = None;
        let mut footnote = None;
        let mut accent = None;
        let mut clickable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => label = Some(decode_from_value(v)?),
                1 => icon = Some(decode_from_value(v)?),
                2 => value = Some(decode_from_value(v)?),
                3 => value_suffix = Some(decode_from_value(v)?),
                4 => format = Some(decode_from_value(v)?),
                5 => trend = Some(decode_from_value(v)?),
                6 => footnote = Some(decode_from_value(v)?),
                7 => accent = Some(decode_from_value(v)?),
                8 => clickable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("StatCard", *other)),
            }
        }
        Ok(StatCard {
            label: label.ok_or_else(|| missing_field("StatCard", "label"))?,
            icon,
            value: value.ok_or_else(|| missing_field("StatCard", "value"))?,
            value_suffix,
            format,
            trend,
            footnote,
            accent,
            clickable: clickable.ok_or_else(|| missing_field("StatCard", "clickable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Stat
// -----------------------------------------------------------------------------
/// Smaller stat without container (catalog §4 0x0209).
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub label: BindRef,
    pub value: BindRef,
    pub format: Option<ValueFormat>,
    pub trend: Option<Trend>,
    pub size: StatSize,
}

impl Stat {
    pub const TAG: u16 = 0x0209;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.label)?));
        entries.push((1, encode_to_value(&self.value)?));
        if let Some(f) = &self.format {
            entries.push((2, encode_to_value(f)?));
        }
        if let Some(t) = &self.trend {
            entries.push((3, encode_to_value(t)?));
        }
        entries.push((4, encode_to_value(&self.size)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Stat")?;
        ensure_no_duplicate_keys("Stat", &c.fields.0)?;
        let mut label = None;
        let mut value = None;
        let mut format = None;
        let mut trend = None;
        let mut size = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => label = Some(decode_from_value(v)?),
                1 => value = Some(decode_from_value(v)?),
                2 => format = Some(decode_from_value(v)?),
                3 => trend = Some(decode_from_value(v)?),
                4 => size = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Stat", *other)),
            }
        }
        Ok(Stat {
            label: label.ok_or_else(|| missing_field("Stat", "label"))?,
            value: value.ok_or_else(|| missing_field("Stat", "value"))?,
            format,
            trend,
            size: size.ok_or_else(|| missing_field("Stat", "size"))?,
        })
    }
}
