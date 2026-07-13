// =============================================================================
// File: protocol/ui/molecules/shell.rs — AppShell / LoginShell / WizardShell (catalog §2)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{IconRef, StepDef};
use super::super::tokens::Spacing;
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};

// -----------------------------------------------------------------------------
// AppShell
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
            sidebar_slot: sidebar_slot.ok_or_else(|| missing_field("AppShell", "sidebar_slot"))?,
            content_slot: content_slot.ok_or_else(|| missing_field("AppShell", "content_slot"))?,
            header_slot,
            // §2 0x0006: default sidebar_width = Spacing::Xl.
            sidebar_width: sidebar_width.unwrap_or(Spacing::Xl),
            collapsible_sidebar: collapsible_sidebar
                .ok_or_else(|| missing_field("AppShell", "collapsible_sidebar"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// LoginShell
// -----------------------------------------------------------------------------
/// Centred container for login / auth flows.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginShell {
    pub logo: IconRef,
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
// WizardShell
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
            footer_slot: footer_slot.ok_or_else(|| missing_field("WizardShell", "footer_slot"))?,
            cancellable: cancellable.ok_or_else(|| missing_field("WizardShell", "cancellable"))?,
        })
    }
}
