// =============================================================================
// File: protocol/ui/layout/containers.rs — Flex/Grid/Stack/Cluster/Split/ScrollContainer (catalog §3)
// =============================================================================

use super::super::component::{Component, FieldMap};
use super::super::inline::{
    GridChild, GridTrack, SplitSize, BoxStyle,
};
use super::super::tokens::{
    BackgroundToken, FlexAlign, FlexDirection, FlexJustify,
    FlexWrap, RadiusToken, ScrollOrientation, SplitOrientation,
};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::super::value::Value;

fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component { tag, id: id.into(), fields: FieldMap(fields), handlers: None, bind: None, a11y: None, visibility: None, test_id: None }
}

// -----------------------------------------------------------------------------
// Flex
// -----------------------------------------------------------------------------
/// Flex container (catalog §3 0x0101).
#[derive(Debug, Clone, PartialEq)]
pub struct Flex {
    pub direction: FlexDirection,
    pub gap: super::super::tokens::Spacing,
    pub justify: FlexJustify,
    pub align: FlexAlign,
    pub wrap: FlexWrap,
    pub children: Vec<Component>,
    pub padding: Option<super::super::tokens::Spacing>,
    pub background: Option<BackgroundToken>,
    pub radius: Option<RadiusToken>,
    pub style: Option<BoxStyle>,
}

impl Flex {
    pub const TAG: u16 = 0x0101;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(10);
        entries.push((0, encode_to_value(&self.direction)?));
        entries.push((1, encode_to_value(&self.gap)?));
        entries.push((2, encode_to_value(&self.justify)?));
        entries.push((3, encode_to_value(&self.align)?));
        entries.push((4, encode_to_value(&self.wrap)?));
        entries.push((5, encode_to_value(&self.children)?));
        if let Some(p) = &self.padding { entries.push((6, encode_to_value(p)?)); }
        if let Some(b) = &self.background { entries.push((7, encode_to_value(b)?)); }
        if let Some(r) = &self.radius { entries.push((8, encode_to_value(r)?)); }
        if let Some(s) = &self.style { entries.push((9, encode_to_value(s)?)); }
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
        let mut style = None;
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
                9 => style = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Flex", *other)),
            }
        }
        Ok(Flex {
            direction: direction.ok_or_else(|| missing_field("Flex", "direction"))?,
            // §3 0x0101 default: gap = "md".
            gap: gap.unwrap_or(super::super::tokens::Spacing::Md),
            justify: justify.ok_or_else(|| missing_field("Flex", "justify"))?,
            align: align.ok_or_else(|| missing_field("Flex", "align"))?,
            wrap: wrap.ok_or_else(|| missing_field("Flex", "wrap"))?,
            children: children.unwrap_or_default(),
            padding,
            background,
            radius,
            style,
        })
    }
}

// -----------------------------------------------------------------------------
// Grid
// -----------------------------------------------------------------------------
/// CSS Grid container (catalog §3 0x0102).
///
/// `row_gap` / `column_gap`: `None` on the wire means "inherit `gap`" — the
/// renderer materialises the catalog-specified default. Decoding preserves
/// absent → `None` to keep the bit-identical roundtrip invariant.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub columns: GridTrack,
    pub gap: super::super::tokens::Spacing,
    pub row_gap: Option<super::super::tokens::Spacing>,
    pub column_gap: Option<super::super::tokens::Spacing>,
    pub children: Vec<GridChild>,
    pub padding: Option<super::super::tokens::Spacing>,
    pub align_items: Option<FlexAlign>,
    pub style: Option<BoxStyle>,
}

impl Grid {
    pub const TAG: u16 = 0x0102;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(8);
        entries.push((0, encode_to_value(&self.columns)?));
        entries.push((1, encode_to_value(&self.gap)?));
        if let Some(g) = &self.row_gap { entries.push((2, encode_to_value(g)?)); }
        if let Some(g) = &self.column_gap { entries.push((3, encode_to_value(g)?)); }
        entries.push((4, encode_to_value(&self.children)?));
        if let Some(p) = &self.padding { entries.push((5, encode_to_value(p)?)); }
        if let Some(a) = &self.align_items { entries.push((6, encode_to_value(a)?)); }
        if let Some(s) = &self.style { entries.push((7, encode_to_value(s)?)); }
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
        let mut style = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => columns = Some(decode_from_value(v)?),
                1 => gap = Some(decode_from_value(v)?),
                2 => row_gap = Some(decode_from_value(v)?),
                3 => column_gap = Some(decode_from_value(v)?),
                4 => children = Some(decode_from_value(v)?),
                5 => padding = Some(decode_from_value(v)?),
                6 => align_items = Some(decode_from_value(v)?),
                7 => style = Some(decode_from_value(v)?),
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
            style,
        })
    }
}

