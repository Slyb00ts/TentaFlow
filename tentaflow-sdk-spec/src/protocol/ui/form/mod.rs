// =============================================================================
// File: protocol/ui/form/mod.rs — §5 Form components (0x0301-0x031D)
// 29 typed components: text inputs, selectors, atomic toggles/checkboxes/radios,
// radio groups, sliders/ranges, numeric/currency, date/time pickers, file/color,
// FormField/Group/Section/Form wrappers plus FormValidator tagged union.
// =============================================================================

pub mod atomic;
pub mod datetime;
pub mod file_color;
pub mod groups;
pub mod inputs;
pub mod range;
pub mod selectors;
pub mod wrappers;

pub use atomic::{Checkbox, Radio, Toggle};
pub use datetime::{DatePicker, DateRangePicker, DateTimePicker, TimePicker};
pub use file_color::{ColorPicker, FileInput};
pub use groups::{RadioCardGroup, RadioGroup};
pub use inputs::{CurrencyInput, Input, MentionInput, NumericInput, SearchBox, TagInput, Textarea};
pub use range::{RangeSlider, Slider, SliderRow};
pub use selectors::{Autocomplete, Combobox, MultiSelect, Select};
pub use wrappers::{Form, FormField, FormGroup, FormSection, FormValidator};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{BindRef, PathSegment, StatePath};
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::inline::{
        DatePreset, DatePresetResolve, RadioCardOption, RadioOption, RangePreset, RangePresetRange,
        SelectGroup, SelectOption, SelectValue, SliderMark,
    };
    use crate::protocol::ui::tokens::{
        AutocompleteHint, CheckboxSize, ColorPickerVariant, ColorToken, DayOfWeek, Density,
        FileCapture, FormFieldLayout, FormLayout, InputMode, InputSize, InputType,
        RadioCardVariant, RadioGroupOrientation, SearchVariant, SliderRowLayout, Spacing,
        TimePrecision, TogglePosition, ToggleSize, Tone,
    };
    use crate::protocol::ui::value_format::{DateStyle, TimeStyle};
    use crate::protocol::value::Value;

    fn p(s: &str) -> StatePath {
        StatePath {
            segments: vec![PathSegment::Key(s.into())],
        }
    }
    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }

    fn rt<T: PartialEq + std::fmt::Debug + Clone>(
        make: T,
        into: impl Fn(T) -> Component,
        from: impl Fn(&Component) -> Result<T, minicbor::decode::Error>,
    ) {
        let c = into(make.clone());
        assert_eq!(from(&c).unwrap(), make);
    }

    #[test]
    fn input_roundtrip() {
        let v = Input {
            r#type: InputType::Email,
            bind_path: p("email"),
            placeholder: Some(lit("you@x")),
            label: Some(lit("Email")),
            hint: None,
            leading_icon: None,
            trailing_icon: None,
            prefix: None,
            suffix: None,
            validators: vec![],
            max_length: Some(120),
            min_length: None,
            pattern: None,
            autocomplete: Some(AutocompleteHint::Email),
            input_mode: Some(InputMode::Email),
            disabled: None,
            readonly: None,
            error: None,
            size: InputSize::Md,
        };
        rt(
            v,
            |m| m.into_component("i").unwrap(),
            Input::try_from_component,
        );
    }

    #[test]
    fn textarea_roundtrip() {
        let v = Textarea {
            bind_path: p("body"),
            placeholder: None,
            label: None,
            hint: None,
            validators: vec![],
            max_length: None,
            min_length: None,
            disabled: None,
            readonly: None,
            error: None,
            size: InputSize::Lg,
            rows: 5,
            autoresize: true,
            max_rows: Some(20),
            monospace: false,
        };
        rt(
            v,
            |m| m.into_component("t").unwrap(),
            Textarea::try_from_component,
        );
    }

    #[test]
    fn searchbox_roundtrip() {
        let v = SearchBox {
            bind_path: p("q"),
            placeholder: lit("Search…"),
            debounce_ms: 250,
            variant: SearchVariant::Default,
            shortcut_hint: Some("Ctrl+K".into()),
            on_search_action_id: Some("doSearch".into()),
        };
        rt(
            v,
            |m| m.into_component("s").unwrap(),
            SearchBox::try_from_component,
        );
    }

    #[test]
    fn taginput_roundtrip() {
        let v = TagInput {
            values_path: p("tags"),
            placeholder: None,
            validators: vec![],
            max_tags: Some(10),
            separator: vec![",".into(), " ".into()],
            dedupe: true,
        };
        rt(
            v,
            |m| m.into_component("ti").unwrap(),
            TagInput::try_from_component,
        );
    }

    #[test]
    fn mentioninput_roundtrip() {
        let v = MentionInput {
            bind_path: p("msg"),
            mentions_path: p("mentions"),
            trigger_chars: vec!["@".into()],
            mention_action_id: "resolveMention".into(),
            placeholder: None,
        };
        rt(
            v,
            |m| m.into_component("mi").unwrap(),
            MentionInput::try_from_component,
        );
    }

    #[test]
    fn numericinput_roundtrip() {
        let v = NumericInput {
            bind_path: p("n"),
            min: Some(0.0),
            max: Some(100.0),
            step: 0.5,
            precision: 2,
            format: None,
            label: Some(lit("Qty")),
            hint: None,
            size: InputSize::Sm,
            locale_aware: true,
        };
        rt(
            v,
            |m| m.into_component("ni").unwrap(),
            NumericInput::try_from_component,
        );
    }

    #[test]
    fn currencyinput_roundtrip() {
        let v = CurrencyInput {
            bind_path: p("price"),
            currency_code: "EUR".into(),
            min: None,
            max: None,
            step: 0.01,
            precision: 2,
            label: None,
            hint: None,
            size: InputSize::Md,
            show_symbol: true,
            locale_aware: true,
        };
        rt(
            v,
            |m| m.into_component("ci").unwrap(),
            CurrencyInput::try_from_component,
        );
    }

    #[test]
    fn select_roundtrip() {
        let v = Select {
            bind_path: p("sel"),
            options: vec![SelectOption {
                value: SelectValue::Text("a".into()),
                label: lit("A"),
                icon: None,
                disabled: false,
                group_id: None,
                description: None,
            }],
            placeholder: None,
            label: None,
            searchable: true,
            clearable: true,
            virtualize: false,
            disabled: None,
            size: InputSize::Md,
            groups: Some(vec![SelectGroup {
                id: "g".into(),
                label: lit("G"),
            }]),
        };
        rt(
            v,
            |m| m.into_component("sl").unwrap(),
            Select::try_from_component,
        );
    }

    #[test]
    fn multiselect_roundtrip() {
        let v = MultiSelect {
            selected_path: p("ms"),
            options: vec![],
            placeholder: None,
            label: None,
            searchable: true,
            clearable: true,
            virtualize: true,
            disabled: None,
            size: InputSize::Md,
            groups: None,
            max_selections: Some(5),
            show_select_all: true,
        };
        rt(
            v,
            |m| m.into_component("ms").unwrap(),
            MultiSelect::try_from_component,
        );
    }

    #[test]
    fn combobox_roundtrip() {
        let v = Combobox {
            bind_path: p("c"),
            options: vec![],
            placeholder: None,
            label: None,
            clearable: false,
            virtualize: false,
            disabled: None,
            size: InputSize::Md,
            groups: None,
            free_input: true,
            min_search_chars: 2,
            remote_search: false,
            remote_action_id: None,
        };
        rt(
            v,
            |m| m.into_component("cb").unwrap(),
            Combobox::try_from_component,
        );
    }

    #[test]
    fn autocomplete_roundtrip() {
        let v = Autocomplete {
            bind_path: p("ac"),
            remote_action_id: "search".into(),
            result_template_id: None,
            min_search_chars: 1,
            debounce_ms: 200,
            placeholder: None,
            label: None,
        };
        rt(
            v,
            |m| m.into_component("ac").unwrap(),
            Autocomplete::try_from_component,
        );
    }

    #[test]
    fn toggle_roundtrip() {
        let v = Toggle {
            bind_path: p("on"),
            label: None,
            hint: None,
            size: ToggleSize::Md,
            tone: Tone::Primary,
            disabled: None,
            label_position: TogglePosition::Trailing,
        };
        rt(
            v,
            |m| m.into_component("tg").unwrap(),
            Toggle::try_from_component,
        );
    }

    #[test]
    fn checkbox_roundtrip() {
        let v = Checkbox {
            bind_path: p("ck"),
            label: Some(lit("OK")),
            hint: None,
            indeterminate: None,
            disabled: None,
            size: CheckboxSize::Md,
        };
        rt(
            v,
            |m| m.into_component("ck").unwrap(),
            Checkbox::try_from_component,
        );
    }

    #[test]
    fn radio_roundtrip() {
        let v = Radio {
            bind_path: p("r"),
            value: SelectValue::UInt(1),
            label: lit("One"),
            hint: None,
            disabled: None,
        };
        rt(
            v,
            |m| m.into_component("r").unwrap(),
            Radio::try_from_component,
        );
    }

    #[test]
    fn radiogroup_roundtrip() {
        let v = RadioGroup {
            bind_path: p("rg"),
            options: vec![RadioOption {
                value: SelectValue::Text("a".into()),
                label: lit("A"),
                hint: None,
                disabled: false,
            }],
            orientation: RadioGroupOrientation::Vertical,
            label: None,
            density: Density::Comfortable,
        };
        rt(
            v,
            |m| m.into_component("rg").unwrap(),
            RadioGroup::try_from_component,
        );
    }

    #[test]
    fn radiocardgroup_roundtrip() {
        let v = RadioCardGroup {
            bind_path: p("rcg"),
            options: vec![RadioCardOption {
                value: SelectValue::Text("a".into()),
                icon: crate::protocol::ui::inline::IconRef::Named {
                    name: crate::protocol::ui::icon_name::IconName::Check,
                    size: None,
                    tone: None,
                },
                title: lit("A"),
                description: None,
                badge: None,
                disabled: false,
            }],
            columns: 2,
            variant: RadioCardVariant::Default,
        };
        rt(
            v,
            |m| m.into_component("rcg").unwrap(),
            RadioCardGroup::try_from_component,
        );
    }

    #[test]
    fn slider_roundtrip() {
        let v = Slider {
            bind_path: p("s"),
            min: 0.0,
            max: 100.0,
            step: 1.0,
            label: None,
            show_value: true,
            format: None,
            marks: Some(vec![SliderMark {
                value: 50.0,
                label: Some(lit("mid")),
            }]),
            tone: Tone::Primary,
        };
        rt(
            v,
            |m| m.into_component("s").unwrap(),
            Slider::try_from_component,
        );
    }

    #[test]
    fn rangeslider_roundtrip() {
        let v = RangeSlider {
            bind_path_min: p("lo"),
            bind_path_max: p("hi"),
            min: 0.0,
            max: 100.0,
            step: 1.0,
            label: None,
            show_value: true,
            format: None,
            marks: None,
            tone: Tone::Primary,
            min_separation: 5.0,
        };
        rt(
            v,
            |m| m.into_component("rs").unwrap(),
            RangeSlider::try_from_component,
        );
    }

    #[test]
    fn sliderrow_roundtrip() {
        let v = SliderRow {
            bind_path: p("sr"),
            min: 0.0,
            max: 1.0,
            step: 0.05,
            label: lit("Opacity"),
            format: None,
            marks: None,
            tone: Tone::Primary,
            layout: SliderRowLayout::Compact,
        };
        rt(
            v,
            |m| m.into_component("sr").unwrap(),
            SliderRow::try_from_component,
        );
    }

    #[test]
    fn datepicker_roundtrip() {
        let v = DatePicker {
            bind_path: p("d"),
            label: None,
            min_date: None,
            max_date: None,
            locale: None,
            format: DateStyle::Medium,
            first_day_of_week: DayOfWeek::Monday,
            disabled_dates: None,
            presets: Some(vec![DatePreset {
                id: "today".into(),
                label: lit("Today"),
                resolve: DatePresetResolve::Today,
            }]),
            placeholder: None,
        };
        rt(
            v,
            |m| m.into_component("d").unwrap(),
            DatePicker::try_from_component,
        );
    }

    #[test]
    fn daterangepicker_roundtrip() {
        let v = DateRangePicker {
            from_path: p("from"),
            to_path: p("to"),
            label: None,
            min_date: None,
            max_date: None,
            locale: None,
            format: DateStyle::Short,
            first_day_of_week: DayOfWeek::Monday,
            disabled_dates: None,
            presets: Some(vec![RangePreset {
                id: "7d".into(),
                label: lit("Last 7 days"),
                range: RangePresetRange {
                    from_offset_days: -7,
                    to_offset_days: 0,
                },
            }]),
            placeholder_from: None,
            placeholder_to: None,
            max_range_days: Some(365),
        };
        rt(
            v,
            |m| m.into_component("drp").unwrap(),
            DateRangePicker::try_from_component,
        );
    }

    #[test]
    fn timepicker_roundtrip() {
        let v = TimePicker {
            bind_path: p("t"),
            precision: TimePrecision::Minute,
            format: TimeStyle::Short,
            step_minutes: 15,
            label: None,
        };
        rt(
            v,
            |m| m.into_component("tp").unwrap(),
            TimePicker::try_from_component,
        );
    }

    #[test]
    fn datetimepicker_roundtrip() {
        let v = DateTimePicker {
            bind_path: p("dt"),
            label: None,
            min_datetime: None,
            max_datetime: None,
            date_format: DateStyle::Medium,
            time_format: TimeStyle::Short,
            time_precision: TimePrecision::Minute,
            step_minutes: 15,
            locale: None,
            first_day_of_week: DayOfWeek::Monday,
            placeholder: None,
            timezone: Some("Europe/Warsaw".into()),
        };
        rt(
            v,
            |m| m.into_component("dtp").unwrap(),
            DateTimePicker::try_from_component,
        );
    }

    #[test]
    fn fileinput_roundtrip() {
        let v = FileInput {
            bind_path: p("files"),
            accept: vec!["image/*".into()],
            max_size_bytes: 1_000_000,
            max_files: 5,
            multiple: true,
            drag_and_drop: true,
            capture: Some(FileCapture::Environment),
            upload_action_id: "uploadFile".into(),
            label: None,
            hint: None,
        };
        rt(
            v,
            |m| m.into_component("fi").unwrap(),
            FileInput::try_from_component,
        );
    }

    #[test]
    fn colorpicker_roundtrip() {
        let v = ColorPicker {
            bind_path: p("c"),
            variant: ColorPickerVariant::TokensOnly,
            allowed_tokens: Some(vec![ColorToken::AccentPrimary, ColorToken::SurfaceDefault]),
            show_alpha: false,
            label: None,
        };
        rt(
            v,
            |m| m.into_component("cp").unwrap(),
            ColorPicker::try_from_component,
        );
    }

    #[test]
    fn formfield_roundtrip() {
        let inner = Component {
            tag: 0x0301,
            id: "x".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        };
        let v = FormField {
            label: lit("Name"),
            hint: None,
            error: None,
            required: true,
            child: inner,
            layout: FormFieldLayout::Stacked,
        };
        rt(
            v,
            |m| m.into_component("ff").unwrap(),
            FormField::try_from_component,
        );
    }

    #[test]
    fn formgroup_roundtrip() {
        let v = FormGroup {
            title: Some(lit("Section")),
            description: None,
            collapsible: true,
            expanded: None,
            children: vec![],
            spacing: Spacing::Md,
        };
        rt(
            v,
            |m| m.into_component("fg").unwrap(),
            FormGroup::try_from_component,
        );
    }

    #[test]
    fn formsection_roundtrip() {
        let v = FormSection {
            title: lit("Header"),
            description: None,
            children: vec![],
            spacing: Spacing::Lg,
            divider_top: true,
        };
        rt(
            v,
            |m| m.into_component("fs").unwrap(),
            FormSection::try_from_component,
        );
    }

    #[test]
    fn form_roundtrip() {
        let v = Form {
            children: vec![],
            scope_id: "loginForm".into(),
            validators: vec![
                FormValidator::AllRequired {
                    field_ids: vec!["email".into(), "password".into()],
                },
                FormValidator::Match {
                    field_a: "password".into(),
                    field_b: "confirm".into(),
                },
            ],
            prevent_default_submit: false,
            layout: FormLayout::Stacked,
            disabled: None,
        };
        rt(
            v,
            |m| m.into_component("f").unwrap(),
            Form::try_from_component,
        );
    }

    #[test]
    fn form_validator_any_required_and_custom() {
        let v = Form {
            children: vec![],
            scope_id: "x".into(),
            validators: vec![
                FormValidator::AnyRequired {
                    field_ids: vec!["a".into(), "b".into()],
                    error_message: lit("provide a or b"),
                },
                FormValidator::Custom {
                    id: "rule1".into(),
                    params: None,
                },
            ],
            prevent_default_submit: true,
            layout: FormLayout::Compact,
            disabled: None,
        };
        rt(
            v,
            |m| m.into_component("f2").unwrap(),
            Form::try_from_component,
        );
    }

    #[test]
    fn tag_mismatch_rejected() {
        let bogus = Component {
            tag: 0x9999,
            id: "x".into(),
            fields: FieldMap::default(),
            handlers: None,
            bind: None,
            a11y: None,
            visibility: None,
            test_id: None,
        };
        assert!(Input::try_from_component(&bogus).is_err());
        assert!(Form::try_from_component(&bogus).is_err());
    }
}
