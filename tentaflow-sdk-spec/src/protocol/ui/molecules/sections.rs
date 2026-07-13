// =============================================================================
// File: protocol/ui/molecules/sections.rs — Toolbar / StatGroup / Inspector (catalog §2)
// =============================================================================

use super::super::super::value::Value;
use super::super::actions::{Button, SegmentedControl};
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::data::StatCard;
use super::super::form::{SearchBox, Select};
use super::super::inline::{FilterChipDef, NavTab};
use super::super::tokens::Density;
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_ref_tag_decode,
    ensure_ref_tag_encode, ensure_tag, missing_field, unknown_field, IntoComponentError,
};

// -----------------------------------------------------------------------------
// Toolbar
// -----------------------------------------------------------------------------
/// Search + filters + actions toolbar (catalog §2 0x0005).
#[derive(Debug, Clone, PartialEq)]
pub struct Toolbar {
    /// `ComponentRef<SearchBox>` (tag 0x0307).
    pub search: Option<Component>,
    pub filters: Vec<FilterChipDef>,
    /// `ComponentRef<SegmentedControl>` (tag 0x0409).
    pub view_mode: Option<Component>,
    /// `ComponentRef<Select>` (tag 0x0303).
    pub sort_control: Option<Component>,
    pub trailing_actions: Vec<Component>,
    pub density: Density,
}

