// =============================================================================
// File: protocol/ui/action/mod.rs — §6 Action components (0x0401-0x040C)
// 12 typed: Button/IconButton/ButtonGroup/LinkButton/Link/MenuButton/Menu/
// ActionBar/SegmentedControl/FilterChips/WizardFooter/Fab.
// =============================================================================

pub mod bars;
pub mod buttons;
pub mod menus;

pub use bars::{ActionBar, FilterChips, SegmentedControl, WizardFooter};
pub use buttons::{Button, ButtonGroup, Fab, IconButton, Link, LinkButton};
pub use menus::{Menu, MenuButton};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{BindRef, PathSegment, StatePath};
    use crate::protocol::ui::component::{Component, FieldMap};
    use crate::protocol::ui::icon_name::IconName;
    use crate::protocol::ui::inline::{
        FilterChipDef, IconRef, MenuItem, SegmentOption, SelectValue,
    };
    use crate::protocol::ui::tokens::{
        ButtonGroupOrientation, ButtonSize, ButtonVariant, Density, FabPosition, FabSize,
        FilterChipsMode, LinkUnderline, MenuPlacement, SegmentSize, Tone,
    };
    use crate::protocol::value::Value;

    fn p(s: &str) -> StatePath {
        StatePath {
            segments: vec![PathSegment::Key(s.into())],
        }
    }
    fn lit(s: &str) -> BindRef {
        BindRef::Literal(Value::Text(s.into()))
    }
    fn icon() -> IconRef {
        IconRef::Named {
            name: IconName::Check,
            size: None,
            tone: None,
        }
    }

    fn rt<T: PartialEq + std::fmt::Debug + Clone>(
        make: T,
        into: impl Fn(T) -> Component,
        from: impl Fn(&Component) -> Result<T, minicbor::decode::Error>,
    ) {
        let c = into(make.clone());
        assert_eq!(from(&c).unwrap(), make);
    }

    fn sample_button() -> Component {
        Button {
            variant: ButtonVariant::Primary,
            tone: Tone::Primary,
            label: lit("OK"),
            icon_leading: None,
            icon_trailing: None,
            size: ButtonSize::Md,
            full_width: false,
            disabled: None,
            loading: None,
            density: Density::Comfortable,
        }
        .into_component("b")
        .unwrap()
    }

    #[test]
    fn button_roundtrip() {
        let v = Button {
            variant: ButtonVariant::Primary,
            tone: Tone::Critical,
            label: lit("Save"),
            icon_leading: Some(icon()),
            icon_trailing: None,
            size: ButtonSize::Lg,
            full_width: true,
            disabled: None,
            loading: None,
            density: Density::Compact,
        };
        rt(
            v,
            |m| m.into_component("b").unwrap(),
            Button::try_from_component,
        );
    }

    #[test]
    fn icon_button_roundtrip() {
        let v = IconButton {
            icon: icon(),
            variant: ButtonVariant::Ghost,
            tone: Tone::Neutral,
            size: ButtonSize::Sm,
            aria_label: "Close".into(),
            disabled: None,
            loading: None,
        };
        rt(
            v,
            |m| m.into_component("ib").unwrap(),
            IconButton::try_from_component,
        );
    }

    #[test]
    fn button_group_roundtrip() {
        let v = ButtonGroup {
            buttons: vec![sample_button(), sample_button()],
            orientation: ButtonGroupOrientation::Horizontal,
            attached: true,
        };
        rt(
            v,
            |m| m.into_component("bg").unwrap(),
            ButtonGroup::try_from_component,
        );
    }

    #[test]
    fn link_button_roundtrip() {
        let v = LinkButton {
            label: lit("Open"),
            icon_leading: None,
            icon_trailing: None,
            tone: Tone::Primary,
            underline: LinkUnderline::Hover,
        };
        rt(
            v,
            |m| m.into_component("lb").unwrap(),
            LinkButton::try_from_component,
        );
    }

    #[test]
    fn link_roundtrip() {
        let v = Link {
            label: lit("Read more"),
            underline: LinkUnderline::Always,
            tone: Tone::Primary,
            leading_icon: None,
            trailing_icon: Some(icon()),
        };
        rt(
            v,
            |m| m.into_component("l").unwrap(),
            Link::try_from_component,
        );
    }

    #[test]
    fn menu_button_roundtrip() {
        let v = MenuButton {
            trigger_label: Some(lit("Actions")),
            trigger_icon: None,
            trigger_variant: ButtonVariant::Secondary,
            items: vec![MenuItem {
                id: "a".into(),
                label: lit("A"),
                icon: None,
                badge: None,
                shortcut: None,
                danger: false,
                disabled: None,
                divider_after: false,
            }],
            placement: MenuPlacement::BottomStart,
        };
        rt(
            v,
            |m| m.into_component("mb").unwrap(),
            MenuButton::try_from_component,
        );
    }

    #[test]
    fn menu_roundtrip() {
        let v = Menu {
            items: vec![],
            search: true,
        };
        rt(
            v,
            |m| m.into_component("m").unwrap(),
            Menu::try_from_component,
        );
    }

    #[test]
    fn action_bar_roundtrip() {
        let v = ActionBar {
            leading_actions: vec![sample_button()],
            trailing_actions: vec![sample_button()],
            divider_between: true,
            sticky: false,
        };
        rt(
            v,
            |m| m.into_component("ab").unwrap(),
            ActionBar::try_from_component,
        );
    }

    #[test]
    fn segmented_control_roundtrip() {
        let v = SegmentedControl {
            bind_path: p("seg"),
            options: vec![SegmentOption {
                value: SelectValue::Text("a".into()),
                label: Some(lit("A")),
                icon: None,
                badge: None,
            }],
            size: SegmentSize::Md,
            full_width: false,
        };
        rt(
            v,
            |m| m.into_component("sc").unwrap(),
            SegmentedControl::try_from_component,
        );
    }

    #[test]
    fn filter_chips_roundtrip() {
        let v = FilterChips {
            chips: vec![FilterChipDef {
                id: "a".into(),
                label: lit("A"),
                icon: None,
                badge: None,
                count_path: None,
            }],
            selected_ids: p("filters"),
            mode: FilterChipsMode::Multi,
            clearable: true,
        };
        rt(
            v,
            |m| m.into_component("fc").unwrap(),
            FilterChips::try_from_component,
        );
    }

    #[test]
    fn wizard_footer_roundtrip() {
        let v = WizardFooter {
            back_action: Some(sample_button()),
            next_action: Some(sample_button()),
            cancel_action: None,
            skip_action: None,
            extra_actions: vec![],
        };
        rt(
            v,
            |m| m.into_component("wf").unwrap(),
            WizardFooter::try_from_component,
        );
    }

    #[test]
    fn fab_roundtrip() {
        let v = Fab {
            icon: icon(),
            tone: Tone::Primary,
            size: FabSize::Lg,
            position: FabPosition::BottomRight,
            label: Some(lit("Add")),
        };
        rt(
            v,
            |m| m.into_component("fab").unwrap(),
            Fab::try_from_component,
        );
    }

    fn non_button() -> Component {
        Fab {
            icon: icon(),
            tone: Tone::Primary,
            size: FabSize::Md,
            position: FabPosition::Inline,
            label: None,
        }
        .into_component("not-a-button")
        .unwrap()
    }

    #[test]
    fn button_group_rejects_non_button_ref() {
        let bad = ButtonGroup {
            buttons: vec![non_button()],
            orientation: ButtonGroupOrientation::Horizontal,
            attached: false,
        };
        assert!(bad.into_component("bg").is_err());
    }

    #[test]
    fn action_bar_rejects_non_button_ref() {
        let bad = ActionBar {
            leading_actions: vec![non_button()],
            trailing_actions: vec![],
            divider_between: false,
            sticky: false,
        };
        assert!(bad.into_component("ab").is_err());
    }

    #[test]
    fn wizard_footer_rejects_non_button_ref() {
        let bad = WizardFooter {
            back_action: Some(non_button()),
            next_action: None,
            cancel_action: None,
            skip_action: None,
            extra_actions: vec![],
        };
        assert!(bad.into_component("wf").is_err());
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
        assert!(Button::try_from_component(&bogus).is_err());
        assert!(Fab::try_from_component(&bogus).is_err());
    }
}
