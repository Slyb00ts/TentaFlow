// =============================================================================
// File: protocol/ui/layout.rs — typed §3 Layout Primitives (0x0100-0x01FF)
// Purpose: per-tag Rust structs for 18 layout components (Flex, Grid, Stack,
// Cluster, Split, Card, SectionCard, Divider, Spacer, Sidebar, Tabs, NavTabs,
// Collapsible, Accordion, Tooltip, Breadcrumb, Pagination, ScrollContainer).
// Same conversion pattern as §2 molecules: into_component(id) → Component,
// try_from_component(&Component) → typed; FieldMap entries built via
// `encode_to_value`. Envelope (handlers/bind/a11y/visibility/test_id) stays
// None on `into_component` — addons attach those on the resulting Component.
// =============================================================================

use super::bind::BindRef;
use super::component::{Component, FieldMap};
use super::inline::{
    AccordionItem, BorderToken, BreadcrumbItem, GridChild, GridTrack, NavTab, SidebarItem,
    SplitSize, TabItem,
};
use super::molecules::IntoComponentError;
use super::tokens::{
    AccordionMode, BackgroundToken, BreadcrumbSeparator, CardVariant, Density,
    DividerOrientation, DividerVariant, DrawerSide, FlexAlign, FlexDirection, FlexJustify,
    FlexWrap, NavTabsVariant, PaginationVariant, RadiusToken, ScrollOrientation, ShadowToken,
    SpacerAxis, SplitOrientation, TabsVariant, Tone,
};
use super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field,
};

use super::super::value::Value;

// -----------------------------------------------------------------------------
// 0x0101 — Flex
// -----------------------------------------------------------------------------

/// Flex container (catalog §3 0x0101).
#[derive(Debug, Clone, PartialEq)]
pub struct Flex {
    pub direction: FlexDirection,
    pub gap: super::tokens::Spacing,
    pub justify: FlexJustify,
    pub align: FlexAlign,
    pub wrap: FlexWrap,
    pub children: Vec<Component>,
    pub padding: Option<super::tokens::Spacing>,
    pub background: Option<BackgroundToken>,
    pub radius: Option<RadiusToken>,
}

