// === File: addon/ui/mod.rs — top-level UiComponent sum + PanelTree + JSON entry points ===

pub mod layout;
pub mod legacy;
pub mod theme;

use serde::{Deserialize, Serialize};

// =============================================================================
// UiComponent — sum type wszystkich kategorii
// =============================================================================

/// Top-level komponent SDK UI. Sum type kategorii: `Layout` (Chunk 2.1),
/// `Legacy` (pre-2.1 — usuwane stopniowo w 2.2-2.6). Kolejne kategorie:
/// Container/DataDisplay/Feedback/Form/Action/Specialized — dorzucane
/// w następnych chunkach.
///
/// Wariant na poziomie JSON jest rozpoznawany przez serde `untagged` —
/// zawartość zachowuje swój własny tag `type` (snake_case), więc każdy
/// sub-enum jest payloadem 1:1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UiComponent {
    Layout(layout::LayoutComponent),
    Legacy(legacy::LegacyComponent),
}

// =============================================================================
// PanelTree — nowy korzeń drzewa UI
// =============================================================================

/// Drzewo UI addonu w formacie v2. `root` to lista komponentów najwyższego
/// poziomu, `overlays` to slot top-level dla modali/drawerów/toastów
/// (poza flow rozkładu), `navigation` definiuje strukturę tabsów/routingu
/// addona (Chunk 2.2 doprecyzowuje).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelTree {
    pub root: Vec<UiComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<Overlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<NavigationSpec>,
}

/// Overlay top-level (modal/drawer/toast) — pełna struktura w Chunk 2.2.
/// Na tym etapie tylko identyfikator + zawartość, żeby PanelTree miał
/// stabilny kształt już teraz.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<UiComponent>,
}

/// Specyfikacja nawigacji addonu (tabs + routing przez `panel_navigate`) —
/// pełna struktura w Chunk 2.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
}

// =============================================================================
// UiPanel — kontener z metadanymi (addon_id/panel_id/title) wokół PanelTree
// =============================================================================

/// Pełny panel UI addonu — metadane + drzewo. `to_json` zwraca strukturę
/// wysyłaną do frontu przez `AddonUiPanelGetRequest`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiPanel {
    pub addon_id: String,
    pub panel_id: String,
    pub title: String,
    pub tree: PanelTree,
}

impl UiPanel {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

// =============================================================================
// Parsowanie i walidacja JSON wejściowego
// =============================================================================

/// Format wejściowy `ui_render`: nowy `PanelTree` ALBO stary kształt
/// `{ "components": [...] }` (sprzed Chunka 2.1). Serde `untagged`
/// rozróżnia po polach — `root` vs `components`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PanelFormat {
    New(PanelTree),
    Legacy {
        #[serde(default)]
        components: Vec<UiComponent>,
        #[serde(default)]
        overlays: Vec<Overlay>,
        #[serde(default)]
        navigation: Option<NavigationSpec>,
    },
}

impl PanelFormat {
    fn into_tree(self) -> PanelTree {
        match self {
            PanelFormat::New(t) => t,
            PanelFormat::Legacy {
                components,
                overlays,
                navigation,
            } => PanelTree {
                root: components,
                overlays,
                navigation,
            },
        }
    }
}

/// Parse + validate panel JSON for `ui_render` host fn. Accepts both v1
/// (`{components: [...]}`) and v2 (`{root: [...], overlays: [...]}`)
/// shapes and returns a JSON value normalised to the new shape (so the
/// UI cache stores a single canonical layout).
///
/// Validation contract matches pre-2.1: malformed components are hard
/// errors (no silent drop), `LiveCameraTile.ttl_secs` is clamped,
/// `camera_id`/`stream_id` are strictly checked. Error messages never
/// echo addon input.
pub fn parse_and_validate_ui_json(json: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let parsed: PanelFormat = serde_json::from_value(json.clone())
        .map_err(|e| anyhow::anyhow!("invalid panel json: {}", e))?;
    let mut tree = parsed.into_tree();
    for c in &mut tree.root {
        validate_and_normalize_component(c)?;
    }
    for overlay in &mut tree.overlays {
        for c in &mut overlay.content {
            validate_and_normalize_component(c)?;
        }
    }
    Ok(serde_json::to_value(&tree)?)
}

/// Recursively validate+normalize a single component. Layout container
/// children are visited transparently; legacy components delegate to
/// `legacy::validate_and_normalize` (preserves pre-2.1 invariants).
pub fn validate_and_normalize_component(component: &mut UiComponent) -> anyhow::Result<()> {
    match component {
        UiComponent::Layout(layout) => validate_layout(layout),
        UiComponent::Legacy(legacy) => legacy::validate_and_normalize(legacy),
    }
}

fn validate_layout(layout: &mut layout::LayoutComponent) -> anyhow::Result<()> {
    use layout::LayoutComponent::*;
    match layout {
        Stack { children, .. } => {
            for c in children {
                validate_and_normalize_component(c)?;
            }
            Ok(())
        }
        Grid { children, .. } => {
            for item in children {
                validate_and_normalize_component(&mut item.component)?;
            }
            Ok(())
        }
        Split {
            primary, secondary, ..
        } => {
            validate_and_normalize_component(primary)?;
            validate_and_normalize_component(secondary)?;
            Ok(())
        }
        Spacer { .. } | Divider { .. } => Ok(()),
    }
}

