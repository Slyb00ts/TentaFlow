// =============================================================================
// File: protocol/ui/molecules.rs — typed §2 Structured Molecules (0x0000-0x00FF)
// Purpose: per-tag Rust structs for the 12 structured molecules (Header,
// PageHeader, EmptyState, SectionHeader, Toolbar, AppShell, LoginShell,
// ErrorBoundary, WelcomeHero, StatGroup, WizardShell, Inspector). Each typed
// struct carries the body fields only — envelope fields (id, handlers, bind,
// a11y, visibility, test_id) live on the wrapping `Component`. Conversion is
// via `into_component(id)` / `try_from_component(&Component)`. ComponentRef<X>
// fields are typed as `Component` and runtime-validated by the host against
// the expected wire tag.
// =============================================================================

use super::bind::BindRef;
use super::component::{Component, FieldMap};
use super::inline::{
    BreadcrumbItem, FeatureItem, FilterChipDef, InlineBadge, InlineChip, NavTab, StepDef,
};
use super::tokens::{Density, EmptyStateVariant, Spacing};
use super::typed_field::{decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field, unknown_field};

use super::super::value::Value;

/// Error type returned from `into_component` builders. Wraps
/// `minicbor::encode::Error<core::convert::Infallible>` from
/// [`encode_to_value`].
pub type IntoComponentError = minicbor::encode::Error<core::convert::Infallible>;

// -----------------------------------------------------------------------------
// 0x0001 — Header
// -----------------------------------------------------------------------------

/// Top-of-page identifier (catalog §2 0x0001). Handlers: none.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub icon: super::inline::IconRef,
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
// 0x0002 — PageHeader
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
// 0x0003 — EmptyState
// -----------------------------------------------------------------------------

/// "No data" / first-use placeholder (catalog §2 0x0003).
#[derive(Debug, Clone, PartialEq)]
pub struct EmptyState {
    pub icon: super::inline::IconRef,
    pub heading: BindRef,
    pub message: Option<BindRef>,
    pub primary_action: Option<Component>,
    pub secondary_action: Option<Component>,
    pub variant: EmptyStateVariant,
}

