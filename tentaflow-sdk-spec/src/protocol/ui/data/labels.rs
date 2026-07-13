// =============================================================================
// File: protocol/ui/data/labels.rs — Badge/Chip/Tag (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{AvatarRef, IconRef};
use super::super::tokens::{BadgeVariant, ChipVariant, TagSize, Tone};
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
// Badge
// -----------------------------------------------------------------------------
/// Status/count pill (catalog §4 0x020A).
#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    pub variant: BadgeVariant,
    pub tone: Tone,
    pub label: BindRef,
    pub icon: Option<IconRef>,
    pub count: Option<BindRef>,
    pub max: u32,
    pub pulse: bool,
}

impl Badge {
    pub const TAG: u16 = 0x020A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.variant)?));
        entries.push((1, encode_to_value(&self.tone)?));
        entries.push((2, encode_to_value(&self.label)?));
        if let Some(i) = &self.icon {
            entries.push((3, encode_to_value(i)?));
        }
        if let Some(c) = &self.count {
            entries.push((4, encode_to_value(c)?));
        }
        entries.push((5, encode_to_value(&self.max)?));
        entries.push((6, encode_to_value(&self.pulse)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Badge")?;
        ensure_no_duplicate_keys("Badge", &c.fields.0)?;
        let mut variant = None;
        let mut tone = None;
        let mut label = None;
        let mut icon = None;
        let mut count = None;
        let mut max = None;
        let mut pulse = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => tone = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => icon = Some(decode_from_value(v)?),
                4 => count = Some(decode_from_value(v)?),
                5 => max = Some(decode_from_value(v)?),
                6 => pulse = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Badge", *other)),
            }
        }
        Ok(Badge {
            variant: variant.ok_or_else(|| missing_field("Badge", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("Badge", "tone"))?,
            label: label.ok_or_else(|| missing_field("Badge", "label"))?,
            icon,
            count,
            max: max.ok_or_else(|| missing_field("Badge", "max"))?,
            pulse: pulse.ok_or_else(|| missing_field("Badge", "pulse"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Chip
// -----------------------------------------------------------------------------
/// Filter/tag chip (catalog §4 0x020B). Handlers: `"click"`, `"remove"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    pub variant: ChipVariant,
    pub tone: Tone,
    pub label: BindRef,
    pub icon: Option<IconRef>,
    pub avatar: Option<AvatarRef>,
    pub selected: Option<BindRef>,
    pub removable: bool,
    /// Leading status dot colored by this tone (independent of the chip
    /// tone — e.g. a neutral chip with an entity-type colored dot).
    pub dot: Option<Tone>,
}

impl Chip {
    pub const TAG: u16 = 0x020B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(8);
        entries.push((0, encode_to_value(&self.variant)?));
        entries.push((1, encode_to_value(&self.tone)?));
        entries.push((2, encode_to_value(&self.label)?));
        if let Some(i) = &self.icon {
            entries.push((3, encode_to_value(i)?));
        }
        if let Some(a) = &self.avatar {
            entries.push((4, encode_to_value(a)?));
        }
        if let Some(s) = &self.selected {
            entries.push((5, encode_to_value(s)?));
        }
        entries.push((6, encode_to_value(&self.removable)?));
        if let Some(d) = &self.dot {
            entries.push((7, encode_to_value(d)?));
        }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Chip")?;
        ensure_no_duplicate_keys("Chip", &c.fields.0)?;
        let mut variant = None;
        let mut tone = None;
        let mut label = None;
        let mut icon = None;
        let mut avatar = None;
        let mut selected = None;
        let mut removable = None;
        let mut dot = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => tone = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => icon = Some(decode_from_value(v)?),
                4 => avatar = Some(decode_from_value(v)?),
                5 => selected = Some(decode_from_value(v)?),
                6 => removable = Some(decode_from_value(v)?),
                7 => dot = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Chip", *other)),
            }
        }
        Ok(Chip {
            variant: variant.ok_or_else(|| missing_field("Chip", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("Chip", "tone"))?,
            label: label.ok_or_else(|| missing_field("Chip", "label"))?,
            icon,
            avatar,
            selected,
            removable: removable.ok_or_else(|| missing_field("Chip", "removable"))?,
            dot,
        })
    }
}

// -----------------------------------------------------------------------------
// Tag
// -----------------------------------------------------------------------------
/// Static read-only label (catalog §4 0x020C).
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub tone: Tone,
    pub label: BindRef,
    pub size: TagSize,
}

impl Tag {
    pub const TAG: u16 = 0x020C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(3);
        entries.push((0, encode_to_value(&self.tone)?));
        entries.push((1, encode_to_value(&self.label)?));
        entries.push((2, encode_to_value(&self.size)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Tag")?;
        ensure_no_duplicate_keys("Tag", &c.fields.0)?;
        let mut tone = None;
        let mut label = None;
        let mut size = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => tone = Some(decode_from_value(v)?),
                1 => label = Some(decode_from_value(v)?),
                2 => size = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Tag", *other)),
            }
        }
        Ok(Tag {
            tone: tone.ok_or_else(|| missing_field("Tag", "tone"))?,
            label: label.ok_or_else(|| missing_field("Tag", "label"))?,
            size: size.ok_or_else(|| missing_field("Tag", "size"))?,
        })
    }
}
