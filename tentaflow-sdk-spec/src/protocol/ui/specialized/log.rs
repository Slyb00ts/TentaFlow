// =============================================================================
// File: protocol/ui/specialized/log.rs — VirtualizedLog (catalog §8 0x0611)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::StatePath;
use super::super::component::{Component, FieldMap};
use super::super::inline::DimensionToken;
use super::super::tokens::{Density, LogLevel, LogVariant};
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

/// Virtualised structured event log (catalog §8 0x0611).
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualizedLog {
    pub events_path: StatePath,
    pub variant: LogVariant,
    pub max_buffer_events: u32,
    pub auto_scroll: bool,
    pub searchable: bool,
    pub filter_levels: Vec<LogLevel>,
    pub show_timestamps: bool,
    pub show_source: bool,
    pub copyable: bool,
    pub height: DimensionToken,
    pub max_height: Option<DimensionToken>,
    pub density: Density,
}

impl VirtualizedLog {
    pub const TAG: u16 = 0x0611;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(12);
        e.push((0, encode_to_value(&self.events_path)?));
        e.push((1, encode_to_value(&self.variant)?));
        e.push((2, encode_to_value(&self.max_buffer_events)?));
        e.push((3, encode_to_value(&self.auto_scroll)?));
        e.push((4, encode_to_value(&self.searchable)?));
        e.push((5, encode_to_value(&self.filter_levels)?));
        e.push((6, encode_to_value(&self.show_timestamps)?));
        e.push((7, encode_to_value(&self.show_source)?));
        e.push((8, encode_to_value(&self.copyable)?));
        e.push((9, encode_to_value(&self.height)?));
        if let Some(m) = &self.max_height {
            e.push((10, encode_to_value(m)?));
        }
        e.push((11, encode_to_value(&self.density)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "VirtualizedLog")?;
        ensure_no_duplicate_keys("VirtualizedLog", &c.fields.0)?;
        let mut events_path = None;
        let mut variant = None;
        let mut max_buffer_events = None;
        let mut auto_scroll = None;
        let mut searchable = None;
        let mut filter_levels = None;
        let mut show_timestamps = None;
        let mut show_source = None;
        let mut copyable = None;
        let mut height = None;
        let mut max_height = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => events_path = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => max_buffer_events = Some(decode_from_value(v)?),
                3 => auto_scroll = Some(decode_from_value(v)?),
                4 => searchable = Some(decode_from_value(v)?),
                5 => filter_levels = Some(decode_from_value(v)?),
                6 => show_timestamps = Some(decode_from_value(v)?),
                7 => show_source = Some(decode_from_value(v)?),
                8 => copyable = Some(decode_from_value(v)?),
                9 => height = Some(decode_from_value(v)?),
                10 => max_height = Some(decode_from_value(v)?),
                11 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("VirtualizedLog", *other)),
            }
        }
        Ok(VirtualizedLog {
            events_path: events_path
                .ok_or_else(|| missing_field("VirtualizedLog", "events_path"))?,
            variant: variant.ok_or_else(|| missing_field("VirtualizedLog", "variant"))?,
            // §8 0x0611 default: max_buffer_events = 10_000.
            max_buffer_events: max_buffer_events.unwrap_or(10_000),
            auto_scroll: auto_scroll
                .ok_or_else(|| missing_field("VirtualizedLog", "auto_scroll"))?,
            searchable: searchable.ok_or_else(|| missing_field("VirtualizedLog", "searchable"))?,
            filter_levels: filter_levels
                .ok_or_else(|| missing_field("VirtualizedLog", "filter_levels"))?,
            show_timestamps: show_timestamps
                .ok_or_else(|| missing_field("VirtualizedLog", "show_timestamps"))?,
            show_source: show_source
                .ok_or_else(|| missing_field("VirtualizedLog", "show_source"))?,
            copyable: copyable.ok_or_else(|| missing_field("VirtualizedLog", "copyable"))?,
            // §8 0x0611 default: height = DimensionToken::Full (host validator may enforce on raw payloads).
            height: height.unwrap_or(DimensionToken::Full),
            max_height,
            density: density.ok_or_else(|| missing_field("VirtualizedLog", "density"))?,
        })
    }
}
