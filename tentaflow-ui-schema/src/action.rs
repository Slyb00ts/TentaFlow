// === File: addon/ui/action.rs — action primitives (Button/IconButton/ButtonGroup/Link/Menu/ActionBar/FilterChips/WizardFooter) ===

use serde::{Deserialize, Serialize};

use super::theme::IconName;
use super::UiComponent;

// =============================================================================
// ActionComponent — sub-enum for buttons, links, menus and action strips
// =============================================================================

/// Action primitives covering buttons (`Button`/`IconButton`/`ButtonGroup`),
/// navigation (`Link`), overflow menus (`Menu`), grouped action strips
/// (`ActionBar`/`FilterChips`) and the dedicated wizard footer.
///
/// JSON tag `button_v2` shadows pre-2.1 `Legacy::Button`. Other tags do not
/// collide with Legacy and use their natural names.
///
/// Embedding rules: variants holding `UiComponent` (`ButtonGroup.buttons`,
/// `Menu.trigger`, `ActionBar.primary`/`secondary`) reject overlay-kind
/// containers through the central recursive validator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionComponent {
    #[serde(rename = "button_v2")]
    Button {
        label: String,
        #[serde(default)]
        variant: ButtonVariant,
        #[serde(default)]
        size: ButtonSize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default)]
        icon_position: IconPosition,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        loading: bool,
        #[serde(default)]
        full_width: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_click: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
    },
    /// Pure-icon button (no label) for compact toolbars. A11y: at least one
    /// of `tooltip` or `aria_label` must be set.
    IconButton {
        icon: IconName,
        #[serde(default)]
        variant: ButtonVariant,
        #[serde(default)]
        size: ButtonSize,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        loading: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_click: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tooltip: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aria_label: Option<String>,
    },
    /// Segmented or spaced group of buttons. `attached=true` renders as a
    /// single visually-connected segmented control.
    ButtonGroup {
        buttons: Vec<UiComponent>,
        #[serde(default)]
        attached: bool,
        #[serde(default)]
        size: ButtonSize,
    },
    /// Hyperlink / inline navigation target. Exactly one of `url`, `panel_id`
    /// or `on_click` must be set.
    Link {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panel_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default)]
        variant: LinkVariant,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_click: Option<String>,
    },
    /// Dropdown menu anchored to a trigger component (typically `IconButton`
    /// or `Button`).
    Menu {
        trigger: Box<UiComponent>,
        items: Vec<MenuItem>,
        #[serde(default)]
        placement: MenuPlacement,
    },
    /// Strip of actions above a table or section ("Add", "Filter", "Export").
    ActionBar {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        primary: Vec<UiComponent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        secondary: Vec<UiComponent>,
        #[serde(default)]
        align: ActionBarAlign,
    },
    /// Strip of filter chips (M06 search filters).
    FilterChips {
        chips: Vec<FilterChip>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_clear_all: Option<String>,
    },
    /// Wizard footer (Back / Next / Submit + optional step indicator).
    WizardFooter {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_back: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        back_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_next: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_label: Option<String>,
        #[serde(default)]
        next_disabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_cancel: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cancel_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_label: Option<String>,
    },
}

