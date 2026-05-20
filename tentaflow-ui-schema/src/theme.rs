// === File: addon/ui/theme.rs — theme tokens (Color/Spacing/TextStyle/Radius/Shadow/FontWeight/Size/utility enums) ===

use serde::{Deserialize, Serialize};

// =============================================================================
// Color — semantic role, NIE hex. Renderer mapuje na konkretną wartość motywu.
// =============================================================================

/// Semantyczna rola koloru. Addon NIGDY nie wybiera hex — wybiera rolę,
/// renderer (HTML/WGPU) podstawia wartość z aktywnego motywu (dark/light/HC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Primary,
    PrimaryHover,
    Accent,
    AccentHover,

    Success,
    Warning,
    Danger,
    Info,

    Text,
    TextMuted,
    TextSubtle,
    TextInverse,

    Bg,
    BgElevated,
    BgSurface,
    BgInput,

    Border,
    BorderHover,
}

// =============================================================================
// Spacing — skala 8-stopniowa
// =============================================================================

/// Skala odstępów. Mapuje na konkretne piksele w rendererze (Xs=6, Sm=8,
/// Md=12, Lg=16, Xl=24, Xxl=36, Xxxl=60).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Spacing {
    None,
    Xxs,
    Xs,
    Sm,
    Md,
    Lg,
    Xl,
    Xxl,
    Xxxl,
}

// =============================================================================
// TextStyle — typografia semantyczna
// =============================================================================

/// Semantyczna typografia — addon mówi "to jest nagłówek sekcji", nie
/// "rozmiar 18 pikseli weight 700". Renderer dobiera konkretne wartości.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStyle {
    Caption,
    Body,
    BodyStrong,
    Heading1,
    Heading2,
    Heading3,
    Heading4,
    Hero,
    Code,
    Label,
}

// =============================================================================
// Radius — zaokrąglenie rogów
// =============================================================================

/// Stopień zaokrąglenia rogów. `Full` daje pełen okrąg (avatar/status-dot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Radius {
    None,
    Sm,
    Md,
    Lg,
    Full,
}

// =============================================================================
// Shadow — głębokość cienia
// =============================================================================

/// Stopień cienia. `Glow` to świecący accent dla aktywnych elementów (np.
/// focus przycisku Primary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shadow {
    None,
    Sm,
    Md,
    Lg,
    Glow,
}

// =============================================================================
// FontWeight — preferuj TextStyle, używaj tylko dla wyjątków
// =============================================================================

/// Waga fontu. W normalnym kodzie addonu używaj `TextStyle` — `FontWeight`
/// jest dla precyzyjnych nadpisań w komponentach niskopoziomowych.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontWeight {
    Regular,
    Medium,
    SemiBold,
    Bold,
}

// =============================================================================
// Utility enums — alignment, direction, cursor
// =============================================================================

/// Wyrównanie na osi poprzecznej (cross-axis) w `Stack`/`Grid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
    Baseline,
}

/// Rozłożenie na osi głównej (main-axis) w `Stack`/`Grid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Kierunek układu — pionowy (kolumna) lub poziomy (rząd).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Vertical,
    Horizontal,
}

/// Styl kursora dla obszarów interaktywnych (Canvas, custom drop-zones).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    Default,
    Pointer,
    Crosshair,
    Move,
    Text,
    NotAllowed,
}

// =============================================================================
// Size / SizeUnit — sizing dla GridTrack i Split
// =============================================================================

/// Jednostka rozmiaru — albo token Spacing (preferowane), albo surowe piksele
/// dla wyjątków (np. fixed track w gridzie 240px sidebar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SizeUnit {
    Spacing { value: Spacing },
    Px { value: u32 },
}

/// Rozmiar trackingu w gridzie lub primary-pane w Split. `Fr` to grid
/// fractional unit (CSS `1fr`), `MinMax` to grid `minmax(min, max)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Size {
    Auto,
    Fill,
    Fixed { unit: SizeUnit },
    Percent { value: u8 },
    Fr { value: u32 },
    MinMax { min: SizeUnit, max: SizeUnit },
}

// =============================================================================
// IconName — whitelist nazw ikon (renderer mapuje na SVG sprite / atlas)
// =============================================================================

