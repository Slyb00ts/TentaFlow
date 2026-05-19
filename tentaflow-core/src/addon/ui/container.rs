// === File: addon/ui/container.rs — container components (Card/Section/Tabs/NavTabs/Toolbar/Sidebar/Window/Drawer/Popover/Breadcrumb/Pagination/Collapsible/Tooltip) ===

use serde::{Deserialize, Serialize};

use super::theme::{IconName, Radius, Shadow, Spacing};
use super::UiComponent;

// =============================================================================
// ContainerComponent — sub-enum dla kontenerów
// =============================================================================

/// Kontenery: powierzchnie z tytułem/akcjami (Card), nawigacja (Tabs/NavTabs/
/// Sidebar/Toolbar/Breadcrumb), warstwy overlay (Window/Drawer/Popover) oraz
/// pomocnicze (Collapsible/Tooltip/Pagination/Section). Wariant `Window`,
/// `Drawer` i `Popover` MUSI być umieszczony w `PanelTree::overlays`, nie
/// w `root` — walidacja w `mod.rs` to wymusza.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContainerComponent {
    /// Powierzchnia z opcjonalnym nagłówkiem (title/subtitle/icon) i listą
    /// akcji w prawym górnym rogu. Padding/radius/shadow są semantyczne.
    Card {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<UiComponent>,
        #[serde(default = "default_card_padding")]
        padding: Spacing,
        #[serde(default = "default_card_radius")]
        radius: Radius,
        #[serde(default = "default_card_shadow")]
        shadow: Shadow,
        children: Vec<UiComponent>,
    },

    /// Wariant `Card` przeznaczony do zagnieżdżenia wewnątrz innej karty —
    /// bez własnego shadow/radius, żeby nie tworzyć "karty w karcie" wizualnie.
    SectionCard {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<UiComponent>,
        children: Vec<UiComponent>,
    },

    /// Semantyczna sekcja z opcjonalnym nagłówkiem. Bez własnego tła —
    /// służy tylko do logicznego grupowania w drzewie a11y/SEO.
    Section {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading: Option<String>,
        children: Vec<UiComponent>,
    },

    /// Lokalne zakładki (bez routingu). Renderer przełącza widoczne dziecko
    /// po stronie klienta. Do ~5 zakładek; więcej = użyj `NavTabs`.
    Tabs {
        tabs: Vec<TabItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_id: Option<String>,
    },

    /// Zakładki nawigacyjne — każdy klik wywołuje `panel_navigate(panel_id)`.
    /// `active_id` MUSI istnieć w `items[].id`.
    NavTabs {
        items: Vec<NavTabItem>,
        active_id: String,
    },

    /// Pasek narzędziowy: opcjonalny breadcrumb / tytuł + lista akcji.
    /// `density` steruje wysokością i wewnętrznym paddingiem.
    Toolbar {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        breadcrumb: Option<Breadcrumb>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<UiComponent>,
        #[serde(default)]
        density: Density,
    },

    /// Lewy panel nawigacji najwyższego poziomu w addonie. Każdy item z
    /// `panel_id` klika do `panel_navigate`. `active_id` MUSI istnieć
    /// w `sections[].items[].id`.
    Sidebar {
        sections: Vec<SidebarSection>,
        #[serde(default)]
        collapsed: bool,
        active_id: String,
    },

    /// Rozwijany blok — renderowany z chevronem; zmiana stanu jest lokalna
    /// po stronie renderera (nie wymaga akcji addonu).
    Collapsible {
        title: String,
        #[serde(default)]
        open: bool,
        children: Vec<UiComponent>,
    },

    /// Wrapper pokazujący tekstowy tooltip nad `target` przy hoverze/focusie.
    /// Renderer odpowiada za pozycjonowanie zgodnie z `placement`.
    Tooltip {
        target: Box<UiComponent>,
        content: String,
        #[serde(default)]
        placement: TooltipPlacement,
    },

    /// Modal — wariant overlay. NIGDY nie umieszczaj w `root[]`; tylko
    /// w `PanelTree::overlays[].content`. Renderer pokazuje backdrop i
    /// (jeśli `dismissable=true`) X w prawym górnym rogu.
    Window {
        title: String,
        #[serde(default)]
        size: WindowSize,
        #[serde(default = "default_dismissable_true")]
        dismissable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_close: Option<String>,
        children: Vec<UiComponent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        footer: Vec<UiComponent>,
    },

    /// Panel wysuwany z brzegu ekranu — wariant overlay. Zasady jak `Window`.
    Drawer {
        title: String,
        #[serde(default)]
        side: DrawerSide,
        #[serde(default)]
        size: WindowSize,
        #[serde(default = "default_dismissable_true")]
        dismissable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_close: Option<String>,
        children: Vec<UiComponent>,
    },

    /// Mały overlay zakotwiczony do elementu o id `target_id` (np. context
    /// menu, picker). Wariant overlay — tylko w `overlays[].content`.
    Popover {
        target_id: String,
        #[serde(default)]
        placement: TooltipPlacement,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_close: Option<String>,
        children: Vec<UiComponent>,
    },

    /// Ścieżka okruchów. Ostatni item zazwyczaj bez `panel_id` (current).
    Breadcrumb { items: Vec<BreadcrumbItem> },

    /// Generic pagination control. `current_page` MUSI być <= `total_pages`
    /// (gdy `total_pages > 0`). `sibling_count` steruje liczbą stron po obu
    /// stronach bieżącej (domyślnie 1).
    Pagination {
        current_page: u32,
        total_pages: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_change: Option<String>,
        #[serde(default = "default_sibling_count")]
        sibling_count: u32,
    },
}

