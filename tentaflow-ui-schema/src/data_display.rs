// === File: addon/ui/data_display.rs — data display primitives (Text/Heading/Badge/Chip/Tag/Avatar/Image/Stat/KeyValue/List/BulletList/Timeline/Table/MonoBlock/CodeBlock/EmptyState) ===

use serde::{Deserialize, Serialize};

use super::theme::{Color, FontWeight, IconName, Radius, TextStyle};
use super::UiComponent;

// =============================================================================
// DataDisplayComponent — sub-enum for read-only / informational primitives
// =============================================================================

/// Read-only primitives that present data to the user: typography, badges,
/// chips, avatars, images, stats, key-value listings, lists, timelines,
/// tables (v2 with typed columns + cursor pagination) and code blocks.
///
/// Embedding rules: variants that contain `UiComponent` children (Table cell
/// `Component`, KeyValueItem `Component`, EmptyState actions) MUST NOT host
/// overlay-kind containers (Window/Drawer/Popover). Validator enforces this
/// by recursing through `super::validate_and_normalize_component` which
/// already rejects nested overlays via `reject_overlay_kind_in_root`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataDisplayComponent {
    #[serde(rename = "text_v2")]
    Text {
        content: String,
        #[serde(default = "default_text_style")]
        style: TextStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Color>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<FontWeight>,
        #[serde(default)]
        align: TextAlign,
        #[serde(default)]
        truncate: bool,
    },
    Heading {
        content: String,
        level: HeadingLevel,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
    },
    #[serde(rename = "badge_v2")]
    Badge {
        label: String,
        #[serde(default)]
        tone: BadgeTone,
        #[serde(default)]
        size: BadgeSize,
    },
    Chip {
        label: String,
        kind: ChipKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default)]
        dismissible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_dismiss: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_click: Option<String>,
    },
    Tag {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Color>,
    },
    Avatar {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_source: Option<ImageSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initials: Option<String>,
        #[serde(default)]
        size: AvatarSize,
        #[serde(default)]
        shape: AvatarShape,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<AvatarStatus>,
    },
    #[serde(rename = "image_v2")]
    Image {
        source: ImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        #[serde(default = "default_image_radius")]
        radius: Radius,
        #[serde(default)]
        fit: ImageFit,
    },
    Stat {
        value: String,
        /// Optional small suffix appended after `value` (e.g. "/ 24" rendered
        /// at ~0.5em, muted) — used for "22 / 24" style KPIs where only the
        /// total slice should fade visually.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_suffix: Option<String>,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sublabel: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trend: Option<StatTrend>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        accent: Option<Color>,
    },
    KeyValue {
        items: Vec<KeyValueItem>,
        #[serde(default)]
        density: KeyValueDensity,
    },
    #[serde(rename = "list_v2")]
    List {
        items: Vec<ListItem>,
        #[serde(default)]
        marker: ListMarker,
        #[serde(default)]
        density: ListDensity,
    },
    BulletList {
        items: Vec<String>,
        #[serde(default)]
        style: BulletStyle,
    },
    Timeline {
        items: Vec<TimelineItem>,
        #[serde(default)]
        orientation: TimelineOrientation,
    },
    #[serde(rename = "table_v2")]
    Table {
        columns: Vec<TableColumn>,
        rows: Vec<TableRow>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pagination: Option<TablePagination>,
        #[serde(default)]
        expandable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_row_click: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_sort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_load_more: Option<String>,
        #[serde(default)]
        density: TableDensity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        empty_state: Option<String>,
    },
    MonoBlock {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    CodeBlock {
        segments: Vec<CodeSegment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default)]
        show_line_numbers: bool,
    },
    EmptyState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<UiComponent>,
    },
}

fn default_text_style() -> TextStyle {
    TextStyle::Body
}

fn default_image_radius() -> Radius {
    Radius::None
}

// =============================================================================
// Supporting enums
// =============================================================================

/// Inline text alignment. Reused by `Text` and `TableColumn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Start,
    Center,
    End,
    Justify,
}

/// Semantic heading level. Renderer maps to `TextStyle::Heading{1..4}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
    H4,
}

/// Tone of a small badge. Maps semantically to color roles in the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BadgeTone {
    #[default]
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

/// Visual size of a badge. `Md` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BadgeSize {
    Sm,
    #[default]
    Md,
}

