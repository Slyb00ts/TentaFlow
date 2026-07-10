// =============================================================================
// File: protocol/ui/specialized/text_io.rs — CodeEditor/Terminal (catalog §8)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::tokens::{CodeEditorTheme, TerminalTheme};
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
// 0x0607 — CodeEditor
// -----------------------------------------------------------------------------

/// Code editor (catalog §8 0x0607).
#[derive(Debug, Clone, PartialEq)]
pub struct CodeEditor {
    pub bind_path: StatePath,
    pub language: String,
    pub read_only: bool,
    pub line_numbers: bool,
    pub word_wrap: bool,
    pub theme: CodeEditorTheme,
    pub min_height_px: u16,
    pub max_height_px: Option<u16>,
    pub tab_size: u8,
    pub indent_with_tabs: bool,
    pub bracket_matching: bool,
    pub autocomplete: bool,
    pub linting_action_id: Option<String>,
}

impl CodeEditor {
    pub const TAG: u16 = 0x0607;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(13);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.language)?));
        e.push((2, encode_to_value(&self.read_only)?));
        e.push((3, encode_to_value(&self.line_numbers)?));
        e.push((4, encode_to_value(&self.word_wrap)?));
        e.push((5, encode_to_value(&self.theme)?));
        e.push((6, encode_to_value(&self.min_height_px)?));
        if let Some(v) = &self.max_height_px {
            e.push((7, encode_to_value(v)?));
        }
        e.push((8, encode_to_value(&self.tab_size)?));
        e.push((9, encode_to_value(&self.indent_with_tabs)?));
        e.push((10, encode_to_value(&self.bracket_matching)?));
        e.push((11, encode_to_value(&self.autocomplete)?));
        if let Some(v) = &self.linting_action_id {
            e.push((12, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "CodeEditor")?;
        ensure_no_duplicate_keys("CodeEditor", &c.fields.0)?;
        let mut bind_path = None;
        let mut language = None;
        let mut read_only = None;
        let mut line_numbers = None;
        let mut word_wrap = None;
        let mut theme = None;
        let mut min_height_px = None;
        let mut max_height_px = None;
        let mut tab_size = None;
        let mut indent_with_tabs = None;
        let mut bracket_matching = None;
        let mut autocomplete = None;
        let mut linting_action_id = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => language = Some(decode_from_value(v)?),
                2 => read_only = Some(decode_from_value(v)?),
                3 => line_numbers = Some(decode_from_value(v)?),
                4 => word_wrap = Some(decode_from_value(v)?),
                5 => theme = Some(decode_from_value(v)?),
                6 => min_height_px = Some(decode_from_value(v)?),
                7 => max_height_px = Some(decode_from_value(v)?),
                8 => tab_size = Some(decode_from_value(v)?),
                9 => indent_with_tabs = Some(decode_from_value(v)?),
                10 => bracket_matching = Some(decode_from_value(v)?),
                11 => autocomplete = Some(decode_from_value(v)?),
                12 => linting_action_id = Some(decode_from_value(v)?),
                other => return Err(unknown_field("CodeEditor", *other)),
            }
        }
        Ok(CodeEditor {
            bind_path: bind_path.ok_or_else(|| missing_field("CodeEditor", "bind_path"))?,
            language: language.ok_or_else(|| missing_field("CodeEditor", "language"))?,
            read_only: read_only.ok_or_else(|| missing_field("CodeEditor", "read_only"))?,
            line_numbers: line_numbers
                .ok_or_else(|| missing_field("CodeEditor", "line_numbers"))?,
            word_wrap: word_wrap.ok_or_else(|| missing_field("CodeEditor", "word_wrap"))?,
            theme: theme.ok_or_else(|| missing_field("CodeEditor", "theme"))?,
            min_height_px: min_height_px
                .ok_or_else(|| missing_field("CodeEditor", "min_height_px"))?,
            max_height_px,
            // §8 0x0607 default: tab_size = 2.
            tab_size: tab_size.unwrap_or(2),
            indent_with_tabs: indent_with_tabs
                .ok_or_else(|| missing_field("CodeEditor", "indent_with_tabs"))?,
            bracket_matching: bracket_matching
                .ok_or_else(|| missing_field("CodeEditor", "bracket_matching"))?,
            autocomplete: autocomplete
                .ok_or_else(|| missing_field("CodeEditor", "autocomplete"))?,
            linting_action_id,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0608 — Terminal
// -----------------------------------------------------------------------------

/// Read-only terminal (catalog §8 0x0608).
#[derive(Debug, Clone, PartialEq)]
pub struct Terminal {
    pub stream_id: BindRef,
    pub rows: u16,
    pub cols: u16,
    pub theme: TerminalTheme,
    pub searchable: bool,
    pub copyable: bool,
    pub max_buffer_lines: u32,
}

impl Terminal {
    pub const TAG: u16 = 0x0608;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.stream_id)?));
        e.push((1, encode_to_value(&self.rows)?));
        e.push((2, encode_to_value(&self.cols)?));
        e.push((3, encode_to_value(&self.theme)?));
        e.push((4, encode_to_value(&self.searchable)?));
        e.push((5, encode_to_value(&self.copyable)?));
        e.push((6, encode_to_value(&self.max_buffer_lines)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Terminal")?;
        ensure_no_duplicate_keys("Terminal", &c.fields.0)?;
        let mut stream_id = None;
        let mut rows = None;
        let mut cols = None;
        let mut theme = None;
        let mut searchable = None;
        let mut copyable = None;
        let mut max_buffer_lines = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => stream_id = Some(decode_from_value(v)?),
                1 => rows = Some(decode_from_value(v)?),
                2 => cols = Some(decode_from_value(v)?),
                3 => theme = Some(decode_from_value(v)?),
                4 => searchable = Some(decode_from_value(v)?),
                5 => copyable = Some(decode_from_value(v)?),
                6 => max_buffer_lines = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Terminal", *other)),
            }
        }
        Ok(Terminal {
            stream_id: stream_id.ok_or_else(|| missing_field("Terminal", "stream_id"))?,
            rows: rows.ok_or_else(|| missing_field("Terminal", "rows"))?,
            cols: cols.ok_or_else(|| missing_field("Terminal", "cols"))?,
            theme: theme.ok_or_else(|| missing_field("Terminal", "theme"))?,
            searchable: searchable.ok_or_else(|| missing_field("Terminal", "searchable"))?,
            copyable: copyable.ok_or_else(|| missing_field("Terminal", "copyable"))?,
            // §8 0x0608 default: max_buffer_lines = 10_000.
            max_buffer_lines: max_buffer_lines.unwrap_or(10_000),
        })
    }
}