impl Flex {
    pub const TAG: u16 = 0x0101;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(9);
        entries.push((0, encode_to_value(&self.direction)?));
        entries.push((1, encode_to_value(&self.gap)?));
        entries.push((2, encode_to_value(&self.justify)?));
        entries.push((3, encode_to_value(&self.align)?));
        entries.push((4, encode_to_value(&self.wrap)?));
        entries.push((5, encode_to_value(&self.children)?));
        if let Some(p) = &self.padding { entries.push((6, encode_to_value(p)?)); }
        if let Some(b) = &self.background { entries.push((7, encode_to_value(b)?)); }
        if let Some(r) = &self.radius { entries.push((8, encode_to_value(r)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Flex")?;
        ensure_no_duplicate_keys("Flex", &c.fields.0)?;
        let mut direction = None;
        let mut gap = None;
        let mut justify = None;
        let mut align = None;
        let mut wrap = None;
        let mut children = None;
        let mut padding = None;
        let mut background = None;
        let mut radius = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => direction = Some(decode_from_value(v)?),
                1 => gap = Some(decode_from_value(v)?),
                2 => justify = Some(decode_from_value(v)?),
                3 => align = Some(decode_from_value(v)?),
                4 => wrap = Some(decode_from_value(v)?),
                5 => children = Some(decode_from_value(v)?),
                6 => padding = Some(decode_from_value(v)?),
                7 => background = Some(decode_from_value(v)?),
                8 => radius = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Flex", *other)),
            }
        }
        Ok(Flex {
            direction: direction.ok_or_else(|| missing_field("Flex", "direction"))?,
            // §3 0x0101 default: gap = "md".
            gap: gap.unwrap_or(super::tokens::Spacing::Md),
            justify: justify.ok_or_else(|| missing_field("Flex", "justify"))?,
            align: align.ok_or_else(|| missing_field("Flex", "align"))?,
            wrap: wrap.ok_or_else(|| missing_field("Flex", "wrap"))?,
            children: children.unwrap_or_default(),
            padding,
            background,
            radius,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0102 — Grid
// -----------------------------------------------------------------------------

/// CSS Grid container (catalog §3 0x0102).
///
/// `row_gap` / `column_gap`: `None` on the wire means "inherit `gap`" — the
/// renderer materialises the catalog-specified default. Decoding preserves
/// absent → `None` to keep the bit-identical roundtrip invariant.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub columns: GridTrack,
    pub gap: super::tokens::Spacing,
    pub row_gap: Option<super::tokens::Spacing>,
    pub column_gap: Option<super::tokens::Spacing>,
    pub children: Vec<GridChild>,
    pub padding: Option<super::tokens::Spacing>,
    pub align_items: Option<FlexAlign>,
}

impl Grid {
    pub const TAG: u16 = 0x0102;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.columns)?));
        entries.push((1, encode_to_value(&self.gap)?));
        if let Some(g) = &self.row_gap { entries.push((2, encode_to_value(g)?)); }
        if let Some(g) = &self.column_gap { entries.push((3, encode_to_value(g)?)); }
        entries.push((4, encode_to_value(&self.children)?));
        if let Some(p) = &self.padding { entries.push((5, encode_to_value(p)?)); }
        if let Some(a) = &self.align_items { entries.push((6, encode_to_value(a)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Grid")?;
        ensure_no_duplicate_keys("Grid", &c.fields.0)?;
        let mut columns = None;
        let mut gap = None;
        let mut row_gap = None;
        let mut column_gap = None;
        let mut children = None;
        let mut padding = None;
        let mut align_items = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => columns = Some(decode_from_value(v)?),
                1 => gap = Some(decode_from_value(v)?),
                2 => row_gap = Some(decode_from_value(v)?),
                3 => column_gap = Some(decode_from_value(v)?),
                4 => children = Some(decode_from_value(v)?),
                5 => padding = Some(decode_from_value(v)?),
                6 => align_items = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Grid", *other)),
            }
        }
        Ok(Grid {
            columns: columns.ok_or_else(|| missing_field("Grid", "columns"))?,
            gap: gap.ok_or_else(|| missing_field("Grid", "gap"))?,
            // §3 0x0102: row_gap / column_gap absent means "inherit `gap`" —
            // renderer materialises the default. We preserve absent=None on
            // the wire to keep the bit-identical roundtrip invariant.
            row_gap,
            column_gap,
            children: children.unwrap_or_default(),
            padding,
            align_items,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0103 — Stack
// -----------------------------------------------------------------------------

/// Vertical Stack — Flex column with built-in defaults (catalog §3 0x0103).
#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    pub gap: super::tokens::Spacing,
    pub align: FlexAlign,
    pub children: Vec<Component>,
    pub padding: Option<super::tokens::Spacing>,
}

impl Stack {
    pub const TAG: u16 = 0x0103;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.gap)?));
        entries.push((1, encode_to_value(&self.align)?));
        entries.push((2, encode_to_value(&self.children)?));
        if let Some(p) = &self.padding { entries.push((3, encode_to_value(p)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Stack")?;
        ensure_no_duplicate_keys("Stack", &c.fields.0)?;
        let mut gap = None;
        let mut align = None;
        let mut children = None;
        let mut padding = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => gap = Some(decode_from_value(v)?),
                1 => align = Some(decode_from_value(v)?),
                2 => children = Some(decode_from_value(v)?),
                3 => padding = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Stack", *other)),
            }
        }
        Ok(Stack {
            // §3 0x0103 defaults: gap="md", align="stretch".
            gap: gap.unwrap_or(super::tokens::Spacing::Md),
            align: align.unwrap_or(FlexAlign::Stretch),
            children: children.unwrap_or_default(),
            padding,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0104 — Cluster
// -----------------------------------------------------------------------------

/// Horizontal auto-wrap flow (catalog §3 0x0104).
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub gap: super::tokens::Spacing,
    pub align: FlexAlign,
    pub justify: FlexJustify,
    pub children: Vec<Component>,
}

impl Cluster {
    pub const TAG: u16 = 0x0104;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.gap)?));
        entries.push((1, encode_to_value(&self.align)?));
        entries.push((2, encode_to_value(&self.justify)?));
        entries.push((3, encode_to_value(&self.children)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Cluster")?;
        ensure_no_duplicate_keys("Cluster", &c.fields.0)?;
        let mut gap = None;
        let mut align = None;
        let mut justify = None;
        let mut children = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => gap = Some(decode_from_value(v)?),
                1 => align = Some(decode_from_value(v)?),
                2 => justify = Some(decode_from_value(v)?),
                3 => children = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Cluster", *other)),
            }
        }
        Ok(Cluster {
            gap: gap.ok_or_else(|| missing_field("Cluster", "gap"))?,
            align: align.ok_or_else(|| missing_field("Cluster", "align"))?,
            justify: justify.ok_or_else(|| missing_field("Cluster", "justify"))?,
            children: children.unwrap_or_default(),
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0105 — Split
// -----------------------------------------------------------------------------

/// 2-column split with resizable divider (catalog §3 0x0105).
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub orientation: SplitOrientation,
    pub primary_size: SplitSize,
    pub min_primary: u32,
    pub max_primary: u32,
    pub resizable: bool,
    pub primary_slot: String,
    pub secondary_slot: String,
}

impl Split {
    pub const TAG: u16 = 0x0105;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.orientation)?));
        entries.push((1, encode_to_value(&self.primary_size)?));
        entries.push((2, encode_to_value(&self.min_primary)?));
        entries.push((3, encode_to_value(&self.max_primary)?));
        entries.push((4, encode_to_value(&self.resizable)?));
        entries.push((5, encode_to_value(&self.primary_slot)?));
        entries.push((6, encode_to_value(&self.secondary_slot)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Split")?;
        ensure_no_duplicate_keys("Split", &c.fields.0)?;
        let mut orientation = None;
        let mut primary_size = None;
        let mut min_primary = None;
        let mut max_primary = None;
        let mut resizable = None;
        let mut primary_slot = None;
        let mut secondary_slot = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => orientation = Some(decode_from_value(v)?),
                1 => primary_size = Some(decode_from_value(v)?),
                2 => min_primary = Some(decode_from_value(v)?),
                3 => max_primary = Some(decode_from_value(v)?),
                4 => resizable = Some(decode_from_value(v)?),
                5 => primary_slot = Some(decode_from_value(v)?),
                6 => secondary_slot = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Split", *other)),
            }
        }
        Ok(Split {
            orientation: orientation.ok_or_else(|| missing_field("Split", "orientation"))?,
            primary_size: primary_size.ok_or_else(|| missing_field("Split", "primary_size"))?,
            min_primary: min_primary.ok_or_else(|| missing_field("Split", "min_primary"))?,
            max_primary: max_primary.ok_or_else(|| missing_field("Split", "max_primary"))?,
            resizable: resizable.ok_or_else(|| missing_field("Split", "resizable"))?,
            primary_slot: primary_slot.ok_or_else(|| missing_field("Split", "primary_slot"))?,
            secondary_slot: secondary_slot.ok_or_else(|| missing_field("Split", "secondary_slot"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0106 — Card
// -----------------------------------------------------------------------------

/// Generic container with padding/radius/shadow (catalog §3 0x0106).
/// Handler: `"click"` (when clickable=true) — addon attaches on the returned
/// Component.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub variant: CardVariant,
    pub padding: super::tokens::Spacing,
    pub gap: super::tokens::Spacing,
    pub radius: RadiusToken,
    pub shadow: ShadowToken,
    pub border: BorderToken,
    pub background: BackgroundToken,
    pub accent: Option<Tone>,
    pub children: Vec<Component>,
    pub interactive: bool,
    pub clickable: bool,
}

impl Card {
    pub const TAG: u16 = 0x0106;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(11);
        entries.push((0, encode_to_value(&self.variant)?));
        entries.push((1, encode_to_value(&self.padding)?));
        entries.push((2, encode_to_value(&self.gap)?));
        entries.push((3, encode_to_value(&self.radius)?));
        entries.push((4, encode_to_value(&self.shadow)?));
        entries.push((5, encode_to_value(&self.border)?));
        entries.push((6, encode_to_value(&self.background)?));
        if let Some(a) = &self.accent { entries.push((7, encode_to_value(a)?)); }
        entries.push((8, encode_to_value(&self.children)?));
        entries.push((9, encode_to_value(&self.interactive)?));
        entries.push((10, encode_to_value(&self.clickable)?));
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
            padding: padding.unwrap_or(super::tokens::Spacing::Lg),
            gap: gap.unwrap_or(super::tokens::Spacing::Md),
            radius: radius.unwrap_or(RadiusToken::Lg),
            shadow: resolved_shadow,
            border: border.ok_or_else(|| missing_field("Card", "border"))?,
            background: background.ok_or_else(|| missing_field("Card", "background"))?,
            accent,
            children: children.unwrap_or_default(),
            interactive: interactive.ok_or_else(|| missing_field("Card", "interactive"))?,
            clickable: clickable.ok_or_else(|| missing_field("Card", "clickable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0107 — SectionCard
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
    pub padding: super::tokens::Spacing,
    pub gap: super::tokens::Spacing,
    pub variant: CardVariant,
    pub radius: RadiusToken,
    pub shadow: ShadowToken,
    pub border: BorderToken,
    pub background: BackgroundToken,
    pub accent: Option<Tone>,
}

impl SectionCard {
    pub const TAG: u16 = 0x0107;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(14);
        entries.push((0, encode_to_value(&self.title)?));
        if let Some(s) = &self.subtitle { entries.push((1, encode_to_value(s)?)); }
        entries.push((2, encode_to_value(&self.header_actions)?));
        entries.push((3, encode_to_value(&self.header_divider)?));
        entries.push((4, encode_to_value(&self.body)?));
        if let Some(f) = &self.footer { entries.push((5, encode_to_value(f)?)); }
        entries.push((6, encode_to_value(&self.padding)?));
        entries.push((7, encode_to_value(&self.gap)?));
        entries.push((8, encode_to_value(&self.variant)?));
        entries.push((9, encode_to_value(&self.radius)?));
        entries.push((10, encode_to_value(&self.shadow)?));
        entries.push((11, encode_to_value(&self.border)?));
        entries.push((12, encode_to_value(&self.background)?));
        if let Some(a) = &self.accent { entries.push((13, encode_to_value(a)?)); }
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
                other => return Err(unknown_field("SectionCard", *other)),
            }
        }
        Ok(SectionCard {
            title: title.ok_or_else(|| missing_field("SectionCard", "title"))?,
            subtitle,
            header_actions: header_actions.unwrap_or_default(),
            header_divider: header_divider.ok_or_else(|| missing_field("SectionCard", "header_divider"))?,
            body: body.unwrap_or_default(),
            footer,
            // §3 0x0107 defaults: padding="lg", gap="md", radius="lg", shadow="subtle".
            padding: padding.unwrap_or(super::tokens::Spacing::Lg),
            gap: gap.unwrap_or(super::tokens::Spacing::Md),
            variant: variant.ok_or_else(|| missing_field("SectionCard", "variant"))?,
            radius: radius.unwrap_or(RadiusToken::Lg),
            shadow: shadow.unwrap_or(ShadowToken::Subtle),
            border: border.ok_or_else(|| missing_field("SectionCard", "border"))?,
            background: background.ok_or_else(|| missing_field("SectionCard", "background"))?,
            accent,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0108 — Divider
// -----------------------------------------------------------------------------

/// Horizontal/vertical line (catalog §3 0x0108).
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    pub orientation: DividerOrientation,
    pub variant: DividerVariant,
    pub spacing: super::tokens::Spacing,
    pub label: Option<BindRef>,
}

impl Divider {
    pub const TAG: u16 = 0x0108;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.orientation)?));
        entries.push((1, encode_to_value(&self.variant)?));
        entries.push((2, encode_to_value(&self.spacing)?));
        if let Some(l) = &self.label { entries.push((3, encode_to_value(l)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Divider")?;
        ensure_no_duplicate_keys("Divider", &c.fields.0)?;
        let mut orientation = None;
        let mut variant = None;
        let mut spacing = None;
        let mut label = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => orientation = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => spacing = Some(decode_from_value(v)?),
                3 => label = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Divider", *other)),
            }
        }
        Ok(Divider {
            orientation: orientation.ok_or_else(|| missing_field("Divider", "orientation"))?,
            variant: variant.ok_or_else(|| missing_field("Divider", "variant"))?,
            spacing: spacing.ok_or_else(|| missing_field("Divider", "spacing"))?,
            label,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0109 — Spacer
// -----------------------------------------------------------------------------

/// Empty layout space (catalog §3 0x0109).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacer {
    pub size: super::tokens::Spacing,
    pub axis: SpacerAxis,
}

impl Spacer {
    pub const TAG: u16 = 0x0109;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(2);
        entries.push((0, encode_to_value(&self.size)?));
        entries.push((1, encode_to_value(&self.axis)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Spacer")?;
        ensure_no_duplicate_keys("Spacer", &c.fields.0)?;
        let mut size = None;
        let mut axis = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => size = Some(decode_from_value(v)?),
                1 => axis = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Spacer", *other)),
            }
        }
        Ok(Spacer {
            size: size.ok_or_else(|| missing_field("Spacer", "size"))?,
            axis: axis.ok_or_else(|| missing_field("Spacer", "axis"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x010A — Sidebar
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
        if let Some(h) = &self.header_slot { entries.push((0, encode_to_value(h)?)); }
        entries.push((1, encode_to_value(&self.items)?));
        if let Some(f) = &self.footer_slot { entries.push((2, encode_to_value(f)?)); }
        if let Some(c) = &self.collapsed { entries.push((3, encode_to_value(c)?)); }
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
// 0x010B — Tabs
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
// 0x010C — NavTabs
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
            scroll_overflow: scroll_overflow.ok_or_else(|| missing_field("NavTabs", "scroll_overflow"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x010D — Collapsible
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
// 0x010E — Accordion
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

// -----------------------------------------------------------------------------
// 0x010F — Tooltip
// -----------------------------------------------------------------------------

/// Hover/focus popup with short description (catalog §3 0x010F).
#[derive(Debug, Clone, PartialEq)]
pub struct Tooltip {
    pub child: Component,
    pub content: BindRef,
    pub side: DrawerSide,
    pub max_width_px: u16,
}

impl Tooltip {
    pub const TAG: u16 = 0x010F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.child)?));
        entries.push((1, encode_to_value(&self.content)?));
        entries.push((2, encode_to_value(&self.side)?));
        entries.push((3, encode_to_value(&self.max_width_px)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Tooltip")?;
        ensure_no_duplicate_keys("Tooltip", &c.fields.0)?;
        let mut child = None;
        let mut content = None;
        let mut side = None;
        let mut max_width_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => child = Some(decode_from_value(v)?),
                1 => content = Some(decode_from_value(v)?),
                2 => side = Some(decode_from_value(v)?),
                3 => max_width_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Tooltip", *other)),
            }
        }
        Ok(Tooltip {
            child: child.ok_or_else(|| missing_field("Tooltip", "child"))?,
            content: content.ok_or_else(|| missing_field("Tooltip", "content"))?,
            side: side.ok_or_else(|| missing_field("Tooltip", "side"))?,
            max_width_px: max_width_px.ok_or_else(|| missing_field("Tooltip", "max_width_px"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0110 — Breadcrumb
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
// 0x0111 — Pagination
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
            current_page: current_page.ok_or_else(|| missing_field("Pagination", "current_page"))?,
            total_pages: total_pages.ok_or_else(|| missing_field("Pagination", "total_pages"))?,
            variant: variant.ok_or_else(|| missing_field("Pagination", "variant"))?,
            show_summary: show_summary.ok_or_else(|| missing_field("Pagination", "show_summary"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0112 — ScrollContainer
// -----------------------------------------------------------------------------

/// Scrollable area with optional sticky header (catalog §3 0x0112).
/// Handler: `"scroll_end"`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollContainer {
    pub orientation: ScrollOrientation,
    pub height: super::inline::DimensionToken,
    pub max_height: Option<super::inline::DimensionToken>,
    pub children: Vec<Component>,
    pub sticky_header_slot: Option<String>,
    pub virtualize: bool,
}

impl ScrollContainer {
    pub const TAG: u16 = 0x0112;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(6);
        entries.push((0, encode_to_value(&self.orientation)?));
        entries.push((1, encode_to_value(&self.height)?));
        if let Some(m) = &self.max_height { entries.push((2, encode_to_value(m)?)); }
        entries.push((3, encode_to_value(&self.children)?));
        if let Some(s) = &self.sticky_header_slot { entries.push((4, encode_to_value(s)?)); }
        entries.push((5, encode_to_value(&self.virtualize)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ScrollContainer")?;
        ensure_no_duplicate_keys("ScrollContainer", &c.fields.0)?;
        let mut orientation = None;
        let mut height = None;
        let mut max_height = None;
        let mut children = None;
        let mut sticky_header_slot = None;
        let mut virtualize = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => orientation = Some(decode_from_value(v)?),
                1 => height = Some(decode_from_value(v)?),
                2 => max_height = Some(decode_from_value(v)?),
                3 => children = Some(decode_from_value(v)?),
                4 => sticky_header_slot = Some(decode_from_value(v)?),
                5 => virtualize = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ScrollContainer", *other)),
            }
        }
        Ok(ScrollContainer {
            orientation: orientation.ok_or_else(|| missing_field("ScrollContainer", "orientation"))?,
            // §3 0x0112 default: height = {kind:"full"}.
            height: height.unwrap_or(super::inline::DimensionToken::Full),
            max_height,
            children: children.unwrap_or_default(),
            sticky_header_slot,
            virtualize: virtualize.ok_or_else(|| missing_field("ScrollContainer", "virtualize"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// Shared helpers
// -----------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::component::FieldMap;
    use crate::protocol::ui::inline::{DimensionToken, GridCol};
    use crate::protocol::ui::tokens::{
        AccordionMode, BackgroundToken, BreadcrumbSeparator, CardVariant, Density,
        DividerOrientation, DividerVariant, FlexAlign, FlexDirection, FlexJustify, FlexWrap,
        NavTabsVariant, PaginationVariant, RadiusToken, ScrollOrientation, ShadowToken,
        SpacerAxis, SplitOrientation, Spacing, TabsVariant, Tone,
    };
    use crate::protocol::value::Value;

    fn dummy(tag: u16) -> Component {
        Component {
            tag,
            id: "x".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        }
    }

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    #[test]
    fn flex_roundtrip() {
        let f = Flex {
            direction: FlexDirection::Row,
            gap: Spacing::Md,
            justify: FlexJustify::SpaceBetween,
            align: FlexAlign::Center,
            wrap: FlexWrap::Wrap,
            children: vec![],
            padding: Some(Spacing::Sm),
            background: Some(BackgroundToken::Subtle),
            radius: Some(RadiusToken::Md),
        };
        let c = f.clone().into_component("f").unwrap();
        assert_eq!(c.tag, Flex::TAG);
        assert_eq!(Flex::try_from_component(&c).unwrap(), f);
    }

    #[test]
    fn grid_roundtrip() {
        let g = Grid {
            columns: GridTrack::Explicit {
                cols: vec![GridCol::Fr { value: 1 }, GridCol::Auto],
            },
            gap: Spacing::Md,
            row_gap: None,
            column_gap: None,
            children: vec![],
            padding: None,
            align_items: None,
        };
        let c = g.clone().into_component("g").unwrap();
        assert_eq!(Grid::try_from_component(&c).unwrap(), g);
    }

    #[test]
    fn stack_defaults_on_absent() {
        let s = Stack {
            gap: Spacing::Lg,
            align: FlexAlign::Stretch,
            children: vec![],
            padding: None,
        };
        let mut c = s.into_component("s").unwrap();
        c.fields.0.retain(|(k, _)| *k != 0 && *k != 1);
        let back = Stack::try_from_component(&c).unwrap();
        assert_eq!(back.gap, Spacing::Md);
        assert_eq!(back.align, FlexAlign::Stretch);
    }

    #[test]
    fn cluster_roundtrip() {
        let cl = Cluster {
            gap: Spacing::Sm,
            align: FlexAlign::Center,
            justify: FlexJustify::Start,
            children: vec![dummy(0x0001)],
        };
        let c = cl.clone().into_component("cl").unwrap();
        assert_eq!(Cluster::try_from_component(&c).unwrap(), cl);
    }

    #[test]
    fn split_roundtrip() {
        let s = Split {
            orientation: SplitOrientation::Horizontal,
            primary_size: SplitSize::Percent { value: 30.0 },
            min_primary: 200,
            max_primary: 600,
            resizable: true,
            primary_slot: "left".into(),
            secondary_slot: "right".into(),
        };
        let c = s.clone().into_component("sp").unwrap();
        assert_eq!(Split::try_from_component(&c).unwrap(), s);
    }

    #[test]
    fn card_roundtrip_with_defaults() {
        let card = Card {
            variant: CardVariant::Filled,
            padding: Spacing::Lg,
            gap: Spacing::Md,
            radius: RadiusToken::Lg,
            shadow: ShadowToken::None,
            border: BorderToken::None,
            background: BackgroundToken::None,
            accent: Some(Tone::Primary),
            children: vec![],
            interactive: false,
            clickable: false,
        };
        let mut c = card.clone().into_component("card").unwrap();
        // Drop default padding(1), gap(2), radius(3) — try_from must fill defaults.
        c.fields.0.retain(|(k, _)| *k != 1 && *k != 2 && *k != 3);
        let back = Card::try_from_component(&c).unwrap();
        assert_eq!(back, card);
    }

    #[test]
    fn divider_roundtrip() {
        let d = Divider {
            orientation: DividerOrientation::Horizontal,
            variant: DividerVariant::Subtle,
            spacing: Spacing::Md,
            label: Some(lit("OR")),
        };
        let c = d.clone().into_component("d").unwrap();
        assert_eq!(Divider::try_from_component(&c).unwrap(), d);
    }

    #[test]
    fn spacer_roundtrip() {
        let s = Spacer { size: Spacing::Lg, axis: SpacerAxis::Both };
        let c = s.into_component("sp").unwrap();
        assert_eq!(Spacer::try_from_component(&c).unwrap(), s);
    }

    #[test]
    fn sidebar_roundtrip() {
        let s = Sidebar {
            header_slot: None,
            items: vec![],
            footer_slot: None,
            collapsed: None,
        };
        let c = s.clone().into_component("sb").unwrap();
        assert_eq!(Sidebar::try_from_component(&c).unwrap(), s);
    }

    #[test]
    fn tabs_roundtrip() {
        let t = Tabs {
            variant: TabsVariant::Pills,
            items: vec![],
            active_id: lit("t1"),
            content_slot: "tab_content".into(),
            density: Density::Default,
        };
        let c = t.clone().into_component("tb").unwrap();
        assert_eq!(Tabs::try_from_component(&c).unwrap(), t);
    }

    #[test]
    fn nav_tabs_roundtrip() {
        let nt = NavTabs {
            items: vec![],
            active_id: lit("nt1"),
            variant: NavTabsVariant::Underlined,
            scroll_overflow: true,
        };
        let c = nt.clone().into_component("ntb").unwrap();
        assert_eq!(NavTabs::try_from_component(&c).unwrap(), nt);
    }

    #[test]
    fn collapsible_roundtrip() {
        let col = Collapsible {
            header: dummy(0x0004),
            body: vec![],
            expanded: lit("expanded"),
            animated: true,
        };
        let c = col.clone().into_component("co").unwrap();
        assert_eq!(Collapsible::try_from_component(&c).unwrap(), col);
    }

    #[test]
    fn accordion_roundtrip() {
        let a = Accordion {
            items: vec![],
            mode: AccordionMode::Single,
            expanded_ids: lit("ids"),
        };
        let c = a.clone().into_component("ac").unwrap();
        assert_eq!(Accordion::try_from_component(&c).unwrap(), a);
    }

    #[test]
    fn tooltip_roundtrip() {
        let t = Tooltip {
            child: dummy(0x0201),
            content: lit("Hint"),
            side: super::DrawerSide::Top,
            max_width_px: 300,
        };
        let c = t.clone().into_component("tt").unwrap();
        assert_eq!(Tooltip::try_from_component(&c).unwrap(), t);
    }

    #[test]
    fn breadcrumb_roundtrip_with_default_max_items() {
        let b = Breadcrumb {
            items: vec![],
            separator: BreadcrumbSeparator::Chevron,
            max_items: 5,
        };
        let mut c = b.clone().into_component("br").unwrap();
        c.fields.0.retain(|(k, _)| *k != 2);
        let back = Breadcrumb::try_from_component(&c).unwrap();
        assert_eq!(back.max_items, 5);
    }

    #[test]
    fn pagination_roundtrip() {
        let p = Pagination {
            current_page: lit("page"),
            total_pages: lit("total"),
            variant: PaginationVariant::Compact,
            show_summary: false,
        };
        let c = p.clone().into_component("pg").unwrap();
        assert_eq!(Pagination::try_from_component(&c).unwrap(), p);
    }

    #[test]
    fn scroll_container_default_height() {
        let s = ScrollContainer {
            orientation: ScrollOrientation::Vertical,
            height: DimensionToken::Px { value: 400 },
            max_height: None,
            children: vec![],
            sticky_header_slot: None,
            virtualize: false,
        };
        let mut c = s.into_component("sc").unwrap();
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = ScrollContainer::try_from_component(&c).unwrap();
        assert_eq!(back.height, DimensionToken::Full);
    }
}