/// Semantic chip kind — drives default tone/icon picked by the renderer.
/// Variants come from the UI audit (install wizard, ownership pills, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChipKind {
    Addon,
    Owner,
    Visibility,
    Required,
    Strategy,
    Status,
    Filter,
    Category,
}

/// Source for an avatar/image. `SignedFrame` references a camera frame by
/// `frame_ref` (signed URL handled by the renderer-side resolver).
/// `Initials` is a placeholder that the renderer draws as solid colour bg
/// + initials text. `Placeholder` is the default neutral fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    SignedFrame { camera_id: String, frame_ref: String },
    Initials { text: String, background: Color },
    Placeholder,
}

/// Avatar size token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AvatarSize {
    Sm,
    #[default]
    Md,
    Lg,
    Xl,
}

/// Avatar outline shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AvatarShape {
    #[default]
    Circle,
    Rounded,
    Square,
}

/// Presence dot on an avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvatarStatus {
    Online,
    Offline,
    Away,
    Busy,
}

/// CSS-like `object-fit` mode for `Image`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageFit {
    #[default]
    Cover,
    Contain,
    Fill,
    None,
}

/// Trend delta shown next to a metric. `delta` is pre-formatted by the addon
/// (e.g. `"+12%"`, `"-3.4"`); the renderer only picks the arrow + colour
/// based on `direction`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatTrend {
    pub direction: TrendDirection,
    pub delta: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<String>,
}

/// Direction of a trend delta. `Neutral` renders without an arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendDirection {
    Up,
    Down,
    Neutral,
}

/// Single row in a `KeyValue` listing. `value` may be a rich `CellValue` so
/// addons can embed a badge/chip/component on the right-hand side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyValueItem {
    pub key: String,
    pub value: CellValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
}

/// Vertical density of a `KeyValue` listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeyValueDensity {
    #[default]
    Normal,
    Compact,
}

/// Row in a structured `List`. Distinct from `BulletList` which only holds
/// bare strings — `ListItem` carries icon/title/subtitle/trailing/on_click.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing: Option<CellValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_click: Option<String>,
}

/// Marker glyph for a structured `List`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ListMarker {
    #[default]
    None,
    Bullet,
    Dash,
    Arrow,
    Check,
    Numbered,
}

/// Vertical density of a structured `List`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ListDensity {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

/// Glyph for a `BulletList`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BulletStyle {
    #[default]
    Disc,
    Dash,
    Arrow,
    Check,
}

/// Entry in a chronological `Timeline`. `timestamp` may be ISO 8601 or a
/// pre-formatted relative string — renderer does not parse it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub timestamp: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Color>,
}

/// Orientation of a `Timeline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimelineOrientation {
    #[default]
    Vertical,
    Horizontal,
}

/// Column declaration for `Table`. `id` is unique within the table; `width`
/// is a px hint that the renderer may ignore in favour of content-driven
/// sizing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableColumn {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default)]
    pub sortable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconName>,
}

/// Row in `Table`. `cells.len()` MUST equal `columns.len()`; `expanded_content`
/// is only rendered when `Table.expandable=true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRow {
    pub id: String,
    pub cells: Vec<CellValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expanded_content: Vec<UiComponent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Color>,
}

/// Typed table cell. `Component` is the escape hatch for arbitrary UI inside
/// a cell — overlays are rejected at validation time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cell", rename_all = "snake_case")]
pub enum CellValue {
    Text {
        value: String,
    },
    Number {
        value: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    Boolean {
        value: bool,
    },
    Date {
        value: String,
    },
    Badge {
        tone: BadgeTone,
        label: String,
    },
    Chip {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ChipKind>,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<IconName>,
    },
    Component {
        value: Box<UiComponent>,
    },
    Empty,
}

/// Table pagination configuration. Use `Cursor` for large datasets and
/// `Pages` for fixed-size browsable tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TablePagination {
    #[serde(flatten)]
    pub mode: PaginationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
}

/// Pagination strategy. `Pages` carries explicit page numbers; `Cursor`
/// is for incremental "load more" / infinite-scroll backends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaginationMode {
    Pages {
        current_page: u32,
        total_pages: u32,
        on_page_change: String,
    },
    Cursor {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_cursor: Option<String>,
    },
}

/// Vertical density of a `Table`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TableDensity {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

