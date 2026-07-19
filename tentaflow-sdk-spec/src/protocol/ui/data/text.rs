// =============================================================================
// File: protocol/ui/data/text.rs — Text/Heading/Paragraph/RichText/MonoBlock/CodeBlock (catalog §4)
// =============================================================================

use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::tokens::{
    MarkdownBlock, MarkdownMark,
    TextAlign, TextStyle, TextWrap, Tone,
};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::value_format::ValueFormat;
use super::super::super::value::Value;

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component { tag, id: id.into(), fields: FieldMap(fields), handlers: None, bind: None, a11y: None, visibility: None, test_id: None }
}

// -----------------------------------------------------------------------------
// Text
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
    /// When set, the renderer treats `content` as an in-progress stream driven by
    /// this bind and shows a semantic streaming caret; addon declares this
    /// intent instead of animating a cursor with its own CSS.
    pub streaming: Option<BindRef>,
}

impl Text {
    pub const TAG: u16 = 0x0201;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut entries: Vec<(u8, Value)> = Vec::with_capacity(8);
        entries.push((0, encode_to_value(&self.content)?));
        entries.push((1, encode_to_value(&self.style)?));
        if let Some(t) = &self.tone { entries.push((2, encode_to_value(t)?)); }
        if let Some(a) = &self.align { entries.push((3, encode_to_value(a)?)); }
        if let Some(w) = &self.wrap { entries.push((4, encode_to_value(w)?)); }
        if let Some(m) = &self.max_lines { entries.push((5, encode_to_value(m)?)); }
        if let Some(f) = &self.format { entries.push((6, encode_to_value(f)?)); }
        if let Some(s) = &self.streaming { entries.push((7, encode_to_value(s)?)); }
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
        let mut streaming = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => style = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => align = Some(decode_from_value(v)?),
                4 => wrap = Some(decode_from_value(v)?),
                5 => max_lines = Some(decode_from_value(v)?),
                6 => format = Some(decode_from_value(v)?),
                7 => streaming = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Text", *other)),
            }
        }
        Ok(Text {
            content: content.ok_or_else(|| missing_field("Text", "content"))?,
            style: style.ok_or_else(|| missing_field("Text", "style"))?,
            tone, align, wrap, max_lines, format, streaming,
        })
    }
}

// -----------------------------------------------------------------------------
// Heading
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
// Paragraph
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
// RichText
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
// MonoBlock
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
// CodeBlock
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

