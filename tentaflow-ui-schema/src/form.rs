// === File: addon/ui/form.rs — form primitives (Input/Textarea/Select/MultiSelect/Checkbox/Radio/RadioGroup/RadioCardGroup/Toggle/Slider/SliderRow/DatePicker/DateRangePicker/TimePicker/FileUpload/Search/Form/FormField/FormGroup) ===

use serde::{Deserialize, Serialize};

use super::data_display::ImageSource;
use super::theme::{Color, IconName};
use super::UiComponent;

// =============================================================================
// FormComponent — sub-enum for interactive form primitives
// =============================================================================

/// Form-input primitives. JSON tags collide with pre-2.1 `Legacy*` variants
/// for `input`/`select`/`form`/`search`; serde `rename = "*_v2"` keeps the
/// untagged `UiComponent` sum unambiguous (same pattern as DataDisplay).
///
/// Embedding rules: variants holding `UiComponent` (`Form.children`,
/// `FormField.child`, `FormGroup.children`) reject overlay-kind containers
/// just like every other category — handled via
/// `super::reject_overlay_kind_in_root` + recursive validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FormComponent {
    #[serde(rename = "input_v2")]
    Input {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default)]
        kind: InputKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        autocomplete: Option<String>,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        readonly: bool,
        #[serde(default)]
        required: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        validations: Vec<Validation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_submit: Option<String>,
    },
    Textarea {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default = "default_textarea_rows")]
        rows: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_rows: Option<u8>,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        readonly: bool,
        #[serde(default)]
        required: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        validations: Vec<Validation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    #[serde(rename = "select_v2")]
    Select {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        options: Vec<SelectOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        required: bool,
        #[serde(default)]
        searchable: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        validations: Vec<Validation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    MultiSelect {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        options: Vec<SelectOption>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        values: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default)]
        disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_selections: Option<u32>,
        #[serde(default)]
        searchable: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        validations: Vec<Validation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    Checkbox {
        id: String,
        label: String,
        #[serde(default)]
        value: bool,
        #[serde(default)]
        disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    Radio {
        id: String,
        label: String,
        #[serde(default)]
        value: bool,
        #[serde(default)]
        disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    RadioGroup {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        options: Vec<RadioOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default)]
        orientation: RadioOrientation,
        #[serde(default)]
        required: bool,
        #[serde(default)]
        disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    RadioCardGroup {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        options: Vec<RadioCardOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default = "default_radio_card_columns")]
        columns: u8,
        #[serde(default)]
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    Toggle {
        id: String,
        label: String,
        #[serde(default)]
        value: bool,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        size: ToggleSize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    Slider {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        min: f64,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        marks: Vec<SliderMark>,
        #[serde(default)]
        disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    SliderRow {
        id: String,
        label: String,
        min: f64,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        value: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    DatePicker {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    DateRangePicker {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        presets: Vec<DateRangePreset>,
        #[serde(default)]
        disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    TimePicker {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
    },
    FileUpload {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        accept: Vec<String>,
        #[serde(default)]
        multiple: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_size_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_files: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<UploadedFile>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_remove: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
    },
    #[serde(rename = "search_v2")]
    Search {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_submit: Option<String>,
        #[serde(default)]
        autofocus: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        debounce_ms: Option<u32>,
    },
    #[serde(rename = "form_v2")]
    Form {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_submit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_cancel: Option<String>,
        #[serde(default)]
        layout: FormLayout,
        children: Vec<UiComponent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submit_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cancel_label: Option<String>,
        #[serde(default)]
        disabled: bool,
    },
    FormField {
        field_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        helper: Option<String>,
        child: Box<UiComponent>,
    },
    FormGroup {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        children: Vec<UiComponent>,
    },
}

fn default_textarea_rows() -> u8 {
    3
}

fn default_radio_card_columns() -> u8 {
    2
}

// =============================================================================
// Supporting enums and structs
// =============================================================================

/// HTML-style input semantic. Drives the renderer's keyboard, autofill and
/// validation hints. `Number` does not imply numeric coercion at this layer —
/// the value stays a `String`; addons coerce inside their action handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    #[default]
    Text,
    Email,
    Password,
    Number,
    Url,
    Tel,
    Search,
}

/// Single dropdown option in `Select`/`MultiSelect`. `group` is an optional
/// optgroup label; renderer batches consecutive options sharing the same
/// `group`. `value` is what the addon receives on `on_change`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Single option inside a `RadioGroup`. `helper` is per-option supplementary
/// text rendered below the label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioOption {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper: Option<String>,
    #[serde(default)]
    pub disabled: bool,
}

/// Wizard-style "card" radio option (M13/M15) — richer than `RadioOption`:
/// title + description + icon/image + optional badge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioCardOption {
    pub value: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default)]
    pub disabled: bool,
}

