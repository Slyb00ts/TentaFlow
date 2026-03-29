// =============================================================================
// Plik: widgets/mod.rs
// Opis: Wspolne widgety UI — karty metryk, badge, gauge, tabele, wykresy.
// =============================================================================

use egui::{Color32, Rect, RichText, Rounding, Sense, Stroke, Ui, Vec2};

// ---------------------------------------------------------------------------
// Metric Card — karta z wartoscia i etykieta (jak na dashboard)
// ---------------------------------------------------------------------------

pub fn metric_card(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    let frame = egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::same(16.0))
        .stroke(Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color));

    frame.show(ui, |ui| {
        ui.set_min_width(140.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(value).size(28.0).color(color).strong());
            ui.add_space(2.0);
            ui.label(RichText::new(label).size(12.0).color(ui.visuals().widgets.inactive.fg_stroke.color));
        });
    });
}

// ---------------------------------------------------------------------------
// Badge — kolorowa etykieta statusu
// ---------------------------------------------------------------------------

pub fn badge(ui: &mut Ui, text: &str, bg_color: Color32) {
    let text_color = if is_dark_color(bg_color) {
        Color32::WHITE
    } else {
        Color32::from_rgb(30, 30, 30)
    };

    let frame = egui::Frame::none()
        .fill(bg_color)
        .rounding(Rounding::same(4.0))
        .inner_margin(egui::Margin::symmetric(8.0, 2.0));

    frame.show(ui, |ui| {
        ui.label(RichText::new(text).size(11.0).color(text_color).strong());
    });
}

/// Badge z kropka statusu
pub fn status_badge(ui: &mut Ui, text: &str, dot_color: Color32) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, dot_color);
        ui.label(RichText::new(text).size(12.0));
    });
}

// ---------------------------------------------------------------------------
// Gauge — pasek uzycia CPU/RAM z procentem
// ---------------------------------------------------------------------------

pub fn usage_gauge(ui: &mut Ui, label: &str, value: f64, width: f32) {
    let color = if value < 50.0 {
        Color32::from_rgb(34, 197, 94)
    } else if value < 80.0 {
        Color32::from_rgb(234, 179, 8)
    } else {
        Color32::from_rgb(239, 68, 68)
    };

    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 16.0), Sense::hover());
        let painter = ui.painter();

        painter.rect_filled(rect, Rounding::same(3.0), Color32::from_rgb(40, 40, 55));

        let fill_width = rect.width() * (value as f32 / 100.0).min(1.0);
        let fill_rect = Rect::from_min_size(rect.min, Vec2::new(fill_width, rect.height()));
        painter.rect_filled(fill_rect, Rounding::same(3.0), color);

        let text = format!("{:.0}%", value);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::new(10.0, egui::FontFamily::Proportional),
            Color32::WHITE,
        );
    });
}

// ---------------------------------------------------------------------------
// Sparkline chart (dla dashboard)
// ---------------------------------------------------------------------------

pub fn sparkline_chart(ui: &mut Ui, data: &[f64], color: Color32, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if data.is_empty() {
        return;
    }

    let painter = ui.painter();

    painter.rect_filled(rect, Rounding::same(4.0), Color32::from_rgb(20, 20, 35));

    // Grid lines
    let grid_color = Color32::from_rgba_premultiplied(60, 60, 80, 80);
    for i in 1..4 {
        let y = rect.top() + rect.height() * (i as f32 / 4.0);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(0.5, grid_color),
        );
    }

    let max_val = data.iter().cloned().fold(1.0_f64, f64::max);
    let n = data.len();
    let margin = 4.0;
    let inner = Rect::from_min_max(
        egui::pos2(rect.left() + margin, rect.top() + margin),
        egui::pos2(rect.right() - margin, rect.bottom() - margin),
    );

    let points: Vec<egui::Pos2> = data
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = inner.left() + inner.width() * (i as f32 / (n.max(2) - 1) as f32);
            let y = inner.bottom() - inner.height() * (v as f32 / max_val as f32);
            egui::pos2(x, y)
        })
        .collect();

    // Fill area
    if points.len() >= 2 {
        let fill_color = Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 30);
        let mut fill_points = points.clone();
        fill_points.push(egui::pos2(inner.right(), inner.bottom()));
        fill_points.push(egui::pos2(inner.left(), inner.bottom()));
        painter.add(egui::Shape::convex_polygon(fill_points, fill_color, Stroke::NONE));
    }

    // Line
    if points.len() >= 2 {
        for w in points.windows(2) {
            painter.line_segment([w[0], w[1]], Stroke::new(2.0, color));
        }
    }

    // Y-axis label
    painter.text(
        egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        format!("{:.0}", max_val),
        egui::FontId::new(9.0, egui::FontFamily::Proportional),
        Color32::from_rgb(120, 120, 140),
    );
}

// ---------------------------------------------------------------------------
// Section header
// ---------------------------------------------------------------------------

pub fn section_header(ui: &mut Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).size(18.0).strong());
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
}

pub fn page_header_with_button(ui: &mut Ui, title: &str, button_label: &str) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.heading(title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(button_label).clicked() {
                clicked = true;
            }
        });
    });
    ui.separator();
    ui.add_space(4.0);
    clicked
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

pub fn empty_table(ui: &mut Ui, message: &str) {
    ui.add_space(32.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(message).size(14.0).color(Color32::from_rgb(108, 112, 134)));
    });
    ui.add_space(32.0);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_dark_color(c: Color32) -> bool {
    let lum = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    lum < 128.0
}

pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{:.0} B", bytes)
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1} KB", bytes / 1024.0)
    } else if bytes < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", bytes / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_ram(used_mb: u64, total_mb: u64) -> String {
    if total_mb > 0 {
        format!("{}/{} MB ({:.0}%)", used_mb, total_mb, used_mb as f64 / total_mb as f64 * 100.0)
    } else {
        format!("{} MB", used_mb)
    }
}
