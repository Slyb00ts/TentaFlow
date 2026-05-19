// === File: addon/ui/specialized.rs — specialized primitives (Canvas/Sparkline/StackedBar/Heatmap/AccessMatrix/WeeklyScheduleGrid/VideoTile/WelcomeHero/StepProgress/ReqCard/DecisionRow/AlarmFeed/FpsCounter) + DrawCommand vocab ===

use serde::{Deserialize, Serialize};

use super::data_display::{ImageSource, TextAlign};
use super::theme::{Color, CursorStyle, IconName, Size};

// =============================================================================
// SpecializedComponent — domain-specific primitives surfaced by the UI audit.
// =============================================================================

/// Specialized primitives that cannot reasonably be expressed by the generic
/// layout/data_display/form/feedback/action categories: a generic 2D Canvas
/// (draw-command vocab), per-type charts (Sparkline/StackedBar/Heatmap),
/// access/schedule grids, live video tiles with overlays, onboarding/wizard
/// chrome (WelcomeHero/StepProgress/ReqCard/DecisionRow) and stream-bound
/// widgets (AlarmFeed/FpsCounter).
///
/// JSON tag uses snake_case names; none collide with other categories.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpecializedComponent {
    /// Generic 2D canvas — addon emits draw commands, renderer rasterizes
    /// (HTML Canvas2D today, WGPU widget tomorrow). Pointer events are
    /// dispatched via `on_pointer` with `{ x, y, action, button }` params.
    Canvas {
        width: Size,
        height: Size,
        commands: Vec<DrawCommand>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<Color>,
        #[serde(default = "default_cursor_style")]
        cursor: CursorStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_pointer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_pointer_throttle_ms: Option<u32>,
    },
    /// Trend line — axis-less micro chart used inside stat cards.
    Sparkline {
        points: Vec<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        #[serde(default)]
        fill: bool,
        #[serde(default)]
        show_dots: bool,
    },
    /// Stacked bar chart (multi-series), horizontal or vertical.
    StackedBar {
        data: Vec<StackedBarItem>,
        #[serde(default)]
        orientation: StackedBarOrientation,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        colors: Vec<Color>,
        #[serde(default)]
        show_legend: bool,
        #[serde(default)]
        show_values: bool,
    },
    /// 2D heatmap (e.g. 24h × 7d access pattern, hourly fps map).
    Heatmap {
        rows: u32,
        cols: u32,
        values: Vec<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        row_labels: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        col_labels: Vec<String>,
        #[serde(default)]
        color_scale: ColorScale,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_cell_click: Option<String>,
        #[serde(default)]
        show_legend: bool,
    },
    /// Grid of allow/deny cells per role × per resource (M08b, M12b).
    AccessMatrix {
        roles: Vec<String>,
        resources: Vec<AccessResource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_toggle: Option<String>,
        #[serde(default)]
        readonly: bool,
    },
    /// Weekly schedule (24h × 7d) — heatmap intensity OR editable toggle grid.
    WeeklyScheduleGrid {
        values: Vec<Vec<f64>>,
        #[serde(default)]
        mode: ScheduleMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_cell_click: Option<String>,
        #[serde(default)]
        readonly: bool,
    },
    /// Live stream video tile. Thin wrapper around `ImageSource` +
    /// frame_storage with optional overlay annotations (bounding boxes,
    /// masks, labels). Distinct from `legacy::LiveCameraTile` (fMP4 stream).
    VideoTile {
        stream_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        camera_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height_px: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overlays: Vec<VideoOverlay>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_click: Option<String>,
        #[serde(default)]
        show_stats: bool,
    },
    /// Hero with gradient title — used by onboarding/install wizard intro.
    WelcomeHero {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent: Option<Color>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<super::UiComponent>,
    },
    /// Wizard step indicator (M13/M15).
    StepProgress {
        steps: Vec<StepInfo>,
        active_index: u32,
        #[serde(default)]
        orientation: StepOrientation,
    },
    /// Request grant card (M15 install wizard access step).
    ReqCard {
        addon_label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        addon_icon: Option<IconName>,
        permission: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default)]
        required: bool,
        #[serde(default)]
        public: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<GrantDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_accept: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_reject: Option<String>,
    },
    /// Decision row — accept/reject side by side (M15b).
    DecisionRow {
        id: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<GrantDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_accept: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_reject: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent: Option<Color>,
    },
    /// Live alarm feed — subscribes to a stream id and renders items
    /// reverse-chronologically.
    AlarmFeed {
        stream_id: String,
        #[serde(default = "default_alarm_feed_max_items")]
        max_items: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_item_click: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height_px: Option<u32>,
    },
    /// Live FPS / metric ticker. Subscribes to a stream and renders the
    /// current value plus an optional sparkline of recent history.
    FpsCounter {
        stream_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default)]
        show_sparkline: bool,
    },
}

fn default_cursor_style() -> CursorStyle {
    CursorStyle::Default
}

fn default_alarm_feed_max_items() -> u32 {
    50
}

// =============================================================================
// DrawCommand — vocabulary for Canvas
// =============================================================================

/// One draw operation queued on a `Canvas`. The vocabulary is intentionally
/// thin: anything more elaborate is composed from primitives. Strokes and
/// fills default to renderer-picked values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrawCommand {
    Line {
        from: Point,
        to: Point,
        #[serde(default = "default_draw_color")]
        color: Color,
        #[serde(default = "default_stroke_width")]
        width: f32,
    },
    Polygon {
        points: Vec<Point>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<StrokeSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default)]
        closed: bool,
    },
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<StrokeSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
        #[serde(default = "default_zero_radius")]
        corner_radius: f32,
    },
    Circle {
        center: Point,
        radius: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke: Option<StrokeSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Color>,
    },
    Text {
        pos: Point,
        text: String,
        #[serde(default = "default_draw_color")]
        color: Color,
        #[serde(default = "default_text_size")]
        size_px: f32,
        #[serde(default)]
        align: TextAlign,
    },
    Image {
        rect: Rect,
        source: ImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f32>,
    },
}

