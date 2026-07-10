// =============================================================================
// File: protocol/ui/form/file_color.rs — FileInput/ColorPicker (catalog §5)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::tokens::{ColorPickerVariant, ColorToken, FileCapture};
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
// 0x0318 — FileInput
// -----------------------------------------------------------------------------

/// File picker (catalog §5 0x0318).
#[derive(Debug, Clone, PartialEq)]
pub struct FileInput {
    pub bind_path: StatePath,
    pub accept: Vec<String>,
    pub max_size_bytes: u64,
    pub max_files: u8,
    pub multiple: bool,
    pub drag_and_drop: bool,
    pub capture: Option<FileCapture>,
    pub upload_action_id: String,
    pub label: Option<BindRef>,
    pub hint: Option<BindRef>,
}

impl FileInput {
    pub const TAG: u16 = 0x0318;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(10);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.accept)?));
        e.push((2, encode_to_value(&self.max_size_bytes)?));
        e.push((3, encode_to_value(&self.max_files)?));
        e.push((4, encode_to_value(&self.multiple)?));
        e.push((5, encode_to_value(&self.drag_and_drop)?));
        if let Some(v) = &self.capture {
            e.push((6, encode_to_value(v)?));
        }
        e.push((7, encode_to_value(&self.upload_action_id)?));
        if let Some(v) = &self.label {
            e.push((8, encode_to_value(v)?));
        }
        if let Some(v) = &self.hint {
            e.push((9, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "FileInput")?;
        ensure_no_duplicate_keys("FileInput", &c.fields.0)?;
        let mut bind_path = None;
        let mut accept = None;
        let mut max_size_bytes = None;
        let mut max_files = None;
        let mut multiple = None;
        let mut drag_and_drop = None;
        let mut capture = None;
        let mut upload_action_id = None;
        let mut label = None;
        let mut hint = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => accept = Some(decode_from_value(v)?),
                2 => max_size_bytes = Some(decode_from_value(v)?),
                3 => max_files = Some(decode_from_value(v)?),
                4 => multiple = Some(decode_from_value(v)?),
                5 => drag_and_drop = Some(decode_from_value(v)?),
                6 => capture = Some(decode_from_value(v)?),
                7 => upload_action_id = Some(decode_from_value(v)?),
                8 => label = Some(decode_from_value(v)?),
                9 => hint = Some(decode_from_value(v)?),
                other => return Err(unknown_field("FileInput", *other)),
            }
        }
        Ok(FileInput {
            bind_path: bind_path.ok_or_else(|| missing_field("FileInput", "bind_path"))?,
            accept: accept.ok_or_else(|| missing_field("FileInput", "accept"))?,
            max_size_bytes: max_size_bytes
                .ok_or_else(|| missing_field("FileInput", "max_size_bytes"))?,
            max_files: max_files.ok_or_else(|| missing_field("FileInput", "max_files"))?,
            multiple: multiple.ok_or_else(|| missing_field("FileInput", "multiple"))?,
            drag_and_drop: drag_and_drop
                .ok_or_else(|| missing_field("FileInput", "drag_and_drop"))?,
            capture,
            upload_action_id: upload_action_id
                .ok_or_else(|| missing_field("FileInput", "upload_action_id"))?,
            label,
            hint,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0319 — ColorPicker
// -----------------------------------------------------------------------------

/// Color picker (catalog §5 0x0319).
#[derive(Debug, Clone, PartialEq)]
pub struct ColorPicker {
    pub bind_path: StatePath,
    pub variant: ColorPickerVariant,
    pub allowed_tokens: Option<Vec<ColorToken>>,
    pub show_alpha: bool,
    pub label: Option<BindRef>,
}

impl ColorPicker {
    pub const TAG: u16 = 0x0319;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.variant)?));
        if let Some(v) = &self.allowed_tokens {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.show_alpha)?));
        if let Some(v) = &self.label {
            e.push((4, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "ColorPicker")?;
        ensure_no_duplicate_keys("ColorPicker", &c.fields.0)?;
        let mut bind_path = None;
        let mut variant = None;
        let mut allowed_tokens = None;
        let mut show_alpha = None;
        let mut label = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => allowed_tokens = Some(decode_from_value(v)?),
                3 => show_alpha = Some(decode_from_value(v)?),
                4 => label = Some(decode_from_value(v)?),
                other => return Err(unknown_field("ColorPicker", *other)),
            }
        }
        Ok(ColorPicker {
            bind_path: bind_path.ok_or_else(|| missing_field("ColorPicker", "bind_path"))?,
            variant: variant.ok_or_else(|| missing_field("ColorPicker", "variant"))?,
            allowed_tokens,
            show_alpha: show_alpha.ok_or_else(|| missing_field("ColorPicker", "show_alpha"))?,
            label,
        })
    }
}