// =============================================================================
// Supporting enums and structs
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ButtonVariant {
    #[default]
    Primary,
    Secondary,
    Ghost,
    Danger,
    Success,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ButtonSize {
    Xs,
    Sm,
    #[default]
    Md,
    Lg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IconPosition {
    #[default]
    Leading,
    Trailing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LinkVariant {
    #[default]
    Default,
    Subtle,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MenuPlacement {
    #[default]
    BottomStart,
    BottomEnd,
    TopStart,
    TopEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActionBarAlign {
    Start,
    #[default]
    SpaceBetween,
    End,
}

/// Single item inside a `Menu`. `divider_before=true` inserts a separator
/// above this row. `destructive=true` renders the row in the danger palette.
/// `shortcut` is display-only — the renderer does not bind the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub destructive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_click: Option<String>,
    #[serde(default)]
    pub divider_before: bool,
}

/// Single filter chip inside `FilterChips`. `removable=true` shows an X that
/// triggers `on_remove`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterChip {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    #[serde(default)]
    pub removable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_remove: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_click: Option<String>,
}

// =============================================================================
// Validation
// =============================================================================

/// Validate a single action component, recursing into embedded
/// `UiComponent` children. Error strings are static and never echo
/// addon-controlled input.
pub fn validate_and_normalize(component: &mut ActionComponent) -> Result<(), &'static str> {
    use ActionComponent::*;
    match component {
        Button { label, .. } => {
            if label.is_empty() {
                return Err("button_label_empty");
            }
            Ok(())
        }
        IconButton {
            tooltip,
            aria_label,
            ..
        } => {
            if tooltip.is_none() && aria_label.is_none() {
                return Err("icon_button_missing_accessibility_label");
            }
            Ok(())
        }
        ButtonGroup { buttons, .. } => {
            validate_children(buttons, "button_group_children_invalid")?;
            Ok(())
        }
        Link {
            url,
            panel_id,
            on_click,
            ..
        } => {
            let set = [url.is_some(), panel_id.is_some(), on_click.is_some()]
                .iter()
                .filter(|b| **b)
                .count();
            if set != 1 {
                return Err("link_must_have_one_target");
            }
            Ok(())
        }
        Menu { trigger, items, .. } => {
            super::reject_overlay_kind_in_root(trigger)
                .map_err(|_| "menu_trigger_invalid")?;
            super::validate_and_normalize_component(trigger)
                .map_err(|_| "menu_trigger_invalid")?;
            let mut seen: Vec<&str> = Vec::with_capacity(items.len());
            for item in items.iter() {
                if seen.iter().any(|s| *s == item.id.as_str()) {
                    return Err("menu_duplicate_item_id");
                }
                seen.push(item.id.as_str());
            }
            Ok(())
        }
        ActionBar {
            primary, secondary, ..
        } => {
            validate_children(primary, "action_bar_children_invalid")?;
            validate_children(secondary, "action_bar_children_invalid")?;
            Ok(())
        }
        FilterChips { chips, .. } => {
            let mut seen: Vec<&str> = Vec::with_capacity(chips.len());
            for chip in chips.iter() {
                if seen.iter().any(|s| *s == chip.id.as_str()) {
                    return Err("filter_chips_duplicate_id");
                }
                seen.push(chip.id.as_str());
            }
            Ok(())
        }
        WizardFooter {
            on_back,
            on_next,
            on_cancel,
            ..
        } => {
            if on_back.is_none() && on_next.is_none() && on_cancel.is_none() {
                return Err("wizard_footer_no_actions");
            }
            Ok(())
        }
    }
}

fn validate_children(
    children: &mut [UiComponent],
    err_tag: &'static str,
) -> Result<(), &'static str> {
    for c in children.iter_mut() {
        super::reject_overlay_kind_in_root(c).map_err(|_| err_tag)?;
        super::validate_and_normalize_component(c).map_err(|_| err_tag)?;
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{ContainerComponent, WindowSize};
    use crate::legacy::LegacyComponent;

    fn legacy_text(s: &str) -> UiComponent {
        UiComponent::Legacy(LegacyComponent::Text {
            content: s.to_string(),
            style: None,
        })
    }

    fn window_overlay() -> UiComponent {
        UiComponent::Container(ContainerComponent::Window {
            title: "x".to_string(),
            size: WindowSize::Md,
            dismissable: true,
            on_close: None,
            children: vec![],
            footer: vec![],
        })
    }

    fn make_button(label: &str) -> UiComponent {
        UiComponent::Action(ActionComponent::Button {
            label: label.to_string(),
            variant: ButtonVariant::Primary,
            size: ButtonSize::Md,
            icon: None,
            icon_position: IconPosition::Leading,
            disabled: false,
            loading: false,
            full_width: false,
            on_click: None,
            params: None,
            tooltip: None,
        })
    }

    fn round_trip(c: &ActionComponent) -> ActionComponent {
        let j = serde_json::to_value(c).expect("serialize");
        serde_json::from_value(j).expect("deserialize")
    }

    #[test]
    fn button_round_trip_and_v2_tag() {
        let c = ActionComponent::Button {
            label: "Save".into(),
            variant: ButtonVariant::Primary,
            size: ButtonSize::Md,
            icon: Some(IconName::Save),
            icon_position: IconPosition::Leading,
            disabled: false,
            loading: false,
            full_width: true,
            on_click: Some("save".into()),
            params: Some(serde_json::json!({"id": 1})),
            tooltip: Some("Save changes".into()),
        };
        let j = serde_json::to_value(&c).expect("ser");
        assert_eq!(j["type"], serde_json::json!("button_v2"));
        let back: ActionComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }

    #[test]
    fn icon_button_round_trip() {
        let c = ActionComponent::IconButton {
            icon: IconName::Delete,
            variant: ButtonVariant::Danger,
            size: ButtonSize::Sm,
            disabled: false,
            loading: false,
            on_click: Some("del".into()),
            params: None,
            tooltip: Some("Delete".into()),
            aria_label: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn button_group_round_trip() {
        let c = ActionComponent::ButtonGroup {
            buttons: vec![make_button("A"), make_button("B")],
            attached: true,
            size: ButtonSize::Sm,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn link_round_trip_with_url() {
        let c = ActionComponent::Link {
            label: "Docs".into(),
            url: Some("https://example.com".into()),
            panel_id: None,
            icon: Some(IconName::Help),
            variant: LinkVariant::Default,
            on_click: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn link_round_trip_with_panel_id() {
        let c = ActionComponent::Link {
            label: "Cameras".into(),
            url: None,
            panel_id: Some("cameras".into()),
            icon: None,
            variant: LinkVariant::Subtle,
            on_click: None,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn menu_round_trip() {
        let c = ActionComponent::Menu {
            trigger: Box::new(UiComponent::Action(ActionComponent::IconButton {
                icon: IconName::More,
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Md,
                disabled: false,
                loading: false,
                on_click: None,
                params: None,
                tooltip: Some("More".into()),
                aria_label: None,
            })),
            items: vec![
                MenuItem {
                    id: "rename".into(),
                    label: "Rename".into(),
                    icon: Some(IconName::Edit),
                    disabled: false,
                    destructive: false,
                    shortcut: Some("Ctrl+R".into()),
                    on_click: Some("rename".into()),
                    divider_before: false,
                },
                MenuItem {
                    id: "delete".into(),
                    label: "Delete".into(),
                    icon: Some(IconName::Delete),
                    disabled: false,
                    destructive: true,
                    shortcut: None,
                    on_click: Some("delete".into()),
                    divider_before: true,
                },
            ],
            placement: MenuPlacement::BottomEnd,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn action_bar_round_trip() {
        let c = ActionComponent::ActionBar {
            primary: vec![make_button("Add")],
            secondary: vec![make_button("Filter")],
            align: ActionBarAlign::SpaceBetween,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn filter_chips_round_trip() {
        let c = ActionComponent::FilterChips {
            chips: vec![
                FilterChip {
                    id: "type".into(),
                    label: "Type: Vehicle".into(),
                    icon: Some(IconName::Vehicle),
                    removable: true,
                    on_remove: Some("rm_type".into()),
                    on_click: None,
                },
                FilterChip {
                    id: "zone".into(),
                    label: "Zone A".into(),
                    icon: None,
                    removable: true,
                    on_remove: Some("rm_zone".into()),
                    on_click: None,
                },
            ],
            on_clear_all: Some("clear".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn wizard_footer_round_trip() {
        let c = ActionComponent::WizardFooter {
            on_back: Some("back".into()),
            back_label: Some("Wstecz".into()),
            on_next: Some("next".into()),
            next_label: Some("Dalej".into()),
            next_disabled: false,
            on_cancel: Some("cancel".into()),
            cancel_label: Some("Anuluj".into()),
            step_label: Some("Krok 3 z 5".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn ui_component_action_round_trip_through_sum() {
        let c = UiComponent::Action(make_button_action("Click"));
        let j = serde_json::to_value(&c).expect("ser");
        let back: UiComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, c);
    }

    fn make_button_action(label: &str) -> ActionComponent {
        ActionComponent::Button {
            label: label.to_string(),
            variant: ButtonVariant::Primary,
            size: ButtonSize::Md,
            icon: None,
            icon_position: IconPosition::Leading,
            disabled: false,
            loading: false,
            full_width: false,
            on_click: None,
            params: None,
            tooltip: None,
        }
    }

    // ---- validation rejection cases ----

    #[test]
    fn button_empty_label_is_rejected() {
        let mut c = make_button_action("");
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "button_label_empty");
    }

    #[test]
    fn icon_button_without_tooltip_or_aria_label_is_rejected() {
        let mut c = ActionComponent::IconButton {
            icon: IconName::Edit,
            variant: ButtonVariant::Primary,
            size: ButtonSize::Md,
            disabled: false,
            loading: false,
            on_click: None,
            params: None,
            tooltip: None,
            aria_label: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "icon_button_missing_accessibility_label");
    }

    #[test]
    fn icon_button_with_aria_label_only_is_ok() {
        let mut c = ActionComponent::IconButton {
            icon: IconName::Edit,
            variant: ButtonVariant::Primary,
            size: ButtonSize::Md,
            disabled: false,
            loading: false,
            on_click: None,
            params: None,
            tooltip: None,
            aria_label: Some("Edit row".into()),
        };
        assert!(validate_and_normalize(&mut c).is_ok());
    }

    #[test]
    fn button_group_with_window_is_rejected() {
        let mut c = ActionComponent::ButtonGroup {
            buttons: vec![window_overlay()],
            attached: false,
            size: ButtonSize::Md,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "button_group_children_invalid");
    }

    #[test]
    fn link_with_no_target_is_rejected() {
        let mut c = ActionComponent::Link {
            label: "x".into(),
            url: None,
            panel_id: None,
            icon: None,
            variant: LinkVariant::Default,
            on_click: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "link_must_have_one_target");
    }

    #[test]
    fn link_with_two_targets_is_rejected() {
        let mut c = ActionComponent::Link {
            label: "x".into(),
            url: Some("https://x".into()),
            panel_id: Some("p".into()),
            icon: None,
            variant: LinkVariant::Default,
            on_click: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "link_must_have_one_target");
    }

    #[test]
    fn link_with_three_targets_is_rejected() {
        let mut c = ActionComponent::Link {
            label: "x".into(),
            url: Some("https://x".into()),
            panel_id: Some("p".into()),
            icon: None,
            variant: LinkVariant::Default,
            on_click: Some("c".into()),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "link_must_have_one_target");
    }

    #[test]
    fn menu_duplicate_item_id_is_rejected() {
        let mut c = ActionComponent::Menu {
            trigger: Box::new(make_button("trigger")),
            items: vec![
                MenuItem {
                    id: "a".into(),
                    label: "A".into(),
                    icon: None,
                    disabled: false,
                    destructive: false,
                    shortcut: None,
                    on_click: None,
                    divider_before: false,
                },
                MenuItem {
                    id: "a".into(),
                    label: "A2".into(),
                    icon: None,
                    disabled: false,
                    destructive: false,
                    shortcut: None,
                    on_click: None,
                    divider_before: false,
                },
            ],
            placement: MenuPlacement::BottomStart,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "menu_duplicate_item_id");
    }

    #[test]
    fn menu_trigger_window_is_rejected() {
        let mut c = ActionComponent::Menu {
            trigger: Box::new(window_overlay()),
            items: vec![],
            placement: MenuPlacement::BottomStart,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "menu_trigger_invalid");
    }

    #[test]
    fn action_bar_with_window_in_primary_is_rejected() {
        let mut c = ActionComponent::ActionBar {
            primary: vec![window_overlay()],
            secondary: vec![],
            align: ActionBarAlign::SpaceBetween,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "action_bar_children_invalid");
    }

    #[test]
    fn action_bar_with_window_in_secondary_is_rejected() {
        let mut c = ActionComponent::ActionBar {
            primary: vec![],
            secondary: vec![window_overlay()],
            align: ActionBarAlign::End,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "action_bar_children_invalid");
    }

    #[test]
    fn filter_chips_duplicate_id_is_rejected() {
        let mut c = ActionComponent::FilterChips {
            chips: vec![
                FilterChip {
                    id: "z".into(),
                    label: "z1".into(),
                    icon: None,
                    removable: false,
                    on_remove: None,
                    on_click: None,
                },
                FilterChip {
                    id: "z".into(),
                    label: "z2".into(),
                    icon: None,
                    removable: false,
                    on_remove: None,
                    on_click: None,
                },
            ],
            on_clear_all: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "filter_chips_duplicate_id");
    }

    #[test]
    fn wizard_footer_with_no_actions_is_rejected() {
        let mut c = ActionComponent::WizardFooter {
            on_back: None,
            back_label: None,
            on_next: None,
            next_label: None,
            next_disabled: false,
            on_cancel: None,
            cancel_label: None,
            step_label: Some("Krok 1 z 3".into()),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "wizard_footer_no_actions");
    }

    #[test]
    fn wizard_footer_with_only_cancel_is_ok() {
        let mut c = ActionComponent::WizardFooter {
            on_back: None,
            back_label: None,
            on_next: None,
            next_label: None,
            next_disabled: false,
            on_cancel: Some("cancel".into()),
            cancel_label: Some("Anuluj".into()),
            step_label: None,
        };
        assert!(validate_and_normalize(&mut c).is_ok());
    }

    #[test]
    fn button_variant_defaults_to_primary() {
        let j = serde_json::json!({ "type": "button_v2", "label": "Hi" });
        let c: ActionComponent = serde_json::from_value(j).expect("de");
        if let ActionComponent::Button { variant, size, .. } = c {
            assert_eq!(variant, ButtonVariant::Primary);
            assert_eq!(size, ButtonSize::Md);
        } else {
            panic!("not button");
        }
    }
}