// -----------------------------------------------------------------------------
// Stack
// -----------------------------------------------------------------------------
/// Vertical Stack — Flex column with built-in defaults (catalog §3 0x0103).
#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    pub gap: super::super::tokens::Spacing,
    pub align: FlexAlign,
    pub children: Vec<Component>,
    pub padding: Option<super::super::tokens::Spacing>,
    /// Główna oś (pionowa) — pozwala rozłożyć dzieci (np. `space_between`)
    /// w Stacku o ustalonej wysokości; brak = domyślne pakowanie od startu.
    pub justify: Option<FlexJustify>,
    pub style: Option<BoxStyle>,
}

impl Stack {
    pub const TAG: u16 = 0x0103;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(6);
        entries.push((0, encode_to_value(&self.gap)?));
        entries.push((1, encode_to_value(&self.align)?));
        entries.push((2, encode_to_value(&self.children)?));
        if let Some(p) = &self.padding { entries.push((3, encode_to_value(p)?)); }
        if let Some(j) = &self.justify { entries.push((4, encode_to_value(j)?)); }
        if let Some(s) = &self.style { entries.push((5, encode_to_value(s)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Stack")?;
        ensure_no_duplicate_keys("Stack", &c.fields.0)?;
        let mut gap = None;
        let mut align = None;
        let mut children = None;
        let mut padding = None;
        let mut justify = None;
        let mut style = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => gap = Some(decode_from_value(v)?),
                1 => align = Some(decode_from_value(v)?),
                2 => children = Some(decode_from_value(v)?),
                3 => padding = Some(decode_from_value(v)?),
                4 => justify = Some(decode_from_value(v)?),
                5 => style = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Stack", *other)),
            }
        }
        Ok(Stack {
            // §3 0x0103 defaults: gap="md", align="stretch".
            gap: gap.unwrap_or(super::super::tokens::Spacing::Md),
            align: align.unwrap_or(FlexAlign::Stretch),
            children: children.unwrap_or_default(),
            padding,
            justify,
            style,
        })
    }
}

// -----------------------------------------------------------------------------
// Cluster
// -----------------------------------------------------------------------------
/// Horizontal auto-wrap flow (catalog §3 0x0104).
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub gap: super::super::tokens::Spacing,
    pub align: FlexAlign,
    pub justify: FlexJustify,
    pub children: Vec<Component>,
    /// Zawijanie do nowej linii. `None`/`Some(true)` = domyślne `flex-wrap:wrap`
    /// (zgodność wsteczna); `Some(false)` wymusza jeden rząd (badge bez zawijania).
    pub wrap: Option<bool>,
}

impl Cluster {
    pub const TAG: u16 = 0x0104;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.gap)?));
        entries.push((1, encode_to_value(&self.align)?));
        entries.push((2, encode_to_value(&self.justify)?));
        entries.push((3, encode_to_value(&self.children)?));
        if let Some(w) = &self.wrap { entries.push((4, encode_to_value(w)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Cluster")?;
        ensure_no_duplicate_keys("Cluster", &c.fields.0)?;
        let mut gap = None;
        let mut align = None;
        let mut justify = None;
        let mut children = None;
        let mut wrap = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => gap = Some(decode_from_value(v)?),
                1 => align = Some(decode_from_value(v)?),
                2 => justify = Some(decode_from_value(v)?),
                3 => children = Some(decode_from_value(v)?),
                4 => wrap = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Cluster", *other)),
            }
        }
        Ok(Cluster {
            gap: gap.ok_or_else(|| missing_field("Cluster", "gap"))?,
            align: align.ok_or_else(|| missing_field("Cluster", "align"))?,
            justify: justify.ok_or_else(|| missing_field("Cluster", "justify"))?,
            children: children.unwrap_or_default(),
            wrap,
        })
    }
}

// -----------------------------------------------------------------------------
// Split
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
// ScrollContainer
// -----------------------------------------------------------------------------
/// Scrollable area with optional sticky header (catalog §3 0x0112).
/// Handler: `"scroll_end"`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollContainer {
    pub orientation: ScrollOrientation,
    pub height: super::super::inline::DimensionToken,
    pub max_height: Option<super::super::inline::DimensionToken>,
    pub children: Vec<Component>,
    pub sticky_header_slot: Option<String>,
    pub virtualize: bool,
    /// Gdy ustawiony, kontener przełącza się w tryb `display:flex` z odstępem
    /// między dziećmi — bez tego goła lista (np. kolekcji) nie ma żadnego gapu.
    pub gap: Option<super::super::tokens::Spacing>,
}