fn default_card_padding() -> Spacing {
    Spacing::Md
}
fn default_card_radius() -> Radius {
    Radius::Md
}
fn default_card_shadow() -> Shadow {
    Shadow::Sm
}
fn default_dismissable_true() -> bool {
    true
}
fn default_sibling_count() -> u32 {
    1
}

// =============================================================================
// Pomocnicze struktury
// =============================================================================

/// Pojedyncza zakładka w `ContainerComponent::Tabs`. `id` unikalne w obrębie
/// jednego `Tabs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    pub children: Vec<UiComponent>,
}

/// Pojedyncza zakładka nawigacyjna — klik wywołuje `panel_navigate(panel_id)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavTabItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    pub panel_id: String,
}

/// Lista pozycji breadcrumb. Wydzielona z `ContainerComponent::Toolbar`,
/// żeby ten sam typ był reużywalny w `NavigationSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub items: Vec<BreadcrumbItem>,
}

/// Pozycja w breadcrumbie. `panel_id == None` oznacza "current page" — nie
/// rendrowane jako link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreadcrumbItem {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_id: Option<String>,
}

/// Sekcja sidebara — opcjonalny nagłówek + lista itemów.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidebarSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    pub items: Vec<SidebarItem>,
}

/// Pozycja sidebara. `panel_id` (gdy `Some`) wywołuje `panel_navigate`;
/// `badge` to opcjonalny licznik (np. "12") obok labela.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidebarItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_id: Option<String>,
}

/// Gęstość paska toolbar — wpływa na padding i wysokość elementu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

/// Rozmiar modala/drawera. `Full` = pełen ekran (np. wizard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowSize {
    Sm,
    #[default]
    Md,
    Lg,
    Xl,
    Full,
}

/// Strona z której wysuwa się drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DrawerSide {
    Left,
    #[default]
    Right,
    Top,
    Bottom,
}

/// Strona względem targetu na której pojawia się tooltip/popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TooltipPlacement {
    #[default]
    Top,
    Right,
    Bottom,
    Left,
}

// =============================================================================
// Walidacja
// =============================================================================

/// Format `panel_id` — lowercase ASCII + dashes + cyfry, 1..=64 znaków.
/// Statyczne, nie echo'uje wartości w błędach.
pub(crate) fn validate_panel_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() || id.len() > 64 {
        return Err("panel_id_invalid_format");
    }
    for b in id.bytes() {
        let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_';
        if !ok {
            return Err("panel_id_invalid_format");
        }
    }
    Ok(())
}