fn default_draw_color() -> Color {
    Color::Text
}
fn default_stroke_width() -> f32 {
    1.0
}
fn default_zero_radius() -> f32 {
    0.0
}
fn default_text_size() -> f32 {
    14.0
}

/// 2D coordinate in canvas units (caller-defined — renderer treats them as
/// device pixels by default).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Axis-aligned rectangle (x, y are top-left).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Stroke specification — colour + width + optional dash pattern. `dash`
/// entries are alternating on/off lengths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeSpec {
    pub color: Color,
    #[serde(default = "default_stroke_spec_width")]
    pub width: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dash: Option<Vec<f32>>,
}

fn default_stroke_spec_width() -> f32 {
    1.0
}

// =============================================================================
// Supporting types
// =============================================================================

/// One row of a stacked bar chart. `values` length must equal the number of
/// series (validated for consistency across rows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackedBarItem {
    pub label: String,
    pub values: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StackedBarOrientation {
    #[default]
    Horizontal,
    Vertical,
}

/// Heatmap colour scale — semantic name; renderer picks the gradient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorScale {
    #[default]
    Sequential,
    Diverging,
    Categorical,
    Heat,
}

/// One resource row inside an `AccessMatrix`. Each `permissions[]` entry
/// pairs a role label with an allow/deny flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessResource {
    pub id: String,
    pub label: String,
    pub permissions: Vec<AccessPermission>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessPermission {
    pub role: String,
    pub granted: bool,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleMode {
    #[default]
    Heatmap,
    Toggle,
}

/// One overlay annotation drawn on top of a `VideoTile`. `rect` is in
/// renderer-defined units (normalised 0..=1 or pixels — the renderer is
/// authoritative; addons should be consistent within one tile).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoOverlay {
    pub kind: VideoOverlayKind,
    pub rect: Rect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoOverlayKind {
    BoundingBox,
    Mask,
    Pose,
    Label,
}

/// One step in a `StepProgress` indicator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepInfo {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub status: StepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    Active,
    Done,
    Error,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepOrientation {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantDecision {
    Accepted,
    Rejected,
}

// =============================================================================
// Validation
// =============================================================================

/// Hard sanity cap for Canvas command lists — bounds memory and renderer
/// work without making the limit feel close in practice.
const CANVAS_MAX_COMMANDS: usize = 100_000;
const POLYGON_MIN_POINTS: usize = 3;
const POLYGON_MAX_POINTS: usize = 1_000;
const SPARKLINE_MIN_POINTS: usize = 2;
const SPARKLINE_MAX_POINTS: usize = 10_000;
const VIDEO_TILE_MIN_HEIGHT: u32 = 100;
const VIDEO_TILE_MAX_HEIGHT: u32 = 2160;
const ALARM_FEED_MIN_ITEMS: u32 = 1;
const ALARM_FEED_MAX_ITEMS: u32 = 1_000;
const ALARM_FEED_MIN_HEIGHT: u32 = 100;
const ALARM_FEED_MAX_HEIGHT: u32 = 2_000;
const SCHEDULE_DAYS: usize = 7;
const SCHEDULE_HOURS: usize = 24;

/// Validate and normalise a `SpecializedComponent`. Recurses into
/// `WelcomeHero.actions` through the central recursive validator. Error
/// reasons are static strings — they never echo addon input.
pub fn validate_and_normalize(
    component: &mut SpecializedComponent,
) -> Result<(), &'static str> {
    use SpecializedComponent::*;
    match component {
        Canvas { commands, .. } => {
            if commands.len() > CANVAS_MAX_COMMANDS {
                return Err("canvas_too_many_commands");
            }
            for cmd in commands.iter() {
                validate_draw_command(cmd)?;
            }
            Ok(())
        }
        Sparkline { points, .. } => {
            if points.len() < SPARKLINE_MIN_POINTS {
                return Err("sparkline_too_few_points");
            }
            if points.len() > SPARKLINE_MAX_POINTS {
                return Err("sparkline_too_many_points");
            }
            Ok(())
        }
        StackedBar { data, colors, .. } => {
            if data.is_empty() {
                return Err("stacked_bar_no_data");
            }
            let series_count = data[0].values.len();
            for item in data.iter() {
                if item.values.len() != series_count {
                    return Err("stacked_bar_inconsistent_series");
                }
            }
            if !colors.is_empty() && colors.len() < series_count {
                return Err("stacked_bar_colors_too_few");
            }
            Ok(())
        }
        Heatmap {
            rows,
            cols,
            values,
            row_labels,
            col_labels,
            ..
        } => {
            if *rows == 0 || *cols == 0 {
                return Err("heatmap_invalid_dimensions");
            }
            if values.len() as u32 != *rows {
                return Err("heatmap_row_count_mismatch");
            }
            for row in values.iter() {
                if row.len() as u32 != *cols {
                    return Err("heatmap_col_count_mismatch");
                }
            }
            if !row_labels.is_empty() && row_labels.len() as u32 != *rows {
                return Err("heatmap_row_labels_mismatch");
            }
            if !col_labels.is_empty() && col_labels.len() as u32 != *cols {
                return Err("heatmap_col_labels_mismatch");
            }
            Ok(())
        }
        AccessMatrix {
            roles, resources, ..
        } => {
            if roles.is_empty() {
                return Err("access_matrix_no_roles");
            }
            let mut seen_ids: Vec<&str> = Vec::with_capacity(resources.len());
            for res in resources.iter() {
                if seen_ids.iter().any(|s| *s == res.id.as_str()) {
                    return Err("access_matrix_duplicate_resource_id");
                }
                seen_ids.push(res.id.as_str());
                for perm in res.permissions.iter() {
                    if !roles.iter().any(|r| r == &perm.role) {
                        return Err("access_matrix_unknown_role");
                    }
                }
            }
            Ok(())
        }
        WeeklyScheduleGrid { values, .. } => {
            if values.len() != SCHEDULE_DAYS {
                return Err("schedule_must_have_7_days");
            }
            for day in values.iter() {
                if day.len() != SCHEDULE_HOURS {
                    return Err("schedule_must_have_24_hours");
                }
            }
            Ok(())
        }
        VideoTile {
            stream_id,
            camera_id,
            height_px,
            ..
        } => {
            if !is_valid_stream_id(stream_id) {
                return Err("video_tile_invalid_stream_id");
            }
            if let Some(cam) = camera_id {
                super::legacy::validate_camera_id(cam)
                    .map_err(|_| "video_tile_invalid_camera_id")?;
            }
            if let Some(h) = height_px {
                if *h < VIDEO_TILE_MIN_HEIGHT || *h > VIDEO_TILE_MAX_HEIGHT {
                    return Err("video_tile_invalid_height");
                }
            }
            Ok(())
        }
        WelcomeHero { title, actions, .. } => {
            if title.is_empty() {
                return Err("welcome_hero_title_empty");
            }
            for a in actions.iter_mut() {
                super::reject_overlay_kind_in_root(a)
                    .map_err(|_| "welcome_hero_action_invalid")?;
                super::validate_and_normalize_component(a)
                    .map_err(|_| "welcome_hero_action_invalid")?;
            }
            Ok(())
        }
        StepProgress {
            steps,
            active_index,
            ..
        } => {
            if steps.is_empty() {
                return Err("step_progress_empty");
            }
            if (*active_index as usize) >= steps.len() {
                return Err("step_progress_active_out_of_range");
            }
            Ok(())
        }
        ReqCard {
            addon_label,
            permission,
            ..
        } => {
            if addon_label.is_empty() {
                return Err("req_card_label_empty");
            }
            if permission.is_empty() {
                return Err("req_card_permission_empty");
            }
            Ok(())
        }
        DecisionRow { id, label, .. } => {
            if id.is_empty() {
                return Err("decision_row_id_empty");
            }
            if label.is_empty() {
                return Err("decision_row_label_empty");
            }
            Ok(())
        }
        AlarmFeed {
            stream_id,
            max_items,
            height_px,
            ..
        } => {
            if !is_valid_stream_id(stream_id) {
                return Err("alarm_feed_invalid_stream_id");
            }
            if *max_items < ALARM_FEED_MIN_ITEMS || *max_items > ALARM_FEED_MAX_ITEMS {
                return Err("alarm_feed_max_items_out_of_range");
            }
            if let Some(h) = height_px {
                if *h < ALARM_FEED_MIN_HEIGHT || *h > ALARM_FEED_MAX_HEIGHT {
                    return Err("alarm_feed_invalid_height");
                }
            }
            Ok(())
        }
        FpsCounter { stream_id, .. } => {
            if !is_valid_stream_id(stream_id) {
                return Err("fps_counter_invalid_stream_id");
            }
            Ok(())
        }
    }
}

fn validate_draw_command(cmd: &DrawCommand) -> Result<(), &'static str> {
    match cmd {
        DrawCommand::Line { width, .. } => {
            if *width <= 0.0 || !width.is_finite() {
                return Err("stroke_invalid_width");
            }
            Ok(())
        }
        DrawCommand::Polygon {
            points,
            stroke,
            ..
        } => {
            if points.len() < POLYGON_MIN_POINTS {
                return Err("polygon_too_few_points");
            }
            if points.len() > POLYGON_MAX_POINTS {
                return Err("polygon_too_many_points");
            }
            if let Some(s) = stroke {
                validate_stroke_spec(s)?;
            }
            Ok(())
        }
        DrawCommand::Rect {
            width,
            height,
            corner_radius,
            stroke,
            ..
        } => {
            if !(*width > 0.0 && width.is_finite() && *height > 0.0 && height.is_finite()) {
                return Err("rect_invalid_dimensions");
            }
            if *corner_radius < 0.0 || !corner_radius.is_finite() {
                return Err("rect_negative_corner_radius");
            }
            if let Some(s) = stroke {
                validate_stroke_spec(s)?;
            }
            Ok(())
        }
        DrawCommand::Circle { radius, stroke, .. } => {
            if !(*radius > 0.0 && radius.is_finite()) {
                return Err("circle_invalid_radius");
            }
            if let Some(s) = stroke {
                validate_stroke_spec(s)?;
            }
            Ok(())
        }
        DrawCommand::Text { size_px, .. } => {
            if !(*size_px > 0.0 && size_px.is_finite()) {
                return Err("text_invalid_size");
            }
            Ok(())
        }
        DrawCommand::Image { rect, .. } => {
            if !(rect.width > 0.0
                && rect.height > 0.0
                && rect.width.is_finite()
                && rect.height.is_finite())
            {
                return Err("image_invalid_rect");
            }
            Ok(())
        }
    }
}

