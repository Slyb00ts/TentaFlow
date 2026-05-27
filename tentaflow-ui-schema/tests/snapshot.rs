// === File: tentaflow-ui-schema/tests/snapshot.rs — JSON snapshot infrastructure for typed panels ===
//
// Backs the P1.b-d migration `json!()` -> typed structs by asserting the
// typed serialization of common UI primitives matches a hand-written JSON
// baseline. Field absence (None Options, empty Vecs) is part of the
// contract, not noise — these snapshots fail if `skip_serializing_if`
// stops emitting the same shape the frontend already consumes.

use serde::Serialize;
use serde_json::Value;

use tentaflow_ui_schema::action::{
    ActionComponent, ButtonSize, ButtonVariant, IconPosition,
};
use tentaflow_ui_schema::container::ContainerComponent;
use tentaflow_ui_schema::data_display::DataDisplayComponent;
use tentaflow_ui_schema::feedback::{FeedbackComponent, FeedbackTone};
use tentaflow_ui_schema::form::{FormComponent, InputKind};
use tentaflow_ui_schema::layout::LayoutComponent;
use tentaflow_ui_schema::legacy::LegacyComponent;
use tentaflow_ui_schema::specialized::{DrawCommand, Point, SpecializedComponent};
use tentaflow_ui_schema::theme::{
    Align, Direction, Justify, Radius, Shadow, Size, Spacing,
};
use tentaflow_ui_schema::{PanelTree, UiComponent};

/// Compare a typed value's serialization to a hand-written JSON baseline.
/// Whitespace and key order are ignored — `serde_json::Value` equality is
/// structural.
pub fn assert_json_equivalent<T: Serialize>(
    typed_struct: &T,
    expected_json: &str,
) -> Result<(), String> {
    let actual_value: Value = serde_json::to_value(typed_struct)
        .map_err(|e| format!("typed struct serialize failed: {e}"))?;
    let expected_value: Value = serde_json::from_str(expected_json)
        .map_err(|e| format!("expected JSON parse failed: {e}"))?;

    if actual_value != expected_value {
        let actual_pretty = serde_json::to_string_pretty(&actual_value).unwrap_or_default();
        let expected_pretty = serde_json::to_string_pretty(&expected_value).unwrap_or_default();
        return Err(format!(
            "JSON output differs!\n\n=== Expected ===\n{expected_pretty}\n\n=== Actual ===\n{actual_pretty}"
        ));
    }
    Ok(())
}

#[test]
fn snapshot_stack_layout() {
    let stack = LayoutComponent::Stack {
        direction: Direction::Vertical,
        gap: Spacing::Md,
        align: Align::Stretch,
        justify: Justify::Start,
        wrap: false,
        padding: None,
        children: vec![],
    };
    let expected = r#"{
        "type": "stack",
        "direction": "vertical",
        "gap": "md",
        "align": "stretch",
        "justify": "start",
        "wrap": false,
        "children": []
    }"#;
    assert_json_equivalent(&stack, expected).expect("stack snapshot");
}

#[test]
fn snapshot_card_container() {
    let card = ContainerComponent::Card {
        title: Some("Hello".to_string()),
        subtitle: None,
        icon: None,
        actions: vec![],
        padding: Spacing::Md,
        radius: Radius::Md,
        shadow: Shadow::Sm,
        children: vec![],
    };
    let expected = r#"{
        "type": "card",
        "title": "Hello",
        "padding": "md",
        "radius": "md",
        "shadow": "sm",
        "children": []
    }"#;
    assert_json_equivalent(&card, expected).expect("card snapshot");
}

#[test]
fn snapshot_stat_data_display() {
    let stat = DataDisplayComponent::Stat {
        value: "42".to_string(),
        value_suffix: None,
        label: "Active".to_string(),
        sublabel: None,
        trend: None,
        icon: None,
        accent: None,
    };
    let expected = r#"{
        "type": "stat",
        "value": "42",
        "label": "Active"
    }"#;
    assert_json_equivalent(&stat, expected).expect("stat snapshot");
}

#[test]
fn snapshot_button_action() {
    let button = ActionComponent::Button {
        label: "Save".to_string(),
        variant: ButtonVariant::Primary,
        size: ButtonSize::Md,
        icon: None,
        icon_position: IconPosition::Leading,
        disabled: false,
        loading: false,
        full_width: false,
        on_click: Some("save_form".to_string()),
        params: None,
        tooltip: None,
    };
    let expected = r#"{
        "type": "button_v2",
        "label": "Save",
        "variant": "primary",
        "size": "md",
        "icon_position": "leading",
        "disabled": false,
        "loading": false,
        "full_width": false,
        "on_click": "save_form"
    }"#;
    assert_json_equivalent(&button, expected).expect("button snapshot");
}

