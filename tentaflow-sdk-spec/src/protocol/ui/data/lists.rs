// =============================================================================
// File: protocol/ui/data/lists.rs — BulletList/Timeline (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::TimelineItem;
use super::super::tokens::{BulletListVariant, Density, TimelineOrientation, Tone};
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
// BulletList
// -----------------------------------------------------------------------------
/// Bullet/numbered/check list (catalog §4 0x020F).
#[derive(Debug, Clone, PartialEq)]
pub struct BulletList {
    pub items: Vec<BindRef>,
    pub variant: BulletListVariant,
    pub tone: Option<Tone>,
    pub density: Density,
}

impl BulletList {
    pub const TAG: u16 = 0x020F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.variant)?));
        if let Some(t) = &self.tone {
            entries.push((2, encode_to_value(t)?));
        }
        entries.push((3, encode_to_value(&self.density)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "BulletList")?;
        ensure_no_duplicate_keys("BulletList", &c.fields.0)?;
        let mut items = None;
        let mut variant = None;
        let mut tone = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("BulletList", *other)),
            }
        }
        Ok(BulletList {
            items: items.unwrap_or_default(),
            variant: variant.ok_or_else(|| missing_field("BulletList", "variant"))?,
            tone,
            density: density.ok_or_else(|| missing_field("BulletList", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Timeline
// -----------------------------------------------------------------------------
/// Chronological events (catalog §4 0x0210). Handler: `"item_click"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    pub items: Vec<TimelineItem>,
    pub orientation: TimelineOrientation,
    pub density: Density,
    pub show_dates: bool,
    pub group_by_day: bool,
}

impl Timeline {
    pub const TAG: u16 = 0x0210;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.orientation)?));
        entries.push((2, encode_to_value(&self.density)?));
        entries.push((3, encode_to_value(&self.show_dates)?));
        entries.push((4, encode_to_value(&self.group_by_day)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Timeline")?;
        ensure_no_duplicate_keys("Timeline", &c.fields.0)?;
        let mut items = None;
        let mut orientation = None;
        let mut density = None;
        let mut show_dates = None;
        let mut group_by_day = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => orientation = Some(decode_from_value(v)?),
                2 => density = Some(decode_from_value(v)?),
                3 => show_dates = Some(decode_from_value(v)?),
                4 => group_by_day = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Timeline", *other)),
            }
        }
        Ok(Timeline {
            items: items.unwrap_or_default(),
            orientation: orientation.ok_or_else(|| missing_field("Timeline", "orientation"))?,
            density: density.ok_or_else(|| missing_field("Timeline", "density"))?,
            show_dates: show_dates.ok_or_else(|| missing_field("Timeline", "show_dates"))?,
            group_by_day: group_by_day.ok_or_else(|| missing_field("Timeline", "group_by_day"))?,
        })
    }
}