/// Layout direction for a `RadioGroup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RadioOrientation {
    #[default]
    Vertical,
    Horizontal,
}

/// Visual size of a `Toggle`. `Md` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToggleSize {
    Sm,
    #[default]
    Md,
}

/// Mark/tick on a `Slider`. `value` MUST fall inside `[min, max]` (validator
/// enforces). `label` is shown below the tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliderMark {
    pub value: f64,
    pub label: String,
}

/// Preset shortcut for `DateRangePicker` (e.g. "last 24h", "this week").
/// `from`/`to` are ISO 8601 date strings (`YYYY-MM-DD`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateRangePreset {
    pub id: String,
    pub label: String,
    pub from: String,
    pub to: String,
}

/// Single file row reported by `FileUpload`. `uploaded=false` signals an
/// in-progress upload; `error` carries the addon-rendered failure message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadedFile {
    pub id: String,
    pub name: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub uploaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Visual layout of a `Form`. `Stack` is the default vertical column;
/// `Grid` flows fields into a two-column responsive grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FormLayout {
    #[default]
    Stack,
    Grid,
}

/// Validation rule applied client-side (with async fallback to the addon).
/// `Custom` is async: renderer invokes `addon_ui_validate(action_id, value)`
/// and shows "Sprawdzanie..." until the response arrives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Validation {
    Required,
    MinLength {
        value: u32,
    },
    MaxLength {
        value: u32,
    },
    Pattern {
        regex: String,
    },
    Range {
        min: f64,
        max: f64,
    },
    Custom {
        action_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        debounce_ms: Option<u32>,
    },
}

// =============================================================================
// Validation
// =============================================================================

const RADIO_CARD_COLUMNS_MIN: u8 = 1;
const RADIO_CARD_COLUMNS_MAX: u8 = 4;
const RADIO_GROUP_MIN_OPTIONS: usize = 2;

