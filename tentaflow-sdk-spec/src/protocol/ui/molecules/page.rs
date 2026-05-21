// =============================================================================
// File: protocol/ui/molecules/page.rs — Header / PageHeader / SectionHeader (catalog §2)
// =============================================================================

use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{
    BreadcrumbItem, IconRef, InlineBadge, InlineChip, NavTab,
};
use super::super::tokens::Density;
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::super::value::Value;


// -----------------------------------------------------------------------------
// Header
// -----------------------------------------------------------------------------
/// Top-of-page identifier (catalog §2 0x0001). Handlers: none.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub icon: IconRef,
    pub title: BindRef,
    pub status_badge: Option<InlineBadge>,
    pub subtitle: Option<BindRef>,
    pub meta_chips: Vec<InlineChip>,
    /// `ComponentRef<Button>` — host validates each entry's `tag` matches
    /// the Button tag (0x0401).
    pub actions: Vec<Component>,
    pub density: Density,
}

impl Header {
    pub const TAG: u16 = 0x0001;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.icon)?));
        entries.push((1, encode_to_value(&self.title)?));
        if let Some(b) = &self.status_badge {
            entries.push((2, encode_to_value(b)?));
        }
        if let Some(s) = &self.subtitle {
            entries.push((3, encode_to_value(s)?));
        }
        entries.push((4, encode_to_value(&self.meta_chips)?));
        entries.push((5, encode_to_value(&self.actions)?));
        entries.push((6, encode_to_value(&self.density)?));
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
        ensure_tag(c.tag, Self::TAG, "Header")?;
        ensure_no_duplicate_keys("Header", &c.fields.0)?;
        let mut icon = None;
        let mut title = None;
        let mut status_badge = None;
        let mut subtitle = None;
        let mut meta_chips = None;
        let mut actions = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => icon = Some(decode_from_value(v)?),
                1 => title = Some(decode_from_value(v)?),
                2 => status_badge = Some(decode_from_value(v)?),
                3 => subtitle = Some(decode_from_value(v)?),
                4 => meta_chips = Some(decode_from_value(v)?),
                5 => actions = Some(decode_from_value(v)?),
                6 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Header", *other)),
            }
        }
        Ok(Header {
            icon: icon.ok_or_else(|| missing_field("Header", "icon"))?,
            title: title.ok_or_else(|| missing_field("Header", "title"))?,
            status_badge,
            subtitle,
            meta_chips: meta_chips.unwrap_or_default(),
            actions: actions.unwrap_or_default(),
            density: density.unwrap_or(Density::Default),
        })
    }
}

// -----------------------------------------------------------------------------
// PageHeader
// -----------------------------------------------------------------------------
/// Generic page-level header (catalog §2 0x0002).
#[derive(Debug, Clone, PartialEq)]
pub struct PageHeader {
    pub title: BindRef,
    pub subtitle: Option<BindRef>,
    pub breadcrumbs: Option<Vec<BreadcrumbItem>>,
    pub actions: Vec<Component>,
    pub tabs: Option<Vec<NavTab>>,
}

impl PageHeader {
    pub const TAG: u16 = 0x0002;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.title)?));
        if let Some(s) = &self.subtitle {
            entries.push((1, encode_to_value(s)?));
        }
        if let Some(b) = &self.breadcrumbs {
            entries.push((2, encode_to_value(b)?));
        }
        entries.push((3, encode_to_value(&self.actions)?));
        if let Some(t) = &self.tabs {
            entries.push((4, encode_to_value(t)?));
        }
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
        ensure_tag(c.tag, Self::TAG, "PageHeader")?;
        ensure_no_duplicate_keys("PageHeader", &c.fields.0)?;
        let mut title = None;
        let mut subtitle = None;
        let mut breadcrumbs = None;
        let mut actions = None;
        let mut tabs = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => subtitle = Some(decode_from_value(v)?),
                2 => breadcrumbs = Some(decode_from_value(v)?),
                3 => actions = Some(decode_from_value(v)?),
                4 => tabs = Some(decode_from_value(v)?),
                other => return Err(unknown_field("PageHeader", *other)),
            }
        }
        Ok(PageHeader {
            title: title.ok_or_else(|| missing_field("PageHeader", "title"))?,
            subtitle,
            breadcrumbs,
            actions: actions.unwrap_or_default(),
            tabs,
        })
    }
}

// -----------------------------------------------------------------------------
// SectionHeader
// -----------------------------------------------------------------------------
/// Section header inside a panel/card (catalog §2 0x0004).
#[derive(Debug, Clone, PartialEq)]
pub struct SectionHeader {
    pub title: BindRef,
    pub subtitle: Option<BindRef>,
    pub actions: Vec<Component>,
    pub divider: bool,
}

impl SectionHeader {
    pub const TAG: u16 = 0x0004;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.title)?));
        if let Some(s) = &self.subtitle {
            entries.push((1, encode_to_value(s)?));
        }
        entries.push((2, encode_to_value(&self.actions)?));
        entries.push((3, encode_to_value(&self.divider)?));
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
        ensure_tag(c.tag, Self::TAG, "SectionHeader")?;
        ensure_no_duplicate_keys("SectionHeader", &c.fields.0)?;
        let mut title = None;
        let mut subtitle = None;
        let mut actions = None;
        let mut divider = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => subtitle = Some(decode_from_value(v)?),
                2 => actions = Some(decode_from_value(v)?),
                3 => divider = Some(decode_from_value(v)?),
                other => return Err(unknown_field("SectionHeader", *other)),
            }
        }
        Ok(SectionHeader {
            title: title.ok_or_else(|| missing_field("SectionHeader", "title"))?,
            subtitle,
            actions: actions.unwrap_or_default(),
            divider: divider.ok_or_else(|| missing_field("SectionHeader", "divider"))?,
        })
    }
}