#[test]
fn snapshot_alert_feedback() {
    let alert = FeedbackComponent::Alert {
        tone: FeedbackTone::Info,
        title: None,
        message: "Profile saved.".to_string(),
        icon: None,
        actions: vec![],
        dismissible: false,
        on_dismiss: None,
    };
    let expected = r#"{
        "type": "alert",
        "tone": "info",
        "message": "Profile saved.",
        "dismissible": false
    }"#;
    assert_json_equivalent(&alert, expected).expect("alert snapshot");
}

#[test]
fn snapshot_input_form() {
    let input = FormComponent::Input {
        id: "email".to_string(),
        label: Some("Email".to_string()),
        placeholder: None,
        value: None,
        kind: InputKind::Email,
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
    let expected = r#"{
        "type": "input_v2",
        "id": "email",
        "label": "Email",
        "kind": "email",
        "disabled": false,
        "readonly": false,
        "required": true
    }"#;
    assert_json_equivalent(&input, expected).expect("input snapshot");
}

#[test]
fn snapshot_canvas_with_draw_commands() {
    let canvas = SpecializedComponent::Canvas {
        width: Size::Fixed {
            unit: tentaflow_ui_schema::theme::SizeUnit::Px { value: 200 },
        },
        height: Size::Fixed {
            unit: tentaflow_ui_schema::theme::SizeUnit::Px { value: 100 },
        },
        commands: vec![DrawCommand::Line {
            from: Point { x: 0.0, y: 0.0 },
            to: Point { x: 10.0, y: 10.0 },
            color: tentaflow_ui_schema::theme::Color::Text,
            width: 1.0,
        }],
        background: None,
        cursor: tentaflow_ui_schema::theme::CursorStyle::Default,
        on_pointer: None,
        on_pointer_throttle_ms: None,
    };
    let expected = r#"{
        "type": "canvas",
        "width":  { "kind": "fixed", "unit": { "kind": "px", "value": 200 } },
        "height": { "kind": "fixed", "unit": { "kind": "px", "value": 100 } },
        "commands": [
            {
                "kind": "line",
                "from": { "x": 0.0, "y": 0.0 },
                "to":   { "x": 10.0, "y": 10.0 },
                "color": "text",
                "width": 1.0
            }
        ],
        "cursor": "default"
    }"#;
    assert_json_equivalent(&canvas, expected).expect("canvas snapshot");
}

#[test]
fn snapshot_panel_tree_with_navigation() {
    use tentaflow_ui_schema::container::{
        Breadcrumb, BreadcrumbItem, SidebarItem, SidebarSection,
    };
    use tentaflow_ui_schema::theme::IconName;
    use tentaflow_ui_schema::NavigationSpec;

    let tree = PanelTree {
        root: vec![UiComponent::Legacy(LegacyComponent::Text {
            content: "Welcome".to_string(),
            style: None,
        })],
        overlays: vec![],
        navigation: Some(NavigationSpec {
            breadcrumb: Some(Breadcrumb {
                items: vec![BreadcrumbItem {
                    label: "Home".to_string(),
                    panel_id: Some("dashboard".to_string()),
                }],
            }),
            current_panel: "dashboard".to_string(),
            sidebar: Some(vec![SidebarSection {
                heading: None,
                items: vec![SidebarItem {
                    id: "home".to_string(),
                    label: "Home".to_string(),
                    icon: Some(IconName::Home),
                    badge: None,
                    panel_id: Some("dashboard".to_string()),
                }],
            }]),
        }),
    };
    let expected = r#"{
        "root": [
            { "type": "text", "content": "Welcome" }
        ],
        "navigation": {
            "breadcrumb": {
                "items": [
                    { "label": "Home", "panel_id": "dashboard" }
                ]
            },
            "current_panel": "dashboard",
            "sidebar": [
                {
                    "items": [
                        {
                            "id": "home",
                            "label": "Home",
                            "icon": "home",
                            "panel_id": "dashboard"
                        }
                    ]
                }
            ]
        }
    }"#;
    assert_json_equivalent(&tree, expected).expect("panel_tree snapshot");
}