/// Validate a single form component, recursing into embedded `UiComponent`
/// children (`Form.children`, `FormField.child`, `FormGroup.children`).
/// Leaf errors are static reason codes; container errors propagate the
/// child's chain so the addon log shows the deepest failure instead of a
/// generic `*_children_invalid`.
pub fn validate_and_normalize(component: &mut FormComponent) -> anyhow::Result<()> {
    use FormComponent::*;
    match component {
        Input {
            kind,
            value,
            validations,
            ..
        } => {
            if matches!(kind, InputKind::Password) && value.is_some() {
                anyhow::bail!("password_initial_value_forbidden");
            }
            for v in validations.iter() {
                validate_validation(v)?;
            }
            Ok(())
        }
        Textarea { validations, .. } => {
            for v in validations.iter() {
                validate_validation(v)?;
            }
            Ok(())
        }
        Select {
            options,
            value,
            validations,
            ..
        } => {
            validate_select_options_unique(options)?;
            if let Some(v) = value {
                if !options.iter().any(|o| o.value == *v) {
                    anyhow::bail!("select_value_not_in_options");
                }
            }
            for v in validations.iter() {
                validate_validation(v)?;
            }
            Ok(())
        }
        MultiSelect {
            options,
            values,
            max_selections,
            validations,
            ..
        } => {
            validate_select_options_unique(options)?;
            for v in values.iter() {
                if !options.iter().any(|o| o.value == *v) {
                    anyhow::bail!("multi_select_value_not_in_options");
                }
            }
            if let Some(max) = max_selections {
                if (values.len() as u32) > *max {
                    anyhow::bail!("multi_select_too_many_selected");
                }
            }
            for v in validations.iter() {
                validate_validation(v)?;
            }
            Ok(())
        }
        Checkbox { .. } | Radio { .. } | Toggle { .. } => Ok(()),
        RadioGroup {
            options, value, ..
        } => {
            if options.len() < RADIO_GROUP_MIN_OPTIONS {
                anyhow::bail!("radio_group_too_few_options");
            }
            validate_radio_options_unique(options)?;
            if let Some(v) = value {
                if !options.iter().any(|o| o.value == *v) {
                    anyhow::bail!("radio_group_value_not_in_options");
                }
            }
            Ok(())
        }
        RadioCardGroup {
            options,
            value,
            columns,
            ..
        } => {
            if !(RADIO_CARD_COLUMNS_MIN..=RADIO_CARD_COLUMNS_MAX).contains(columns) {
                anyhow::bail!("radio_card_columns_out_of_range");
            }
            validate_radio_card_options_unique(options)?;
            if let Some(v) = value {
                if !options.iter().any(|o| o.value == *v) {
                    anyhow::bail!("radio_card_value_not_in_options");
                }
            }
            for o in options.iter() {
                if let Some(img) = &o.image {
                    validate_image_source(img)?;
                }
            }
            Ok(())
        }
        Slider {
            min,
            max,
            value,
            marks,
            ..
        } => {
            if !(min < max) {
                anyhow::bail!("slider_min_not_less_than_max");
            }
            if let Some(v) = value {
                if v < min || v > max {
                    anyhow::bail!("slider_value_out_of_range");
                }
            }
            for m in marks.iter() {
                if m.value < *min || m.value > *max {
                    anyhow::bail!("slider_mark_out_of_range");
                }
            }
            Ok(())
        }
        SliderRow {
            min, max, value, ..
        } => {
            if !(min < max) {
                anyhow::bail!("slider_min_not_less_than_max");
            }
            if value < min || value > max {
                anyhow::bail!("slider_value_out_of_range");
            }
            Ok(())
        }
        DatePicker {
            value, min, max, ..
        } => {
            if let Some(v) = value {
                validate_iso_date(v)?;
            }
            if let Some(v) = min {
                validate_iso_date(v)?;
            }
            if let Some(v) = max {
                validate_iso_date(v)?;
            }
            Ok(())
        }
        DateRangePicker {
            from,
            to,
            min,
            max,
            presets,
            ..
        } => {
            if let Some(v) = from {
                validate_iso_date(v)?;
            }
            if let Some(v) = to {
                validate_iso_date(v)?;
            }
            if let Some(v) = min {
                validate_iso_date(v)?;
            }
            if let Some(v) = max {
                validate_iso_date(v)?;
            }
            if let (Some(f), Some(t)) = (from.as_deref(), to.as_deref()) {
                if f > t {
                    anyhow::bail!("date_range_from_after_to");
                }
            }
            for p in presets.iter() {
                validate_iso_date(&p.from)?;
                validate_iso_date(&p.to)?;
                if p.from > p.to {
                    anyhow::bail!("date_range_from_after_to");
                }
            }
            Ok(())
        }
        TimePicker { value, .. } => {
            if let Some(v) = value {
                validate_iso_time(v)?;
            }
            Ok(())
        }
        FileUpload {
            max_files, files, ..
        } => {
            if let Some(max) = max_files {
                if *max == 0 {
                    anyhow::bail!("file_upload_max_files_zero");
                }
                if (files.len() as u32) > *max {
                    anyhow::bail!("file_upload_too_many_files");
                }
            }
            Ok(())
        }
        Search { .. } => Ok(()),
        Form { children, .. } => {
            for c in children.iter_mut() {
                super::reject_overlay_kind_in_root(c)
                    .map_err(|e| anyhow::anyhow!("form_children: {e}"))?;
                super::validate_and_normalize_component(c)
                    .map_err(|e| anyhow::anyhow!("form_children: {e}"))?;
            }
            Ok(())
        }
        FormField {
            field_id, child, ..
        } => {
            if field_id.is_empty() {
                anyhow::bail!("form_field_id_empty");
            }
            super::reject_overlay_kind_in_root(child)
                .map_err(|e| anyhow::anyhow!("form_field_child: {e}"))?;
            super::validate_and_normalize_component(child)
                .map_err(|e| anyhow::anyhow!("form_field_child: {e}"))?;
            Ok(())
        }
        FormGroup { children, .. } => {
            for c in children.iter_mut() {
                super::reject_overlay_kind_in_root(c)
                    .map_err(|e| anyhow::anyhow!("form_group_children: {e}"))?;
                super::validate_and_normalize_component(c)
                    .map_err(|e| anyhow::anyhow!("form_group_children: {e}"))?;
            }
            Ok(())
        }
    }
}

fn validate_validation(v: &Validation) -> anyhow::Result<()> {
    match v {
        Validation::Required => Ok(()),
        Validation::MinLength { .. } | Validation::MaxLength { .. } => Ok(()),
        Validation::Pattern { regex } => {
            regex::Regex::new(regex)
                .map_err(|_| anyhow::anyhow!("validation_pattern_invalid_regex"))?;
            Ok(())
        }
        Validation::Range { min, max } => {
            if !(min < max) {
                anyhow::bail!("validation_range_min_not_less_than_max");
            }
            Ok(())
        }
        Validation::Custom { action_id, .. } => {
            if action_id.is_empty() {
                anyhow::bail!("validation_custom_action_id_empty");
            }
            Ok(())
        }
    }
}

fn validate_select_options_unique(options: &[SelectOption]) -> anyhow::Result<()> {
    let mut seen: Vec<&str> = Vec::with_capacity(options.len());
    for o in options {
        if seen.iter().any(|s| *s == o.value.as_str()) {
            anyhow::bail!("select_duplicate_option_value");
        }
        seen.push(o.value.as_str());
    }
    Ok(())
}

