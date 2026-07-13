// =============================================================================
// File: protocol/ui/layout/nav.rs — Sidebar/Tabs/NavTabs/Breadcrumb/Pagination (catalog §3)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{BreadcrumbItem, NavTab, SidebarItem, TabItem};
use super::super::tokens::{
    BreadcrumbSeparator, Density, NavTabsVariant, PaginationVariant, TabsVariant,
};
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
// Sidebar
// -----------------------------------------------------------------------------
/// Vertical nav container (catalog §3 0x010A).
#[derive(Debug, Clone, PartialEq)]
pub struct Sidebar {
    pub header_slot: Option<String>,
    pub items: Vec<SidebarItem>,
    pub footer_slot: Option<String>,
    pub collapsed: Option<BindRef>,
}

impl Sidebar {
    pub const TAG: u16 = 0x010A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        if let Some(h) = &self.header_slot {
            entries.push((0, encode_to_value(h)?));
        }
        entries.push((1, encode_to_value(&self.items)?));
        if let Some(f) = &self.footer_slot {
            entries.push((2, encode_to_value(f)?));
        }
        if let Some(c) = &self.collapsed {
            entries.push((3, encode_to_value(c)?));
        }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Sidebar")?;
        ensure_no_duplicate_keys("Sidebar", &c.fields.0)?;
        let mut header_slot = None;
        let mut items = None;
        let mut footer_slot = None;
        let mut collapsed = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => header_slot = Some(decode_from_value(v)?),
                1 => items = Some(decode_from_value(v)?),
                2 => footer_slot = Some(decode_from_value(v)?),
                3 => collapsed = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Sidebar", *other)),
            }
        }
        Ok(Sidebar {
            header_slot,
            items: items.unwrap_or_default(),
            footer_slot,
            collapsed,
        })
    }
}

// -----------------------------------------------------------------------------
// Tabs
// -----------------------------------------------------------------------------
/// Horizontal tabs with content area (catalog §3 0x010B). Handler: `"select"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Tabs {
    pub variant: TabsVariant,
    pub items: Vec<TabItem>,
    pub active_id: BindRef,
    pub content_slot: String,
    pub density: Density,
}

impl Tabs {
    pub const TAG: u16 = 0x010B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.variant)?));
        entries.push((1, encode_to_value(&self.items)?));
        entries.push((2, encode_to_value(&self.active_id)?));
        entries.push((3, encode_to_value(&self.content_slot)?));
        entries.push((4, encode_to_value(&self.density)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Tabs")?;
        ensure_no_duplicate_keys("Tabs", &c.fields.0)?;
        let mut variant = None;
        let mut items = None;
        let mut active_id = None;
        let mut content_slot = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => items = Some(decode_from_value(v)?),
                2 => active_id = Some(decode_from_value(v)?),
                3 => content_slot = Some(decode_from_value(v)?),
                4 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Tabs", *other)),
            }
        }
        Ok(Tabs {
            variant: variant.ok_or_else(|| missing_field("Tabs", "variant"))?,
            items: items.unwrap_or_default(),
            active_id: active_id.ok_or_else(|| missing_field("Tabs", "active_id"))?,
            content_slot: content_slot.ok_or_else(|| missing_field("Tabs", "content_slot"))?,
            density: density.ok_or_else(|| missing_field("Tabs", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// NavTabs
// -----------------------------------------------------------------------------
/// Page-level routing tabs (catalog §3 0x010C). Handler: `"select"`.
#[derive(Debug, Clone, PartialEq)]
pub struct NavTabs {
    pub items: Vec<NavTab>,
    pub active_id: BindRef,
    pub variant: NavTabsVariant,
    pub scroll_overflow: bool,
}

impl NavTabs {
    pub const TAG: u16 = 0x010C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.active_id)?));
        entries.push((2, encode_to_value(&self.variant)?));
        entries.push((3, encode_to_value(&self.scroll_overflow)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "NavTabs")?;
        ensure_no_duplicate_keys("NavTabs", &c.fields.0)?;
        let mut items = None;
        let mut active_id = None;
        let mut variant = None;
        let mut scroll_overflow = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => active_id = Some(decode_from_value(v)?),
                2 => variant = Some(decode_from_value(v)?),
                3 => scroll_overflow = Some(decode_from_value(v)?),
                other => return Err(unknown_field("NavTabs", *other)),
            }
        }
        Ok(NavTabs {
            items: items.unwrap_or_default(),
            active_id: active_id.ok_or_else(|| missing_field("NavTabs", "active_id"))?,
            variant: variant.ok_or_else(|| missing_field("NavTabs", "variant"))?,
            scroll_overflow: scroll_overflow
                .ok_or_else(|| missing_field("NavTabs", "scroll_overflow"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Breadcrumb
// -----------------------------------------------------------------------------
/// Path navigation (catalog §3 0x0110).
#[derive(Debug, Clone, PartialEq)]
pub struct Breadcrumb {
    pub items: Vec<BreadcrumbItem>,
    pub separator: BreadcrumbSeparator,
    pub max_items: u8,
}

impl Breadcrumb {
    pub const TAG: u16 = 0x0110;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(3);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.separator)?));
        entries.push((2, encode_to_value(&self.max_items)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Breadcrumb")?;
        ensure_no_duplicate_keys("Breadcrumb", &c.fields.0)?;
        let mut items = None;
        let mut separator = None;
        let mut max_items = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => separator = Some(decode_from_value(v)?),
                2 => max_items = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Breadcrumb", *other)),
            }
        }
        Ok(Breadcrumb {
            items: items.unwrap_or_default(),
            separator: separator.ok_or_else(|| missing_field("Breadcrumb", "separator"))?,
            // §3 0x0110 default: max_items = 5.
            max_items: max_items.unwrap_or(5),
        })
    }
}

// -----------------------------------------------------------------------------
// Pagination
// -----------------------------------------------------------------------------
/// Page selector (catalog §3 0x0111). Handler: `"change"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pagination {
    pub current_page: BindRef,
    pub total_pages: BindRef,
    pub variant: PaginationVariant,
    pub show_summary: bool,
}

impl Pagination {
    pub const TAG: u16 = 0x0111;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.current_page)?));
        entries.push((1, encode_to_value(&self.total_pages)?));
        entries.push((2, encode_to_value(&self.variant)?));
        entries.push((3, encode_to_value(&self.show_summary)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Pagination")?;
        ensure_no_duplicate_keys("Pagination", &c.fields.0)?;
        let mut current_page = None;
        let mut total_pages = None;
        let mut variant = None;
        let mut show_summary = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => current_page = Some(decode_from_value(v)?),
                1 => total_pages = Some(decode_from_value(v)?),
                2 => variant = Some(decode_from_value(v)?),
                3 => show_summary = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Pagination", *other)),
            }
        }
        Ok(Pagination {
            current_page: current_page
                .ok_or_else(|| missing_field("Pagination", "current_page"))?,
            total_pages: total_pages.ok_or_else(|| missing_field("Pagination", "total_pages"))?,
            variant: variant.ok_or_else(|| missing_field("Pagination", "variant"))?,
            show_summary: show_summary
                .ok_or_else(|| missing_field("Pagination", "show_summary"))?,
        })
    }
}
