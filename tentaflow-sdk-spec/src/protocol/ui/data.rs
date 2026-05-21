// =============================================================================
// File: protocol/ui/data.rs — typed §4 Data Display (0x0200-0x02FF), part 1
// Purpose: per-tag Rust structs for text/list/stat/badge/avatar/timeline
// components (16 typed: Text/Heading/Paragraph/RichText/MonoBlock/CodeBlock/
// KeyValue/StatCard/Stat/Badge/Chip/Tag/Avatar/AvatarGroup/BulletList/
// Timeline). Same conversion pattern as molecules.rs / layout.rs.
// =============================================================================

use super::bind::BindRef;
use super::component::{Component, FieldMap};
use super::inline::{AvatarRef, Footnote, IconRef, KvItem, TimelineItem, Trend};
use super::molecules::IntoComponentError;
use super::tokens::{
    AvatarOverlap, AvatarShape, AvatarSize, AvatarStatus, BadgeVariant, BulletListVariant,
    ChipVariant, Density, KvLayout, MarkdownBlock, MarkdownMark, StatSize, TagSize,
    TextAlign, TextStyle, TextWrap, TimelineOrientation, Tone,
};
use super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field,
};
use super::value_format::ValueFormat;

use super::super::value::Value;

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
// 0x0201 — Text
// -----------------------------------------------------------------------------

/// Single-string text (catalog §4 0x0201).
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub content: BindRef,
    pub style: TextStyle,
    pub tone: Option<Tone>,
    pub align: Option<TextAlign>,
    pub wrap: Option<TextWrap>,
    pub max_lines: Option<u8>,
    pub format: Option<ValueFormat>,
}

