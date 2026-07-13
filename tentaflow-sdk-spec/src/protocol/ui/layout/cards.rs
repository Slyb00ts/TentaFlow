// =============================================================================
// File: protocol/ui/layout/cards.rs — Card/SectionCard/Collapsible/Accordion (catalog §3)
// =============================================================================

use super::super::super::value::Value;
use super::super::actions::Button;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::inline::{AccordionItem, BorderToken, BoxStyle};
use super::super::tokens::{
    AccordionMode, BackgroundToken, CardVariant, RadiusToken, ShadowToken, Tone,
};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_ref_tag_decode,
    ensure_ref_tag_encode, ensure_tag, missing_field, unknown_field, IntoComponentError,
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
// Card
// -----------------------------------------------------------------------------
/// Generic container with padding/radius/shadow (catalog §3 0x0106).
/// Handler: `"click"` (when clickable=true) — addon attaches on the returned
/// Component.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub variant: CardVariant,
    pub padding: super::super::tokens::Spacing,
    pub gap: super::super::tokens::Spacing,
    pub radius: RadiusToken,
    pub shadow: ShadowToken,
    pub border: BorderToken,
    pub background: BackgroundToken,
    pub accent: Option<Tone>,
    pub children: Vec<Component>,
    pub interactive: bool,
    pub clickable: bool,
    pub style: Option<BoxStyle>,
}

impl Card {
    pub const TAG: u16 = 0x0106;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(12);
        entries.push((0, encode_to_value(&self.variant)?));
        entries.push((1, encode_to_value(&self.padding)?));
        entries.push((2, encode_to_value(&self.gap)?));
        entries.push((3, encode_to_value(&self.radius)?));
        entries.push((4, encode_to_value(&self.shadow)?));
        entries.push((5, encode_to_value(&self.border)?));
        entries.push((6, encode_to_value(&self.background)?));
        if let Some(a) = &self.accent {
            entries.push((7, encode_to_value(a)?));
        }
        entries.push((8, encode_to_value(&self.children)?));
        entries.push((9, encode_to_value(&self.interactive)?));
        entries.push((10, encode_to_value(&self.clickable)?));
        if let Some(s) = &self.style {
            entries.push((11, encode_to_value(s)?));
        }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Card")?;
        ensure_no_duplicate_keys("Card", &c.fields.0)?;
        let mut variant = None;
        let mut padding = None;
        let mut gap = None;
        let mut radius = None;
        let mut shadow = None;
        let mut border = None;
        let mut background = None;
        let mut accent = None;
        let mut children = None;
        let mut interactive = None;
        let mut clickable = None;
        let mut style = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => padding = Some(decode_from_value(v)?),
                2 => gap = Some(decode_from_value(v)?),
                3 => radius = Some(decode_from_value(v)?),
                4 => shadow = Some(decode_from_value(v)?),
                5 => border = Some(decode_from_value(v)?),
                6 => background = Some(decode_from_value(v)?),
                7 => accent = Some(decode_from_value(v)?),
                8 => children = Some(decode_from_value(v)?),
                9 => interactive = Some(decode_from_value(v)?),
                10 => clickable = Some(decode_from_value(v)?),
                11 => style = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Card", *other)),
            }
        }
        let resolved_variant = variant.ok_or_else(|| missing_field("Card", "variant"))?;
        // §3 0x0106: shadow default = "none" for filled/outlined/ghost, "subtle" for elevated.
        let resolved_shadow = shadow.unwrap_or_else(|| match resolved_variant {
            CardVariant::Elevated => ShadowToken::Subtle,
            CardVariant::Filled | CardVariant::Outlined | CardVariant::Ghost => ShadowToken::None,
        });
        Ok(Card {
            variant: resolved_variant,
            // §3 0x0106 defaults: padding="lg", gap="md", radius="lg".
            padding: padding.unwrap_or(super::super::tokens::Spacing::Lg),
            gap: gap.unwrap_or(super::super::tokens::Spacing::Md),
            radius: radius.unwrap_or(RadiusToken::Lg),
            shadow: resolved_shadow,
            border: border.ok_or_else(|| missing_field("Card", "border"))?,
            background: background.ok_or_else(|| missing_field("Card", "background"))?,
            accent,
            children: children.unwrap_or_default(),
            interactive: interactive.ok_or_else(|| missing_field("Card", "interactive"))?,
            clickable: clickable.ok_or_else(|| missing_field("Card", "clickable"))?,
            style,
        })
    }
}

// -----------------------------------------------------------------------------
// SectionCard
// -----------------------------------------------------------------------------
/// Card with built-in SectionHeader (catalog §3 0x0107).
#[derive(Debug, Clone, PartialEq)]
pub struct SectionCard {
    pub title: BindRef,
    pub subtitle: Option<BindRef>,
    pub header_actions: Vec<Component>,
    pub header_divider: bool,
    pub body: Vec<Component>,
    pub footer: Option<Vec<Component>>,
    pub padding: super::super::tokens::Spacing,
    pub gap: super::super::tokens::Spacing,
    pub variant: CardVariant,
    pub radius: RadiusToken,
    pub shadow: ShadowToken,
    pub border: BorderToken,
    pub background: BackgroundToken,
    pub accent: Option<Tone>,
    pub style: Option<BoxStyle>,
}