/// Lenient parse used by callers that only want a flat component list
/// (back-compat with pre-2.1 `parse_components_from_json`). Returns
/// the panel `root` slice if input is the new shape, the legacy
/// `components` array if input is v1, or a single-element vec if the
/// input is a single component value.
pub fn parse_components_from_json(json: &serde_json::Value) -> Vec<UiComponent> {
    // A bare `{type: "...", ...}` is a single component — try that first so
    // a top-level legacy node does not silently parse as an empty PanelFormat.
    if json.get("type").is_some() {
        if let Ok(component) = serde_json::from_value::<UiComponent>(json.clone()) {
            return vec![component];
        }
    }
    if let Ok(parsed) = serde_json::from_value::<PanelFormat>(json.clone()) {
        return parsed.into_tree().root;
    }
    if let Ok(component) = serde_json::from_value::<UiComponent>(json.clone()) {
        return vec![component];
    }
    Vec::new()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use legacy::{LegacyComponent, LIVE_CAMERA_TILE_TTL_MAX};

    fn good_cam_id() -> String {
        "cam_550e8400-e29b-41d4-a716-446655440000".to_string()
    }

    #[test]
    fn panel_tree_parses_legacy_components_shape() {
        let json = serde_json::json!({
            "components": [
                { "type": "text", "content": "hi" }
            ]
        });
        let out = parse_and_validate_ui_json(&json).expect("ok");
        assert!(out["root"].is_array());
        assert_eq!(out["root"][0]["type"], "text");
    }

    #[test]
    fn panel_tree_parses_new_root_shape() {
        let json = serde_json::json!({
            "root": [
                { "type": "stack", "direction": "vertical", "gap": "md",
                  "align": "stretch", "justify": "start", "children": [] }
            ]
        });
        let out = parse_and_validate_ui_json(&json).expect("ok");
        assert_eq!(out["root"][0]["type"], "stack");
        assert!(out["overlays"].is_null() || out["overlays"].as_array().unwrap().is_empty());
    }

    #[test]
    fn panel_tree_clamps_ttl_inside_stack_children() {
        let json = serde_json::json!({
            "root": [
                {
                    "type": "stack",
                    "direction": "vertical",
                    "gap": "md",
                    "align": "stretch",
                    "justify": "start",
                    "children": [
                        {
                            "type": "live_camera_tile",
                            "camera_id": good_cam_id(),
                            "ttl_secs": 9999
                        }
                    ]
                }
            ]
        });
        let out = parse_and_validate_ui_json(&json).expect("ok");
        let ttl = &out["root"][0]["children"][0]["ttl_secs"];
        assert_eq!(*ttl, serde_json::json!(LIVE_CAMERA_TILE_TTL_MAX));
    }

    #[test]
    fn panel_tree_rejects_bad_camera_id_without_echoing() {
        let json = serde_json::json!({
            "components": [
                { "type": "live_camera_tile", "camera_id": "../etc/passwd", "ttl_secs": 30 }
            ]
        });
        let err = parse_and_validate_ui_json(&json).expect_err("must reject");
        let msg = format!("{}", err);
        assert!(!msg.contains("../etc/passwd"));
    }

    #[test]
    fn ui_component_round_trip_layout_and_legacy() {
        let stack = UiComponent::Layout(layout::LayoutComponent::Stack {
            direction: theme::Direction::Vertical,
            gap: theme::Spacing::Md,
            align: theme::Align::Stretch,
            justify: theme::Justify::Start,
            wrap: false,
            padding: None,
            children: vec![UiComponent::Legacy(LegacyComponent::Text {
                content: "x".to_string(),
                style: None,
            })],
        });
        let j = serde_json::to_value(&stack).expect("serialize");
        let back: UiComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, stack);
    }

    #[test]
    fn panel_tree_preserves_overlays_round_trip() {
        let tree = PanelTree {
            root: vec![UiComponent::Legacy(LegacyComponent::Text {
                content: "main".to_string(),
                style: None,
            })],
            overlays: vec![Overlay {
                id: "confirm".to_string(),
                content: vec![UiComponent::Legacy(LegacyComponent::Text {
                    content: "Sure?".to_string(),
                    style: None,
                })],
            }],
            navigation: None,
        };
        let j = serde_json::to_value(&tree).expect("serialize");
        let back: PanelTree = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, tree);
    }

    #[test]
    fn parse_components_from_json_handles_single_component() {
        let json = serde_json::json!({
            "type": "text",
            "content": "single"
        });
        let v = parse_components_from_json(&json);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn parse_components_from_json_handles_legacy_shape() {
        let json = serde_json::json!({
            "components": [
                { "type": "text", "content": "a" },
                { "type": "divider" }
            ]
        });
        let v = parse_components_from_json(&json);
        assert_eq!(v.len(), 2);
    }
}