fn validate_radio_options_unique(options: &[RadioOption]) -> anyhow::Result<()> {
    let mut seen: Vec<&str> = Vec::with_capacity(options.len());
    for o in options {
        if seen.iter().any(|s| *s == o.value.as_str()) {
            anyhow::bail!("radio_group_duplicate_option_value");
        }
        seen.push(o.value.as_str());
    }
    Ok(())
}

fn validate_radio_card_options_unique(
    options: &[RadioCardOption],
) -> anyhow::Result<()> {
    let mut seen: Vec<&str> = Vec::with_capacity(options.len());
    for o in options {
        if seen.iter().any(|s| *s == o.value.as_str()) {
            anyhow::bail!("radio_card_duplicate_option_value");
        }
        seen.push(o.value.as_str());
    }
    Ok(())
}

/// ISO 8601 `YYYY-MM-DD` shape check: ASCII digits at positions 0..4, 5..7,
/// 8..10 and dashes at 4 and 7. Does not validate calendar correctness
/// (no leap-year math) — the renderer's date picker normalises that.
fn validate_iso_date(s: &str) -> anyhow::Result<()> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        anyhow::bail!("date_format_invalid");
    }
    for (i, &c) in b.iter().enumerate() {
        if i == 4 || i == 7 {
            continue;
        }
        if !c.is_ascii_digit() {
            anyhow::bail!("date_format_invalid");
        }
    }
    Ok(())
}

/// `HH:MM` 24-hour clock shape check. Does not enforce HH < 24 or MM < 60
/// strictly — renderer's time picker clamps that. We only reject obviously
/// malformed strings (wrong length, wrong separator, non-digits).
fn validate_iso_time(s: &str) -> anyhow::Result<()> {
    let b = s.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        anyhow::bail!("time_format_invalid");
    }
    for (i, &c) in b.iter().enumerate() {
        if i == 2 {
            continue;
        }
        if !c.is_ascii_digit() {
            anyhow::bail!("time_format_invalid");
        }
    }
    Ok(())
}

fn validate_image_source(src: &ImageSource) -> anyhow::Result<()> {
    if let ImageSource::SignedFrame { camera_id, .. } = src {
        validate_camera_id(camera_id)?;
    }
    Ok(())
}

const CAMERA_ID_LEN: usize = 40;
const CAMERA_ID_PREFIX: &str = "cam_";