/// Walidacja pojedynczego komponentu kontenerowego + rekurencyjna walidacja
/// dzieci. Walidacja overlay-only kinds (Window/Drawer/Popover) jest osobno
/// w `mod.rs` (kontekstowa: w root[] vs overlay.content).
pub fn validate_and_normalize(
    component: &mut ContainerComponent,
) -> Result<(), &'static str> {
    use ContainerComponent::*;
    match component {
        Card {
            actions, children, ..
        } => {
            for c in actions {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "card_actions_invalid")?;
            }
            for c in children {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "card_children_invalid")?;
            }
            Ok(())
        }
        SectionCard {
            actions, children, ..
        } => {
            for c in actions {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "section_card_actions_invalid")?;
            }
            for c in children {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "section_card_children_invalid")?;
            }
            Ok(())
        }
        Section { children, .. } => {
            for c in children {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "section_children_invalid")?;
            }
            Ok(())
        }
        Tabs { tabs, active_id } => {
            let mut seen: Vec<&str> = Vec::with_capacity(tabs.len());
            for t in tabs.iter() {
                if seen.iter().any(|s| *s == t.id.as_str()) {
                    return Err("tabs_duplicate_tab_id");
                }
                seen.push(t.id.as_str());
            }
            if let Some(active) = active_id {
                if !tabs.iter().any(|t| &t.id == active) {
                    return Err("tabs_active_not_in_items");
                }
            }
            for t in tabs.iter_mut() {
                for c in &mut t.children {
                    super::validate_and_normalize_component(c)
                        .map_err(|_| "tabs_children_invalid")?;
                }
            }
            Ok(())
        }
        NavTabs { items, active_id } => {
            let mut seen: Vec<&str> = Vec::with_capacity(items.len());
            for it in items.iter() {
                if seen.iter().any(|s| *s == it.id.as_str()) {
                    return Err("nav_tabs_duplicate_id");
                }
                seen.push(it.id.as_str());
                validate_panel_id(&it.panel_id)?;
            }
            if !items.iter().any(|i| &i.id == active_id) {
                return Err("nav_tabs_active_not_in_items");
            }
            Ok(())
        }
        Toolbar { actions, .. } => {
            for c in actions {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "toolbar_actions_invalid")?;
            }
            Ok(())
        }
        Sidebar {
            sections,
            active_id,
            ..
        } => {
            let mut seen: Vec<&str> = Vec::new();
            let mut found_active = false;
            for s in sections.iter() {
                for it in s.items.iter() {
                    if seen.iter().any(|x| *x == it.id.as_str()) {
                        return Err("sidebar_duplicate_id");
                    }
                    seen.push(it.id.as_str());
                    if let Some(pid) = &it.panel_id {
                        validate_panel_id(pid)?;
                    }
                    if &it.id == active_id {
                        found_active = true;
                    }
                }
            }
            if !found_active {
                return Err("sidebar_active_not_in_items");
            }
            Ok(())
        }
        Collapsible { children, .. } => {
            for c in children {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "collapsible_children_invalid")?;
            }
            Ok(())
        }
        Tooltip { target, .. } => {
            super::validate_and_normalize_component(target)
                .map_err(|_| "tooltip_target_invalid")?;
            Ok(())
        }
        Window {
            children, footer, ..
        } => {
            for c in children {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "window_children_invalid")?;
            }
            for c in footer {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "window_footer_invalid")?;
            }
            Ok(())
        }
        Drawer { children, .. } => {
            for c in children {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "drawer_children_invalid")?;
            }
            Ok(())
        }
        Popover { children, .. } => {
            for c in children {
                super::validate_and_normalize_component(c)
                    .map_err(|_| "popover_children_invalid")?;
            }
            Ok(())
        }
        Breadcrumb { items } => {
            for it in items.iter() {
                if let Some(pid) = &it.panel_id {
                    validate_panel_id(pid)?;
                }
            }
            Ok(())
        }
        Pagination {
            current_page,
            total_pages,
            ..
        } => {
            if *total_pages > 0 && *current_page > *total_pages {
                return Err("pagination_current_exceeds_total");
            }
            Ok(())
        }
    }
}

