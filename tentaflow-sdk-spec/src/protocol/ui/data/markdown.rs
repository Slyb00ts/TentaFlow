// =============================================================================
// File: protocol/ui/data/markdown.rs — Markdown/DataDefinitionList/JsonViewer (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::DefItem;
use super::super::tokens::{DlLayout, LinkTarget, MarkdownFeature};
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
// 0x0220 — Markdown
// -----------------------------------------------------------------------------

/// Trusted markdown source renderer (catalog §4 0x0220). Allows headings,
/// lists, tables, code blocks (vs `Paragraph` which only allows inline marks).
#[derive(Debug, Clone, PartialEq)]
pub struct Markdown {
    pub content: BindRef,
    pub allowed_features: Vec<MarkdownFeature>,
    pub max_height_px: Option<u16>,
    pub link_target: LinkTarget,
}

impl Markdown {
    pub const TAG: u16 = 0x0220;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.content)?));
        e.push((1, encode_to_value(&self.allowed_features)?));
        if let Some(h) = &self.max_height_px {
            e.push((2, encode_to_value(h)?));
        }
        e.push((3, encode_to_value(&self.link_target)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Markdown")?;
        ensure_no_duplicate_keys("Markdown", &c.fields.0)?;
        let mut content = None;
        let mut allowed_features = None;
        let mut max_height_px = None;
        let mut link_target = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => content = Some(decode_from_value(v)?),
                1 => allowed_features = Some(decode_from_value(v)?),
                2 => max_height_px = Some(decode_from_value(v)?),
                3 => link_target = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Markdown", *other)),
            }
        }
        Ok(Markdown {
            content: content.ok_or_else(|| missing_field("Markdown", "content"))?,
            allowed_features: allowed_features.unwrap_or_default(),
            max_height_px,
            link_target: link_target.ok_or_else(|| missing_field("Markdown", "link_target"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0221 — DataDefinitionList
// -----------------------------------------------------------------------------

/// `<dl>` semantic term/definition list (catalog §4 0x0221).
#[derive(Debug, Clone, PartialEq)]
pub struct DataDefinitionList {
    pub items: Vec<DefItem>,
    pub layout: DlLayout,
}

impl DataDefinitionList {
    pub const TAG: u16 = 0x0221;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(2);
        e.push((0, encode_to_value(&self.items)?));
        e.push((1, encode_to_value(&self.layout)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "DataDefinitionList")?;
        ensure_no_duplicate_keys("DataDefinitionList", &c.fields.0)?;
        let mut items = None;
        let mut layout = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items = Some(decode_from_value(v)?),
                1 => layout = Some(decode_from_value(v)?),
                other => return Err(unknown_field("DataDefinitionList", *other)),
            }
        }
        Ok(DataDefinitionList {
            items: items.unwrap_or_default(),
            layout: layout.ok_or_else(|| missing_field("DataDefinitionList", "layout"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0222 — JsonViewer
// -----------------------------------------------------------------------------

/// Read-only JSON tree explorer (catalog §4 0x0222).
#[derive(Debug, Clone, PartialEq)]
pub struct JsonViewer {
    pub value_path: StatePath,
    pub collapsed_depth: u8,
    pub max_height_px: u16,
    pub searchable: bool,
}

impl JsonViewer {
    pub const TAG: u16 = 0x0222;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(4);
        e.push((0, encode_to_value(&self.value_path)?));
        e.push((1, encode_to_value(&self.collapsed_depth)?));
        e.push((2, encode_to_value(&self.max_height_px)?));
        e.push((3, encode_to_value(&self.searchable)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "JsonViewer")?;
        ensure_no_duplicate_keys("JsonViewer", &c.fields.0)?;
        let mut value_path = None;
        let mut collapsed_depth = None;
        let mut max_height_px = None;
        let mut searchable = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => value_path = Some(decode_from_value(v)?),
                1 => collapsed_depth = Some(decode_from_value(v)?),
                2 => max_height_px = Some(decode_from_value(v)?),
                3 => searchable = Some(decode_from_value(v)?),
                other => return Err(unknown_field("JsonViewer", *other)),
            }
        }
        Ok(JsonViewer {
            value_path: value_path.ok_or_else(|| missing_field("JsonViewer", "value_path"))?,
            // §4 0x0222 default: collapsed_depth = 2.
            collapsed_depth: collapsed_depth.unwrap_or(2),
            max_height_px: max_height_px
                .ok_or_else(|| missing_field("JsonViewer", "max_height_px"))?,
            searchable: searchable.ok_or_else(|| missing_field("JsonViewer", "searchable"))?,
        })
    }
}
