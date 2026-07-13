// =============================================================================
// File: protocol/ui/molecules/empty.rs — EmptyState / ErrorBoundary / WelcomeHero (catalog §2)
// =============================================================================

use super::super::super::value::Value;
use super::super::actions::Button;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{FeatureItem, IconRef};
use super::super::tokens::EmptyStateVariant;
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_ref_tag_decode,
    ensure_ref_tag_encode, ensure_tag, missing_field, unknown_field, IntoComponentError,
};

// -----------------------------------------------------------------------------
// EmptyState
// -----------------------------------------------------------------------------
/// "No data" / first-use placeholder (catalog §2 0x0003).
#[derive(Debug, Clone, PartialEq)]
pub struct EmptyState {
    pub icon: IconRef,
    pub heading: BindRef,
    pub message: Option<BindRef>,
    pub primary_action: Option<Component>,
    pub secondary_action: Option<Component>,
    pub variant: EmptyStateVariant,
}

impl EmptyState {
    pub const TAG: u16 = 0x0003;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        if let Some(p) = &self.primary_action {
            ensure_ref_tag_encode(p.tag, Button::TAG, "EmptyState", "primary_action")?;
        }
        if let Some(s) = &self.secondary_action {
            ensure_ref_tag_encode(s.tag, Button::TAG, "EmptyState", "secondary_action")?;
        }
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
        let primary_action: Option<Component> = primary_action;
        let secondary_action: Option<Component> = secondary_action;
        if let Some(p) = &primary_action {
            ensure_ref_tag_decode(p.tag, Button::TAG, "EmptyState", "primary_action")?;
        }
        if let Some(s) = &secondary_action {
            ensure_ref_tag_decode(s.tag, Button::TAG, "EmptyState", "secondary_action")?;
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
// ErrorBoundary
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
        for b in &self.actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "ErrorBoundary", "actions")?;
        }
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
        let actions: Vec<Component> = actions.unwrap_or_default();
        for b in &actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "ErrorBoundary", "actions")?;
        }
        Ok(ErrorBoundary {
            error_code,
            title: title.ok_or_else(|| missing_field("ErrorBoundary", "title"))?,
            message,
            actions,
            technical_details,
        })
    }
}

// -----------------------------------------------------------------------------
// WelcomeHero
// -----------------------------------------------------------------------------
/// Onboarding / welcome screen (catalog §2 0x0009).
#[derive(Debug, Clone, PartialEq)]
pub struct WelcomeHero {
    pub illustration: IconRef,
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
        ensure_ref_tag_encode(
            self.primary_action.tag,
            Button::TAG,
            "WelcomeHero",
            "primary_action",
        )?;
        if let Some(s) = &self.secondary_action {
            ensure_ref_tag_encode(s.tag, Button::TAG, "WelcomeHero", "secondary_action")?;
        }
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
        let primary_action: Component =
            primary_action.ok_or_else(|| missing_field("WelcomeHero", "primary_action"))?;
        let secondary_action: Option<Component> = secondary_action;
        ensure_ref_tag_decode(
            primary_action.tag,
            Button::TAG,
            "WelcomeHero",
            "primary_action",
        )?;
        if let Some(s) = &secondary_action {
            ensure_ref_tag_decode(s.tag, Button::TAG, "WelcomeHero", "secondary_action")?;
        }
        Ok(WelcomeHero {
            illustration: illustration
                .ok_or_else(|| missing_field("WelcomeHero", "illustration"))?,
            title: title.ok_or_else(|| missing_field("WelcomeHero", "title"))?,
            subtitle: subtitle.ok_or_else(|| missing_field("WelcomeHero", "subtitle"))?,
            features: features.unwrap_or_default(),
            primary_action,
            secondary_action,
        })
    }
}
