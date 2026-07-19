// =============================================================================
// File: protocol/ui/form/inputs.rs — text/numeric input components (catalog §5)
// Input/Textarea/SearchBox/TagInput/MentionInput/NumericInput/CurrencyInput.
// =============================================================================

use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::IconRef;
use super::super::tokens::{AutocompleteHint, InputMode, InputSize, InputType, InputVariant, SearchVariant};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::validation::ValidationRule;
use super::super::value_format::ValueFormat;
use super::super::super::value::Value;

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component { tag, id: id.into(), fields: FieldMap(fields), handlers: None, bind: None, a11y: None, visibility: None, test_id: None }
}

// -----------------------------------------------------------------------------
// 0x0301 — Input
// -----------------------------------------------------------------------------

/// Single-line text input (catalog §5 0x0301). Handlers: input/change/submit/focus/blur.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub r#type: InputType,
    pub bind_path: StatePath,
    pub placeholder: Option<BindRef>,
    pub label: Option<BindRef>,
    pub hint: Option<BindRef>,
    pub leading_icon: Option<IconRef>,
    pub trailing_icon: Option<IconRef>,
    pub prefix: Option<BindRef>,
    pub suffix: Option<BindRef>,
    pub validators: Vec<ValidationRule>,
    pub max_length: Option<u16>,
    pub min_length: Option<u16>,
    pub pattern: Option<String>,
    pub autocomplete: Option<AutocompleteHint>,
    pub input_mode: Option<InputMode>,
    pub disabled: Option<BindRef>,
    pub readonly: Option<BindRef>,
    pub error: Option<BindRef>,
    pub size: InputSize,
    /// Visual variant; absent = `outlined` (the classic framed field).
    pub variant: Option<InputVariant>,
}