/// Czy wariant kontenera jest typu "overlay" — tj. musi siedzieć w
/// `PanelTree::overlays[].content`, nie w `root[]`. Używane przez walidator
/// w `mod.rs`.
pub(crate) fn is_overlay_kind(c: &ContainerComponent) -> bool {
    matches!(
        c,
        ContainerComponent::Window { .. }
            | ContainerComponent::Drawer { .. }
            | ContainerComponent::Popover { .. }
    )
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::ui::legacy::LegacyComponent;

    fn leaf(text: &str) -> UiComponent {
        UiComponent::Legacy(LegacyComponent::Text {
            content: text.to_string(),
            style: None,
        })
    }

    #[test]
    fn card_round_trip_with_defaults() {
        let c = ContainerComponent::Card {
            title: Some("Hello".to_string()),
            subtitle: None,
            icon: None,
            actions: vec![],
            padding: Spacing::Md,
            radius: Radius::Md,
            shadow: Shadow::Sm,
            children: vec![leaf("body")],
        };
        let j = serde_json::to_value(&c).expect("serialize");
        assert_eq!(j["type"], "card");
        assert_eq!(j["padding"], "md");
        let back: ContainerComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, c);
    }

    #[test]
    fn card_defaults_via_serde() {
        let j = serde_json::json!({
            "type": "card",
            "children": []
        });
        let c: ContainerComponent = serde_json::from_value(j).expect("deserialize");
        match c {
            ContainerComponent::Card {
                padding,
                radius,
                shadow,
                ..
            } => {
                assert_eq!(padding, Spacing::Md);
                assert_eq!(radius, Radius::Md);
                assert_eq!(shadow, Shadow::Sm);
            }
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn tabs_duplicate_id_rejected() {
        let mut c = ContainerComponent::Tabs {
            tabs: vec![
                TabItem {
                    id: "a".to_string(),
                    label: "A".to_string(),
                    icon: None,
                    children: vec![],
                },
                TabItem {
                    id: "a".to_string(),
                    label: "A2".to_string(),
                    icon: None,
                    children: vec![],
                },
            ],
            active_id: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "tabs_duplicate_tab_id");
    }

    #[test]
    fn tabs_active_id_must_exist() {
        let mut c = ContainerComponent::Tabs {
            tabs: vec![TabItem {
                id: "a".to_string(),
                label: "A".to_string(),
                icon: None,
                children: vec![],
            }],
            active_id: Some("missing".to_string()),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "tabs_active_not_in_items");
    }

    #[test]
    fn nav_tabs_active_id_must_exist() {
        let mut c = ContainerComponent::NavTabs {
            items: vec![NavTabItem {
                id: "home".to_string(),
                label: "Home".to_string(),
                icon: None,
                panel_id: "dashboard".to_string(),
            }],
            active_id: "missing".to_string(),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "nav_tabs_active_not_in_items");
    }

    #[test]
    fn nav_tabs_invalid_panel_id_rejected() {
        let mut c = ContainerComponent::NavTabs {
            items: vec![NavTabItem {
                id: "home".to_string(),
                label: "Home".to_string(),
                icon: None,
                panel_id: "BadPanelID!".to_string(),
            }],
            active_id: "home".to_string(),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "panel_id_invalid_format");
    }

    #[test]
    fn sidebar_active_id_must_exist() {
        let mut c = ContainerComponent::Sidebar {
            sections: vec![SidebarSection {
                heading: None,
                items: vec![SidebarItem {
                    id: "home".to_string(),
                    label: "Home".to_string(),
                    icon: None,
                    badge: None,
                    panel_id: Some("dashboard".to_string()),
                }],
            }],
            collapsed: false,
            active_id: "missing".to_string(),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "sidebar_active_not_in_items");
    }

    #[test]
    fn sidebar_duplicate_item_id_rejected() {
        let mut c = ContainerComponent::Sidebar {
            sections: vec![
                SidebarSection {
                    heading: None,
                    items: vec![SidebarItem {
                        id: "x".to_string(),
                        label: "X".to_string(),
                        icon: None,
                        badge: None,
                        panel_id: None,
                    }],
                },
                SidebarSection {
                    heading: None,
                    items: vec![SidebarItem {
                        id: "x".to_string(),
                        label: "X2".to_string(),
                        icon: None,
                        badge: None,
                        panel_id: None,
                    }],
                },
            ],
            collapsed: false,
            active_id: "x".to_string(),
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "sidebar_duplicate_id");
    }

    #[test]
    fn pagination_current_exceeds_total_rejected() {
        let mut c = ContainerComponent::Pagination {
            current_page: 10,
            total_pages: 5,
            on_change: None,
            sibling_count: 1,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "pagination_current_exceeds_total");
    }

    #[test]
    fn pagination_zero_total_is_ok() {
        let mut c = ContainerComponent::Pagination {
            current_page: 0,
            total_pages: 0,
            on_change: None,
            sibling_count: 1,
        };
        validate_and_normalize(&mut c).expect("ok");
    }

    #[test]
    fn window_round_trip_with_footer() {
        let w = ContainerComponent::Window {
            title: "Confirm".to_string(),
            size: WindowSize::Md,
            dismissable: true,
            on_close: Some("close".to_string()),
            children: vec![leaf("body")],
            footer: vec![leaf("ok-btn")],
        };
        let j = serde_json::to_value(&w).expect("serialize");
        assert_eq!(j["type"], "window");
        assert_eq!(j["dismissable"], true);
        let back: ContainerComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, w);
    }

    #[test]
    fn drawer_default_side_is_right() {
        let j = serde_json::json!({
            "type": "drawer",
            "title": "Detail",
            "children": []
        });
        let d: ContainerComponent = serde_json::from_value(j).expect("deserialize");
        match d {
            ContainerComponent::Drawer { side, dismissable, .. } => {
                assert_eq!(side, DrawerSide::Right);
                assert!(dismissable);
            }
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn breadcrumb_invalid_panel_id_rejected() {
        let mut c = ContainerComponent::Breadcrumb {
            items: vec![BreadcrumbItem {
                label: "Bad".to_string(),
                panel_id: Some("UPPER".to_string()),
            }],
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "panel_id_invalid_format");
    }

    #[test]
    fn tooltip_round_trip() {
        let t = ContainerComponent::Tooltip {
            target: Box::new(leaf("hover-me")),
            content: "Helpful".to_string(),
            placement: TooltipPlacement::Right,
        };
        let j = serde_json::to_value(&t).expect("serialize");
        assert_eq!(j["type"], "tooltip");
        assert_eq!(j["placement"], "right");
        let back: ContainerComponent = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, t);
    }

    #[test]
    fn collapsible_round_trip_default_closed() {
        let j = serde_json::json!({
            "type": "collapsible",
            "title": "Advanced",
            "children": []
        });
        let c: ContainerComponent = serde_json::from_value(j).expect("deserialize");
        match c {
            ContainerComponent::Collapsible { open, .. } => assert!(!open),
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn density_default_is_normal() {
        let j = serde_json::json!({
            "type": "toolbar",
            "actions": []
        });
        let t: ContainerComponent = serde_json::from_value(j).expect("deserialize");
        match t {
            ContainerComponent::Toolbar { density, .. } => assert_eq!(density, Density::Normal),
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn is_overlay_kind_detects_overlay_variants() {
        let w = ContainerComponent::Window {
            title: "x".to_string(),
            size: WindowSize::Md,
            dismissable: true,
            on_close: None,
            children: vec![],
            footer: vec![],
        };
        let card = ContainerComponent::Card {
            title: None,
            subtitle: None,
            icon: None,
            actions: vec![],
            padding: Spacing::Md,
            radius: Radius::Md,
            shadow: Shadow::Sm,
            children: vec![],
        };
        assert!(is_overlay_kind(&w));
        assert!(!is_overlay_kind(&card));
    }

    #[test]
    fn validate_panel_id_accepts_valid() {
        assert!(validate_panel_id("dashboard").is_ok());
        assert!(validate_panel_id("cameras-list").is_ok());
        assert!(validate_panel_id("foo_bar-1").is_ok());
    }

    #[test]
    fn validate_panel_id_rejects_invalid() {
        assert!(validate_panel_id("").is_err());
        assert!(validate_panel_id("UPPER").is_err());
        assert!(validate_panel_id("../escape").is_err());
        assert!(validate_panel_id("with space").is_err());
    }
}