fn validate_stroke_spec(spec: &StrokeSpec) -> Result<(), &'static str> {
    if !(spec.width > 0.0 && spec.width.is_finite()) {
        return Err("stroke_invalid_width");
    }
    if let Some(dash) = &spec.dash {
        for v in dash.iter() {
            if !(*v > 0.0 && v.is_finite()) {
                return Err("stroke_dash_invalid");
            }
        }
    }
    Ok(())
}

/// Stream id contract: `<namespace>:<id>` where namespace is non-empty
/// lower-snake alpha and id is non-empty. Mirrors the shape used by
/// `stream_hub` (e.g. `camera:cam_<uuid>`, `alarms:org-<uuid>`,
/// `fps:cam_<uuid>`).
fn is_valid_stream_id(id: &str) -> bool {
    let Some(colon) = id.find(':') else {
        return false;
    };
    let (ns, rest) = id.split_at(colon);
    let after = &rest[1..];
    if ns.is_empty() || after.is_empty() {
        return false;
    }
    ns.bytes()
        .all(|b| b.is_ascii_lowercase() || b == b'_')
        && after
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::ui::action::ActionComponent;
    use crate::addon::ui::UiComponent;

    fn good_cam_id() -> String {
        "cam_550e8400-e29b-41d4-a716-446655440000".to_string()
    }

    fn good_stream_id() -> String {
        format!("camera:{}", good_cam_id())
    }

    // ---- Canvas + DrawCommand round-trips ----

    #[test]
    fn canvas_round_trip_empty_commands() {
        let c = SpecializedComponent::Canvas {
            width: Size::Fill,
            height: Size::Fr { value: 1 },
            commands: vec![],
            background: Some(Color::BgElevated),
            cursor: CursorStyle::Crosshair,
            on_pointer: Some("a.draw".into()),
            on_pointer_throttle_ms: Some(50),
        };
        let j = serde_json::to_value(&c).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn canvas_cursor_defaults_to_default_style() {
        let json = serde_json::json!({
            "type": "canvas",
            "width": { "kind": "fill" },
            "height": { "kind": "fill" },
            "commands": []
        });
        let back: SpecializedComponent = serde_json::from_value(json).unwrap();
        match back {
            SpecializedComponent::Canvas { cursor, .. } => {
                assert_eq!(cursor, CursorStyle::Default);
            }
            _ => panic!("expected canvas"),
        }
    }

    #[test]
    fn draw_command_line_round_trip() {
        let cmd = DrawCommand::Line {
            from: Point { x: 0.0, y: 0.0 },
            to: Point { x: 10.0, y: 10.0 },
            color: Color::Primary,
            width: 2.0,
        };
        let j = serde_json::to_value(&cmd).unwrap();
        let back: DrawCommand = serde_json::from_value(j).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn draw_command_polygon_round_trip() {
        let cmd = DrawCommand::Polygon {
            points: vec![
                Point { x: 0.0, y: 0.0 },
                Point { x: 1.0, y: 0.0 },
                Point { x: 0.5, y: 1.0 },
            ],
            stroke: Some(StrokeSpec {
                color: Color::Accent,
                width: 1.5,
                dash: Some(vec![4.0, 2.0]),
            }),
            fill: Some(Color::BgSurface),
            closed: true,
        };
        let j = serde_json::to_value(&cmd).unwrap();
        let back: DrawCommand = serde_json::from_value(j).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn draw_command_rect_round_trip() {
        let cmd = DrawCommand::Rect {
            x: 5.0,
            y: 5.0,
            width: 20.0,
            height: 10.0,
            stroke: None,
            fill: Some(Color::Primary),
            corner_radius: 4.0,
        };
        let j = serde_json::to_value(&cmd).unwrap();
        let back: DrawCommand = serde_json::from_value(j).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn draw_command_circle_round_trip() {
        let cmd = DrawCommand::Circle {
            center: Point { x: 0.0, y: 0.0 },
            radius: 3.0,
            stroke: Some(StrokeSpec {
                color: Color::Danger,
                width: 1.0,
                dash: None,
            }),
            fill: None,
        };
        let j = serde_json::to_value(&cmd).unwrap();
        let back: DrawCommand = serde_json::from_value(j).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn draw_command_text_round_trip() {
        let cmd = DrawCommand::Text {
            pos: Point { x: 1.0, y: 2.0 },
            text: "hi".into(),
            color: Color::Text,
            size_px: 12.0,
            align: TextAlign::Center,
        };
        let j = serde_json::to_value(&cmd).unwrap();
        let back: DrawCommand = serde_json::from_value(j).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn draw_command_image_round_trip() {
        let cmd = DrawCommand::Image {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            source: ImageSource::Placeholder,
            opacity: Some(0.5),
        };
        let j = serde_json::to_value(&cmd).unwrap();
        let back: DrawCommand = serde_json::from_value(j).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn canvas_rejects_too_many_commands() {
        let mut cmds = Vec::with_capacity(CANVAS_MAX_COMMANDS + 1);
        for _ in 0..CANVAS_MAX_COMMANDS + 1 {
            cmds.push(DrawCommand::Line {
                from: Point { x: 0.0, y: 0.0 },
                to: Point { x: 1.0, y: 1.0 },
                color: Color::Text,
                width: 1.0,
            });
        }
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: cmds,
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "canvas_too_many_commands"
        );
    }

    #[test]
    fn canvas_rejects_polygon_too_few_points() {
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: vec![DrawCommand::Polygon {
                points: vec![Point { x: 0.0, y: 0.0 }, Point { x: 1.0, y: 1.0 }],
                stroke: None,
                fill: None,
                closed: true,
            }],
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "polygon_too_few_points"
        );
    }

    #[test]
    fn canvas_rejects_polygon_too_many_points() {
        let mut points = Vec::with_capacity(POLYGON_MAX_POINTS + 1);
        for _ in 0..POLYGON_MAX_POINTS + 1 {
            points.push(Point { x: 0.0, y: 0.0 });
        }
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: vec![DrawCommand::Polygon {
                points,
                stroke: None,
                fill: None,
                closed: true,
            }],
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "polygon_too_many_points"
        );
    }

    #[test]
    fn canvas_rejects_rect_invalid_dimensions() {
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: vec![DrawCommand::Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 10.0,
                stroke: None,
                fill: None,
                corner_radius: 0.0,
            }],
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "rect_invalid_dimensions"
        );
    }

    #[test]
    fn canvas_rejects_rect_negative_corner_radius() {
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: vec![DrawCommand::Rect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                stroke: None,
                fill: None,
                corner_radius: -1.0,
            }],
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "rect_negative_corner_radius"
        );
    }

    #[test]
    fn canvas_rejects_circle_invalid_radius() {
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: vec![DrawCommand::Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.0,
                stroke: None,
                fill: None,
            }],
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "circle_invalid_radius"
        );
    }

    #[test]
    fn canvas_rejects_stroke_invalid_width() {
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: vec![DrawCommand::Line {
                from: Point { x: 0.0, y: 0.0 },
                to: Point { x: 1.0, y: 1.0 },
                color: Color::Text,
                width: 0.0,
            }],
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "stroke_invalid_width"
        );
    }

    #[test]
    fn canvas_rejects_stroke_dash_invalid() {
        let mut c = SpecializedComponent::Canvas {
            width: Size::Auto,
            height: Size::Auto,
            commands: vec![DrawCommand::Polygon {
                points: vec![
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 1.0, y: 0.0 },
                    Point { x: 0.5, y: 1.0 },
                ],
                stroke: Some(StrokeSpec {
                    color: Color::Text,
                    width: 1.0,
                    dash: Some(vec![4.0, 0.0]),
                }),
                fill: None,
                closed: true,
            }],
            background: None,
            cursor: CursorStyle::Default,
            on_pointer: None,
            on_pointer_throttle_ms: None,
        };
        assert_eq!(
            validate_and_normalize(&mut c).unwrap_err(),
            "stroke_dash_invalid"
        );
    }

    // ---- Sparkline ----

    #[test]
    fn sparkline_round_trip() {
        let s = SpecializedComponent::Sparkline {
            points: vec![1.0, 2.0, 3.0],
            color: Some(Color::Accent),
            height: Some(32),
            fill: true,
            show_dots: true,
        };
        let j = serde_json::to_value(&s).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn sparkline_rejects_too_few_points() {
        let mut s = SpecializedComponent::Sparkline {
            points: vec![1.0],
            color: None,
            height: None,
            fill: false,
            show_dots: false,
        };
        assert_eq!(
            validate_and_normalize(&mut s).unwrap_err(),
            "sparkline_too_few_points"
        );
    }

    #[test]
    fn sparkline_rejects_too_many_points() {
        let mut s = SpecializedComponent::Sparkline {
            points: vec![0.0; SPARKLINE_MAX_POINTS + 1],
            color: None,
            height: None,
            fill: false,
            show_dots: false,
        };
        assert_eq!(
            validate_and_normalize(&mut s).unwrap_err(),
            "sparkline_too_many_points"
        );
    }

    // ---- StackedBar ----

    #[test]
    fn stacked_bar_round_trip() {
        let sb = SpecializedComponent::StackedBar {
            data: vec![
                StackedBarItem {
                    label: "Mon".into(),
                    values: vec![1.0, 2.0, 3.0],
                    total: Some(6.0),
                },
                StackedBarItem {
                    label: "Tue".into(),
                    values: vec![4.0, 5.0, 6.0],
                    total: None,
                },
            ],
            orientation: StackedBarOrientation::Vertical,
            colors: vec![Color::Primary, Color::Accent, Color::Success],
            show_legend: true,
            show_values: false,
        };
        let j = serde_json::to_value(&sb).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, sb);
    }

    #[test]
    fn stacked_bar_rejects_empty_data() {
        let mut sb = SpecializedComponent::StackedBar {
            data: vec![],
            orientation: StackedBarOrientation::Horizontal,
            colors: vec![],
            show_legend: false,
            show_values: false,
        };
        assert_eq!(
            validate_and_normalize(&mut sb).unwrap_err(),
            "stacked_bar_no_data"
        );
    }

    #[test]
    fn stacked_bar_rejects_inconsistent_series() {
        let mut sb = SpecializedComponent::StackedBar {
            data: vec![
                StackedBarItem {
                    label: "A".into(),
                    values: vec![1.0, 2.0],
                    total: None,
                },
                StackedBarItem {
                    label: "B".into(),
                    values: vec![1.0],
                    total: None,
                },
            ],
            orientation: StackedBarOrientation::Horizontal,
            colors: vec![],
            show_legend: false,
            show_values: false,
        };
        assert_eq!(
            validate_and_normalize(&mut sb).unwrap_err(),
            "stacked_bar_inconsistent_series"
        );
    }

    #[test]
    fn stacked_bar_rejects_colors_too_few() {
        let mut sb = SpecializedComponent::StackedBar {
            data: vec![StackedBarItem {
                label: "A".into(),
                values: vec![1.0, 2.0, 3.0],
                total: None,
            }],
            orientation: StackedBarOrientation::Horizontal,
            colors: vec![Color::Primary],
            show_legend: false,
            show_values: false,
        };
        assert_eq!(
            validate_and_normalize(&mut sb).unwrap_err(),
            "stacked_bar_colors_too_few"
        );
    }

    // ---- Heatmap ----

    #[test]
    fn heatmap_round_trip() {
        let h = SpecializedComponent::Heatmap {
            rows: 2,
            cols: 3,
            values: vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]],
            row_labels: vec!["r1".into(), "r2".into()],
            col_labels: vec!["c1".into(), "c2".into(), "c3".into()],
            color_scale: ColorScale::Heat,
            on_cell_click: Some("on.cell".into()),
            show_legend: true,
        };
        let j = serde_json::to_value(&h).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn heatmap_rejects_zero_dim() {
        let mut h = SpecializedComponent::Heatmap {
            rows: 0,
            cols: 3,
            values: vec![],
            row_labels: vec![],
            col_labels: vec![],
            color_scale: ColorScale::default(),
            on_cell_click: None,
            show_legend: false,
        };
        assert_eq!(
            validate_and_normalize(&mut h).unwrap_err(),
            "heatmap_invalid_dimensions"
        );
    }

    #[test]
    fn heatmap_rejects_row_count_mismatch() {
        let mut h = SpecializedComponent::Heatmap {
            rows: 3,
            cols: 1,
            values: vec![vec![0.0]],
            row_labels: vec![],
            col_labels: vec![],
            color_scale: ColorScale::default(),
            on_cell_click: None,
            show_legend: false,
        };
        assert_eq!(
            validate_and_normalize(&mut h).unwrap_err(),
            "heatmap_row_count_mismatch"
        );
    }

    #[test]
    fn heatmap_rejects_col_count_mismatch() {
        let mut h = SpecializedComponent::Heatmap {
            rows: 1,
            cols: 3,
            values: vec![vec![0.0, 0.1]],
            row_labels: vec![],
            col_labels: vec![],
            color_scale: ColorScale::default(),
            on_cell_click: None,
            show_legend: false,
        };
        assert_eq!(
            validate_and_normalize(&mut h).unwrap_err(),
            "heatmap_col_count_mismatch"
        );
    }

    // ---- AccessMatrix ----

    #[test]
    fn access_matrix_round_trip() {
        let am = SpecializedComponent::AccessMatrix {
            roles: vec!["admin".into(), "viewer".into()],
            resources: vec![AccessResource {
                id: "cameras".into(),
                label: "Cameras".into(),
                permissions: vec![
                    AccessPermission {
                        role: "admin".into(),
                        granted: true,
                        disabled: false,
                    },
                    AccessPermission {
                        role: "viewer".into(),
                        granted: false,
                        disabled: true,
                    },
                ],
            }],
            on_toggle: Some("am.tog".into()),
            readonly: false,
        };
        let j = serde_json::to_value(&am).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, am);
    }

    #[test]
    fn access_matrix_rejects_no_roles() {
        let mut am = SpecializedComponent::AccessMatrix {
            roles: vec![],
            resources: vec![],
            on_toggle: None,
            readonly: false,
        };
        assert_eq!(
            validate_and_normalize(&mut am).unwrap_err(),
            "access_matrix_no_roles"
        );
    }

    #[test]
    fn access_matrix_rejects_duplicate_resource_id() {
        let mut am = SpecializedComponent::AccessMatrix {
            roles: vec!["admin".into()],
            resources: vec![
                AccessResource {
                    id: "x".into(),
                    label: "X".into(),
                    permissions: vec![],
                },
                AccessResource {
                    id: "x".into(),
                    label: "X2".into(),
                    permissions: vec![],
                },
            ],
            on_toggle: None,
            readonly: false,
        };
        assert_eq!(
            validate_and_normalize(&mut am).unwrap_err(),
            "access_matrix_duplicate_resource_id"
        );
    }

    #[test]
    fn access_matrix_rejects_unknown_role() {
        let mut am = SpecializedComponent::AccessMatrix {
            roles: vec!["admin".into()],
            resources: vec![AccessResource {
                id: "x".into(),
                label: "X".into(),
                permissions: vec![AccessPermission {
                    role: "ghost".into(),
                    granted: true,
                    disabled: false,
                }],
            }],
            on_toggle: None,
            readonly: false,
        };
        assert_eq!(
            validate_and_normalize(&mut am).unwrap_err(),
            "access_matrix_unknown_role"
        );
    }

    // ---- WeeklyScheduleGrid ----

    #[test]
    fn weekly_schedule_round_trip() {
        let values: Vec<Vec<f64>> = (0..7).map(|_| (0..24).map(|h| h as f64 / 23.0).collect()).collect();
        let g = SpecializedComponent::WeeklyScheduleGrid {
            values,
            mode: ScheduleMode::Toggle,
            accent: Some(Color::Primary),
            on_cell_click: Some("g.click".into()),
            readonly: false,
        };
        let j = serde_json::to_value(&g).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn weekly_schedule_rejects_wrong_day_count() {
        let mut g = SpecializedComponent::WeeklyScheduleGrid {
            values: vec![vec![0.0; 24]; 6],
            mode: ScheduleMode::default(),
            accent: None,
            on_cell_click: None,
            readonly: false,
        };
        assert_eq!(
            validate_and_normalize(&mut g).unwrap_err(),
            "schedule_must_have_7_days"
        );
    }

    #[test]
    fn weekly_schedule_rejects_wrong_hour_count() {
        let mut values = vec![vec![0.0; 24]; 7];
        values[3] = vec![0.0; 12];
        let mut g = SpecializedComponent::WeeklyScheduleGrid {
            values,
            mode: ScheduleMode::default(),
            accent: None,
            on_cell_click: None,
            readonly: false,
        };
        assert_eq!(
            validate_and_normalize(&mut g).unwrap_err(),
            "schedule_must_have_24_hours"
        );
    }

    // ---- VideoTile ----

    #[test]
    fn video_tile_round_trip() {
        let v = SpecializedComponent::VideoTile {
            stream_id: good_stream_id(),
            camera_id: Some(good_cam_id()),
            label: Some("Entrance".into()),
            height_px: Some(320),
            overlays: vec![VideoOverlay {
                kind: VideoOverlayKind::BoundingBox,
                rect: Rect {
                    x: 0.1,
                    y: 0.1,
                    width: 0.2,
                    height: 0.3,
                },
                label: Some("person".into()),
                confidence: Some(0.92),
                color: Some(Color::Success),
            }],
            on_click: Some("v.click".into()),
            show_stats: true,
        };
        let j = serde_json::to_value(&v).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn video_tile_rejects_bad_stream_id() {
        let mut v = SpecializedComponent::VideoTile {
            stream_id: "no_colon".into(),
            camera_id: None,
            label: None,
            height_px: None,
            overlays: vec![],
            on_click: None,
            show_stats: false,
        };
        assert_eq!(
            validate_and_normalize(&mut v).unwrap_err(),
            "video_tile_invalid_stream_id"
        );
    }

    #[test]
    fn video_tile_rejects_bad_camera_id() {
        let mut v = SpecializedComponent::VideoTile {
            stream_id: good_stream_id(),
            camera_id: Some("../bad".into()),
            label: None,
            height_px: None,
            overlays: vec![],
            on_click: None,
            show_stats: false,
        };
        assert_eq!(
            validate_and_normalize(&mut v).unwrap_err(),
            "video_tile_invalid_camera_id"
        );
    }

    #[test]
    fn video_tile_rejects_out_of_range_height() {
        let mut v = SpecializedComponent::VideoTile {
            stream_id: good_stream_id(),
            camera_id: None,
            label: None,
            height_px: Some(50),
            overlays: vec![],
            on_click: None,
            show_stats: false,
        };
        assert_eq!(
            validate_and_normalize(&mut v).unwrap_err(),
            "video_tile_invalid_height"
        );
    }

    // ---- WelcomeHero ----

    #[test]
    fn welcome_hero_round_trip_and_action_recursion() {
        let h = SpecializedComponent::WelcomeHero {
            title: "Witaj".into(),
            subtitle: Some("Start tutaj".into()),
            icon: Some(IconName::Home),
            accent: Some(Color::Primary),
            actions: vec![UiComponent::Action(ActionComponent::Button {
                label: "Dalej".into(),
                variant: Default::default(),
                size: Default::default(),
                icon: None,
                icon_position: Default::default(),
                disabled: false,
                loading: false,
                full_width: false,
                on_click: Some("hero.next".into()),
                params: None,
                tooltip: None,
            })],
        };
        let j = serde_json::to_value(&h).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, h);

        let mut hh = h;
        validate_and_normalize(&mut hh).unwrap();
    }

    #[test]
    fn welcome_hero_rejects_empty_title() {
        let mut h = SpecializedComponent::WelcomeHero {
            title: "".into(),
            subtitle: None,
            icon: None,
            accent: None,
            actions: vec![],
        };
        assert_eq!(
            validate_and_normalize(&mut h).unwrap_err(),
            "welcome_hero_title_empty"
        );
    }

    #[test]
    fn welcome_hero_rejects_action_with_bad_button_label() {
        let mut h = SpecializedComponent::WelcomeHero {
            title: "ok".into(),
            subtitle: None,
            icon: None,
            accent: None,
            actions: vec![UiComponent::Action(ActionComponent::Button {
                label: "".into(),
                variant: Default::default(),
                size: Default::default(),
                icon: None,
                icon_position: Default::default(),
                disabled: false,
                loading: false,
                full_width: false,
                on_click: None,
                params: None,
                tooltip: None,
            })],
        };
        assert_eq!(
            validate_and_normalize(&mut h).unwrap_err(),
            "welcome_hero_action_invalid"
        );
    }

    // ---- StepProgress ----

    #[test]
    fn step_progress_round_trip() {
        let s = SpecializedComponent::StepProgress {
            steps: vec![
                StepInfo {
                    label: "S1".into(),
                    description: Some("d1".into()),
                    status: StepStatus::Done,
                },
                StepInfo {
                    label: "S2".into(),
                    description: None,
                    status: StepStatus::Active,
                },
            ],
            active_index: 1,
            orientation: StepOrientation::Vertical,
        };
        let j = serde_json::to_value(&s).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn step_progress_rejects_empty() {
        let mut s = SpecializedComponent::StepProgress {
            steps: vec![],
            active_index: 0,
            orientation: StepOrientation::default(),
        };
        assert_eq!(
            validate_and_normalize(&mut s).unwrap_err(),
            "step_progress_empty"
        );
    }

    #[test]
    fn step_progress_rejects_out_of_range_active() {
        let mut s = SpecializedComponent::StepProgress {
            steps: vec![StepInfo {
                label: "x".into(),
                description: None,
                status: StepStatus::default(),
            }],
            active_index: 5,
            orientation: StepOrientation::default(),
        };
        assert_eq!(
            validate_and_normalize(&mut s).unwrap_err(),
            "step_progress_active_out_of_range"
        );
    }

    // ---- ReqCard ----

    #[test]
    fn req_card_round_trip() {
        let r = SpecializedComponent::ReqCard {
            addon_label: "Eureka".into(),
            addon_icon: Some(IconName::Document),
            permission: "http.request".into(),
            description: Some("Pobiera dane z MF".into()),
            required: true,
            public: false,
            decision: Some(GrantDecision::Accepted),
            on_accept: Some("a".into()),
            on_reject: Some("r".into()),
        };
        let j = serde_json::to_value(&r).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn req_card_rejects_empty_label() {
        let mut r = SpecializedComponent::ReqCard {
            addon_label: "".into(),
            addon_icon: None,
            permission: "p".into(),
            description: None,
            required: false,
            public: false,
            decision: None,
            on_accept: None,
            on_reject: None,
        };
        assert_eq!(
            validate_and_normalize(&mut r).unwrap_err(),
            "req_card_label_empty"
        );
    }

    #[test]
    fn req_card_rejects_empty_permission() {
        let mut r = SpecializedComponent::ReqCard {
            addon_label: "x".into(),
            addon_icon: None,
            permission: "".into(),
            description: None,
            required: false,
            public: false,
            decision: None,
            on_accept: None,
            on_reject: None,
        };
        assert_eq!(
            validate_and_normalize(&mut r).unwrap_err(),
            "req_card_permission_empty"
        );
    }

    // ---- DecisionRow ----

    #[test]
    fn decision_row_round_trip() {
        let d = SpecializedComponent::DecisionRow {
            id: "row1".into(),
            label: "Akceptuj".into(),
            description: Some("opis".into()),
            decision: Some(GrantDecision::Rejected),
            on_accept: Some("a".into()),
            on_reject: Some("r".into()),
            accent: Some(Color::Warning),
        };
        let j = serde_json::to_value(&d).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn decision_row_rejects_empty_id_and_label() {
        let mut d = SpecializedComponent::DecisionRow {
            id: "".into(),
            label: "x".into(),
            description: None,
            decision: None,
            on_accept: None,
            on_reject: None,
            accent: None,
        };
        assert_eq!(
            validate_and_normalize(&mut d).unwrap_err(),
            "decision_row_id_empty"
        );
        let mut d2 = SpecializedComponent::DecisionRow {
            id: "ok".into(),
            label: "".into(),
            description: None,
            decision: None,
            on_accept: None,
            on_reject: None,
            accent: None,
        };
        assert_eq!(
            validate_and_normalize(&mut d2).unwrap_err(),
            "decision_row_label_empty"
        );
    }

    // ---- AlarmFeed ----

    #[test]
    fn alarm_feed_round_trip() {
        let a = SpecializedComponent::AlarmFeed {
            stream_id: "alarms:org-12345".into(),
            max_items: 100,
            on_item_click: Some("a.click".into()),
            height_px: Some(400),
        };
        let j = serde_json::to_value(&a).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn alarm_feed_default_max_items_is_50() {
        let json = serde_json::json!({
            "type": "alarm_feed",
            "stream_id": "alarms:org-1"
        });
        let back: SpecializedComponent = serde_json::from_value(json).unwrap();
        match back {
            SpecializedComponent::AlarmFeed { max_items, .. } => assert_eq!(max_items, 50),
            _ => panic!("expected alarm_feed"),
        }
    }

    #[test]
    fn alarm_feed_rejects_bad_stream_id() {
        let mut a = SpecializedComponent::AlarmFeed {
            stream_id: "".into(),
            max_items: 10,
            on_item_click: None,
            height_px: None,
        };
        assert_eq!(
            validate_and_normalize(&mut a).unwrap_err(),
            "alarm_feed_invalid_stream_id"
        );
    }

    #[test]
    fn alarm_feed_rejects_out_of_range_max_items() {
        let mut a = SpecializedComponent::AlarmFeed {
            stream_id: "alarms:x".into(),
            max_items: 0,
            on_item_click: None,
            height_px: None,
        };
        assert_eq!(
            validate_and_normalize(&mut a).unwrap_err(),
            "alarm_feed_max_items_out_of_range"
        );
    }

    #[test]
    fn alarm_feed_rejects_invalid_height() {
        let mut a = SpecializedComponent::AlarmFeed {
            stream_id: "alarms:x".into(),
            max_items: 10,
            on_item_click: None,
            height_px: Some(50),
        };
        assert_eq!(
            validate_and_normalize(&mut a).unwrap_err(),
            "alarm_feed_invalid_height"
        );
    }

    // ---- FpsCounter ----

    #[test]
    fn fps_counter_round_trip() {
        let f = SpecializedComponent::FpsCounter {
            stream_id: format!("fps:{}", good_cam_id()),
            label: Some("FPS".into()),
            format: Some("{value:.1f}".into()),
            show_sparkline: true,
        };
        let j = serde_json::to_value(&f).unwrap();
        let back: SpecializedComponent = serde_json::from_value(j).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn fps_counter_rejects_bad_stream_id() {
        let mut f = SpecializedComponent::FpsCounter {
            stream_id: "bad id with spaces".into(),
            label: None,
            format: None,
            show_sparkline: false,
        };
        assert_eq!(
            validate_and_normalize(&mut f).unwrap_err(),
            "fps_counter_invalid_stream_id"
        );
    }

    // ---- Stream id helper ----

    #[test]
    fn stream_id_helper_accepts_valid_shapes() {
        assert!(is_valid_stream_id("camera:cam_abc"));
        assert!(is_valid_stream_id("alarms:org-12345"));
        assert!(is_valid_stream_id("fps:cam_1"));
    }

    #[test]
    fn stream_id_helper_rejects_invalid_shapes() {
        assert!(!is_valid_stream_id(""));
        assert!(!is_valid_stream_id("noColon"));
        assert!(!is_valid_stream_id(":missing_ns"));
        assert!(!is_valid_stream_id("ns:"));
        assert!(!is_valid_stream_id("BAD:x"));
        assert!(!is_valid_stream_id("camera:bad id"));
    }
}