impl Input {
    pub const TAG: u16 = 0x0301;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(20);
        e.push((0, encode_to_value(&self.r#type)?));
        e.push((1, encode_to_value(&self.bind_path)?));
        if let Some(v) = &self.placeholder { e.push((2, encode_to_value(v)?)); }
        if let Some(v) = &self.label { e.push((3, encode_to_value(v)?)); }
        if let Some(v) = &self.hint { e.push((4, encode_to_value(v)?)); }
        if let Some(v) = &self.leading_icon { e.push((5, encode_to_value(v)?)); }
        if let Some(v) = &self.trailing_icon { e.push((6, encode_to_value(v)?)); }
        if let Some(v) = &self.prefix { e.push((7, encode_to_value(v)?)); }
        if let Some(v) = &self.suffix { e.push((8, encode_to_value(v)?)); }
        e.push((9, encode_to_value(&self.validators)?));
        if let Some(v) = &self.max_length { e.push((10, encode_to_value(v)?)); }
        if let Some(v) = &self.min_length { e.push((11, encode_to_value(v)?)); }
        if let Some(v) = &self.pattern { e.push((12, encode_to_value(v)?)); }
        if let Some(v) = &self.autocomplete { e.push((13, encode_to_value(v)?)); }
        if let Some(v) = &self.input_mode { e.push((14, encode_to_value(v)?)); }
        if let Some(v) = &self.disabled { e.push((15, encode_to_value(v)?)); }
        if let Some(v) = &self.readonly { e.push((16, encode_to_value(v)?)); }
        if let Some(v) = &self.error { e.push((17, encode_to_value(v)?)); }
        e.push((18, encode_to_value(&self.size)?));
        if let Some(v) = &self.variant { e.push((19, encode_to_value(v)?)); }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Input")?;
        ensure_no_duplicate_keys("Input", &c.fields.0)?;
        let mut r#type = None; let mut bind_path = None; let mut placeholder = None;
        let mut label = None; let mut hint = None; let mut leading_icon = None;
        let mut trailing_icon = None; let mut prefix = None; let mut suffix = None;
        let mut validators = None; let mut max_length = None; let mut min_length = None;
        let mut pattern = None; let mut autocomplete = None; let mut input_mode = None;
        let mut disabled = None; let mut readonly = None; let mut error = None; let mut size = None;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => r#type = Some(decode_from_value(v)?),
                1 => bind_path = Some(decode_from_value(v)?),
                2 => placeholder = Some(decode_from_value(v)?),
                3 => label = Some(decode_from_value(v)?),
                4 => hint = Some(decode_from_value(v)?),
                5 => leading_icon = Some(decode_from_value(v)?),
                6 => trailing_icon = Some(decode_from_value(v)?),
                7 => prefix = Some(decode_from_value(v)?),
                8 => suffix = Some(decode_from_value(v)?),
                9 => validators = Some(decode_from_value(v)?),
                10 => max_length = Some(decode_from_value(v)?),
                11 => min_length = Some(decode_from_value(v)?),
                12 => pattern = Some(decode_from_value(v)?),
                13 => autocomplete = Some(decode_from_value(v)?),
                14 => input_mode = Some(decode_from_value(v)?),
                15 => disabled = Some(decode_from_value(v)?),
                16 => readonly = Some(decode_from_value(v)?),
                17 => error = Some(decode_from_value(v)?),
                18 => size = Some(decode_from_value(v)?),
                19 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Input", *other)),
            }
        }
        Ok(Input {
            r#type: r#type.ok_or_else(|| missing_field("Input", "type"))?,
            bind_path: bind_path.ok_or_else(|| missing_field("Input", "bind_path"))?,
            placeholder, label, hint, leading_icon, trailing_icon, prefix, suffix,
            validators: validators.ok_or_else(|| missing_field("Input", "validators"))?,
            max_length, min_length, pattern, autocomplete, input_mode,
            disabled, readonly, error,
            size: size.ok_or_else(|| missing_field("Input", "size"))?,
            variant,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0302 — Textarea
// -----------------------------------------------------------------------------

/// Multi-line text input (catalog §5 0x0302).
#[derive(Debug, Clone, PartialEq)]
pub struct Textarea {
    pub bind_path: StatePath,
    pub placeholder: Option<BindRef>,
    pub label: Option<BindRef>,
    pub hint: Option<BindRef>,
    pub validators: Vec<ValidationRule>,
    pub max_length: Option<u16>,
    pub min_length: Option<u16>,
    pub disabled: Option<BindRef>,
    pub readonly: Option<BindRef>,
    pub error: Option<BindRef>,
    pub size: InputSize,
    pub rows: u8,
    pub autoresize: bool,
    pub max_rows: Option<u8>,
    pub monospace: bool,
    /// Visual variant; absent = `outlined` (the classic framed field).
    pub variant: Option<InputVariant>,
}

impl Textarea {
    pub const TAG: u16 = 0x0302;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(16);
        e.push((0, encode_to_value(&self.bind_path)?));
        if let Some(v) = &self.placeholder { e.push((1, encode_to_value(v)?)); }
        if let Some(v) = &self.label { e.push((2, encode_to_value(v)?)); }
        if let Some(v) = &self.hint { e.push((3, encode_to_value(v)?)); }
        e.push((4, encode_to_value(&self.validators)?));
        if let Some(v) = &self.max_length { e.push((5, encode_to_value(v)?)); }
        if let Some(v) = &self.min_length { e.push((6, encode_to_value(v)?)); }
        if let Some(v) = &self.disabled { e.push((7, encode_to_value(v)?)); }
        if let Some(v) = &self.readonly { e.push((8, encode_to_value(v)?)); }
        if let Some(v) = &self.error { e.push((9, encode_to_value(v)?)); }
        e.push((10, encode_to_value(&self.size)?));
        e.push((11, encode_to_value(&self.rows)?));
        e.push((12, encode_to_value(&self.autoresize)?));
        if let Some(v) = &self.max_rows { e.push((13, encode_to_value(v)?)); }
        e.push((14, encode_to_value(&self.monospace)?));
        if let Some(v) = &self.variant { e.push((15, encode_to_value(v)?)); }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Textarea")?;
        ensure_no_duplicate_keys("Textarea", &c.fields.0)?;
        let mut bind_path = None; let mut placeholder = None; let mut label = None;
        let mut hint = None; let mut validators = None; let mut max_length = None;
        let mut min_length = None; let mut disabled = None; let mut readonly = None;
        let mut error = None; let mut size = None; let mut rows = None;
        let mut autoresize = None; let mut max_rows = None; let mut monospace = None;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => placeholder = Some(decode_from_value(v)?),
                2 => label = Some(decode_from_value(v)?),
                3 => hint = Some(decode_from_value(v)?),
                4 => validators = Some(decode_from_value(v)?),
                5 => max_length = Some(decode_from_value(v)?),
                6 => min_length = Some(decode_from_value(v)?),
                7 => disabled = Some(decode_from_value(v)?),
                8 => readonly = Some(decode_from_value(v)?),
                9 => error = Some(decode_from_value(v)?),
                10 => size = Some(decode_from_value(v)?),
                11 => rows = Some(decode_from_value(v)?),
                12 => autoresize = Some(decode_from_value(v)?),
                13 => max_rows = Some(decode_from_value(v)?),
                14 => monospace = Some(decode_from_value(v)?),
                15 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Textarea", *other)),
            }
        }
        Ok(Textarea {
            bind_path: bind_path.ok_or_else(|| missing_field("Textarea", "bind_path"))?,
            placeholder, label, hint,
            validators: validators.ok_or_else(|| missing_field("Textarea", "validators"))?,
            max_length, min_length, disabled, readonly, error,
            size: size.ok_or_else(|| missing_field("Textarea", "size"))?,
            // §5 0x0302 default: rows = 3.
            rows: rows.unwrap_or(3),
            autoresize: autoresize.ok_or_else(|| missing_field("Textarea", "autoresize"))?,
            max_rows,
            monospace: monospace.ok_or_else(|| missing_field("Textarea", "monospace"))?,
            variant,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0307 — SearchBox
// -----------------------------------------------------------------------------

/// Specialised search input for toolbars (catalog §5 0x0307).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchBox {
    pub bind_path: StatePath,
    pub placeholder: BindRef,
    pub debounce_ms: u16,
    pub variant: SearchVariant,
    pub shortcut_hint: Option<String>,
    pub on_search_action_id: Option<String>,
}

impl SearchBox {
    pub const TAG: u16 = 0x0307;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.placeholder)?));
        e.push((2, encode_to_value(&self.debounce_ms)?));
        e.push((3, encode_to_value(&self.variant)?));
        if let Some(v) = &self.shortcut_hint { e.push((4, encode_to_value(v)?)); }
        if let Some(v) = &self.on_search_action_id { e.push((5, encode_to_value(v)?)); }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "SearchBox")?;
        ensure_no_duplicate_keys("SearchBox", &c.fields.0)?;
        let mut bind_path = None; let mut placeholder = None; let mut debounce_ms = None;
        let mut variant = None; let mut shortcut_hint = None; let mut on_search_action_id = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => placeholder = Some(decode_from_value(v)?),
                2 => debounce_ms = Some(decode_from_value(v)?),
                3 => variant = Some(decode_from_value(v)?),
                4 => shortcut_hint = Some(decode_from_value(v)?),
                5 => on_search_action_id = Some(decode_from_value(v)?),
                other => return Err(unknown_field("SearchBox", *other)),
            }
        }
        Ok(SearchBox {
            bind_path: bind_path.ok_or_else(|| missing_field("SearchBox", "bind_path"))?,
            placeholder: placeholder.ok_or_else(|| missing_field("SearchBox", "placeholder"))?,
            // §5 0x0307 default: debounce_ms = 300.
            debounce_ms: debounce_ms.unwrap_or(300),
            variant: variant.ok_or_else(|| missing_field("SearchBox", "variant"))?,
            shortcut_hint, on_search_action_id,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0308 — TagInput
// -----------------------------------------------------------------------------

/// Multiple values displayed as chips inside input (catalog §5 0x0308).
#[derive(Debug, Clone, PartialEq)]
pub struct TagInput {
    pub values_path: StatePath,
    pub placeholder: Option<BindRef>,
    pub validators: Vec<ValidationRule>,
    pub max_tags: Option<u32>,
    pub separator: Vec<String>,
    pub dedupe: bool,
}

impl TagInput {
    pub const TAG: u16 = 0x0308;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.values_path)?));
        if let Some(v) = &self.placeholder { e.push((1, encode_to_value(v)?)); }
        e.push((2, encode_to_value(&self.validators)?));
        if let Some(v) = &self.max_tags { e.push((3, encode_to_value(v)?)); }
        e.push((4, encode_to_value(&self.separator)?));
        e.push((5, encode_to_value(&self.dedupe)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "TagInput")?;
        ensure_no_duplicate_keys("TagInput", &c.fields.0)?;
        let mut values_path = None; let mut placeholder = None; let mut validators = None;
        let mut max_tags = None; let mut separator = None; let mut dedupe = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => values_path = Some(decode_from_value(v)?),
                1 => placeholder = Some(decode_from_value(v)?),
                2 => validators = Some(decode_from_value(v)?),
                3 => max_tags = Some(decode_from_value(v)?),
                4 => separator = Some(decode_from_value(v)?),
                5 => dedupe = Some(decode_from_value(v)?),
                other => return Err(unknown_field("TagInput", *other)),
            }
        }
        Ok(TagInput {
            values_path: values_path.ok_or_else(|| missing_field("TagInput", "values_path"))?,
            placeholder,
            validators: validators.ok_or_else(|| missing_field("TagInput", "validators"))?,
            max_tags,
            separator: separator.ok_or_else(|| missing_field("TagInput", "separator"))?,
            dedupe: dedupe.ok_or_else(|| missing_field("TagInput", "dedupe"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0309 — MentionInput
// -----------------------------------------------------------------------------

/// Textarea with @-mention autocomplete trigger (catalog §5 0x0309).
#[derive(Debug, Clone, PartialEq)]
pub struct MentionInput {
    pub bind_path: StatePath,
    pub mentions_path: StatePath,
    pub trigger_chars: Vec<String>,
    pub mention_action_id: String,
    pub placeholder: Option<BindRef>,
}

impl MentionInput {
    pub const TAG: u16 = 0x0309;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.mentions_path)?));
        e.push((2, encode_to_value(&self.trigger_chars)?));
        e.push((3, encode_to_value(&self.mention_action_id)?));
        if let Some(v) = &self.placeholder { e.push((4, encode_to_value(v)?)); }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "MentionInput")?;
        ensure_no_duplicate_keys("MentionInput", &c.fields.0)?;
        let mut bind_path = None; let mut mentions_path = None; let mut trigger_chars = None;
        let mut mention_action_id = None; let mut placeholder = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => mentions_path = Some(decode_from_value(v)?),
                2 => trigger_chars = Some(decode_from_value(v)?),
                3 => mention_action_id = Some(decode_from_value(v)?),
                4 => placeholder = Some(decode_from_value(v)?),
                other => return Err(unknown_field("MentionInput", *other)),
            }
        }
        Ok(MentionInput {
            bind_path: bind_path.ok_or_else(|| missing_field("MentionInput", "bind_path"))?,
            mentions_path: mentions_path.ok_or_else(|| missing_field("MentionInput", "mentions_path"))?,
            trigger_chars: trigger_chars.ok_or_else(|| missing_field("MentionInput", "trigger_chars"))?,
            mention_action_id: mention_action_id.ok_or_else(|| missing_field("MentionInput", "mention_action_id"))?,
            placeholder,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0312 — NumericInput
// -----------------------------------------------------------------------------

/// Number input with spinners (catalog §5 0x0312).
#[derive(Debug, Clone, PartialEq)]
pub struct NumericInput {
    pub bind_path: StatePath,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: f64,
    pub precision: u8,
    pub format: Option<ValueFormat>,
    pub label: Option<BindRef>,
    pub hint: Option<BindRef>,
    pub size: InputSize,
    pub locale_aware: bool,
}

impl NumericInput {
    pub const TAG: u16 = 0x0312;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(10);
        e.push((0, encode_to_value(&self.bind_path)?));
        if let Some(v) = &self.min { e.push((1, encode_to_value(v)?)); }
        if let Some(v) = &self.max { e.push((2, encode_to_value(v)?)); }
        e.push((3, encode_to_value(&self.step)?));
        e.push((4, encode_to_value(&self.precision)?));
        if let Some(v) = &self.format { e.push((5, encode_to_value(v)?)); }
        if let Some(v) = &self.label { e.push((6, encode_to_value(v)?)); }
        if let Some(v) = &self.hint { e.push((7, encode_to_value(v)?)); }
        e.push((8, encode_to_value(&self.size)?));
        e.push((9, encode_to_value(&self.locale_aware)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "NumericInput")?;
        ensure_no_duplicate_keys("NumericInput", &c.fields.0)?;
        let mut bind_path = None; let mut min = None; let mut max = None;
        let mut step = None; let mut precision = None; let mut format = None;
        let mut label = None; let mut hint = None; let mut size = None; let mut locale_aware = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => min = Some(decode_from_value(v)?),
                2 => max = Some(decode_from_value(v)?),
                3 => step = Some(decode_from_value(v)?),
                4 => precision = Some(decode_from_value(v)?),
                5 => format = Some(decode_from_value(v)?),
                6 => label = Some(decode_from_value(v)?),
                7 => hint = Some(decode_from_value(v)?),
                8 => size = Some(decode_from_value(v)?),
                9 => locale_aware = Some(decode_from_value(v)?),
                other => return Err(unknown_field("NumericInput", *other)),
            }
        }
        Ok(NumericInput {
            bind_path: bind_path.ok_or_else(|| missing_field("NumericInput", "bind_path"))?,
            min, max,
            step: step.ok_or_else(|| missing_field("NumericInput", "step"))?,
            precision: precision.ok_or_else(|| missing_field("NumericInput", "precision"))?,
            format, label, hint,
            size: size.ok_or_else(|| missing_field("NumericInput", "size"))?,
            locale_aware: locale_aware.ok_or_else(|| missing_field("NumericInput", "locale_aware"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0313 — CurrencyInput
// -----------------------------------------------------------------------------

/// NumericInput specialised for currency (catalog §5 0x0313).
#[derive(Debug, Clone, PartialEq)]
pub struct CurrencyInput {
    pub bind_path: StatePath,
    pub currency_code: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: f64,
    pub precision: u8,
    pub label: Option<BindRef>,
    pub hint: Option<BindRef>,
    pub size: InputSize,
    pub show_symbol: bool,
    pub locale_aware: bool,
}

impl CurrencyInput {
    pub const TAG: u16 = 0x0313;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(11);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.currency_code)?));
        if let Some(v) = &self.min { e.push((2, encode_to_value(v)?)); }
        if let Some(v) = &self.max { e.push((3, encode_to_value(v)?)); }
        e.push((4, encode_to_value(&self.step)?));
        e.push((5, encode_to_value(&self.precision)?));
        if let Some(v) = &self.label { e.push((6, encode_to_value(v)?)); }
        if let Some(v) = &self.hint { e.push((7, encode_to_value(v)?)); }
        e.push((8, encode_to_value(&self.size)?));
        e.push((9, encode_to_value(&self.show_symbol)?));
        e.push((10, encode_to_value(&self.locale_aware)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "CurrencyInput")?;
        ensure_no_duplicate_keys("CurrencyInput", &c.fields.0)?;
        let mut bind_path = None; let mut currency_code = None;
        let mut min = None; let mut max = None; let mut step = None; let mut precision = None;
        let mut label = None; let mut hint = None; let mut size = None;
        let mut show_symbol = None; let mut locale_aware = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => currency_code = Some(decode_from_value(v)?),
                2 => min = Some(decode_from_value(v)?),
                3 => max = Some(decode_from_value(v)?),
                4 => step = Some(decode_from_value(v)?),
                5 => precision = Some(decode_from_value(v)?),
                6 => label = Some(decode_from_value(v)?),
                7 => hint = Some(decode_from_value(v)?),
                8 => size = Some(decode_from_value(v)?),
                9 => show_symbol = Some(decode_from_value(v)?),
                10 => locale_aware = Some(decode_from_value(v)?),
                other => return Err(unknown_field("CurrencyInput", *other)),
            }
        }
        Ok(CurrencyInput {
            bind_path: bind_path.ok_or_else(|| missing_field("CurrencyInput", "bind_path"))?,
            currency_code: currency_code.ok_or_else(|| missing_field("CurrencyInput", "currency_code"))?,
            min, max,
            // §5 0x0313 default: step = 0.01.
            step: step.unwrap_or(0.01),
            // §5 0x0313 default: precision = 2.
            precision: precision.unwrap_or(2),
            label, hint,
            size: size.ok_or_else(|| missing_field("CurrencyInput", "size"))?,
            show_symbol: show_symbol.ok_or_else(|| missing_field("CurrencyInput", "show_symbol"))?,
            locale_aware: locale_aware.ok_or_else(|| missing_field("CurrencyInput", "locale_aware"))?,
        })
    }
}