impl Toolbar {
    pub const TAG: u16 = 0x0005;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        if let Some(s) = &self.search {
            ensure_ref_tag_encode(s.tag, SearchBox::TAG, "Toolbar", "search")?;
        }
        if let Some(v) = &self.view_mode {
            ensure_ref_tag_encode(v.tag, SegmentedControl::TAG, "Toolbar", "view_mode")?;
        }
        if let Some(s) = &self.sort_control {
            ensure_ref_tag_encode(s.tag, Select::TAG, "Toolbar", "sort_control")?;
        }
        for b in &self.trailing_actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "Toolbar", "trailing_actions")?;
        }
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(6);
        if let Some(s) = &self.search {
            entries.push((0, encode_to_value(s)?));
        }
        entries.push((1, encode_to_value(&self.filters)?));
        if let Some(v) = &self.view_mode {
            entries.push((2, encode_to_value(v)?));
        }
        if let Some(s) = &self.sort_control {
            entries.push((3, encode_to_value(s)?));
        }
        entries.push((4, encode_to_value(&self.trailing_actions)?));
        entries.push((5, encode_to_value(&self.density)?));
        Ok(Component {
            tag: Self::TAG,
            id: id.into(),
            fields: FieldMap(entries),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        })
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Toolbar")?;
        ensure_no_duplicate_keys("Toolbar", &c.fields.0)?;
        let mut search = None;
        let mut filters = None;
        let mut view_mode = None;
        let mut sort_control = None;
        let mut trailing_actions = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => search = Some(decode_from_value(v)?),
                1 => filters = Some(decode_from_value(v)?),
                2 => view_mode = Some(decode_from_value(v)?),
                3 => sort_control = Some(decode_from_value(v)?),
                4 => trailing_actions = Some(decode_from_value(v)?),
                5 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Toolbar", *other)),
            }
        }
        let trailing_actions: Vec<Component> = trailing_actions.unwrap_or_default();
        for b in &trailing_actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "Toolbar", "trailing_actions")?;
        }
        let search: Option<Component> = search;
        let view_mode: Option<Component> = view_mode;
        let sort_control: Option<Component> = sort_control;
        if let Some(s) = &search {
            ensure_ref_tag_decode(s.tag, SearchBox::TAG, "Toolbar", "search")?;
        }
        if let Some(v) = &view_mode {
            ensure_ref_tag_decode(v.tag, SegmentedControl::TAG, "Toolbar", "view_mode")?;
        }
        if let Some(s) = &sort_control {
            ensure_ref_tag_decode(s.tag, Select::TAG, "Toolbar", "sort_control")?;
        }
        Ok(Toolbar {
            search,
            filters: filters.unwrap_or_default(),
            view_mode,
            sort_control,
            trailing_actions,
            density: density.ok_or_else(|| missing_field("Toolbar", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// StatGroup
// -----------------------------------------------------------------------------
/// Grid of `StatCard`s with synced spacing (catalog §2 0x000A).
#[derive(Debug, Clone, PartialEq)]
pub struct StatGroup {
    /// `ComponentRef<StatCard>` entries (tag 0x0208). Min 2, max 6 enforced
    /// by host validator.
    pub stats: Vec<Component>,
    /// 2 | 3 | 4 | 6 — default = stats.len() (host validator default-fills).
    pub columns: u8,
    pub density: Density,
}

impl StatGroup {
    pub const TAG: u16 = 0x000A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        for s in &self.stats {
            ensure_ref_tag_encode(s.tag, StatCard::TAG, "StatGroup", "stats")?;
        }
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(3);
        entries.push((0, encode_to_value(&self.stats)?));
        entries.push((1, encode_to_value(&self.columns)?));
        entries.push((2, encode_to_value(&self.density)?));
        Ok(Component {
            tag: Self::TAG,
            id: id.into(),
            fields: FieldMap(entries),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        })
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "StatGroup")?;
        ensure_no_duplicate_keys("StatGroup", &c.fields.0)?;
        let mut stats = None;
        let mut columns = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => stats = Some(decode_from_value(v)?),
                1 => columns = Some(decode_from_value(v)?),
                2 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("StatGroup", *other)),
            }
        }
        let stats_vec: Vec<Component> = stats.unwrap_or_default();
        for s in &stats_vec {
            ensure_ref_tag_decode(s.tag, StatCard::TAG, "StatGroup", "stats")?;
        }
        // §2 0x000A: default `columns = stats.len()` (clamped to u8 range).
        let columns_default = u8::try_from(stats_vec.len()).unwrap_or(u8::MAX);
        Ok(StatGroup {
            stats: stats_vec,
            columns: columns.unwrap_or(columns_default),
            density: density.ok_or_else(|| missing_field("StatGroup", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Inspector
// -----------------------------------------------------------------------------
/// Right-rail detail panel (catalog §2 0x000C).
#[derive(Debug, Clone, PartialEq)]
pub struct Inspector {
    pub title: BindRef,
    pub content_slot: String,
    pub actions: Vec<Component>,
    pub tabs: Option<Vec<NavTab>>,
    pub collapsible: bool,
}

impl Inspector {
    pub const TAG: u16 = 0x000C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        for b in &self.actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "Inspector", "actions")?;
        }
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.title)?));
        entries.push((1, encode_to_value(&self.content_slot)?));
        entries.push((2, encode_to_value(&self.actions)?));
        if let Some(t) = &self.tabs {
            entries.push((3, encode_to_value(t)?));
        }
        entries.push((4, encode_to_value(&self.collapsible)?));
        Ok(Component {
            tag: Self::TAG,
            id: id.into(),
            fields: FieldMap(entries),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        })
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Inspector")?;
        ensure_no_duplicate_keys("Inspector", &c.fields.0)?;
        let mut title = None;
        let mut content_slot = None;
        let mut actions = None;
        let mut tabs = None;
        let mut collapsible = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => content_slot = Some(decode_from_value(v)?),
                2 => actions = Some(decode_from_value(v)?),
                3 => tabs = Some(decode_from_value(v)?),
                4 => collapsible = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Inspector", *other)),
            }
        }
        let actions: Vec<Component> = actions.unwrap_or_default();
        for b in &actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "Inspector", "actions")?;
        }
        Ok(Inspector {
            title: title.ok_or_else(|| missing_field("Inspector", "title"))?,
            content_slot: content_slot.ok_or_else(|| missing_field("Inspector", "content_slot"))?,
            actions,
            tabs,
            collapsible: collapsible.ok_or_else(|| missing_field("Inspector", "collapsible"))?,
        })
    }
}