impl ScrollContainer {
    pub const TAG: u16 = 0x0112;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.orientation)?));
        entries.push((1, encode_to_value(&self.height)?));
        if let Some(m) = &self.max_height { entries.push((2, encode_to_value(m)?)); }
        entries.push((3, encode_to_value(&self.children)?));
        if let Some(s) = &self.sticky_header_slot { entries.push((4, encode_to_value(s)?)); }
        entries.push((5, encode_to_value(&self.virtualize)?));
        if let Some(g) = &self.gap { entries.push((6, encode_to_value(g)?)); }
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
        let mut gap = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => orientation = Some(decode_from_value(v)?),
                1 => height = Some(decode_from_value(v)?),
                2 => max_height = Some(decode_from_value(v)?),
                3 => children = Some(decode_from_value(v)?),
                4 => sticky_header_slot = Some(decode_from_value(v)?),
                5 => virtualize = Some(decode_from_value(v)?),
                6 => gap = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ScrollContainer", *other)),
            }
        }
        Ok(ScrollContainer {
            orientation: orientation.ok_or_else(|| missing_field("ScrollContainer", "orientation"))?,
            // §3 0x0112 default: height = {kind:"full"}.
            height: height.unwrap_or(super::super::inline::DimensionToken::Full),
            max_height,
            children: children.unwrap_or_default(),
            sticky_header_slot,
            virtualize: virtualize.ok_or_else(|| missing_field("ScrollContainer", "virtualize"))?,
            gap,
        })
    }
}

// -----------------------------------------------------------------------------
// Box
// -----------------------------------------------------------------------------
/// Lekki wrapper kontroli pojedynczego/zbiorczego dziecka (catalog §3 0x0115).
///
/// Rozwiązuje brak sterowania marginesem i rozmiarem dziecka wewnątrz
/// Flex/Cluster: `grow` pozwala jednemu elementowi rosnąć (np. „info rośnie,
/// badge stały"), `width` ustala wymiar, `margin` dokłada zewnętrzny odstęp.
/// `style` daje pełną, HTML-podobną kontrolę pudełka (BoxStyle §1.5), a
/// opcjonalne `direction`/`gap`/`align`/`justify` włączają proste zachowanie
/// flex dla dzieci. Wszystkie pola opcjonalne — pusty Box jest przezroczystym
/// `div`em.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Box {
    pub width: Option<super::super::inline::DimensionToken>,
    pub grow: Option<bool>,
    pub align_self: Option<FlexAlign>,
    pub padding: Option<super::super::tokens::Spacing>,
    pub margin: Option<super::super::tokens::Spacing>,
    pub children: Vec<Component>,
    pub style: Option<BoxStyle>,
    pub direction: Option<FlexDirection>,
    pub gap: Option<super::super::tokens::Spacing>,
    pub align: Option<FlexAlign>,
    pub justify: Option<FlexJustify>,
}

impl Box {
    pub const TAG: u16 = 0x0115;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(11);
        if let Some(w) = &self.width { entries.push((0, encode_to_value(w)?)); }
        if let Some(g) = &self.grow { entries.push((1, encode_to_value(g)?)); }
        if let Some(a) = &self.align_self { entries.push((2, encode_to_value(a)?)); }
        if let Some(p) = &self.padding { entries.push((3, encode_to_value(p)?)); }
        if let Some(m) = &self.margin { entries.push((4, encode_to_value(m)?)); }
        entries.push((5, encode_to_value(&self.children)?));
        if let Some(s) = &self.style { entries.push((6, encode_to_value(s)?)); }
        if let Some(d) = &self.direction { entries.push((7, encode_to_value(d)?)); }
        if let Some(g) = &self.gap { entries.push((8, encode_to_value(g)?)); }
        if let Some(a) = &self.align { entries.push((9, encode_to_value(a)?)); }
        if let Some(j) = &self.justify { entries.push((10, encode_to_value(j)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Box")?;
        ensure_no_duplicate_keys("Box", &c.fields.0)?;
        let mut width = None;
        let mut grow = None;
        let mut align_self = None;
        let mut padding = None;
        let mut margin = None;
        let mut children = None;
        let mut style = None;
        let mut direction = None;
        let mut gap = None;
        let mut align = None;
        let mut justify = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => width = Some(decode_from_value(v)?),
                1 => grow = Some(decode_from_value(v)?),
                2 => align_self = Some(decode_from_value(v)?),
                3 => padding = Some(decode_from_value(v)?),
                4 => margin = Some(decode_from_value(v)?),
                5 => children = Some(decode_from_value(v)?),
                6 => style = Some(decode_from_value(v)?),
                7 => direction = Some(decode_from_value(v)?),
                8 => gap = Some(decode_from_value(v)?),
                9 => align = Some(decode_from_value(v)?),
                10 => justify = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Box", *other)),
            }
        }
        Ok(Box {
            width,
            grow,
            align_self,
            padding,
            margin,
            children: children.unwrap_or_default(),
            style,
            direction,
            gap,
            align,
            justify,
        })
    }
}

