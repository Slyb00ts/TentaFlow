// =============================================================================
// File: protocol/ui/form/selectors.rs — Select/MultiSelect/Combobox/Autocomplete (catalog §5)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::{SelectGroup, SelectOption};
use super::super::tokens::InputSize;
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
// 0x0303 — Select
// -----------------------------------------------------------------------------

/// Single-value dropdown (catalog §5 0x0303).
#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub bind_path: StatePath,
    pub options: Vec<SelectOption>,
    pub placeholder: Option<BindRef>,
    pub label: Option<BindRef>,
    pub searchable: bool,
    pub clearable: bool,
    pub virtualize: bool,
    pub disabled: Option<BindRef>,
    pub size: InputSize,
    pub groups: Option<Vec<SelectGroup>>,
}

impl Select {
    pub const TAG: u16 = 0x0303;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(10);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.options)?));
        if let Some(v) = &self.placeholder {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.label {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.searchable)?));
        e.push((5, encode_to_value(&self.clearable)?));
        e.push((6, encode_to_value(&self.virtualize)?));
        if let Some(v) = &self.disabled {
            e.push((7, encode_to_value(v)?));
        }
        e.push((8, encode_to_value(&self.size)?));
        if let Some(v) = &self.groups {
            e.push((9, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Select")?;
        ensure_no_duplicate_keys("Select", &c.fields.0)?;
        let mut bind_path = None;
        let mut options = None;
        let mut placeholder = None;
        let mut label = None;
        let mut searchable = None;
        let mut clearable = None;
        let mut virtualize = None;
        let mut disabled = None;
        let mut size = None;
        let mut groups = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => options = Some(decode_from_value(v)?),
                2 => placeholder = Some(decode_from_value(v)?),
                3 => label = Some(decode_from_value(v)?),
                4 => searchable = Some(decode_from_value(v)?),
                5 => clearable = Some(decode_from_value(v)?),
                6 => virtualize = Some(decode_from_value(v)?),
                7 => disabled = Some(decode_from_value(v)?),
                8 => size = Some(decode_from_value(v)?),
                9 => groups = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Select", *other)),
            }
        }
        Ok(Select {
            bind_path: bind_path.ok_or_else(|| missing_field("Select", "bind_path"))?,
            options: options.ok_or_else(|| missing_field("Select", "options"))?,
            placeholder,
            label,
            searchable: searchable.ok_or_else(|| missing_field("Select", "searchable"))?,
            clearable: clearable.ok_or_else(|| missing_field("Select", "clearable"))?,
            virtualize: virtualize.ok_or_else(|| missing_field("Select", "virtualize"))?,
            disabled,
            size: size.ok_or_else(|| missing_field("Select", "size"))?,
            groups,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0304 — MultiSelect
// -----------------------------------------------------------------------------

/// Multi-value chip-based select (catalog §5 0x0304).
#[derive(Debug, Clone, PartialEq)]
pub struct MultiSelect {
    pub selected_path: StatePath,
    pub options: Vec<SelectOption>,
    pub placeholder: Option<BindRef>,
    pub label: Option<BindRef>,
    pub searchable: bool,
    pub clearable: bool,
    pub virtualize: bool,
    pub disabled: Option<BindRef>,
    pub size: InputSize,
    pub groups: Option<Vec<SelectGroup>>,
    pub max_selections: Option<u32>,
    pub show_select_all: bool,
}

impl MultiSelect {
    pub const TAG: u16 = 0x0304;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(12);
        e.push((0, encode_to_value(&self.selected_path)?));
        e.push((1, encode_to_value(&self.options)?));
        if let Some(v) = &self.placeholder {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.label {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.searchable)?));
        e.push((5, encode_to_value(&self.clearable)?));
        e.push((6, encode_to_value(&self.virtualize)?));
        if let Some(v) = &self.disabled {
            e.push((7, encode_to_value(v)?));
        }
        e.push((8, encode_to_value(&self.size)?));
        if let Some(v) = &self.groups {
            e.push((9, encode_to_value(v)?));
        }
        if let Some(v) = &self.max_selections {
            e.push((10, encode_to_value(v)?));
        }
        e.push((11, encode_to_value(&self.show_select_all)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "MultiSelect")?;
        ensure_no_duplicate_keys("MultiSelect", &c.fields.0)?;
        let mut selected_path = None;
        let mut options = None;
        let mut placeholder = None;
        let mut label = None;
        let mut searchable = None;
        let mut clearable = None;
        let mut virtualize = None;
        let mut disabled = None;
        let mut size = None;
        let mut groups = None;
        let mut max_selections = None;
        let mut show_select_all = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => selected_path = Some(decode_from_value(v)?),
                1 => options = Some(decode_from_value(v)?),
                2 => placeholder = Some(decode_from_value(v)?),
                3 => label = Some(decode_from_value(v)?),
                4 => searchable = Some(decode_from_value(v)?),
                5 => clearable = Some(decode_from_value(v)?),
                6 => virtualize = Some(decode_from_value(v)?),
                7 => disabled = Some(decode_from_value(v)?),
                8 => size = Some(decode_from_value(v)?),
                9 => groups = Some(decode_from_value(v)?),
                10 => max_selections = Some(decode_from_value(v)?),
                11 => show_select_all = Some(decode_from_value(v)?),
                other => return Err(unknown_field("MultiSelect", *other)),
            }
        }
        Ok(MultiSelect {
            selected_path: selected_path
                .ok_or_else(|| missing_field("MultiSelect", "selected_path"))?,
            options: options.ok_or_else(|| missing_field("MultiSelect", "options"))?,
            placeholder,
            label,
            searchable: searchable.ok_or_else(|| missing_field("MultiSelect", "searchable"))?,
            clearable: clearable.ok_or_else(|| missing_field("MultiSelect", "clearable"))?,
            virtualize: virtualize.ok_or_else(|| missing_field("MultiSelect", "virtualize"))?,
            disabled,
            size: size.ok_or_else(|| missing_field("MultiSelect", "size"))?,
            groups,
            max_selections,
            show_select_all: show_select_all
                .ok_or_else(|| missing_field("MultiSelect", "show_select_all"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0305 — Combobox
// -----------------------------------------------------------------------------

/// Filterable input with autocomplete (catalog §5 0x0305).
#[derive(Debug, Clone, PartialEq)]
pub struct Combobox {
    pub bind_path: StatePath,
    pub options: Vec<SelectOption>,
    pub placeholder: Option<BindRef>,
    pub label: Option<BindRef>,
    pub clearable: bool,
    pub virtualize: bool,
    pub disabled: Option<BindRef>,
    pub size: InputSize,
    pub groups: Option<Vec<SelectGroup>>,
    pub free_input: bool,
    pub min_search_chars: u8,
    pub remote_search: bool,
    pub remote_action_id: Option<String>,
}

impl Combobox {
    pub const TAG: u16 = 0x0305;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(14);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.options)?));
        if let Some(v) = &self.placeholder {
            e.push((2, encode_to_value(v)?));
        }
        if let Some(v) = &self.label {
            e.push((3, encode_to_value(v)?));
        }
        // §5 0x0305: searchable always true — hardcoded.
        e.push((4, encode_to_value(&true)?));
        e.push((5, encode_to_value(&self.clearable)?));
        e.push((6, encode_to_value(&self.virtualize)?));
        if let Some(v) = &self.disabled {
            e.push((7, encode_to_value(v)?));
        }
        e.push((8, encode_to_value(&self.size)?));
        if let Some(v) = &self.groups {
            e.push((9, encode_to_value(v)?));
        }
        e.push((10, encode_to_value(&self.free_input)?));
        e.push((11, encode_to_value(&self.min_search_chars)?));
        e.push((12, encode_to_value(&self.remote_search)?));
        if let Some(v) = &self.remote_action_id {
            e.push((13, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Combobox")?;
        ensure_no_duplicate_keys("Combobox", &c.fields.0)?;
        let mut bind_path = None;
        let mut options = None;
        let mut placeholder = None;
        let mut label = None;
        let mut seen_searchable = false;
        let mut clearable = None;
        let mut virtualize = None;
        let mut disabled = None;
        let mut size = None;
        let mut groups = None;
        let mut free_input = None;
        let mut min_search_chars = None;
        let mut remote_search = None;
        let mut remote_action_id = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => options = Some(decode_from_value(v)?),
                2 => placeholder = Some(decode_from_value(v)?),
                3 => label = Some(decode_from_value(v)?),
                4 => {
                    let s: bool = decode_from_value(v)?;
                    if !s {
                        return Err(minicbor::decode::Error::message(
                            "Combobox.searchable must be true (catalog §5 0x0305)",
                        ));
                    }
                    seen_searchable = true;
                }
                5 => clearable = Some(decode_from_value(v)?),
                6 => virtualize = Some(decode_from_value(v)?),
                7 => disabled = Some(decode_from_value(v)?),
                8 => size = Some(decode_from_value(v)?),
                9 => groups = Some(decode_from_value(v)?),
                10 => free_input = Some(decode_from_value(v)?),
                11 => min_search_chars = Some(decode_from_value(v)?),
                12 => remote_search = Some(decode_from_value(v)?),
                13 => remote_action_id = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Combobox", *other)),
            }
        }
        if !seen_searchable {
            return Err(missing_field("Combobox", "searchable"));
        }
        Ok(Combobox {
            bind_path: bind_path.ok_or_else(|| missing_field("Combobox", "bind_path"))?,
            options: options.ok_or_else(|| missing_field("Combobox", "options"))?,
            placeholder,
            label,
            clearable: clearable.ok_or_else(|| missing_field("Combobox", "clearable"))?,
            virtualize: virtualize.ok_or_else(|| missing_field("Combobox", "virtualize"))?,
            disabled,
            size: size.ok_or_else(|| missing_field("Combobox", "size"))?,
            groups,
            free_input: free_input.ok_or_else(|| missing_field("Combobox", "free_input"))?,
            min_search_chars: min_search_chars
                .ok_or_else(|| missing_field("Combobox", "min_search_chars"))?,
            remote_search: remote_search
                .ok_or_else(|| missing_field("Combobox", "remote_search"))?,
            remote_action_id,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0306 — Autocomplete
// -----------------------------------------------------------------------------

/// Remote-search autocomplete (catalog §5 0x0306).
#[derive(Debug, Clone, PartialEq)]
pub struct Autocomplete {
    pub bind_path: StatePath,
    pub remote_action_id: String,
    pub result_template_id: Option<String>,
    pub min_search_chars: u8,
    pub debounce_ms: u16,
    pub placeholder: Option<BindRef>,
    pub label: Option<BindRef>,
}

impl Autocomplete {
    pub const TAG: u16 = 0x0306;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.bind_path)?));
        e.push((1, encode_to_value(&self.remote_action_id)?));
        if let Some(v) = &self.result_template_id {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.min_search_chars)?));
        e.push((4, encode_to_value(&self.debounce_ms)?));
        if let Some(v) = &self.placeholder {
            e.push((5, encode_to_value(v)?));
        }
        if let Some(v) = &self.label {
            e.push((6, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Autocomplete")?;
        ensure_no_duplicate_keys("Autocomplete", &c.fields.0)?;
        let mut bind_path = None;
        let mut remote_action_id = None;
        let mut result_template_id = None;
        let mut min_search_chars = None;
        let mut debounce_ms = None;
        let mut placeholder = None;
        let mut label = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => bind_path = Some(decode_from_value(v)?),
                1 => remote_action_id = Some(decode_from_value(v)?),
                2 => result_template_id = Some(decode_from_value(v)?),
                3 => min_search_chars = Some(decode_from_value(v)?),
                4 => debounce_ms = Some(decode_from_value(v)?),
                5 => placeholder = Some(decode_from_value(v)?),
                6 => label = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Autocomplete", *other)),
            }
        }
        Ok(Autocomplete {
            bind_path: bind_path.ok_or_else(|| missing_field("Autocomplete", "bind_path"))?,
            remote_action_id: remote_action_id
                .ok_or_else(|| missing_field("Autocomplete", "remote_action_id"))?,
            result_template_id,
            min_search_chars: min_search_chars
                .ok_or_else(|| missing_field("Autocomplete", "min_search_chars"))?,
            debounce_ms: debounce_ms.ok_or_else(|| missing_field("Autocomplete", "debounce_ms"))?,
            placeholder,
            label,
        })
    }
}
