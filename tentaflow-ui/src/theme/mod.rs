// =============================================================================
// Plik: theme/mod.rs
// Opis: Motyw wizualny — kolory, fonty, spacing, dark/light mode.
// =============================================================================

use egui::{Color32, FontFamily, FontId, Rounding, Stroke, Style, TextStyle, Visuals};

/// Rozmiary fontow
#[derive(Debug, Clone)]
pub struct FontSizes {
    pub header: f32,
    pub body: f32,
    pub small: f32,
    pub code: f32,
}

impl Default for FontSizes {
    fn default() -> Self {
        Self {
            header: 22.0,
            body: 14.0,
            small: 11.0,
            code: 13.0,
        }
    }
}

/// Status peera w mesh
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshStatus {
    Online,
    Suspect,
    Dead,
}

/// Typ serwisu AI
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Llm,
    Tts,
    Stt,
    Rag,
    Embedding,
    Vision,
    Router,
    Memory,
}

/// Styl przyciskow, inputow i paneli
#[derive(Debug, Clone)]
pub struct WidgetStyle {
    pub button_rounding: f32,
    pub input_rounding: f32,
    pub panel_rounding: f32,
    pub button_padding: f32,
    pub panel_padding: f32,
    pub border_width: f32,
}

impl Default for WidgetStyle {
    fn default() -> Self {
        Self {
            button_rounding: 6.0,
            input_rounding: 4.0,
            panel_rounding: 8.0,
            button_padding: 8.0,
            panel_padding: 12.0,
            border_width: 1.0,
        }
    }
}

/// Paleta kolorow zalezna od trybu dark/light
#[derive(Debug, Clone)]
pub struct Palette {
    pub bg_primary: Color32,
    pub bg_secondary: Color32,
    pub bg_panel: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub border: Color32,
    pub sidebar_bg: Color32,
    pub header_bg: Color32,
    pub status_bar_bg: Color32,
}

impl Palette {
    fn dark() -> Self {
        Self {
            bg_primary: Color32::from_rgb(17, 17, 27),
            bg_secondary: Color32::from_rgb(24, 24, 37),
            bg_panel: Color32::from_rgb(30, 30, 46),
            text_primary: Color32::from_rgb(205, 214, 244),
            text_secondary: Color32::from_rgb(166, 173, 200),
            text_muted: Color32::from_rgb(108, 112, 134),
            border: Color32::from_rgb(69, 71, 90),
            sidebar_bg: Color32::from_rgb(24, 24, 37),
            header_bg: Color32::from_rgb(30, 30, 46),
            status_bar_bg: Color32::from_rgb(17, 17, 27),
        }
    }

    fn light() -> Self {
        Self {
            bg_primary: Color32::from_rgb(239, 241, 245),
            bg_secondary: Color32::from_rgb(230, 233, 239),
            bg_panel: Color32::WHITE,
            text_primary: Color32::from_rgb(76, 79, 105),
            text_secondary: Color32::from_rgb(108, 111, 133),
            text_muted: Color32::from_rgb(156, 160, 176),
            border: Color32::from_rgb(188, 192, 204),
            sidebar_bg: Color32::from_rgb(230, 233, 239),
            header_bg: Color32::WHITE,
            status_bar_bg: Color32::from_rgb(230, 233, 239),
        }
    }
}

/// Motyw aplikacji
#[derive(Debug, Clone)]
pub struct Theme {
    pub dark_mode: bool,
    pub accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    pub fonts: FontSizes,
    pub widgets: WidgetStyle,
    pub palette: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            dark_mode: true,
            accent: Color32::from_rgb(99, 102, 241),   // indigo
            success: Color32::from_rgb(34, 197, 94),    // green
            warning: Color32::from_rgb(234, 179, 8),    // yellow
            error: Color32::from_rgb(239, 68, 68),      // red
            fonts: FontSizes::default(),
            widgets: WidgetStyle::default(),
            palette: Palette::dark(),
        }
    }
}

