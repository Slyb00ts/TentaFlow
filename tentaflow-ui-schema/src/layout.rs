// === File: addon/ui/layout.rs — layout primitives (Stack/Grid/Spacer/Divider/Split) ===

use serde::{Deserialize, Serialize};

use super::theme::{Align, Direction, Justify, Size, SizeUnit, Spacing};
use super::UiComponent;

// =============================================================================
// LayoutComponent — sub-enum dla prymitywów rozkładu
// =============================================================================

/// Kontenery rozkładu (flex/grid). Sub-enum włączany do `UiComponent::Layout`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutComponent {
    /// Kontener kierunkowy (zastępuje ad-hoc CSS flex). Pionowo = kolumna,
    /// poziomo = rząd. Wsparcie dla wrap, gap, padding, align/justify.
    Stack {
        direction: Direction,
        gap: Spacing,
        align: Align,
        justify: Justify,
        #[serde(default)]
        wrap: bool,
        #[serde(default)]
        padding: Option<Spacing>,
        children: Vec<UiComponent>,
    },

    /// Grid (CSS Grid-like). Trackingi definiowane przez `GridTrack`,
    /// opcjonalne named template areas, każdy item może mieć span.
    Grid {
        columns: GridTrack,
        rows: GridTrack,
        gap: Spacing,
        #[serde(default)]
        areas: Option<Vec<Vec<String>>>,
        children: Vec<GridItem>,
    },

    /// Wymuszony odstęp na osi (Vertical = vspace, Horizontal = hspace).
    Spacer {
        size: Spacing,
        #[serde(default = "default_direction_vertical")]
        direction: Direction,
    },

    /// Linia rozdzielająca. `direction` = oś linii (Horizontal = pozioma
    /// kreska między sekcjami, Vertical = pionowa w toolbarze).
    Divider {
        direction: Direction,
        #[serde(default = "default_spacing_none")]
        spacing: Spacing,
    },

    /// 2-pane split — `primary_size` decyduje o udziale pierwszego panelu
    /// (np. `Percent(40)` = 40% / 60%). Brak resize na razie (Chunk 2.x).
    Split {
        direction: Direction,
        primary_size: Size,
        gap: Spacing,
        primary: Box<UiComponent>,
        secondary: Box<UiComponent>,
    },
}

fn default_direction_vertical() -> Direction {
    Direction::Vertical
}

fn default_spacing_none() -> Spacing {
    Spacing::None
}

// =============================================================================
// GridTrack — definicja trackingów dla Grid
// =============================================================================

/// Sposób definiowania trackingów (kolumn/wierszy) w `LayoutComponent::Grid`.
/// `Repeat` = N równych trackingów; `Explicit` = lista konkretnych rozmiarów;
/// `AutoFill` = `repeat(auto-fill, minmax(min, max))`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GridTrack {
    Repeat { count: u32, size: Size },
    Explicit { tracks: Vec<Size> },
    AutoFill { min: SizeUnit, max: SizeUnit },
}

// =============================================================================
// GridItem — komórka grida z pozycjonowaniem
// =============================================================================

/// Pojedyncza komórka grida. Można podać `area` (odniesienie do `areas`
/// w `Grid`) ALBO konkretne linie startu/końca w kolumnie/wierszu.
/// Domyślnie auto-flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_end: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_end: Option<u32>,
    pub component: UiComponent,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy::LegacyComponent;

    fn leaf_text(content: &str) -> UiComponent {
        UiComponent::Legacy(LegacyComponent::Text {
            content: content.to_string(),
            style: None,
        })
    }

    #[test]
    fn stack_round_trip_with_defaults() {
        let s = LayoutComponent::Stack {
            direction: Direction::Vertical,
            gap: Spacing::Md,
            align: Align::Stretch,
            justify: Justify::Start,
            wrap: false,
            padding: None,
            children: vec![leaf_text("a"), leaf_text("b")],
        };
        let j = serde_json::to_value(&s).expect("serialize");
        assert_eq!(j["type"], "stack");
        assert_eq!(j["gap"], "md");
        let back: LayoutComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, s);
    }

    #[test]
    fn stack_wrap_and_padding_defaults_on_deserialize() {
        let j = serde_json::json!({
            "type": "stack",
            "direction": "horizontal",
            "gap": "sm",
            "align": "center",
            "justify": "start",
            "children": []
        });
        let s: LayoutComponent = serde_json::from_value(j).expect("deserialize");
        match s {
            LayoutComponent::Stack {
                wrap, padding, ..
            } => {
                assert!(!wrap);
                assert_eq!(padding, None);
            }
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn grid_repeat_round_trip() {
        let g = LayoutComponent::Grid {
            columns: GridTrack::Repeat {
                count: 3,
                size: Size::Fr { value: 1 },
            },
            rows: GridTrack::Repeat {
                count: 1,
                size: Size::Auto,
            },
            gap: Spacing::Lg,
            areas: None,
            children: vec![GridItem {
                area: None,
                column_start: None,
                column_end: None,
                row_start: None,
                row_end: None,
                component: leaf_text("cell"),
            }],
        };
        let j = serde_json::to_value(&g).expect("serialize");
        let back: LayoutComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, g);
    }

    #[test]
    fn grid_with_areas_and_spans() {
        let g = LayoutComponent::Grid {
            columns: GridTrack::Explicit {
                tracks: vec![
                    Size::Fixed {
                        unit: SizeUnit::Px { value: 240 },
                    },
                    Size::Fr { value: 1 },
                ],
            },
            rows: GridTrack::Repeat {
                count: 2,
                size: Size::Auto,
            },
            gap: Spacing::Md,
            areas: Some(vec![
                vec!["sidebar".to_string(), "main".to_string()],
                vec!["sidebar".to_string(), "main".to_string()],
            ]),
            children: vec![GridItem {
                area: Some("main".to_string()),
                column_start: None,
                column_end: None,
                row_start: None,
                row_end: None,
                component: leaf_text("body"),
            }],
        };
        let j = serde_json::to_value(&g).expect("serialize");
        let back: LayoutComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, g);
    }

    #[test]
    fn spacer_default_direction_is_vertical() {
        let j = serde_json::json!({"type": "spacer", "size": "lg"});
        let s: LayoutComponent = serde_json::from_value(j).expect("deserialize");
        match s {
            LayoutComponent::Spacer { direction, size } => {
                assert_eq!(direction, Direction::Vertical);
                assert_eq!(size, Spacing::Lg);
            }
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn divider_round_trip() {
        let d = LayoutComponent::Divider {
            direction: Direction::Horizontal,
            spacing: Spacing::Sm,
        };
        let j = serde_json::to_value(&d).expect("serialize");
        let back: LayoutComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, d);
    }

    #[test]
    fn split_round_trip() {
        let s = LayoutComponent::Split {
            direction: Direction::Horizontal,
            primary_size: Size::Percent { value: 40 },
            gap: Spacing::Md,
            primary: Box::new(leaf_text("left")),
            secondary: Box::new(leaf_text("right")),
        };
        let j = serde_json::to_value(&s).expect("serialize");
        assert_eq!(j["type"], "split");
        let back: LayoutComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, s);
    }

    #[test]
    fn grid_autofill_round_trip() {
        let g = GridTrack::AutoFill {
            min: SizeUnit::Px { value: 240 },
            max: SizeUnit::Spacing {
                value: Spacing::Xxxl,
            },
        };
        let j = serde_json::to_value(&g).expect("serialize");
        let back: GridTrack = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, g);
    }
}