impl Text {
    pub const TAG: u16 = 0x0201;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.content)?));
        entries.push((1, encode_to_value(&self.style)?));
        if let Some(t) = &self.tone { entries.push((2, encode_to_value(t)?)); }
        if let Some(a) = &self.align { entries.push((3, encode_to_value(a)?)); }
        if let Some(w) = &self.wrap { entries.push((4, encode_to_value(w)?)); }
        if let Some(m) = &self.max_lines { entries.push((5, encode_to_value(m)?)); }
        if let Some(f) = &self.format { entries.push((6, encode_to_value(f)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Text")?;
        ensure_no_duplicate_keys("Text", &c.fields.0)?;
        let mut content = None;
        let mut style = None;
        let mut tone = None;
        let mut align = None;
        let mut wrap = None;
        let mut max_lines = None;
        let mut format = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => style = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => align = Some(decode_from_value(v)?),
                4 => wrap = Some(decode_from_value(v)?),
                5 => max_lines = Some(decode_from_value(v)?),
                6 => format = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Text", *other)),
            }
        }
        Ok(Text {
            content: content.ok_or_else(|| missing_field("Text", "content"))?,
            style: style.ok_or_else(|| missing_field("Text", "style"))?,
            tone, align, wrap, max_lines, format,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0202 — Heading
// -----------------------------------------------------------------------------

/// Semantic heading h1-h6 (catalog §4 0x0202).
#[derive(Debug, Clone, PartialEq)]
pub struct Heading {
    pub content: BindRef,
    /// 1..=6 (HTML h1-h6). Validated by host validator (Krok 4).
    pub level: u8,
    pub tone: Option<Tone>,
    pub align: Option<TextAlign>,
}

impl Heading {
    pub const TAG: u16 = 0x0202;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.content)?));
        entries.push((1, encode_to_value(&self.level)?));
        if let Some(t) = &self.tone { entries.push((2, encode_to_value(t)?)); }
        if let Some(a) = &self.align { entries.push((3, encode_to_value(a)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Heading")?;
        ensure_no_duplicate_keys("Heading", &c.fields.0)?;
        let mut content = None;
        let mut level = None;
        let mut tone = None;
        let mut align = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => level = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => align = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Heading", *other)),
            }
        }
        Ok(Heading {
            content: content.ok_or_else(|| missing_field("Heading", "content"))?,
            level: level.ok_or_else(|| missing_field("Heading", "level"))?,
            tone, align,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0203 — Paragraph
// -----------------------------------------------------------------------------

/// Multi-line text with markdown-light support (catalog §4 0x0203).
#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    pub content: BindRef,
    pub style: TextStyle,
    pub allowed_marks: Vec<MarkdownMark>,
    pub allow_links: bool,
    pub max_lines: Option<u8>,
}

impl Paragraph {
    pub const TAG: u16 = 0x0203;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.content)?));
        entries.push((1, encode_to_value(&self.style)?));
        entries.push((2, encode_to_value(&self.allowed_marks)?));
        entries.push((3, encode_to_value(&self.allow_links)?));
        if let Some(m) = &self.max_lines { entries.push((4, encode_to_value(m)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Paragraph")?;
        ensure_no_duplicate_keys("Paragraph", &c.fields.0)?;
        let mut content = None;
        let mut style = None;
        let mut allowed_marks = None;
        let mut allow_links = None;
        let mut max_lines = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => style = Some(decode_from_value(v)?),
                2 => allowed_marks = Some(decode_from_value(v)?),
                3 => allow_links = Some(decode_from_value(v)?),
                4 => max_lines = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Paragraph", *other)),
            }
        }
        Ok(Paragraph {
            content: content.ok_or_else(|| missing_field("Paragraph", "content"))?,
            // §4 0x0203 default: style = "body".
            style: style.unwrap_or(TextStyle::Body),
            allowed_marks: allowed_marks.unwrap_or_default(),
            allow_links: allow_links.ok_or_else(|| missing_field("Paragraph", "allow_links"))?,
            max_lines,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0204 — RichText
// -----------------------------------------------------------------------------

/// Markdown source with controlled subset (catalog §4 0x0204).
#[derive(Debug, Clone, PartialEq)]
pub struct RichText {
    pub content: BindRef,
    pub allowed_blocks: Vec<MarkdownBlock>,
    pub allowed_marks: Vec<MarkdownMark>,
    pub max_height_px: Option<u16>,
}

impl RichText {
    pub const TAG: u16 = 0x0204;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.content)?));
        entries.push((1, encode_to_value(&self.allowed_blocks)?));
        entries.push((2, encode_to_value(&self.allowed_marks)?));
        if let Some(h) = &self.max_height_px { entries.push((3, encode_to_value(h)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "RichText")?;
        ensure_no_duplicate_keys("RichText", &c.fields.0)?;
        let mut content = None;
        let mut allowed_blocks = None;
        let mut allowed_marks = None;
        let mut max_height_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => allowed_blocks = Some(decode_from_value(v)?),
                2 => allowed_marks = Some(decode_from_value(v)?),
                3 => max_height_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("RichText", *other)),
            }
        }
        Ok(RichText {
            content: content.ok_or_else(|| missing_field("RichText", "content"))?,
            allowed_blocks: allowed_blocks.unwrap_or_default(),
            allowed_marks: allowed_marks.unwrap_or_default(),
            max_height_px,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0205 — MonoBlock
// -----------------------------------------------------------------------------

/// Preformatted text (no syntax highlighting) (catalog §4 0x0205).
#[derive(Debug, Clone, PartialEq)]
pub struct MonoBlock {
    pub content: BindRef,
    pub max_height_px: Option<u16>,
    pub word_wrap: bool,
    pub copyable: bool,
}

impl MonoBlock {
    pub const TAG: u16 = 0x0205;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.content)?));
        if let Some(h) = &self.max_height_px { entries.push((1, encode_to_value(h)?)); }
        entries.push((2, encode_to_value(&self.word_wrap)?));
        entries.push((3, encode_to_value(&self.copyable)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "MonoBlock")?;
        ensure_no_duplicate_keys("MonoBlock", &c.fields.0)?;
        let mut content = None;
        let mut max_height_px = None;
        let mut word_wrap = None;
        let mut copyable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => max_height_px = Some(decode_from_value(v)?),
                2 => word_wrap = Some(decode_from_value(v)?),
                3 => copyable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("MonoBlock", *other)),
            }
        }
        Ok(MonoBlock {
            content: content.ok_or_else(|| missing_field("MonoBlock", "content"))?,
            max_height_px,
            word_wrap: word_wrap.ok_or_else(|| missing_field("MonoBlock", "word_wrap"))?,
            copyable: copyable.ok_or_else(|| missing_field("MonoBlock", "copyable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0206 — CodeBlock
// -----------------------------------------------------------------------------

/// Syntax-highlighted code (catalog §4 0x0206).
#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub content: BindRef,
    pub language: String,
    pub show_line_numbers: bool,
    pub copyable: bool,
    pub max_height_px: Option<u16>,
    pub highlight_lines: Vec<u32>,
}

impl CodeBlock {
    pub const TAG: u16 = 0x0206;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(6);
        entries.push((0, encode_to_value(&self.content)?));
        entries.push((1, encode_to_value(&self.language)?));
        entries.push((2, encode_to_value(&self.show_line_numbers)?));
        entries.push((3, encode_to_value(&self.copyable)?));
        if let Some(h) = &self.max_height_px { entries.push((4, encode_to_value(h)?)); }
        entries.push((5, encode_to_value(&self.highlight_lines)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "CodeBlock")?;
        ensure_no_duplicate_keys("CodeBlock", &c.fields.0)?;
        let mut content = None;
        let mut language = None;
        let mut show_line_numbers = None;
        let mut copyable = None;
        let mut max_height_px = None;
        let mut highlight_lines = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => language = Some(decode_from_value(v)?),
                2 => show_line_numbers = Some(decode_from_value(v)?),
                3 => copyable = Some(decode_from_value(v)?),
                4 => max_height_px = Some(decode_from_value(v)?),
                5 => highlight_lines = Some(decode_from_value(v)?),
                other => return Err(unknown_field("CodeBlock", *other)),
            }
        }
        Ok(CodeBlock {
            content: content.ok_or_else(|| missing_field("CodeBlock", "content"))?,
            language: language.ok_or_else(|| missing_field("CodeBlock", "language"))?,
            show_line_numbers: show_line_numbers.ok_or_else(|| missing_field("CodeBlock", "show_line_numbers"))?,
            copyable: copyable.ok_or_else(|| missing_field("CodeBlock", "copyable"))?,
            max_height_px,
            highlight_lines: highlight_lines.unwrap_or_default(),
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0207 — KeyValue
// -----------------------------------------------------------------------------

/// 2-column label:value list (catalog §4 0x0207).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyValue {
    pub items: Vec<KvItem>,
    pub density: Density,
    pub layout: KvLayout,
    pub label_width: Option<super::tokens::Spacing>,
}

impl KeyValue {
    pub const TAG: u16 = 0x0207;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.density)?));
        entries.push((2, encode_to_value(&self.layout)?));
        if let Some(lw) = &self.label_width { entries.push((3, encode_to_value(lw)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "KeyValue")?;
        ensure_no_duplicate_keys("KeyValue", &c.fields.0)?;
        let mut items = None;
        let mut density = None;
        let mut layout = None;
        let mut label_width = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => density = Some(decode_from_value(v)?),
                2 => layout = Some(decode_from_value(v)?),
                3 => label_width = Some(decode_from_value(v)?),
                other => return Err(unknown_field("KeyValue", *other)),
            }
        }
        Ok(KeyValue {
            items: items.unwrap_or_default(),
            density: density.ok_or_else(|| missing_field("KeyValue", "density"))?,
            layout: layout.ok_or_else(|| missing_field("KeyValue", "layout"))?,
            label_width,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0208 — StatCard
// -----------------------------------------------------------------------------

/// Big-number metric card (catalog §4 0x0208). Handler: `"click"` if clickable.
#[derive(Debug, Clone, PartialEq)]
pub struct StatCard {
    pub label: BindRef,
    pub icon: Option<IconRef>,
    pub value: BindRef,
    pub value_suffix: Option<BindRef>,
    pub format: Option<ValueFormat>,
    pub trend: Option<Trend>,
    pub footnote: Option<Footnote>,
    pub accent: Option<Tone>,
    pub clickable: bool,
}

impl StatCard {
    pub const TAG: u16 = 0x0208;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(9);
        entries.push((0, encode_to_value(&self.label)?));
        if let Some(i) = &self.icon { entries.push((1, encode_to_value(i)?)); }
        entries.push((2, encode_to_value(&self.value)?));
        if let Some(s) = &self.value_suffix { entries.push((3, encode_to_value(s)?)); }
        if let Some(f) = &self.format { entries.push((4, encode_to_value(f)?)); }
        if let Some(t) = &self.trend { entries.push((5, encode_to_value(t)?)); }
        if let Some(fn_) = &self.footnote { entries.push((6, encode_to_value(fn_)?)); }
        if let Some(a) = &self.accent { entries.push((7, encode_to_value(a)?)); }
        entries.push((8, encode_to_value(&self.clickable)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "StatCard")?;
        ensure_no_duplicate_keys("StatCard", &c.fields.0)?;
        let mut label = None;
        let mut icon = None;
        let mut value = None;
        let mut value_suffix = None;
        let mut format = None;
        let mut trend = None;
        let mut footnote = None;
        let mut accent = None;
        let mut clickable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => label = Some(decode_from_value(v)?),
                1 => icon = Some(decode_from_value(v)?),
                2 => value = Some(decode_from_value(v)?),
                3 => value_suffix = Some(decode_from_value(v)?),
                4 => format = Some(decode_from_value(v)?),
                5 => trend = Some(decode_from_value(v)?),
                6 => footnote = Some(decode_from_value(v)?),
                7 => accent = Some(decode_from_value(v)?),
                8 => clickable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("StatCard", *other)),
            }
        }
        Ok(StatCard {
            label: label.ok_or_else(|| missing_field("StatCard", "label"))?,
            icon,
            value: value.ok_or_else(|| missing_field("StatCard", "value"))?,
            value_suffix, format, trend, footnote, accent,
            clickable: clickable.ok_or_else(|| missing_field("StatCard", "clickable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0209 — Stat
// -----------------------------------------------------------------------------

/// Smaller stat without container (catalog §4 0x0209).
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub label: BindRef,
    pub value: BindRef,
    pub format: Option<ValueFormat>,
    pub trend: Option<Trend>,
    pub size: StatSize,
}

impl Stat {
    pub const TAG: u16 = 0x0209;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.label)?));
        entries.push((1, encode_to_value(&self.value)?));
        if let Some(f) = &self.format { entries.push((2, encode_to_value(f)?)); }
        if let Some(t) = &self.trend { entries.push((3, encode_to_value(t)?)); }
        entries.push((4, encode_to_value(&self.size)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Stat")?;
        ensure_no_duplicate_keys("Stat", &c.fields.0)?;
        let mut label = None;
        let mut value = None;
        let mut format = None;
        let mut trend = None;
        let mut size = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => label = Some(decode_from_value(v)?),
                1 => value = Some(decode_from_value(v)?),
                2 => format = Some(decode_from_value(v)?),
                3 => trend = Some(decode_from_value(v)?),
                4 => size = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Stat", *other)),
            }
        }
        Ok(Stat {
            label: label.ok_or_else(|| missing_field("Stat", "label"))?,
            value: value.ok_or_else(|| missing_field("Stat", "value"))?,
            format, trend,
            size: size.ok_or_else(|| missing_field("Stat", "size"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x020A — Badge
// -----------------------------------------------------------------------------

/// Status/count pill (catalog §4 0x020A).
#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    pub variant: BadgeVariant,
    pub tone: Tone,
    pub label: BindRef,
    pub icon: Option<IconRef>,
    pub count: Option<BindRef>,
    pub max: u32,
    pub pulse: bool,
}

impl Badge {
    pub const TAG: u16 = 0x020A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.variant)?));
        entries.push((1, encode_to_value(&self.tone)?));
        entries.push((2, encode_to_value(&self.label)?));
        if let Some(i) = &self.icon { entries.push((3, encode_to_value(i)?)); }
        if let Some(c) = &self.count { entries.push((4, encode_to_value(c)?)); }
        entries.push((5, encode_to_value(&self.max)?));
        entries.push((6, encode_to_value(&self.pulse)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Badge")?;
        ensure_no_duplicate_keys("Badge", &c.fields.0)?;
        let mut variant = None;
        let mut tone = None;
        let mut label = None;
        let mut icon = None;
        let mut count = None;
        let mut max = None;
        let mut pulse = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => tone = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => icon = Some(decode_from_value(v)?),
                4 => count = Some(decode_from_value(v)?),
                5 => max = Some(decode_from_value(v)?),
                6 => pulse = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Badge", *other)),
            }
        }
        Ok(Badge {
            variant: variant.ok_or_else(|| missing_field("Badge", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("Badge", "tone"))?,
            label: label.ok_or_else(|| missing_field("Badge", "label"))?,
            icon, count,
            max: max.ok_or_else(|| missing_field("Badge", "max"))?,
            pulse: pulse.ok_or_else(|| missing_field("Badge", "pulse"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x020B — Chip
// -----------------------------------------------------------------------------

/// Filter/tag chip (catalog §4 0x020B). Handlers: `"click"`, `"remove"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    pub variant: ChipVariant,
    pub tone: Tone,
    pub label: BindRef,
    pub icon: Option<IconRef>,
    pub avatar: Option<AvatarRef>,
    pub selected: Option<BindRef>,
    pub removable: bool,
}

impl Chip {
    pub const TAG: u16 = 0x020B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(7);
        entries.push((0, encode_to_value(&self.variant)?));
        entries.push((1, encode_to_value(&self.tone)?));
        entries.push((2, encode_to_value(&self.label)?));
        if let Some(i) = &self.icon { entries.push((3, encode_to_value(i)?)); }
        if let Some(a) = &self.avatar { entries.push((4, encode_to_value(a)?)); }
        if let Some(s) = &self.selected { entries.push((5, encode_to_value(s)?)); }
        entries.push((6, encode_to_value(&self.removable)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Chip")?;
        ensure_no_duplicate_keys("Chip", &c.fields.0)?;
        let mut variant = None;
        let mut tone = None;
        let mut label = None;
        let mut icon = None;
        let mut avatar = None;
        let mut selected = None;
        let mut removable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                1 => tone = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => icon = Some(decode_from_value(v)?),
                4 => avatar = Some(decode_from_value(v)?),
                5 => selected = Some(decode_from_value(v)?),
                6 => removable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Chip", *other)),
            }
        }
        Ok(Chip {
            variant: variant.ok_or_else(|| missing_field("Chip", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("Chip", "tone"))?,
            label: label.ok_or_else(|| missing_field("Chip", "label"))?,
            icon, avatar, selected,
            removable: removable.ok_or_else(|| missing_field("Chip", "removable"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x020C — Tag
// -----------------------------------------------------------------------------

/// Static read-only label (catalog §4 0x020C).
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub tone: Tone,
    pub label: BindRef,
    pub size: TagSize,
}

impl Tag {
    pub const TAG: u16 = 0x020C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(3);
        entries.push((0, encode_to_value(&self.tone)?));
        entries.push((1, encode_to_value(&self.label)?));
        entries.push((2, encode_to_value(&self.size)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Tag")?;
        ensure_no_duplicate_keys("Tag", &c.fields.0)?;
        let mut tone = None;
        let mut label = None;
        let mut size = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => tone = Some(decode_from_value(v)?),
                1 => label = Some(decode_from_value(v)?),
                2 => size = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Tag", *other)),
            }
        }
        Ok(Tag {
            tone: tone.ok_or_else(|| missing_field("Tag", "tone"))?,
            label: label.ok_or_else(|| missing_field("Tag", "label"))?,
            size: size.ok_or_else(|| missing_field("Tag", "size"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x020D — Avatar
// -----------------------------------------------------------------------------

/// User avatar (catalog §4 0x020D).
#[derive(Debug, Clone, PartialEq)]
pub struct Avatar {
    pub source: AvatarRef,
    pub size: AvatarSize,
    pub shape: AvatarShape,
    pub status: Option<AvatarStatus>,
    pub tone: Option<Tone>,
}

impl Avatar {
    pub const TAG: u16 = 0x020D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.source)?));
        entries.push((1, encode_to_value(&self.size)?));
        entries.push((2, encode_to_value(&self.shape)?));
        if let Some(s) = &self.status { entries.push((3, encode_to_value(s)?)); }
        if let Some(t) = &self.tone { entries.push((4, encode_to_value(t)?)); }
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Avatar")?;
        ensure_no_duplicate_keys("Avatar", &c.fields.0)?;
        let mut source = None;
        let mut size = None;
        let mut shape = None;
        let mut status = None;
        let mut tone = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => source = Some(decode_from_value(v)?),
                1 => size = Some(decode_from_value(v)?),
                2 => shape = Some(decode_from_value(v)?),
                3 => status = Some(decode_from_value(v)?),
                4 => tone = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Avatar", *other)),
            }
        }
        Ok(Avatar {
            source: source.ok_or_else(|| missing_field("Avatar", "source"))?,
            size: size.ok_or_else(|| missing_field("Avatar", "size"))?,
            shape: shape.ok_or_else(|| missing_field("Avatar", "shape"))?,
            status, tone,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x020E — AvatarGroup
// -----------------------------------------------------------------------------

/// Stack of avatars with overflow indicator (catalog §4 0x020E).
#[derive(Debug, Clone, PartialEq)]
pub struct AvatarGroup {
    /// `ComponentRef<Avatar>` entries (tag 0x020D).
    pub avatars: Vec<Component>,
    pub max_visible: u8,
    pub overlap: AvatarOverlap,
    pub size: AvatarSize,
}

impl AvatarGroup {
    pub const TAG: u16 = 0x020E;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.avatars)?));
        entries.push((1, encode_to_value(&self.max_visible)?));
        entries.push((2, encode_to_value(&self.overlap)?));
        entries.push((3, encode_to_value(&self.size)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "AvatarGroup")?;
        ensure_no_duplicate_keys("AvatarGroup", &c.fields.0)?;
        let mut avatars = None;
        let mut max_visible = None;
        let mut overlap = None;
        let mut size = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => avatars = Some(decode_from_value(v)?),
                1 => max_visible = Some(decode_from_value(v)?),
                2 => overlap = Some(decode_from_value(v)?),
                3 => size = Some(decode_from_value(v)?),
                other => return Err(unknown_field("AvatarGroup", *other)),
            }
        }
        Ok(AvatarGroup {
            avatars: avatars.unwrap_or_default(),
            max_visible: max_visible.ok_or_else(|| missing_field("AvatarGroup", "max_visible"))?,
            overlap: overlap.ok_or_else(|| missing_field("AvatarGroup", "overlap"))?,
            size: size.ok_or_else(|| missing_field("AvatarGroup", "size"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x020F — BulletList
// -----------------------------------------------------------------------------

/// Bullet/numbered/check list (catalog §4 0x020F).
#[derive(Debug, Clone, PartialEq)]
pub struct BulletList {
    pub items: Vec<BindRef>,
    pub variant: BulletListVariant,
    pub tone: Option<Tone>,
    pub density: Density,
}

impl BulletList {
    pub const TAG: u16 = 0x020F;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(4);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.variant)?));
        if let Some(t) = &self.tone { entries.push((2, encode_to_value(t)?)); }
        entries.push((3, encode_to_value(&self.density)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "BulletList")?;
        ensure_no_duplicate_keys("BulletList", &c.fields.0)?;
        let mut items = None;
        let mut variant = None;
        let mut tone = None;
        let mut density = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => density = Some(decode_from_value(v)?),
                other => return Err(unknown_field("BulletList", *other)),
            }
        }
        Ok(BulletList {
            items: items.unwrap_or_default(),
            variant: variant.ok_or_else(|| missing_field("BulletList", "variant"))?,
            tone,
            density: density.ok_or_else(|| missing_field("BulletList", "density"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0210 — Timeline
// -----------------------------------------------------------------------------

/// Chronological events (catalog §4 0x0210). Handler: `"item_click"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    pub items: Vec<TimelineItem>,
    pub orientation: TimelineOrientation,
    pub density: Density,
    pub show_dates: bool,
    pub group_by_day: bool,
}

impl Timeline {
    pub const TAG: u16 = 0x0210;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(5);
        entries.push((0, encode_to_value(&self.items)?));
        entries.push((1, encode_to_value(&self.orientation)?));
        entries.push((2, encode_to_value(&self.density)?));
        entries.push((3, encode_to_value(&self.show_dates)?));
        entries.push((4, encode_to_value(&self.group_by_day)?));
        Ok(component(Self::TAG, id, entries))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Timeline")?;
        ensure_no_duplicate_keys("Timeline", &c.fields.0)?;
        let mut items = None;
        let mut orientation = None;
        let mut density = None;
        let mut show_dates = None;
        let mut group_by_day = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => orientation = Some(decode_from_value(v)?),
                2 => density = Some(decode_from_value(v)?),
                3 => show_dates = Some(decode_from_value(v)?),
                4 => group_by_day = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Timeline", *other)),
            }
        }
        Ok(Timeline {
            items: items.unwrap_or_default(),
            orientation: orientation.ok_or_else(|| missing_field("Timeline", "orientation"))?,
            density: density.ok_or_else(|| missing_field("Timeline", "density"))?,
            show_dates: show_dates.ok_or_else(|| missing_field("Timeline", "show_dates"))?,
            group_by_day: group_by_day.ok_or_else(|| missing_field("Timeline", "group_by_day"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::component::FieldMap;
    use crate::protocol::ui::icon_name::IconName;
    use crate::protocol::ui::inline::{IconRef, TrendDirection};
    use crate::protocol::value::Value;

    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    fn dummy(tag: u16) -> Component {
        Component {
            tag, id: "x".into(), fields: FieldMap::default(),
            handlers: None, bind: None, a11y: None, visibility: None, test_id: None,
        }
    }

    fn rt<T>(make: T, into: impl Fn(T) -> Component, from: impl Fn(&Component) -> Result<T, minicbor::decode::Error>)
    where T: PartialEq + std::fmt::Debug + Clone {
        let c = into(make.clone());
        assert_eq!(from(&c).unwrap(), make);
    }

    #[test]
    fn text_roundtrip() {
        let t = Text {
            content: lit("hello"), style: TextStyle::Body,
            tone: Some(Tone::Primary), align: Some(TextAlign::Start),
            wrap: Some(TextWrap::Wrap), max_lines: Some(3), format: None,
        };
        rt(t, |m| m.into_component("t").unwrap(), Text::try_from_component);
    }

    #[test]
    fn heading_roundtrip() {
        let h = Heading {
            content: lit("Title"), level: 1, tone: None, align: None,
        };
        rt(h, |m| m.into_component("h").unwrap(), Heading::try_from_component);
    }

    #[test]
    fn paragraph_full_roundtrip() {
        let p = Paragraph {
            content: lit("Hello"), style: TextStyle::H3,
            allowed_marks: vec![MarkdownMark::Bold, MarkdownMark::Code],
            allow_links: true, max_lines: Some(4),
        };
        rt(p, |m| m.into_component("p").unwrap(), Paragraph::try_from_component);
    }

    #[test]
    fn paragraph_style_default_on_absent() {
        let p = Paragraph {
            content: lit("Hello"), style: TextStyle::Body,
            allowed_marks: vec![],
            allow_links: true, max_lines: None,
        };
        let mut c = p.into_component("p").unwrap();
        c.fields.0.retain(|(k, _)| *k != 1);
        let back = Paragraph::try_from_component(&c).unwrap();
        assert_eq!(back.style, TextStyle::Body);
    }

    #[test]
    fn rich_text_roundtrip() {
        let r = RichText {
            content: lit("# Heading\n\n- item"),
            allowed_blocks: vec![MarkdownBlock::Heading, MarkdownBlock::List],
            allowed_marks: vec![MarkdownMark::Bold],
            max_height_px: Some(400),
        };
        rt(r, |m| m.into_component("r").unwrap(), RichText::try_from_component);
    }

    #[test]
    fn mono_block_roundtrip() {
        let m = MonoBlock {
            content: lit("plain text"), max_height_px: None,
            word_wrap: false, copyable: true,
        };
        rt(m, |x| x.into_component("m").unwrap(), MonoBlock::try_from_component);
    }

    #[test]
    fn code_block_roundtrip() {
        let cb = CodeBlock {
            content: lit("fn main() {}"), language: "rust".into(),
            show_line_numbers: true, copyable: true,
            max_height_px: Some(300), highlight_lines: vec![1, 2],
        };
        rt(cb, |x| x.into_component("cb").unwrap(), CodeBlock::try_from_component);
    }

    #[test]
    fn key_value_roundtrip() {
        let kv = KeyValue {
            items: vec![], density: Density::Default,
            layout: KvLayout::Stacked, label_width: None,
        };
        rt(kv, |m| m.into_component("kv").unwrap(), KeyValue::try_from_component);
    }

    #[test]
    fn stat_card_roundtrip_with_trend() {
        let sc = StatCard {
            label: lit("Active cameras"), icon: None,
            value: BindRef::Literal(Value::U64(42)),
            value_suffix: Some(lit("/50")), format: None,
            trend: Some(Trend {
                direction: TrendDirection::Up,
                percent: 5.0, label: None, tone: None,
            }),
            footnote: None, accent: Some(Tone::Success), clickable: false,
        };
        rt(sc, |m| m.into_component("sc").unwrap(), StatCard::try_from_component);
    }

    #[test]
    fn stat_roundtrip() {
        let s = Stat {
            label: lit("Total"),
            value: BindRef::Literal(Value::U64(100)),
            format: None, trend: None, size: StatSize::Md,
        };
        rt(s, |m| m.into_component("s").unwrap(), Stat::try_from_component);
    }

    #[test]
    fn badge_roundtrip() {
        let b = Badge {
            variant: BadgeVariant::Solid, tone: Tone::Success,
            label: lit("OK"), icon: None, count: None, max: 99, pulse: false,
        };
        rt(b, |m| m.into_component("b").unwrap(), Badge::try_from_component);
    }

    #[test]
    fn chip_roundtrip() {
        let ch = Chip {
            variant: ChipVariant::Removable, tone: Tone::Info,
            label: lit("filter1"), icon: None, avatar: None,
            selected: None, removable: true,
        };
        rt(ch, |m| m.into_component("ch").unwrap(), Chip::try_from_component);
    }

    #[test]
    fn tag_roundtrip() {
        let t = Tag { tone: Tone::Neutral, label: lit("v1.0"), size: TagSize::Sm };
        rt(t, |m| m.into_component("t").unwrap(), Tag::try_from_component);
    }

    #[test]
    fn avatar_roundtrip() {
        let a = Avatar {
            source: AvatarRef::Initials { initials: "PJ".into() },
            size: AvatarSize::Md, shape: AvatarShape::Circle,
            status: Some(AvatarStatus::Online), tone: None,
        };
        rt(a, |m| m.into_component("a").unwrap(), Avatar::try_from_component);
    }

    #[test]
    fn avatar_group_roundtrip() {
        let ag = AvatarGroup {
            avatars: vec![dummy(Avatar::TAG)],
            max_visible: 5, overlap: AvatarOverlap::Default, size: AvatarSize::Sm,
        };
        rt(ag, |m| m.into_component("ag").unwrap(), AvatarGroup::try_from_component);
    }

    #[test]
    fn bullet_list_roundtrip() {
        let bl = BulletList {
            items: vec![lit("a"), lit("b")],
            variant: BulletListVariant::Numbered, tone: None,
            density: Density::Compact,
        };
        rt(bl, |m| m.into_component("bl").unwrap(), BulletList::try_from_component);
    }

    #[test]
    fn timeline_roundtrip() {
        let t = Timeline {
            items: vec![], orientation: TimelineOrientation::Vertical,
            density: Density::Default, show_dates: true, group_by_day: false,
        };
        rt(t, |m| m.into_component("tl").unwrap(), Timeline::try_from_component);
    }

    #[test]
    fn _unused_iconname_smoke() {
        let _ = IconName::Brain;
    }
}