/// One token in a `CodeBlock`. The renderer picks colours from the active
/// theme based on `kind` — addon never sends hex.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeSegment {
    pub text: String,
    #[serde(default)]
    pub kind: CodeSegmentKind,
}

/// Syntax-highlight class of a `CodeSegment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CodeSegmentKind {
    #[default]
    Plain,
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Type,
    Operator,
}

// =============================================================================
// Validation
// =============================================================================

const AVATAR_INITIALS_MAX_LEN: usize = 3;
const STAT_TREND_DELTA_MAX_LEN: usize = 16;
const CAMERA_ID_LEN: usize = 40;
const CAMERA_ID_PREFIX: &str = "cam_";

/// Validate a single data-display component, recursing into embedded
/// children (`Component` cells, `KeyValueItem::Component`, `Table` expanded
/// rows, `EmptyState` actions). Error strings are static and never echo
/// addon-controlled input.
pub fn validate_and_normalize(
    component: &mut DataDisplayComponent,
) -> Result<(), &'static str> {
    use DataDisplayComponent::*;
    match component {
        Text { .. } | Heading { .. } | Badge { .. } | Tag { .. } => Ok(()),
        Chip { .. } => Ok(()),
        Avatar { initials, image_source, .. } => {
            if let Some(s) = initials {
                if s.chars().count() > AVATAR_INITIALS_MAX_LEN {
                    return Err("avatar_initials_too_long");
                }
            }
            if let Some(src) = image_source {
                validate_image_source(src)?;
            }
            Ok(())
        }
        Image { source, .. } => validate_image_source(source),
        Stat { trend, .. } => {
            if let Some(t) = trend {
                if t.delta.chars().count() > STAT_TREND_DELTA_MAX_LEN {
                    return Err("stat_trend_delta_too_long");
                }
            }
            Ok(())
        }
        KeyValue { items, .. } => {
            for it in items.iter_mut() {
                validate_cell_value(&mut it.value)?;
            }
            Ok(())
        }
        List { items, .. } => {
            for it in items.iter_mut() {
                if let Some(t) = it.trailing.as_mut() {
                    validate_cell_value(t)?;
                }
            }
            Ok(())
        }
        BulletList { .. } => Ok(()),
        Timeline { .. } => Ok(()),
        Table {
            columns,
            rows,
            pagination,
            ..
        } => validate_table(columns, rows, pagination.as_mut()),
        MonoBlock { .. } | CodeBlock { .. } => Ok(()),
        EmptyState { actions, .. } => {
            for a in actions {
                super::reject_overlay_kind_in_root(a)
                    .map_err(|_| "empty_state_actions_invalid")?;
                super::validate_and_normalize_component(a)
                    .map_err(|_| "empty_state_actions_invalid")?;
            }
            Ok(())
        }
    }
}

fn validate_image_source(src: &ImageSource) -> Result<(), &'static str> {
    if let ImageSource::SignedFrame { camera_id, .. } = src {
        validate_camera_id(camera_id)?;
    }
    Ok(())
}

/// Local copy of the legacy `cam_<uuid v4>` shape check. Kept here rather
/// than imported from `legacy.rs` to keep this module free of `anyhow`
/// (validator surface is `Result<_, &'static str>`).
fn validate_camera_id(id: &str) -> Result<(), &'static str> {
    if id.len() != CAMERA_ID_LEN || !id.starts_with(CAMERA_ID_PREFIX) {
        return Err("image_camera_id_invalid_format");
    }
    let uuid = &id[CAMERA_ID_PREFIX.len()..];
    let bytes = uuid.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let dash_pos = matches!(i, 8 | 13 | 18 | 23);
        if dash_pos {
            if b != b'-' {
                return Err("image_camera_id_invalid_format");
            }
        } else if !(b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("image_camera_id_invalid_format");
        }
    }
    if bytes[14] != b'4' {
        return Err("image_camera_id_invalid_format");
    }
    if !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        return Err("image_camera_id_invalid_format");
    }
    Ok(())
}

fn validate_cell_value(value: &mut CellValue) -> Result<(), &'static str> {
    if let CellValue::Component { value } = value {
        super::reject_overlay_kind_in_root(value)
            .map_err(|_| "cell_component_overlay_not_allowed")?;
        super::validate_and_normalize_component(value)
            .map_err(|_| "cell_component_invalid")?;
    }
    Ok(())
}