fn validate_camera_id(id: &str) -> anyhow::Result<()> {
    if id.len() != CAMERA_ID_LEN || !id.starts_with(CAMERA_ID_PREFIX) {
        anyhow::bail!("image_camera_id_invalid_format");
    }
    let uuid = &id[CAMERA_ID_PREFIX.len()..];
    let bytes = uuid.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let dash_pos = matches!(i, 8 | 13 | 18 | 23);
        if dash_pos {
            if b != b'-' {
                anyhow::bail!("image_camera_id_invalid_format");
            }
        } else if !(b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            anyhow::bail!("image_camera_id_invalid_format");
        }
    }
    if bytes[14] != b'4' {
        anyhow::bail!("image_camera_id_invalid_format");
    }
    if !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        anyhow::bail!("image_camera_id_invalid_format");
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::LegacyComponent;

    fn legacy_text(s: &str) -> UiComponent {
        UiComponent::Legacy(LegacyComponent::Text {
            content: s.to_string(),
            style: None,
        })
    }

    fn window_overlay() -> UiComponent {
        UiComponent::Container(crate::container::ContainerComponent::Window {
            title: "x".to_string(),
            size: crate::container::WindowSize::Md,
            dismissable: true,
            on_close: None,
            children: vec![],
            footer: vec![],
        })
    }

    fn round_trip(c: &FormComponent) -> FormComponent {
        let j = serde_json::to_value(c).expect("serialize");
        serde_json::from_value(j).expect("deserialize")
    }

    #[test]
    fn input_round_trip() {
        let c = FormComponent::Input {
            id: "name".into(),
            label: Some("Name".into()),
            placeholder: Some("Type...".into()),
            value: Some("Alice".into()),
            kind: InputKind::Text,
            icon: Some(IconName::User),
            suffix: Some("@x".into()),
            autocomplete: Some("username".into()),
            disabled: false,
            readonly: false,
            required: true,
            validations: vec![Validation::Required, Validation::MaxLength { value: 64 }],
            helper: Some("h".into()),
            on_change: Some("nm".into()),
            on_submit: Some("sb".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn input_type_tag_is_input_v2() {
        let c = FormComponent::Input {
            id: "x".into(),
            label: None,
            placeholder: None,
            value: None,
            kind: InputKind::Text,
            icon: None,
            suffix: None,
            autocomplete: None,
            disabled: false,
            readonly: false,
            required: false,
            validations: vec![],
            helper: None,
            on_change: None,
            on_submit: None,
        };
        let j = serde_json::to_value(&c).expect("ser");
        assert_eq!(j["type"], serde_json::json!("input_v2"));
    }

    #[test]
    fn textarea_round_trip_with_default_rows() {
        let j = serde_json::json!({
            "type": "textarea",
            "id": "msg"
        });
        let c: FormComponent = serde_json::from_value(j).expect("de");
        if let FormComponent::Textarea { rows, .. } = &c {
            assert_eq!(*rows, 3);
        } else {
            panic!("not textarea");
        }
    }

    #[test]
    fn select_round_trip() {
        let c = FormComponent::Select {
            id: "country".into(),
            label: Some("Country".into()),
            options: vec![
                SelectOption {
                    value: "pl".into(),
                    label: "Poland".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
                SelectOption {
                    value: "de".into(),
                    label: "Germany".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
            ],
            value: Some("pl".into()),
            placeholder: None,
            disabled: false,
            required: true,
            searchable: true,
            validations: vec![Validation::Required],
            helper: None,
            on_change: Some("ch".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn multi_select_round_trip() {
        let c = FormComponent::MultiSelect {
            id: "tags".into(),
            label: None,
            options: vec![
                SelectOption {
                    value: "a".into(),
                    label: "A".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
                SelectOption {
                    value: "b".into(),
                    label: "B".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
            ],
            values: vec!["a".into()],
            placeholder: None,
            disabled: false,
            max_selections: Some(5),
            searchable: false,
            validations: vec![],
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn checkbox_round_trip() {
        let c = FormComponent::Checkbox {
            id: "agree".into(),
            label: "Accept".into(),
            value: true,
            disabled: false,
            helper: None,
            on_change: Some("ch".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn radio_round_trip() {
        let c = FormComponent::Radio {
            id: "opt".into(),
            label: "Option".into(),
            value: false,
            disabled: false,
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn radio_group_round_trip() {
        let c = FormComponent::RadioGroup {
            id: "lvl".into(),
            label: Some("Level".into()),
            options: vec![
                RadioOption {
                    value: "low".into(),
                    label: "Low".into(),
                    helper: None,
                    disabled: false,
                },
                RadioOption {
                    value: "high".into(),
                    label: "High".into(),
                    helper: Some("Aggressive".into()),
                    disabled: false,
                },
            ],
            value: Some("low".into()),
            orientation: RadioOrientation::Horizontal,
            required: true,
            disabled: false,
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn radio_card_group_round_trip() {
        let c = FormComponent::RadioCardGroup {
            id: "mode".into(),
            label: Some("Mode".into()),
            options: vec![
                RadioCardOption {
                    value: "basic".into(),
                    title: "Basic".into(),
                    description: Some("Default".into()),
                    icon: Some(IconName::Info),
                    image: None,
                    badge: Some("Recommended".into()),
                    disabled: false,
                },
                RadioCardOption {
                    value: "pro".into(),
                    title: "Pro".into(),
                    description: None,
                    icon: None,
                    image: None,
                    badge: None,
                    disabled: false,
                },
            ],
            value: Some("basic".into()),
            columns: 2,
            required: true,
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn toggle_round_trip() {
        let c = FormComponent::Toggle {
            id: "wifi".into(),
            label: "Wi-Fi".into(),
            value: true,
            disabled: false,
            size: ToggleSize::Sm,
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn slider_round_trip() {
        let c = FormComponent::Slider {
            id: "vol".into(),
            label: Some("Volume".into()),
            min: 0.0,
            max: 100.0,
            step: Some(1.0),
            value: Some(42.0),
            format: Some("%.0f%%".into()),
            marks: vec![SliderMark {
                value: 50.0,
                label: "mid".into(),
            }],
            disabled: false,
            helper: None,
            on_change: Some("ch".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn slider_row_round_trip() {
        let c = FormComponent::SliderRow {
            id: "th".into(),
            label: "Threshold".into(),
            min: 0.0,
            max: 1.0,
            step: Some(0.05),
            value: 0.5,
            value_format: Some("{value} %".into()),
            accent: Some(Color::Accent),
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn date_picker_round_trip() {
        let c = FormComponent::DatePicker {
            id: "d".into(),
            label: None,
            value: Some("2026-05-19".into()),
            min: Some("2020-01-01".into()),
            max: Some("2030-12-31".into()),
            disabled: false,
            required: false,
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn date_range_picker_round_trip() {
        let c = FormComponent::DateRangePicker {
            id: "r".into(),
            label: None,
            from: Some("2026-05-01".into()),
            to: Some("2026-05-19".into()),
            min: None,
            max: None,
            presets: vec![DateRangePreset {
                id: "24h".into(),
                label: "Last 24h".into(),
                from: "2026-05-18".into(),
                to: "2026-05-19".into(),
            }],
            disabled: false,
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn time_picker_round_trip() {
        let c = FormComponent::TimePicker {
            id: "t".into(),
            label: None,
            value: Some("09:30".into()),
            disabled: false,
            required: false,
            helper: None,
            on_change: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn file_upload_round_trip() {
        let c = FormComponent::FileUpload {
            id: "f".into(),
            label: None,
            accept: vec![".png".into(), "image/jpeg".into()],
            multiple: true,
            max_size_bytes: Some(1024 * 1024),
            max_files: Some(5),
            files: vec![UploadedFile {
                id: "f1".into(),
                name: "a.png".into(),
                size_bytes: 1234,
                mime_type: Some("image/png".into()),
                uploaded: true,
                error: None,
            }],
            on_change: None,
            on_remove: None,
            helper: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn search_round_trip() {
        let c = FormComponent::Search {
            id: "s".into(),
            placeholder: Some("Search...".into()),
            value: None,
            on_change: Some("q".into()),
            on_submit: None,
            autofocus: true,
            debounce_ms: Some(300),
        };
        let j = serde_json::to_value(&c).expect("ser");
        assert_eq!(j["type"], serde_json::json!("search_v2"));
        let back: FormComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }

    #[test]
    fn form_round_trip() {
        let c = FormComponent::Form {
            id: "main".into(),
            on_submit: Some("submit".into()),
            on_cancel: Some("cancel".into()),
            layout: FormLayout::Grid,
            children: vec![legacy_text("inner")],
            submit_label: Some("Save".into()),
            cancel_label: Some("Cancel".into()),
            disabled: false,
        };
        let j = serde_json::to_value(&c).expect("ser");
        assert_eq!(j["type"], serde_json::json!("form_v2"));
        let back: FormComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }

    #[test]
    fn form_field_round_trip() {
        let c = FormComponent::FormField {
            field_id: "name".into(),
            label: Some("Name".into()),
            required: true,
            helper: Some("h".into()),
            child: Box::new(legacy_text("input here")),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn form_group_round_trip() {
        let c = FormComponent::FormGroup {
            heading: Some("Advanced".into()),
            description: Some("Power user options".into()),
            children: vec![legacy_text("a")],
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn validation_custom_round_trip() {
        let v = Validation::Custom {
            action_id: "validate_url".into(),
            debounce_ms: Some(400),
        };
        let j = serde_json::to_value(&v).expect("ser");
        assert_eq!(j["kind"], serde_json::json!("custom"));
        let back: Validation = serde_json::from_value(j).expect("de");
        assert_eq!(back, v);
    }

    #[test]
    fn validation_all_kinds_round_trip() {
        let vs = vec![
            Validation::Required,
            Validation::MinLength { value: 3 },
            Validation::MaxLength { value: 99 },
            Validation::Pattern {
                regex: r"^\d+$".into(),
            },
            Validation::Range {
                min: 0.0,
                max: 100.0,
            },
        ];
        for v in vs {
            let j = serde_json::to_value(&v).expect("ser");
            let back: Validation = serde_json::from_value(j).expect("de");
            assert_eq!(back, v);
        }
    }

    // ---- validation rejection cases ----

    #[test]
    fn password_initial_value_is_rejected() {
        let mut c = FormComponent::Input {
            id: "pw".into(),
            label: None,
            placeholder: None,
            value: Some("secret".into()),
            kind: InputKind::Password,
            icon: None,
            suffix: None,
            autocomplete: None,
            disabled: false,
            readonly: false,
            required: true,
            validations: vec![],
            helper: None,
            on_change: None,
            on_submit: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("password_initial_value_forbidden"));
    }

    #[test]
    fn select_duplicate_option_value_is_rejected() {
        let mut c = FormComponent::Select {
            id: "s".into(),
            label: None,
            options: vec![
                SelectOption {
                    value: "a".into(),
                    label: "A".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
                SelectOption {
                    value: "a".into(),
                    label: "A2".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
            ],
            value: None,
            placeholder: None,
            disabled: false,
            required: false,
            searchable: false,
            validations: vec![],
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("select_duplicate_option_value"));
    }

    #[test]
    fn select_value_not_in_options_is_rejected() {
        let mut c = FormComponent::Select {
            id: "s".into(),
            label: None,
            options: vec![SelectOption {
                value: "a".into(),
                label: "A".into(),
                icon: None,
                disabled: false,
                group: None,
            }],
            value: Some("z".into()),
            placeholder: None,
            disabled: false,
            required: false,
            searchable: false,
            validations: vec![],
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("select_value_not_in_options"));
    }

    #[test]
    fn multi_select_value_not_in_options_is_rejected() {
        let mut c = FormComponent::MultiSelect {
            id: "m".into(),
            label: None,
            options: vec![SelectOption {
                value: "a".into(),
                label: "A".into(),
                icon: None,
                disabled: false,
                group: None,
            }],
            values: vec!["b".into()],
            placeholder: None,
            disabled: false,
            max_selections: None,
            searchable: false,
            validations: vec![],
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("multi_select_value_not_in_options"));
    }

    #[test]
    fn multi_select_too_many_selected_is_rejected() {
        let mut c = FormComponent::MultiSelect {
            id: "m".into(),
            label: None,
            options: vec![
                SelectOption {
                    value: "a".into(),
                    label: "A".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
                SelectOption {
                    value: "b".into(),
                    label: "B".into(),
                    icon: None,
                    disabled: false,
                    group: None,
                },
            ],
            values: vec!["a".into(), "b".into()],
            placeholder: None,
            disabled: false,
            max_selections: Some(1),
            searchable: false,
            validations: vec![],
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("multi_select_too_many_selected"));
    }

    #[test]
    fn radio_group_too_few_options_is_rejected() {
        let mut c = FormComponent::RadioGroup {
            id: "r".into(),
            label: None,
            options: vec![RadioOption {
                value: "a".into(),
                label: "A".into(),
                helper: None,
                disabled: false,
            }],
            value: None,
            orientation: RadioOrientation::Vertical,
            required: false,
            disabled: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("radio_group_too_few_options"));
    }

    #[test]
    fn radio_card_columns_out_of_range_is_rejected() {
        let mut c = FormComponent::RadioCardGroup {
            id: "r".into(),
            label: None,
            options: vec![
                RadioCardOption {
                    value: "a".into(),
                    title: "A".into(),
                    description: None,
                    icon: None,
                    image: None,
                    badge: None,
                    disabled: false,
                },
                RadioCardOption {
                    value: "b".into(),
                    title: "B".into(),
                    description: None,
                    icon: None,
                    image: None,
                    badge: None,
                    disabled: false,
                },
            ],
            value: None,
            columns: 5,
            required: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("radio_card_columns_out_of_range"));
    }

    #[test]
    fn radio_card_zero_columns_is_rejected() {
        let mut c = FormComponent::RadioCardGroup {
            id: "r".into(),
            label: None,
            options: vec![RadioCardOption {
                value: "a".into(),
                title: "A".into(),
                description: None,
                icon: None,
                image: None,
                badge: None,
                disabled: false,
            }],
            value: None,
            columns: 0,
            required: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("radio_card_columns_out_of_range"));
    }

    #[test]
    fn slider_min_not_less_than_max_is_rejected() {
        let mut c = FormComponent::Slider {
            id: "s".into(),
            label: None,
            min: 10.0,
            max: 5.0,
            step: None,
            value: None,
            format: None,
            marks: vec![],
            disabled: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("slider_min_not_less_than_max"));
    }

    #[test]
    fn slider_value_out_of_range_is_rejected() {
        let mut c = FormComponent::Slider {
            id: "s".into(),
            label: None,
            min: 0.0,
            max: 10.0,
            step: None,
            value: Some(99.0),
            format: None,
            marks: vec![],
            disabled: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("slider_value_out_of_range"));
    }

    #[test]
    fn slider_row_value_out_of_range_is_rejected() {
        let mut c = FormComponent::SliderRow {
            id: "s".into(),
            label: "x".into(),
            min: 0.0,
            max: 1.0,
            step: None,
            value: 2.0,
            value_format: None,
            accent: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("slider_value_out_of_range"));
    }

    #[test]
    fn date_picker_bad_format_is_rejected() {
        let mut c = FormComponent::DatePicker {
            id: "d".into(),
            label: None,
            value: Some("19-05-2026".into()),
            min: None,
            max: None,
            disabled: false,
            required: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("date_format_invalid"));
    }

    #[test]
    fn date_range_from_after_to_is_rejected() {
        let mut c = FormComponent::DateRangePicker {
            id: "r".into(),
            label: None,
            from: Some("2026-05-20".into()),
            to: Some("2026-05-10".into()),
            min: None,
            max: None,
            presets: vec![],
            disabled: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("date_range_from_after_to"));
    }

    #[test]
    fn time_picker_bad_format_is_rejected() {
        let mut c = FormComponent::TimePicker {
            id: "t".into(),
            label: None,
            value: Some("9:30am".into()),
            disabled: false,
            required: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("time_format_invalid"));
    }

    #[test]
    fn file_upload_too_many_files_is_rejected() {
        let mut c = FormComponent::FileUpload {
            id: "f".into(),
            label: None,
            accept: vec![],
            multiple: true,
            max_size_bytes: None,
            max_files: Some(1),
            files: vec![
                UploadedFile {
                    id: "1".into(),
                    name: "a".into(),
                    size_bytes: 1,
                    mime_type: None,
                    uploaded: true,
                    error: None,
                },
                UploadedFile {
                    id: "2".into(),
                    name: "b".into(),
                    size_bytes: 1,
                    mime_type: None,
                    uploaded: true,
                    error: None,
                },
            ],
            on_change: None,
            on_remove: None,
            helper: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("file_upload_too_many_files"));
    }

    #[test]
    fn file_upload_max_files_zero_is_rejected() {
        let mut c = FormComponent::FileUpload {
            id: "f".into(),
            label: None,
            accept: vec![],
            multiple: false,
            max_size_bytes: None,
            max_files: Some(0),
            files: vec![],
            on_change: None,
            on_remove: None,
            helper: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("file_upload_max_files_zero"));
    }

    #[test]
    fn form_field_empty_id_is_rejected() {
        let mut c = FormComponent::FormField {
            field_id: "".into(),
            label: None,
            required: false,
            helper: None,
            child: Box::new(legacy_text("x")),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("form_field_id_empty"));
    }

    #[test]
    fn form_children_with_window_is_rejected() {
        let mut c = FormComponent::Form {
            id: "f".into(),
            on_submit: None,
            on_cancel: None,
            layout: FormLayout::Stack,
            children: vec![window_overlay()],
            submit_label: None,
            cancel_label: None,
            disabled: false,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("form_children"));
        assert!(err.to_string().contains("overlay_kind_outside_overlays"));
    }

    #[test]
    fn form_field_with_window_child_is_rejected() {
        let mut c = FormComponent::FormField {
            field_id: "x".into(),
            label: None,
            required: false,
            helper: None,
            child: Box::new(window_overlay()),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("form_field_child"));
        assert!(err.to_string().contains("overlay_kind_outside_overlays"));
    }

    #[test]
    fn form_group_with_window_is_rejected() {
        let mut c = FormComponent::FormGroup {
            heading: None,
            description: None,
            children: vec![window_overlay()],
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("form_group_children"));
        assert!(err.to_string().contains("overlay_kind_outside_overlays"));
    }

    #[test]
    fn validation_pattern_invalid_regex_is_rejected() {
        let v = Validation::Pattern {
            regex: "(unclosed".into(),
        };
        let err = validate_validation(&v).expect_err("must reject");
        assert!(err.to_string().contains("validation_pattern_invalid_regex"));
    }

    #[test]
    fn validation_range_min_not_less_than_max_is_rejected() {
        let v = Validation::Range {
            min: 5.0,
            max: 1.0,
        };
        let err = validate_validation(&v).expect_err("must reject");
        assert!(err.to_string().contains("validation_range_min_not_less_than_max"));
    }

    #[test]
    fn validation_custom_empty_action_id_is_rejected() {
        let v = Validation::Custom {
            action_id: "".into(),
            debounce_ms: None,
        };
        let err = validate_validation(&v).expect_err("must reject");
        assert!(err.to_string().contains("validation_custom_action_id_empty"));
    }

    #[test]
    fn input_with_invalid_validation_is_rejected() {
        let mut c = FormComponent::Input {
            id: "x".into(),
            label: None,
            placeholder: None,
            value: None,
            kind: InputKind::Text,
            icon: None,
            suffix: None,
            autocomplete: None,
            disabled: false,
            readonly: false,
            required: false,
            validations: vec![Validation::Pattern {
                regex: "[unclosed".into(),
            }],
            helper: None,
            on_change: None,
            on_submit: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("validation_pattern_invalid_regex"));
    }

    #[test]
    fn radio_card_with_bad_camera_id_image_is_rejected() {
        let mut c = FormComponent::RadioCardGroup {
            id: "r".into(),
            label: None,
            options: vec![
                RadioCardOption {
                    value: "a".into(),
                    title: "A".into(),
                    description: None,
                    icon: None,
                    image: Some(ImageSource::SignedFrame {
                        camera_id: "bad".into(),
                        frame_ref: "x".into(),
                    }),
                    badge: None,
                    disabled: false,
                },
                RadioCardOption {
                    value: "b".into(),
                    title: "B".into(),
                    description: None,
                    icon: None,
                    image: None,
                    badge: None,
                    disabled: false,
                },
            ],
            value: None,
            columns: 2,
            required: false,
            helper: None,
            on_change: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert!(err.to_string().contains("image_camera_id_invalid_format"));
    }

    #[test]
    fn ui_component_form_round_trip_through_sum() {
        let c = UiComponent::Form(FormComponent::Toggle {
            id: "t".into(),
            label: "On".into(),
            value: true,
            disabled: false,
            size: ToggleSize::Md,
            helper: None,
            on_change: None,
        });
        let j = serde_json::to_value(&c).expect("ser");
        let back: UiComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }
}