impl SectionCard {
    pub const TAG: u16 = 0x0107;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        for b in &self.header_actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "SectionCard", "header_actions")?;
        }
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(15);
        entries.push((0, encode_to_value(&self.title)?));
        if let Some(s) = &self.subtitle {
            entries.push((1, encode_to_value(s)?));
        }
        entries.push((2, encode_to_value(&self.header_actions)?));
        entries.push((3, encode_to_value(&self.header_divider)?));
        entries.push((4, encode_to_value(&self.body)?));
        if let Some(f) = &self.footer {
            entries.push((5, encode_to_value(f)?));
        }
        entries.push((6, encode_to_value(&self.padding)?));
        entries.push((7, encode_to_value(&self.gap)?));
        entries.push((8, encode_to_value(&self.variant)?));
        entries.push((9, encode_to_value(&self.radius)?));
        entries.push((10, encode_to_value(&self.shadow)?));
        entries.push((11, encode_to_value(&self.border)?));
        entries.push((12, encode_to_value(&self.background)?));
        if let Some(a) = &self.accent {
            entries.push((13, encode_to_value(a)?));
        }
        if let Some(s) = &self.style {
            entries.push((14, encode_to_value(s)?));
        }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "SectionCard")?;
        ensure_no_duplicate_keys("SectionCard", &c.fields.0)?;
        let mut title = None;
        let mut subtitle = None;
        let mut header_actions = None;
        let mut header_divider = None;
        let mut body = None;
        let mut footer = None;
        let mut padding = None;
        let mut gap = None;
        let mut variant = None;
        let mut radius = None;
        let mut shadow = None;
        let mut border = None;
        let mut background = None;
        let mut accent = None;
        let mut style = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => subtitle = Some(decode_from_value(v)?),
                2 => header_actions = Some(decode_from_value(v)?),
                3 => header_divider = Some(decode_from_value(v)?),
                4 => body = Some(decode_from_value(v)?),
                5 => footer = Some(decode_from_value(v)?),
                6 => padding = Some(decode_from_value(v)?),
                7 => gap = Some(decode_from_value(v)?),
                8 => variant = Some(decode_from_value(v)?),
                9 => radius = Some(decode_from_value(v)?),
                10 => shadow = Some(decode_from_value(v)?),
                11 => border = Some(decode_from_value(v)?),
                12 => background = Some(decode_from_value(v)?),
                13 => accent = Some(decode_from_value(v)?),
                14 => style = Some(decode_from_value(v)?),
                other => return Err(unknown_field("SectionCard", *other)),
            }
        }
        let header_actions: Vec<Component> = header_actions.unwrap_or_default();
        for b in &header_actions {
            ensure_ref_tag_decode(b.tag, Button::TAG, "SectionCard", "header_actions")?;
        }
        Ok(SectionCard {
            title: title.ok_or_else(|| missing_field("SectionCard", "title"))?,
            subtitle,
            header_actions,
            header_divider: header_divider
                .ok_or_else(|| missing_field("SectionCard", "header_divider"))?,
            body: body.unwrap_or_default(),
            footer,
            // §3 0x0107 defaults: padding="lg", gap="md", radius="lg", shadow="subtle".
            padding: padding.unwrap_or(super::super::tokens::Spacing::Lg),
            gap: gap.unwrap_or(super::super::tokens::Spacing::Md),
            variant: variant.ok_or_else(|| missing_field("SectionCard", "variant"))?,
            radius: radius.unwrap_or(RadiusToken::Lg),
            shadow: shadow.unwrap_or(ShadowToken::Subtle),
            border: border.ok_or_else(|| missing_field("SectionCard", "border"))?,
            background: background.ok_or_else(|| missing_field("SectionCard", "background"))?,
            accent,
            style,
        })
    }
}

// -----------------------------------------------------------------------------
// Collapsible
// -----------------------------------------------------------------------------
/// Expandable/collapsible section (catalog §3 0x010D). Handlers: `"open"`, `"close"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Collapsible {
    pub header: Component,
    pub body: Vec<Component>,
    pub expanded: BindRef,
    pub animated: bool,
}

impl Collapsible {
    pub const TAG: u16 = 0x010D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.header)?));
        entries.push((1, encode_to_value(&self.body)?));
        entries.push((2, encode_to_value(&self.expanded)?));
        entries.push((3, encode_to_value(&self.animated)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Collapsible")?;
        ensure_no_duplicate_keys("Collapsible", &c.fields.0)?;
        let mut header = None;
        let mut body = None;
        let mut expanded = None;
        let mut animated = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => header = Some(decode_from_value(v)?),
                1 => body = Some(decode_from_value(v)?),
                2 => expanded = Some(decode_from_value(v)?),
                3 => animated = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Collapsible", *other)),
            }
        }
        Ok(Collapsible {
            header: header.ok_or_else(|| missing_field("Collapsible", "header"))?,
            body: body.unwrap_or_default(),
            expanded: expanded.ok_or_else(|| missing_field("Collapsible", "expanded"))?,
            animated: animated.ok_or_else(|| missing_field("Collapsible", "animated"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Accordion
// -----------------------------------------------------------------------------
/// Multiple-Collapsible with mutex/multi-open behavior (catalog §3 0x010E).
#[derive(Debug, Clone, PartialEq)]
pub struct Accordion {
    pub items: Vec<AccordionItem>,
    pub mode: AccordionMode,
    pub expanded_ids: BindRef,
}

impl Accordion {
    pub const TAG: u16 = 0x010E;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(3);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.mode)?));
        entries.push((2, encode_to_value(&self.expanded_ids)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Accordion")?;
        ensure_no_duplicate_keys("Accordion", &c.fields.0)?;
        let mut items = None;
        let mut mode = None;
        let mut expanded_ids = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => mode = Some(decode_from_value(v)?),
                2 => expanded_ids = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Accordion", *other)),
            }
        }
        Ok(Accordion {
            items: items.unwrap_or_default(),
            mode: mode.ok_or_else(|| missing_field("Accordion", "mode"))?,
            expanded_ids: expanded_ids.ok_or_else(|| missing_field("Accordion", "expanded_ids"))?,
        })
    }
}