/// Whitelisted set of icon names. Addon NIGDY nie sends raw SVG/PNG — tylko
/// semantic name z tej listy. Renderer (HTML: SVG sprite z `icons.svg`,
/// WGPU: texture atlas) wybiera konkretny glif. Nowe ikony dodajemy tutaj
/// po dodaniu sprite'u do renderera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconName {
    Home,
    Dashboard,
    Cameras,
    Alarms,
    Profiles,
    Models,
    Zones,
    Audit,
    Evidence,
    Settings,
    Help,

    Add,
    Edit,
    Delete,
    Save,
    Cancel,
    Search,
    Filter,
    Refresh,
    More,
    Close,
    Check,

    Video,
    Image,
    Person,
    Vehicle,
    Face,
    Document,
    File,
    Folder,
    Code,

    Success,
    Warning,
    Danger,
    Info,
    Locked,
    Unlocked,
    Eye,
    EyeOff,

    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ChevronUp,
    ChevronDown,
    ChevronLeft,
    ChevronRight,

    Power,
    Settings2,
    User,
    Users,
    Logout,
    Bell,
    Star,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_round_trip() {
        let c = Color::Primary;
        let j = serde_json::to_value(c).expect("serialize");
        assert_eq!(j, serde_json::json!("primary"));
        let back: Color = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, Color::Primary);
    }

    #[test]
    fn color_all_roles_serialize_snake_case() {
        for c in [
            Color::PrimaryHover,
            Color::AccentHover,
            Color::TextSubtle,
            Color::BgElevated,
            Color::BorderHover,
        ] {
            let s = serde_json::to_string(&c).expect("serialize");
            // wszystkie wartości to lower_snake_case w cudzysłowach
            assert!(s.starts_with('"') && s.ends_with('"'));
            assert!(s.chars().all(|ch| ch == '"' || ch.is_ascii_lowercase() || ch == '_'));
        }
    }

    #[test]
    fn spacing_round_trip() {
        let j = serde_json::to_value(Spacing::Md).expect("serialize");
        assert_eq!(j, serde_json::json!("md"));
        let back: Spacing = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, Spacing::Md);
    }

    #[test]
    fn text_style_round_trip() {
        let j = serde_json::to_value(TextStyle::Heading2).expect("serialize");
        assert_eq!(j, serde_json::json!("heading2"));
        let back: TextStyle = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, TextStyle::Heading2);
    }

    #[test]
    fn radius_shadow_font_weight_round_trip() {
        assert_eq!(
            serde_json::to_value(Radius::Full).unwrap(),
            serde_json::json!("full")
        );
        assert_eq!(
            serde_json::to_value(Shadow::Glow).unwrap(),
            serde_json::json!("glow")
        );
        assert_eq!(
            serde_json::to_value(FontWeight::SemiBold).unwrap(),
            serde_json::json!("semi_bold")
        );
    }

    #[test]
    fn size_fixed_round_trip() {
        let s = Size::Fixed {
            unit: SizeUnit::Px { value: 240 },
        };
        let j = serde_json::to_value(&s).expect("serialize");
        assert_eq!(
            j,
            serde_json::json!({"kind": "fixed", "unit": {"kind": "px", "value": 240}})
        );
        let back: Size = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, s);
    }

    #[test]
    fn size_fr_and_percent_round_trip() {
        let fr = Size::Fr { value: 2 };
        let pct = Size::Percent { value: 40 };
        let fr_back: Size = serde_json::from_value(serde_json::to_value(&fr).unwrap()).unwrap();
        let pct_back: Size = serde_json::from_value(serde_json::to_value(&pct).unwrap()).unwrap();
        assert_eq!(fr, fr_back);
        assert_eq!(pct, pct_back);
    }

    #[test]
    fn size_minmax_round_trip() {
        let s = Size::MinMax {
            min: SizeUnit::Spacing {
                value: Spacing::Lg,
            },
            max: SizeUnit::Px { value: 480 },
        };
        let j = serde_json::to_value(&s).expect("serialize");
        let back: Size = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, s);
    }

    #[test]
    fn icon_name_round_trip_snake_case() {
        let j = serde_json::to_value(IconName::ChevronRight).expect("serialize");
        assert_eq!(j, serde_json::json!("chevron_right"));
        let back: IconName = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, IconName::ChevronRight);
    }

    #[test]
    fn icon_name_rejects_unknown() {
        let j = serde_json::json!("not_a_real_icon");
        let r: Result<IconName, _> = serde_json::from_value(j);
        assert!(r.is_err());
    }

    #[test]
    fn align_justify_direction_round_trip() {
        assert_eq!(
            serde_json::to_value(Align::Stretch).unwrap(),
            serde_json::json!("stretch")
        );
        assert_eq!(
            serde_json::to_value(Justify::SpaceBetween).unwrap(),
            serde_json::json!("space_between")
        );
        assert_eq!(
            serde_json::to_value(Direction::Horizontal).unwrap(),
            serde_json::json!("horizontal")
        );
        assert_eq!(
            serde_json::to_value(CursorStyle::NotAllowed).unwrap(),
            serde_json::json!("not_allowed")
        );
    }
}