fn validate_table(
    columns: &[TableColumn],
    rows: &mut [TableRow],
    pagination: Option<&mut TablePagination>,
) -> Result<(), &'static str> {
    let mut col_ids: Vec<&str> = Vec::with_capacity(columns.len());
    for c in columns {
        if col_ids.iter().any(|s| *s == c.id.as_str()) {
            return Err("table_duplicate_column_id");
        }
        col_ids.push(c.id.as_str());
    }

    let mut row_ids: Vec<&str> = Vec::with_capacity(rows.len());
    for r in rows.iter() {
        if row_ids.iter().any(|s| *s == r.id.as_str()) {
            return Err("table_duplicate_row_id");
        }
        row_ids.push(r.id.as_str());
        if r.cells.len() != columns.len() {
            return Err("table_row_cell_count_mismatch");
        }
    }

    for r in rows.iter_mut() {
        for cell in r.cells.iter_mut() {
            validate_cell_value(cell)?;
        }
        for child in r.expanded_content.iter_mut() {
            super::reject_overlay_kind_in_root(child)
                .map_err(|_| "table_expanded_overlay_not_allowed")?;
            super::validate_and_normalize_component(child)
                .map_err(|_| "table_expanded_invalid")?;
        }
    }

    if let Some(p) = pagination {
        if let PaginationMode::Pages {
            current_page,
            total_pages,
            ..
        } = &p.mode
        {
            if *total_pages > 0 && *current_page > *total_pages {
                return Err("pagination_current_exceeds_total");
            }
        }
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::ContainerComponent;
    use crate::legacy::LegacyComponent;

    fn good_cam_id() -> String {
        "cam_550e8400-e29b-41d4-a716-446655440000".to_string()
    }

    fn legacy_text(s: &str) -> UiComponent {
        UiComponent::Legacy(LegacyComponent::Text {
            content: s.to_string(),
            style: None,
        })
    }

    fn window_overlay() -> UiComponent {
        UiComponent::Container(ContainerComponent::Window {
            title: "x".to_string(),
            size: crate::container::WindowSize::Md,
            dismissable: true,
            on_close: None,
            children: vec![],
            footer: vec![],
        })
    }

    fn round_trip(c: &DataDisplayComponent) -> DataDisplayComponent {
        let j = serde_json::to_value(c).expect("serialize");
        serde_json::from_value(j).expect("deserialize")
    }

    #[test]
    fn text_round_trip() {
        let c = DataDisplayComponent::Text {
            content: "hello".into(),
            style: TextStyle::Body,
            color: Some(Color::TextMuted),
            weight: Some(FontWeight::SemiBold),
            align: TextAlign::Center,
            truncate: true,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn heading_round_trip() {
        let c = DataDisplayComponent::Heading {
            content: "Hi".into(),
            level: HeadingLevel::H2,
            icon: Some(IconName::Home),
            subtitle: Some("sub".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn badge_round_trip() {
        let c = DataDisplayComponent::Badge {
            label: "online".into(),
            tone: BadgeTone::Success,
            size: BadgeSize::Sm,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn chip_round_trip() {
        let c = DataDisplayComponent::Chip {
            label: "admin".into(),
            kind: ChipKind::Owner,
            icon: Some(IconName::User),
            dismissible: true,
            on_dismiss: Some("rm".into()),
            on_click: Some("open".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn tag_round_trip() {
        let c = DataDisplayComponent::Tag {
            label: "yolo".into(),
            color: Some(Color::Accent),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn avatar_round_trip() {
        let c = DataDisplayComponent::Avatar {
            image_source: Some(ImageSource::Placeholder),
            initials: Some("AB".into()),
            size: AvatarSize::Lg,
            shape: AvatarShape::Rounded,
            status: Some(AvatarStatus::Online),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn image_round_trip() {
        let c = DataDisplayComponent::Image {
            source: ImageSource::Url { url: "https://x".into() },
            alt: Some("a".into()),
            width: Some(120),
            height: Some(80),
            radius: Radius::Md,
            fit: ImageFit::Contain,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn stat_round_trip() {
        let c = DataDisplayComponent::Stat {
            value: "1,234".into(),
            value_suffix: None,
            label: "Frames".into(),
            sublabel: Some("today".into()),
            trend: Some(StatTrend {
                direction: TrendDirection::Up,
                delta: "+12%".into(),
                period: Some("vs yesterday".into()),
            }),
            icon: Some(IconName::Video),
            accent: Some(Color::Accent),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn key_value_round_trip() {
        let c = DataDisplayComponent::KeyValue {
            items: vec![KeyValueItem {
                key: "Name".into(),
                value: CellValue::Text { value: "Camera 1".into() },
                icon: Some(IconName::Cameras),
                tooltip: Some("t".into()),
            }],
            density: KeyValueDensity::Compact,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn list_round_trip() {
        let c = DataDisplayComponent::List {
            items: vec![ListItem {
                icon: Some(IconName::Bell),
                title: "Alert".into(),
                subtitle: Some("just now".into()),
                trailing: Some(CellValue::Badge {
                    tone: BadgeTone::Warning,
                    label: "new".into(),
                }),
                on_click: Some("open".into()),
            }],
            marker: ListMarker::Arrow,
            density: ListDensity::Compact,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn bullet_list_round_trip() {
        let c = DataDisplayComponent::BulletList {
            items: vec!["a".into(), "b".into()],
            style: BulletStyle::Check,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn timeline_round_trip() {
        let c = DataDisplayComponent::Timeline {
            items: vec![TimelineItem {
                timestamp: "2026-05-19T10:00:00Z".into(),
                title: "Created".into(),
                description: Some("by admin".into()),
                icon: Some(IconName::Add),
                accent: Some(Color::Success),
            }],
            orientation: TimelineOrientation::Horizontal,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn table_round_trip_with_cursor_pagination() {
        let c = DataDisplayComponent::Table {
            columns: vec![
                TableColumn {
                    id: "name".into(),
                    label: "Name".into(),
                    width: Some(160),
                    align: TextAlign::Start,
                    sortable: true,
                    icon: None,
                },
                TableColumn {
                    id: "status".into(),
                    label: "Status".into(),
                    width: None,
                    align: TextAlign::Center,
                    sortable: false,
                    icon: Some(IconName::Info),
                },
            ],
            rows: vec![TableRow {
                id: "r1".into(),
                cells: vec![
                    CellValue::Text { value: "Camera A".into() },
                    CellValue::Badge {
                        tone: BadgeTone::Success,
                        label: "ok".into(),
                    },
                ],
                expanded_content: vec![],
                accent: None,
            }],
            pagination: Some(TablePagination {
                mode: PaginationMode::Cursor {
                    next_cursor: Some("abc".into()),
                },
                page_size: Some(50),
            }),
            expandable: false,
            on_row_click: Some("open_row".into()),
            on_sort: Some("sort".into()),
            on_load_more: Some("more".into()),
            density: TableDensity::Compact,
            empty_state: Some("No cameras".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn mono_block_round_trip() {
        let c = DataDisplayComponent::MonoBlock {
            content: "fn main() {}".into(),
            language: Some("rust".into()),
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn code_block_round_trip() {
        let c = DataDisplayComponent::CodeBlock {
            segments: vec![
                CodeSegment {
                    text: "fn".into(),
                    kind: CodeSegmentKind::Keyword,
                },
                CodeSegment {
                    text: " main".into(),
                    kind: CodeSegmentKind::Function,
                },
            ],
            language: Some("rust".into()),
            show_line_numbers: true,
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn empty_state_round_trip() {
        let c = DataDisplayComponent::EmptyState {
            icon: Some(IconName::Cameras),
            title: "No cameras yet".into(),
            message: Some("Add your first camera".into()),
            actions: vec![legacy_text("add")],
        };
        assert_eq!(round_trip(&c), c);
    }

    #[test]
    fn cell_value_component_round_trip() {
        let cell = CellValue::Component {
            value: Box::new(legacy_text("inner")),
        };
        let j = serde_json::to_value(&cell).expect("ser");
        let back: CellValue = serde_json::from_value(j).expect("de");
        assert_eq!(back, cell);
    }

    #[test]
    fn table_cell_count_mismatch_is_rejected() {
        let mut c = DataDisplayComponent::Table {
            columns: vec![
                TableColumn {
                    id: "a".into(),
                    label: "A".into(),
                    width: None,
                    align: TextAlign::Start,
                    sortable: false,
                    icon: None,
                },
                TableColumn {
                    id: "b".into(),
                    label: "B".into(),
                    width: None,
                    align: TextAlign::Start,
                    sortable: false,
                    icon: None,
                },
            ],
            rows: vec![TableRow {
                id: "r1".into(),
                cells: vec![CellValue::Text { value: "only-one".into() }],
                expanded_content: vec![],
                accent: None,
            }],
            pagination: None,
            expandable: false,
            on_row_click: None,
            on_sort: None,
            on_load_more: None,
            density: TableDensity::Normal,
            empty_state: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "table_row_cell_count_mismatch");
    }

    #[test]
    fn table_duplicate_column_id_is_rejected() {
        let mut c = DataDisplayComponent::Table {
            columns: vec![
                TableColumn {
                    id: "a".into(),
                    label: "A".into(),
                    width: None,
                    align: TextAlign::Start,
                    sortable: false,
                    icon: None,
                },
                TableColumn {
                    id: "a".into(),
                    label: "A2".into(),
                    width: None,
                    align: TextAlign::Start,
                    sortable: false,
                    icon: None,
                },
            ],
            rows: vec![],
            pagination: None,
            expandable: false,
            on_row_click: None,
            on_sort: None,
            on_load_more: None,
            density: TableDensity::Normal,
            empty_state: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "table_duplicate_column_id");
    }

    #[test]
    fn table_duplicate_row_id_is_rejected() {
        let mut c = DataDisplayComponent::Table {
            columns: vec![TableColumn {
                id: "a".into(),
                label: "A".into(),
                width: None,
                align: TextAlign::Start,
                sortable: false,
                icon: None,
            }],
            rows: vec![
                TableRow {
                    id: "r".into(),
                    cells: vec![CellValue::Empty],
                    expanded_content: vec![],
                    accent: None,
                },
                TableRow {
                    id: "r".into(),
                    cells: vec![CellValue::Empty],
                    expanded_content: vec![],
                    accent: None,
                },
            ],
            pagination: None,
            expandable: false,
            on_row_click: None,
            on_sort: None,
            on_load_more: None,
            density: TableDensity::Normal,
            empty_state: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "table_duplicate_row_id");
    }

    #[test]
    fn cell_value_component_with_legal_child_validates() {
        let mut c = DataDisplayComponent::Table {
            columns: vec![TableColumn {
                id: "a".into(),
                label: "A".into(),
                width: None,
                align: TextAlign::Start,
                sortable: false,
                icon: None,
            }],
            rows: vec![TableRow {
                id: "r".into(),
                cells: vec![CellValue::Component {
                    value: Box::new(legacy_text("ok")),
                }],
                expanded_content: vec![],
                accent: None,
            }],
            pagination: None,
            expandable: false,
            on_row_click: None,
            on_sort: None,
            on_load_more: None,
            density: TableDensity::Normal,
            empty_state: None,
        };
        validate_and_normalize(&mut c).expect("ok");
    }

    #[test]
    fn cell_value_component_with_window_is_rejected() {
        let mut c = DataDisplayComponent::Table {
            columns: vec![TableColumn {
                id: "a".into(),
                label: "A".into(),
                width: None,
                align: TextAlign::Start,
                sortable: false,
                icon: None,
            }],
            rows: vec![TableRow {
                id: "r".into(),
                cells: vec![CellValue::Component {
                    value: Box::new(window_overlay()),
                }],
                expanded_content: vec![],
                accent: None,
            }],
            pagination: None,
            expandable: false,
            on_row_click: None,
            on_sort: None,
            on_load_more: None,
            density: TableDensity::Normal,
            empty_state: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "cell_component_overlay_not_allowed");
    }

    #[test]
    fn avatar_initials_too_long_is_rejected() {
        let mut c = DataDisplayComponent::Avatar {
            image_source: None,
            initials: Some("ABCD".into()),
            size: AvatarSize::Md,
            shape: AvatarShape::Circle,
            status: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "avatar_initials_too_long");
    }

    #[test]
    fn stat_trend_delta_too_long_is_rejected() {
        let mut c = DataDisplayComponent::Stat {
            value: "v".into(),
            value_suffix: None,
            label: "l".into(),
            sublabel: None,
            trend: Some(StatTrend {
                direction: TrendDirection::Up,
                delta: "x".repeat(STAT_TREND_DELTA_MAX_LEN + 1),
                period: None,
            }),
            icon: None,
            accent: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "stat_trend_delta_too_long");
    }

    #[test]
    fn pagination_pages_overflow_is_rejected() {
        let mut c = DataDisplayComponent::Table {
            columns: vec![TableColumn {
                id: "a".into(),
                label: "A".into(),
                width: None,
                align: TextAlign::Start,
                sortable: false,
                icon: None,
            }],
            rows: vec![],
            pagination: Some(TablePagination {
                mode: PaginationMode::Pages {
                    current_page: 5,
                    total_pages: 3,
                    on_page_change: "p".into(),
                },
                page_size: None,
            }),
            expandable: false,
            on_row_click: None,
            on_sort: None,
            on_load_more: None,
            density: TableDensity::Normal,
            empty_state: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "pagination_current_exceeds_total");
    }

    #[test]
    fn image_signed_frame_bad_camera_id_is_rejected() {
        let mut c = DataDisplayComponent::Image {
            source: ImageSource::SignedFrame {
                camera_id: "../etc/passwd".into(),
                frame_ref: "x".into(),
            },
            alt: None,
            width: None,
            height: None,
            radius: Radius::None,
            fit: ImageFit::Cover,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "image_camera_id_invalid_format");
    }

    #[test]
    fn image_signed_frame_good_camera_id_is_ok() {
        let mut c = DataDisplayComponent::Image {
            source: ImageSource::SignedFrame {
                camera_id: good_cam_id(),
                frame_ref: "abc".into(),
            },
            alt: None,
            width: None,
            height: None,
            radius: Radius::None,
            fit: ImageFit::Cover,
        };
        validate_and_normalize(&mut c).expect("ok");
    }

    #[test]
    fn avatar_signed_frame_bad_camera_id_is_rejected() {
        let mut c = DataDisplayComponent::Avatar {
            image_source: Some(ImageSource::SignedFrame {
                camera_id: "cam_bad".into(),
                frame_ref: "x".into(),
            }),
            initials: None,
            size: AvatarSize::Md,
            shape: AvatarShape::Circle,
            status: None,
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "image_camera_id_invalid_format");
    }

    #[test]
    fn empty_state_window_action_is_rejected() {
        let mut c = DataDisplayComponent::EmptyState {
            icon: None,
            title: "x".into(),
            message: None,
            actions: vec![window_overlay()],
        };
        let err = validate_and_normalize(&mut c).expect_err("must reject");
        assert_eq!(err, "empty_state_actions_invalid");
    }

    #[test]
    fn ui_component_data_display_round_trip_through_sum() {
        let comp = UiComponent::DataDisplay(DataDisplayComponent::Text {
            content: "hi".into(),
            style: TextStyle::Body,
            color: None,
            weight: None,
            align: TextAlign::Start,
            truncate: false,
        });
        let j = serde_json::to_value(&comp).expect("ser");
        let back: UiComponent = serde_json::from_value(j).expect("de");
        assert_eq!(back, comp);
    }

    #[test]
    fn cell_value_chip_omits_kind_when_none() {
        // Addons may emit chip cells without a semantic `kind`; the wire form
        // must skip the field entirely so the renderer falls back to defaults.
        let v = CellValue::Chip {
            kind: None,
            label: "x".into(),
            icon: None,
        };
        let j = serde_json::to_value(&v).expect("serialize");
        assert_eq!(j, serde_json::json!({"cell": "chip", "label": "x"}));
        let back: CellValue = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, v);
    }

    #[test]
    fn cell_value_chip_with_kind_and_icon_round_trip() {
        let v = CellValue::Chip {
            kind: Some(ChipKind::Status),
            label: "x".into(),
            icon: Some(IconName::Plus),
        };
        let j = serde_json::to_value(&v).expect("serialize");
        assert_eq!(
            j,
            serde_json::json!({
                "cell": "chip",
                "kind": "status",
                "label": "x",
                "icon": "plus"
            })
        );
        let back: CellValue = serde_json::from_value(j).expect("deserialize");
        assert_eq!(back, v);
    }

    #[test]
    fn cell_value_kind_tag_is_snake_case() {
        let v = CellValue::Number {
            value: 1.5,
            format: Some("%.2f".into()),
        };
        let j = serde_json::to_value(&v).expect("ser");
        assert_eq!(j["cell"], serde_json::json!("number"));
    }
}