impl Theme {
    /// Przelacz miedzy dark a light mode
    pub fn toggle(&mut self) {
        self.dark_mode = !self.dark_mode;
        self.palette = if self.dark_mode {
            Palette::dark()
        } else {
            Palette::light()
        };
    }

    /// Kolor statusu peera w mesh
    pub fn mesh_status_color(&self, status: MeshStatus) -> Color32 {
        match status {
            MeshStatus::Online => self.success,
            MeshStatus::Suspect => self.warning,
            MeshStatus::Dead => self.error,
        }
    }

    /// Kolor typu serwisu AI
    pub fn service_kind_color(&self, kind: ServiceKind) -> Color32 {
        match kind {
            ServiceKind::Llm => Color32::from_rgb(99, 102, 241),       // indigo
            ServiceKind::Tts => Color32::from_rgb(168, 85, 247),       // purple
            ServiceKind::Stt => Color32::from_rgb(236, 72, 153),       // pink
            ServiceKind::Rag => Color32::from_rgb(14, 165, 233),       // sky
            ServiceKind::Embedding => Color32::from_rgb(20, 184, 166), // teal
            ServiceKind::Vision => Color32::from_rgb(245, 158, 11),    // amber
            ServiceKind::Router => Color32::from_rgb(34, 197, 94),     // green
            ServiceKind::Memory => Color32::from_rgb(244, 63, 94),     // rose
        }
    }

    /// Zastosuj motyw do kontekstu egui — kolory, fonty, spacing, widget style
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark_mode {
            Visuals::dark()
        } else {
            Visuals::light()
        };

        // Kolory tla i paneli
        visuals.panel_fill = self.palette.bg_panel;
        visuals.window_fill = self.palette.bg_secondary;
        visuals.extreme_bg_color = self.palette.bg_primary;
        visuals.faint_bg_color = self.palette.bg_secondary;

        // Kolory widgetow — nieaktywne
        visuals.widgets.inactive.bg_fill = self.palette.bg_secondary;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, self.palette.text_secondary);
        visuals.widgets.inactive.rounding = Rounding::same(self.widgets.button_rounding);
        visuals.widgets.inactive.bg_stroke = Stroke::new(self.widgets.border_width, self.palette.border);

        // Kolory widgetow — hover
        visuals.widgets.hovered.bg_fill = self.accent.linear_multiply(0.15);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, self.palette.text_primary);
        visuals.widgets.hovered.rounding = Rounding::same(self.widgets.button_rounding);
        visuals.widgets.hovered.bg_stroke = Stroke::new(self.widgets.border_width, self.accent);

        // Kolory widgetow — aktywne (wcisniete)
        visuals.widgets.active.bg_fill = self.accent.linear_multiply(0.3);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, self.palette.text_primary);
        visuals.widgets.active.rounding = Rounding::same(self.widgets.button_rounding);

        // Kolory widgetow — otwarte (np. dropdown)
        visuals.widgets.open.bg_fill = self.palette.bg_panel;
        visuals.widgets.open.fg_stroke = Stroke::new(1.0, self.accent);

        // Zaznaczenie (selectables, selection)
        visuals.selection.bg_fill = self.accent.linear_multiply(0.2);
        visuals.selection.stroke = Stroke::new(1.0, self.accent);

        // Okna
        visuals.window_rounding = Rounding::same(self.widgets.panel_rounding);
        visuals.window_stroke = Stroke::new(self.widgets.border_width, self.palette.border);

        ctx.set_visuals(visuals);

        // Rozmiary fontow
        let mut style = (*ctx.style()).clone();
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(self.fonts.header, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(self.fonts.body, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(self.fonts.small, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(self.fonts.code, FontFamily::Monospace),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(self.fonts.body, FontFamily::Proportional),
        );

        // Spacing
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(self.widgets.button_padding, self.widgets.button_padding * 0.5);
        style.spacing.window_margin = egui::Margin::same(self.widgets.panel_padding);

        ctx.set_style(style);
    }
}