impl EmptyState {
    pub const TAG: u16 = 0x0003;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(6);
        entries.push((0, encode_to_value(&self.icon)?));
        entries.push((1, encode_to_value(&self.heading)?));
        if let Some(m) = &self.message {
            entries.push((2, encode_to_value(m)?));
        }
        if let Some(p) = &self.primary_action {
            entries.push((3, encode_to_value(p)?));
        }
        if let Some(s) = &self.secondary_action {
            entries.push((4, encode_to_value(s)?));
        }
        entries.push((5, encode_to_value(&self.variant)?));
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
        ensure_tag(c.tag, Self::TAG, "EmptyState")?;
        ensure_no_duplicate_keys("EmptyState", &c.fields.0)?;
        let mut icon = None;
        let mut heading = None;
        let mut message = None;
        let mut primary_action = None;
        let mut secondary_action = None;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => icon = Some(decode_from_value(v)?),
                1 => heading = Some(decode_from_value(v)?),
                2 => message = Some(decode_from_value(v)?),
                3 => primary_action = Some(decode_from_value(v)?),
                4 => secondary_action = Some(decode_from_value(v)?),
                5 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("EmptyState", *other)),
            }
        }
        Ok(EmptyState {
            icon: icon.ok_or_else(|| missing_field("EmptyState", "icon"))?,
            heading: heading.ok_or_else(|| missing_field("EmptyState", "heading"))?,
            message,
            primary_action,
            secondary_action,
            variant: variant.ok_or_else(|| missing_field("EmptyState", "variant"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0004 — SectionHeader
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

// -----------------------------------------------------------------------------
// 0x0005 — Toolbar
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
        Ok(Toolbar {
            search,
            filters: filters.unwrap_or_default(),
            view_mode,
            sort_control,
            trailing_actions: trailing_actions.unwrap_or_default(),
            density: density.ok_or_else(|| missing_field("Toolbar", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0006 — AppShell
// -----------------------------------------------------------------------------

/// Top-level layout (sidebar + content) for addon application panels.
#[derive(Debug, Clone, PartialEq)]
pub struct AppShell {
    pub sidebar_slot: String,
    pub content_slot: String,
    pub header_slot: Option<String>,
    pub sidebar_width: Spacing,
    pub collapsible_sidebar: bool,
}

impl AppShell {
    pub const TAG: u16 = 0x0006;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.sidebar_slot)?));
        entries.push((1, encode_to_value(&self.content_slot)?));
        if let Some(h) = &self.header_slot {
            entries.push((2, encode_to_value(h)?));
        }
        entries.push((3, encode_to_value(&self.sidebar_width)?));
        entries.push((4, encode_to_value(&self.collapsible_sidebar)?));
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
        ensure_tag(c.tag, Self::TAG, "AppShell")?;
        ensure_no_duplicate_keys("AppShell", &c.fields.0)?;
        let mut sidebar_slot = None;
        let mut content_slot = None;
        let mut header_slot = None;
        let mut sidebar_width = None;
        let mut collapsible_sidebar = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => sidebar_slot = Some(decode_from_value(v)?),
                1 => content_slot = Some(decode_from_value(v)?),
                2 => header_slot = Some(decode_from_value(v)?),
                3 => sidebar_width = Some(decode_from_value(v)?),
                4 => collapsible_sidebar = Some(decode_from_value(v)?),
                other => return Err(unknown_field("AppShell", *other)),
            }
        }
        Ok(AppShell {
            sidebar_slot: sidebar_slot
                .ok_or_else(|| missing_field("AppShell", "sidebar_slot"))?,
            content_slot: content_slot
                .ok_or_else(|| missing_field("AppShell", "content_slot"))?,
            header_slot,
            // §2 0x0006: default sidebar_width = Spacing::Xl.
            sidebar_width: sidebar_width.unwrap_or(Spacing::Xl),
            collapsible_sidebar: collapsible_sidebar
                .ok_or_else(|| missing_field("AppShell", "collapsible_sidebar"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0007 — LoginShell
// -----------------------------------------------------------------------------

/// Centred container for login / auth flows.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginShell {
    pub logo: super::inline::IconRef,
    pub title: BindRef,
    pub subtitle: Option<BindRef>,
    pub content_slot: String,
    pub footer_slot: Option<String>,
}

impl LoginShell {
    pub const TAG: u16 = 0x0007;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.logo)?));
        entries.push((1, encode_to_value(&self.title)?));
        if let Some(s) = &self.subtitle {
            entries.push((2, encode_to_value(s)?));
        }
        entries.push((3, encode_to_value(&self.content_slot)?));
        if let Some(f) = &self.footer_slot {
            entries.push((4, encode_to_value(f)?));
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
        ensure_tag(c.tag, Self::TAG, "LoginShell")?;
        ensure_no_duplicate_keys("LoginShell", &c.fields.0)?;
        let mut logo = None;
        let mut title = None;
        let mut subtitle = None;
        let mut content_slot = None;
        let mut footer_slot = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => logo = Some(decode_from_value(v)?),
                1 => title = Some(decode_from_value(v)?),
                2 => subtitle = Some(decode_from_value(v)?),
                3 => content_slot = Some(decode_from_value(v)?),
                4 => footer_slot = Some(decode_from_value(v)?),
                other => return Err(unknown_field("LoginShell", *other)),
            }
        }
        Ok(LoginShell {
            logo: logo.ok_or_else(|| missing_field("LoginShell", "logo"))?,
            title: title.ok_or_else(|| missing_field("LoginShell", "title"))?,
            subtitle,
            content_slot: content_slot
                .ok_or_else(|| missing_field("LoginShell", "content_slot"))?,
            footer_slot,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0008 — ErrorBoundary
// -----------------------------------------------------------------------------

/// Standardised error display (catalog §2 0x0008).
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorBoundary {
    pub error_code: Option<BindRef>,
    pub title: BindRef,
    pub message: Option<BindRef>,
    pub actions: Vec<Component>,
    pub technical_details: Option<BindRef>,
}

impl ErrorBoundary {
    pub const TAG: u16 = 0x0008;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        if let Some(c) = &self.error_code {
            entries.push((0, encode_to_value(c)?));
        }
        entries.push((1, encode_to_value(&self.title)?));
        if let Some(m) = &self.message {
            entries.push((2, encode_to_value(m)?));
        }
        entries.push((3, encode_to_value(&self.actions)?));
        if let Some(t) = &self.technical_details {
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
        ensure_tag(c.tag, Self::TAG, "ErrorBoundary")?;
        ensure_no_duplicate_keys("ErrorBoundary", &c.fields.0)?;
        let mut error_code = None;
        let mut title = None;
        let mut message = None;
        let mut actions = None;
        let mut technical_details = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => error_code = Some(decode_from_value(v)?),
                1 => title = Some(decode_from_value(v)?),
                2 => message = Some(decode_from_value(v)?),
                3 => actions = Some(decode_from_value(v)?),
                4 => technical_details = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ErrorBoundary", *other)),
            }
        }
        Ok(ErrorBoundary {
            error_code,
            title: title.ok_or_else(|| missing_field("ErrorBoundary", "title"))?,
            message,
            actions: actions.unwrap_or_default(),
            technical_details,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0009 — WelcomeHero
// -----------------------------------------------------------------------------

/// Onboarding / welcome screen (catalog §2 0x0009).
#[derive(Debug, Clone, PartialEq)]
pub struct WelcomeHero {
    pub illustration: super::inline::IconRef,
    pub title: BindRef,
    pub subtitle: BindRef,
    pub features: Vec<FeatureItem>,
    /// `ComponentRef<Button>` (required).
    pub primary_action: Component,
    /// `ComponentRef<Button>` (optional).
    pub secondary_action: Option<Component>,
}

impl WelcomeHero {
    pub const TAG: u16 = 0x0009;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(6);
        entries.push((0, encode_to_value(&self.illustration)?));
        entries.push((1, encode_to_value(&self.title)?));
        entries.push((2, encode_to_value(&self.subtitle)?));
        entries.push((3, encode_to_value(&self.features)?));
        entries.push((4, encode_to_value(&self.primary_action)?));
        if let Some(s) = &self.secondary_action {
            entries.push((5, encode_to_value(s)?));
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
        ensure_tag(c.tag, Self::TAG, "WelcomeHero")?;
        ensure_no_duplicate_keys("WelcomeHero", &c.fields.0)?;
        let mut illustration = None;
        let mut title = None;
        let mut subtitle = None;
        let mut features = None;
        let mut primary_action = None;
        let mut secondary_action = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => illustration = Some(decode_from_value(v)?),
                1 => title = Some(decode_from_value(v)?),
                2 => subtitle = Some(decode_from_value(v)?),
                3 => features = Some(decode_from_value(v)?),
                4 => primary_action = Some(decode_from_value(v)?),
                5 => secondary_action = Some(decode_from_value(v)?),
                other => return Err(unknown_field("WelcomeHero", *other)),
            }
        }
        Ok(WelcomeHero {
            illustration: illustration
                .ok_or_else(|| missing_field("WelcomeHero", "illustration"))?,
            title: title.ok_or_else(|| missing_field("WelcomeHero", "title"))?,
            subtitle: subtitle.ok_or_else(|| missing_field("WelcomeHero", "subtitle"))?,
            features: features.unwrap_or_default(),
            primary_action: primary_action
                .ok_or_else(|| missing_field("WelcomeHero", "primary_action"))?,
            secondary_action,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x000A — StatGroup
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
// 0x000B — WizardShell
// -----------------------------------------------------------------------------

/// Multi-step wizard layout. Handlers: `"step_change"`.
#[derive(Debug, Clone, PartialEq)]
pub struct WizardShell {
    pub steps: Vec<StepDef>,
    pub current_step_id: BindRef,
    pub content_slot: String,
    pub footer_slot: String,
    pub cancellable: bool,
}

impl WizardShell {
    pub const TAG: u16 = 0x000B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.steps)?));
        entries.push((1, encode_to_value(&self.current_step_id)?));
        entries.push((2, encode_to_value(&self.content_slot)?));
        entries.push((3, encode_to_value(&self.footer_slot)?));
        entries.push((4, encode_to_value(&self.cancellable)?));
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
        ensure_tag(c.tag, Self::TAG, "WizardShell")?;
        ensure_no_duplicate_keys("WizardShell", &c.fields.0)?;
        let mut steps = None;
        let mut current_step_id = None;
        let mut content_slot = None;
        let mut footer_slot = None;
        let mut cancellable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => steps = Some(decode_from_value(v)?),
                1 => current_step_id = Some(decode_from_value(v)?),
                2 => content_slot = Some(decode_from_value(v)?),
                3 => footer_slot = Some(decode_from_value(v)?),
                4 => cancellable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("WizardShell", *other)),
            }
        }
        Ok(WizardShell {
            steps: steps.unwrap_or_default(),
            current_step_id: current_step_id
                .ok_or_else(|| missing_field("WizardShell", "current_step_id"))?,
            content_slot: content_slot
                .ok_or_else(|| missing_field("WizardShell", "content_slot"))?,
            footer_slot: footer_slot
                .ok_or_else(|| missing_field("WizardShell", "footer_slot"))?,
            cancellable: cancellable
                .ok_or_else(|| missing_field("WizardShell", "cancellable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x000C — Inspector
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
        Ok(Inspector {
            title: title.ok_or_else(|| missing_field("Inspector", "title"))?,
            content_slot: content_slot
                .ok_or_else(|| missing_field("Inspector", "content_slot"))?,
            actions: actions.unwrap_or_default(),
            tabs,
            collapsible: collapsible
                .ok_or_else(|| missing_field("Inspector", "collapsible"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Shared helpers
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::inline::{IconRef, InlineChip};
    use crate::protocol::ui::tokens::{ChipVariant, Density, EmptyStateVariant, Spacing, Tone};
    use crate::protocol::ui::icon_name::IconName;
    use crate::protocol::value::Value;

    fn dummy_button(id: &str) -> Component {
        // §6 0x0401 Button — chunk 1.8d will provide a typed Button; here we
        // just emit a Component with the Button tag for testing.
        Component {
            tag: 0x0401,
            id: id.into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    fn icon(name: IconName) -> IconRef {
        IconRef::Named { name, size: None, tone: None }
    }

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    fn rt_molecule<F, M>(make: F, tag: u16, into: impl Fn(M) -> Component, from: impl Fn(&Component) -> Result<M, minicbor::decode::Error>)
    where
        F: Fn() -> M,
        M: PartialEq + std::fmt::Debug + Clone,
    {
        let m = make();
        let c = into(m.clone());
        assert_eq!(c.tag, tag);
        let back = from(&c).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn header_roundtrip() {
        let make = || Header {
            icon: icon(IconName::Brain),
            title: lit("TentaVision"),
            status_badge: Some(InlineBadge {
                variant: crate::protocol::ui::tokens::BadgeVariant::Solid,
                tone: Tone::Success,
                label: Some(lit("Live")),
                count: None,
                icon: None,
                pulse: false,
            }),
            subtitle: Some(lit("Camera platform")),
            meta_chips: vec![InlineChip {
                variant: ChipVariant::Soft,
                tone: Tone::Info,
                label: lit("v1.0"),
                icon: None,
                avatar: None,
                selected: None,
                removable: false,
            }],
            actions: vec![dummy_button("btn_add")],
            density: Density::Default,
        };
        rt_molecule(make, Header::TAG, |m| m.into_component("h1").unwrap(), Header::try_from_component);
    }

    #[test]
    fn page_header_roundtrip() {
        let make = || PageHeader {
            title: lit("Settings"),
            subtitle: None,
            breadcrumbs: None,
            actions: vec![dummy_button("save")],
            tabs: None,
        };
        rt_molecule(make, PageHeader::TAG, |m| m.into_component("ph").unwrap(), PageHeader::try_from_component);
    }

    #[test]
    fn empty_state_roundtrip() {
        let make = || EmptyState {
            icon: icon(IconName::Search),
            heading: lit("Nothing here"),
            message: Some(lit("Add cameras to get started")),
            primary_action: Some(dummy_button("add_cam")),
            secondary_action: None,
            variant: EmptyStateVariant::Illustrated,
        };
        rt_molecule(make, EmptyState::TAG, |m| m.into_component("es").unwrap(), EmptyState::try_from_component);
    }

    #[test]
    fn section_header_roundtrip() {
        let make = || SectionHeader {
            title: lit("Cameras"),
            subtitle: Some(lit("Manage")),
            actions: vec![],
            divider: true,
        };
        rt_molecule(make, SectionHeader::TAG, |m| m.into_component("sh").unwrap(), SectionHeader::try_from_component);
    }

    #[test]
    fn toolbar_roundtrip() {
        let make = || Toolbar {
            search: None,
            filters: vec![],
            view_mode: None,
            sort_control: None,
            trailing_actions: vec![],
            density: Density::Compact,
        };
        rt_molecule(make, Toolbar::TAG, |m| m.into_component("tb").unwrap(), Toolbar::try_from_component);
    }

    #[test]
    fn app_shell_roundtrip() {
        let make = || AppShell {
            sidebar_slot: "sidebar".into(),
            content_slot: "main".into(),
            header_slot: Some("top".into()),
            sidebar_width: Spacing::Xl,
            collapsible_sidebar: true,
        };
        rt_molecule(make, AppShell::TAG, |m| m.into_component("shell").unwrap(), AppShell::try_from_component);
    }

    #[test]
    fn login_shell_roundtrip() {
        let make = || LoginShell {
            logo: icon(IconName::Shield),
            title: lit("Sign in"),
            subtitle: None,
            content_slot: "form".into(),
            footer_slot: None,
        };
        rt_molecule(make, LoginShell::TAG, |m| m.into_component("login").unwrap(), LoginShell::try_from_component);
    }

    #[test]
    fn error_boundary_roundtrip() {
        let make = || ErrorBoundary {
            error_code: Some(lit("E_TIMEOUT")),
            title: lit("Connection lost"),
            message: Some(lit("Try again")),
            actions: vec![dummy_button("retry")],
            technical_details: None,
        };
        rt_molecule(make, ErrorBoundary::TAG, |m| m.into_component("err").unwrap(), ErrorBoundary::try_from_component);
    }

    #[test]
    fn welcome_hero_roundtrip() {
        let make = || WelcomeHero {
            illustration: icon(IconName::Sparkle),
            title: lit("Welcome"),
            subtitle: lit("Get started"),
            features: vec![],
            primary_action: dummy_button("start"),
            secondary_action: None,
        };
        rt_molecule(make, WelcomeHero::TAG, |m| m.into_component("wh").unwrap(), WelcomeHero::try_from_component);
    }

    #[test]
    fn stat_group_roundtrip() {
        let make = || StatGroup {
            stats: vec![],
            columns: 4,
            density: Density::Default,
        };
        rt_molecule(make, StatGroup::TAG, |m| m.into_component("sg").unwrap(), StatGroup::try_from_component);
    }

    #[test]
    fn wizard_shell_roundtrip() {
        let make = || WizardShell {
            steps: vec![],
            current_step_id: lit("step1"),
            content_slot: "wizard_content".into(),
            footer_slot: "wizard_footer".into(),
            cancellable: true,
        };
        rt_molecule(make, WizardShell::TAG, |m| m.into_component("wz").unwrap(), WizardShell::try_from_component);
    }

    #[test]
    fn inspector_roundtrip() {
        let make = || Inspector {
            title: lit("Details"),
            content_slot: "ins_content".into(),
            actions: vec![],
            tabs: None,
            collapsible: true,
        };
        rt_molecule(make, Inspector::TAG, |m| m.into_component("ins").unwrap(), Inspector::try_from_component);
    }

    #[test]
    fn tag_mismatch_rejected() {
        let mut c = dummy_button("x"); // tag 0x0401
        c.tag = 0x9999;
        assert!(Header::try_from_component(&c).is_err());
    }

    #[test]
    fn duplicate_field_key_rejected() {
        let mut c = Header {
            icon: icon(IconName::Brain),
            title: lit("T"),
            status_badge: None,
            subtitle: None,
            meta_chips: vec![],
            actions: vec![],
            density: Density::Default,
        }
        .into_component("h")
        .unwrap();
        // Duplicate key 1 (title appears twice).
        let title_val = c.fields.0[1].1.clone();
        c.fields.0.push((1, title_val));
        let err = Header::try_from_component(&c).unwrap_err();
        assert!(format!("{err}").contains("duplicate"));
    }

    #[test]
    fn header_density_absent_defaults_to_default() {
        let mut c = Header {
            icon: icon(IconName::Brain),
            title: lit("T"),
            status_badge: None,
            subtitle: None,
            meta_chips: vec![],
            actions: vec![],
            density: Density::Compact,
        }
        .into_component("h")
        .unwrap();
        // Strip the density entry (key 6).
        c.fields.0.retain(|(k, _)| *k != 6);
        let back = Header::try_from_component(&c).unwrap();
        assert_eq!(back.density, Density::Default);
    }

    #[test]
    fn app_shell_sidebar_width_absent_defaults_to_xl() {
        let mut c = AppShell {
            sidebar_slot: "s".into(),
            content_slot: "m".into(),
            header_slot: None,
            sidebar_width: Spacing::Sm,
            collapsible_sidebar: false,
        }
        .into_component("shell")
        .unwrap();
        c.fields.0.retain(|(k, _)| *k != 3);
        let back = AppShell::try_from_component(&c).unwrap();
        assert_eq!(back.sidebar_width, Spacing::Xl);
    }

    #[test]
    fn stat_group_columns_default_equals_stats_len() {
        let mut c = StatGroup {
            stats: vec![],
            columns: 4,
            density: Density::Default,
        }
        .into_component("sg")
        .unwrap();
        // Drop columns key (1).
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = StatGroup::try_from_component(&c).unwrap();
        assert_eq!(back.columns, back.stats.len() as u8);
    }

    #[test]
    fn unknown_field_key_rejected() {
        let mut c = Header {
            icon: icon(IconName::Brain),
            title: lit("T"),
            status_badge: None,
            subtitle: None,
            meta_chips: vec![],
            actions: vec![],
            density: Density::Default,
        }
        .into_component("h")
        .unwrap();
        c.fields.0.push((99, Value::U64(1)));
        assert!(Header::try_from_component(&c).is_err());
    }
}
