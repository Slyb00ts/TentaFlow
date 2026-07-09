// =============================================================================
// File: addons/sdk-showcase/src/catalog.rs
// Purpose: schema-driven sample generator for the SDK component catalog.
//          Walks tentaflow_sdk_spec::protocol::ui::schema registries
//          (ALL_COMPONENTS / ALL_ENUMS / ALL_INLINE_STRUCTS / ALL_TAGGED_UNIONS)
//          and builds one representative Component instance per catalog tag
//          with sample props, grouped per catalog section.
// =============================================================================

use tentaflow_sdk_spec::protocol::ui::bind::{BindRef, PathSegment, StatePath};
use tentaflow_sdk_spec::protocol::ui::component::{Component, FieldMap};
use tentaflow_sdk_spec::protocol::ui::data::{
    AreaChart, BarChart, Gauge, Heatmap, LineChart, PieChart, ProgressBar, RatingDisplay,
    Sparkline, StackedBar, Text,
};
use tentaflow_sdk_spec::protocol::ui::inline::{
    ChartAxis, ChartLegend, ChartSeries, ChartTooltip, GaugeThreshold, HeatmapColumn, HeatmapRow,
    HeatmapScale, StackSegment,
};
use tentaflow_sdk_spec::protocol::ui::layout::Stack;
use tentaflow_sdk_spec::protocol::ui::slot::StateEntry;
use tentaflow_sdk_spec::protocol::ui::tokens::{
    AreaStacking, BarStacking, ChartAxisScale, ChartLegendAlign, ChartLegendPosition,
    ChartOrientation, ChartSeriesStyle, ChartZoomMode, GaugeVariant, HeatmapLegendPosition,
    PieVariant, ProgressSize, ProgressVariant, RatingPrecision, RatingVariant, SparklineVariant,
};
use tentaflow_sdk_spec::protocol::ui::schema::{
    section, ComponentMeta, ALL_COMPONENTS, ALL_ENUMS, ALL_INLINE_STRUCTS, ALL_TAGGED_UNIONS,
};
use tentaflow_sdk_spec::protocol::ui::tokens::{FlexAlign, Spacing, TextStyle, Tone};
use tentaflow_sdk_spec::protocol::ui::typed_field::encode_to_value;
use tentaflow_sdk_spec::protocol::value::Value;
use tentaflow_sdk_spec::protocol::control::CborMap;
use tentaflow_sdk_spec::protocol::ui::a11y::{Accessibility, EventKind};
use tentaflow_sdk_spec::protocol::ui::component::HandlerMap;
use tentaflow_sdk_spec::protocol::ui::handler::{FailurePolicy, Handler};

/// Nested component sampling depth cap — below this the generator emits a
/// plain Text leaf instead of recursing further.
const MAX_DEPTH: u32 = 4;

/// Component tags the JS sdk-runtime has NO renderer for yet — rendering any
/// of them throws "no renderer registered" and kills the whole slot, so the
/// catalog skips them and shows a per-tab info line instead.
/// MUST stay in sync with KNOWN_MISSING in
/// tentaflow-core/www/js/sdk-runtime/component-registry-completeness.test.js
/// and may only SHRINK (remove a tag here once its JS renderer ships).
const RENDERER_NOT_IMPLEMENTED: &[u16] = &[
    0x0601, // Canvas2D
    0x0602, // WebGLSurface
    0x0603, // WGPUSurface
    0x0701, // PermissionMatrix
    0x0702, // NetworkRuleEditor
    0x0704, // AlarmFeed
    0x0705, // WeeklyScheduleGrid
    0x0706, // AccessMatrix
    0x0707, // ReqCard
    0x0708, // DecisionRow
    0x0709, // Inbox
    0x070A, // RuntimeStatusGrid
];

/// Overlay components that render page-level chrome (backdrop, drawer panel,
/// floating popover) when mounted. Sampling them inline would cover the whole
/// dashboard with an open backdrop, so the catalog skips them.
const OVERLAY_NOT_SAMPLED: &[u16] = &[
    0x0509, // Modal
    0x050A, // Drawer
    0x050B, // Popover
    0x050C, // Sheet
    0x050D, // GateScreen
    0x050E, // ConfirmationDialog (renders through tf-modal)
];

/// Tab id → catalog section header. Returns None for non-catalog tabs.
pub fn section_for_tab(tab: &str) -> Option<&'static str> {
    match tab {
        "molecules" => Some(section::MOLECULES),
        "layout" => Some(section::LAYOUT),
        "data" => Some(section::DATA),
        "form" => Some(section::FORM),
        "action" => Some(section::ACTION),
        "feedback" => Some(section::FEEDBACK),
        "specialized" => Some(section::SPECIALIZED),
        _ => None,
    }
}

/// Build the catalog tab fragment for one section: a Stack interleaving a
/// caption (component name + tag) with a generated sample instance for every
/// component the schema declares in that section.
pub fn section_stack(tab: &str, section_header: &str) -> Component {
    let mut ctr: u64 = 0;
    let mut children: Vec<Component> = Vec::new();
    let mut hidden: u64 = 0;

    for meta in ALL_COMPONENTS.iter().filter(|m| m.section == section_header) {
        if RENDERER_NOT_IMPLEMENTED.contains(&meta.tag) || OVERLAY_NOT_SAMPLED.contains(&meta.tag)
        {
            hidden += 1;
            continue;
        }
        ctr += 1;
        let caption = Text {
            content: BindRef::Literal(Value::Text(format!(
                "{} (0x{:04X})",
                meta.name, meta.tag
            ))),
            style: TextStyle::BodyStrong,
            tone: Some(Tone::Muted),
            align: None,
            wrap: None,
            max_lines: None,
            format: None,
            streaming: None,
        }
        .into_component(format!("cat-{}-hdr-{}", tab, ctr))
        .expect("Text caption encode");
        children.push(caption);
        children.push(sample_component(meta, 0, &mut ctr));
    }

    if hidden > 0 {
        let note = Text {
            content: BindRef::Literal(Value::Text(format!(
                "{} component{} hidden — missing JS renderer or page-level overlay",
                hidden,
                if hidden == 1 { "" } else { "s" }
            ))),
            style: TextStyle::Caption,
            tone: Some(Tone::Muted),
            align: None,
            wrap: None,
            max_lines: None,
            format: None,
            streaming: None,
        }
        .into_component(format!("cat-{}-hidden-note", tab))
        .expect("Text hidden-note encode");
        children.push(note);
    }

    Stack {
        gap: Spacing::Lg,
        align: FlexAlign::Stretch,
        children,
        padding: Some(Spacing::Md),
        justify: None,
        style: None,
        responsive: None,
    }
    .into_component(format!("catalog-{}", tab))
    .expect("Stack encode")
}

// =============================================================================
// Component instance synthesis
// =============================================================================

/// Build a sample Component for one schema entry. Every non-Option field gets
/// a synthesized value matching its wire type-string; Option fields are
/// omitted (decoders default them).
fn sample_component(meta: &ComponentMeta, depth: u32, ctr: &mut u64) -> Component {
    if depth > MAX_DEPTH {
        return text_leaf("nested sample", ctr);
    }
    // Chart / data-viz components read their plotted data from `StatePath`s (or
    // need multi-point inline structures). The generic field walker would emit a
    // single placeholder point or an unseeded path, so the chart renders blank.
    // Hand-build a representative, multi-point sample whose data paths line up
    // with the entries returned by `chart_state_entries`.
    if let Some(comp) = chart_sample(meta.tag, ctr) {
        return comp;
    }
    let mut entries: Vec<(u8, Value)> = Vec::new();
    for f in meta.fields {
        // Optional BindRefs are included: several renderers require at least
        // one display field among optional ones (e.g. MenuButton demands
        // trigger_label or trigger_icon) and a text sample is always safe.
        if f.wire.starts_with("Option<") && f.wire != "Option<BindRef>" {
            continue;
        }
        entries.push((f.key, sample_value(f.wire, f.name, depth, ctr)));
    }
    *ctr += 1;
    Component {
        tag: meta.tag,
        id: format!("demo-{}-{}", meta.name.to_lowercase(), ctr),
        fields: FieldMap(entries),
        handlers: None,
        bind: None,
        // Interactive components without a visible label (Toggle, IconButton,
        // ...) require an accessible name — give every sample one.
        a11y: Some(Accessibility {
            label: Some(BindRef::Literal(Value::Text(format!(
                "Sample {}",
                meta.name
            )))),
            ..Accessibility::default()
        }),
        visibility: None,
        test_id: None,
    }
}

/// Minimal Text leaf used for nested `Component` fields and depth cutoff.
fn text_leaf(content: &str, ctr: &mut u64) -> Component {
    *ctr += 1;
    Text {
        content: BindRef::Literal(Value::Text(content.into())),
        style: TextStyle::Body,
        tone: None,
        align: None,
        wrap: None,
        max_lines: None,
        format: None,
        streaming: None,
    }
    .into_component(format!("demo-leaf-{}", ctr))
    .expect("Text leaf encode")
}

// =============================================================================
// Chart / data-viz samples — multi-point so the catalog actually plots
// =============================================================================

/// Root key for every seeded chart data path. The `data` tab SlotContent
/// state_overlay (see `chart_state_entries`) writes the points/slices/cells
/// arrays under `["charts", <key>]`, and the chart samples reference the same
/// paths so the renderers read real data instead of an empty array.
const CHART_ROOT: &str = "charts";

fn chart_path(key: &str) -> StatePath {
    StatePath::new(vec![
        PathSegment::Key(CHART_ROOT.into()),
        PathSegment::Key(key.into()),
    ])
}

/// Fixed data-path keys (one per series / data source). Kept in lockstep with
/// `chart_state_entries`.
const PATH_LINE_A: &str = "line_a";
const PATH_LINE_B: &str = "line_b";
const PATH_BAR_A: &str = "bar_a";
const PATH_BAR_B: &str = "bar_b";
const PATH_AREA_A: &str = "area_a";
const PATH_AREA_B: &str = "area_b";
const PATH_SPARK: &str = "spark";
const PATH_PIE: &str = "pie";
const PATH_HEATMAP: &str = "heatmap_cells";

/// Builds a chart/data-viz sample for one tag, or `None` when the tag is not a
/// special-cased chart (the generic walker then handles it).
fn chart_sample(tag: u16, ctr: &mut u64) -> Option<Component> {
    *ctr += 1;
    let id = |name: &str| format!("demo-{}-{}", name, ctr);
    let comp = match tag {
        Sparkline::TAG => Sparkline {
            data_path: chart_path(PATH_SPARK),
            variant: SparklineVariant::Line,
            tone: Tone::Primary,
            width_px: 160,
            height_px: 40,
            show_min_max: true,
        }
        .into_component(id("sparkline"))
        .expect("Sparkline sample encode"),
        LineChart::TAG => LineChart {
            series: vec![
                chart_series("line-a", "Requests", PATH_LINE_A, Tone::Primary),
                chart_series("line-b", "Errors", PATH_LINE_B, Tone::Critical),
            ],
            x_axis: category_axis(),
            y_axis: linear_axis(),
            legend: legend(),
            tooltip: tooltip(),
            zoom: ChartZoomMode::None,
            brush: false,
            height_px: 220,
        }
        .into_component(id("linechart"))
        .expect("LineChart sample encode"),
        BarChart::TAG => BarChart {
            series: vec![
                chart_series("bar-a", "This week", PATH_BAR_A, Tone::Primary),
                chart_series("bar-b", "Last week", PATH_BAR_B, Tone::Info),
            ],
            x_axis: category_axis(),
            y_axis: linear_axis(),
            orientation: ChartOrientation::Vertical,
            stacking: BarStacking::None,
            legend: legend(),
            height_px: 220,
        }
        .into_component(id("barchart"))
        .expect("BarChart sample encode"),
        AreaChart::TAG => AreaChart {
            series: vec![
                chart_series("area-a", "CPU", PATH_AREA_A, Tone::Primary),
                chart_series("area-b", "Memory", PATH_AREA_B, Tone::Success),
            ],
            x_axis: category_axis(),
            y_axis: linear_axis(),
            legend: legend(),
            tooltip: tooltip(),
            zoom: ChartZoomMode::None,
            brush: false,
            height_px: 220,
            stacking: AreaStacking::Stacked,
            opacity: 0.4,
        }
        .into_component(id("areachart"))
        .expect("AreaChart sample encode"),
        PieChart::TAG => PieChart {
            data_path: chart_path(PATH_PIE),
            variant: PieVariant::Donut,
            show_labels: true,
            show_legend: true,
            max_segments: 6,
            height_px: 220,
        }
        .into_component(id("piechart"))
        .expect("PieChart sample encode"),
        StackedBar::TAG => StackedBar {
            segments: vec![
                stack_segment("used", "Used", 48.0, Tone::Primary),
                stack_segment("reserved", "Reserved", 24.0, Tone::Warning),
                stack_segment("free", "Free", 28.0, Tone::Success),
            ],
            total: BindRef::Literal(Value::F64(100.0)),
            show_legend: true,
            show_percentages: true,
            height_px: 40,
        }
        .into_component(id("stackedbar"))
        .expect("StackedBar sample encode"),
        Heatmap::TAG => Heatmap {
            rows: heatmap_rows(),
            columns: heatmap_columns(),
            cells_path: chart_path(PATH_HEATMAP),
            scale: HeatmapScale::Linear {
                min: 0.0,
                max: 100.0,
                color_from: Tone::Info,
                color_to: Tone::Critical,
            },
            legend_position: HeatmapLegendPosition::TopRight,
            cell_size_px: 28,
            tooltip: true,
        }
        .into_component(id("heatmap"))
        .expect("Heatmap sample encode"),
        Gauge::TAG => Gauge {
            value: BindRef::Literal(Value::F64(72.0)),
            min: 0.0,
            max: 100.0,
            thresholds: vec![
                gauge_threshold(60.0, Tone::Warning),
                gauge_threshold(85.0, Tone::Critical),
            ],
            variant: GaugeVariant::Arc,
            label: Some(BindRef::Literal(Value::Text("Throughput".into()))),
            format: None,
            size_px: 160,
        }
        .into_component(id("gauge"))
        .expect("Gauge sample encode"),
        ProgressBar::TAG => ProgressBar {
            value: BindRef::Literal(Value::F64(72.0)),
            max: 100.0,
            variant: ProgressVariant::Default,
            tone: Tone::Primary,
            show_label: true,
            label: None,
            size: ProgressSize::Md,
        }
        .into_component(id("progressbar"))
        .expect("ProgressBar sample encode"),
        RatingDisplay::TAG => RatingDisplay {
            value: BindRef::Literal(Value::F64(3.5)),
            max: 5,
            variant: RatingVariant::Stars,
            show_value: true,
            precision: RatingPrecision::Half,
        }
        .into_component(id("ratingdisplay"))
        .expect("RatingDisplay sample encode"),
        _ => return None,
    };
    Some(with_chart_a11y(comp))
}

/// Interactive-less charts still need an accessible name (the generic path adds
/// one for every other sample); mirror that so the catalog stays uniform.
fn with_chart_a11y(mut comp: Component) -> Component {
    comp.a11y = Some(Accessibility {
        label: Some(BindRef::Literal(Value::Text("Sample chart".into()))),
        ..Accessibility::default()
    });
    comp
}

fn chart_series(id: &str, name: &str, path_key: &str, tone: Tone) -> ChartSeries {
    ChartSeries {
        id: id.into(),
        name: BindRef::Literal(Value::Text(name.into())),
        data_path: chart_path(path_key),
        tone: Some(tone),
        style: ChartSeriesStyle::Solid,
        show_in_legend: true,
    }
}

fn category_axis() -> ChartAxis {
    ChartAxis {
        label: None,
        format: None,
        min: None,
        max: None,
        ticks: None,
        scale: ChartAxisScale::Category,
    }
}

fn linear_axis() -> ChartAxis {
    ChartAxis {
        label: None,
        format: None,
        min: None,
        max: None,
        ticks: None,
        scale: ChartAxisScale::Linear,
    }
}

fn legend() -> ChartLegend {
    ChartLegend {
        position: ChartLegendPosition::Bottom,
        alignment: ChartLegendAlign::Center,
    }
}

fn tooltip() -> ChartTooltip {
    ChartTooltip {
        enabled: true,
        format: None,
    }
}

fn stack_segment(id: &str, label: &str, value: f64, tone: Tone) -> StackSegment {
    StackSegment {
        id: id.into(),
        value: BindRef::Literal(Value::F64(value)),
        label: Some(BindRef::Literal(Value::Text(label.into()))),
        tone,
    }
}

fn gauge_threshold(value: f64, tone: Tone) -> GaugeThreshold {
    GaugeThreshold {
        value,
        tone,
        label: None,
    }
}

/// Five day-of-week rows / seven hour columns for the heatmap grid (7x5 cells).
const HEATMAP_ROW_IDS: &[(&str, &str)] = &[
    ("mon", "Mon"),
    ("tue", "Tue"),
    ("wed", "Wed"),
    ("thu", "Thu"),
    ("fri", "Fri"),
];
const HEATMAP_COL_IDS: &[(&str, &str)] = &[
    ("h08", "08"),
    ("h10", "10"),
    ("h12", "12"),
    ("h14", "14"),
    ("h16", "16"),
    ("h18", "18"),
    ("h20", "20"),
];

fn heatmap_rows() -> Vec<HeatmapRow> {
    HEATMAP_ROW_IDS
        .iter()
        .map(|(id, label)| HeatmapRow {
            id: (*id).into(),
            label: BindRef::Literal(Value::Text((*label).into())),
        })
        .collect()
}

fn heatmap_columns() -> Vec<HeatmapColumn> {
    HEATMAP_COL_IDS
        .iter()
        .map(|(id, label)| HeatmapColumn {
            id: (*id).into(),
            label: BindRef::Literal(Value::Text((*label).into())),
        })
        .collect()
}

/// State entries seeding every chart data path referenced by the chart samples.
/// Returned to the `data` tab SlotContent so the renderers read real numbers and
/// the catalog plots meaningful curves/slices/cells instead of blank frames.
pub fn chart_state_entries() -> Vec<StateEntry> {
    let mut entries = Vec::new();

    // Line: two diverging category series (x = weekday label, y varies).
    entries.push(point_series(PATH_LINE_A, &[12.0, 19.0, 14.0, 23.0, 28.0, 21.0, 31.0]));
    entries.push(point_series(PATH_LINE_B, &[3.0, 5.0, 2.0, 7.0, 4.0, 6.0, 3.0]));

    // Bar: two week-over-week category series.
    entries.push(point_series(PATH_BAR_A, &[40.0, 55.0, 48.0, 62.0, 51.0, 70.0]));
    entries.push(point_series(PATH_BAR_B, &[33.0, 47.0, 41.0, 50.0, 44.0, 58.0]));

    // Area: two stacked utilisation series.
    entries.push(point_series(PATH_AREA_A, &[22.0, 28.0, 35.0, 30.0, 42.0, 38.0]));
    entries.push(point_series(PATH_AREA_B, &[15.0, 18.0, 20.0, 26.0, 24.0, 30.0]));

    // Sparkline: bare numeric array (no x/y objects).
    entries.push(StateEntry {
        path: chart_path(PATH_SPARK),
        value: number_array(&[4.0, 8.0, 6.0, 11.0, 9.0, 14.0, 12.0, 16.0]),
    });

    // Pie: four labelled slices with distinct tones.
    entries.push(StateEntry {
        path: chart_path(PATH_PIE),
        value: pie_slices(),
    });

    // Heatmap: 7x5 grid of varied cell values addressed by row_id/col_id.
    entries.push(StateEntry {
        path: chart_path(PATH_HEATMAP),
        value: heatmap_cells(),
    });

    entries
}

/// `Array<{x: <weekday>, y: <value>}>` for a line/bar/area series.
fn point_series(key: &str, ys: &[f64]) -> StateEntry {
    const X_LABELS: &[&str] = &["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let points = ys
        .iter()
        .enumerate()
        .map(|(i, y)| {
            let x = X_LABELS.get(i).copied().unwrap_or("?");
            Value::Map(vec![
                (Value::Text("x".into()), Value::Text(x.into())),
                (Value::Text("y".into()), Value::F64(*y)),
            ])
        })
        .collect();
    StateEntry {
        path: chart_path(key),
        value: Value::Array(points),
    }
}

fn number_array(ns: &[f64]) -> Value {
    Value::Array(ns.iter().map(|n| Value::F64(*n)).collect())
}

/// `Array<{id, label, value, tone}>` for the pie chart.
fn pie_slices() -> Value {
    let slice = |id: &str, label: &str, value: f64, tone: &str| {
        Value::Map(vec![
            (Value::Text("id".into()), Value::Text(id.into())),
            (Value::Text("label".into()), Value::Text(label.into())),
            (Value::Text("value".into()), Value::F64(value)),
            (Value::Text("tone".into()), Value::Text(tone.into())),
        ])
    };
    Value::Array(vec![
        slice("ssd", "SSD", 42.0, "primary"),
        slice("hdd", "HDD", 28.0, "info"),
        slice("cache", "Cache", 18.0, "success"),
        slice("other", "Other", 12.0, "warning"),
    ])
}

/// `Array<{row_id, col_id, value}>` covering every cell of the 7x5 grid.
fn heatmap_cells() -> Value {
    let mut cells = Vec::with_capacity(HEATMAP_ROW_IDS.len() * HEATMAP_COL_IDS.len());
    for (r, (row_id, _)) in HEATMAP_ROW_IDS.iter().enumerate() {
        for (c, (col_id, _)) in HEATMAP_COL_IDS.iter().enumerate() {
            // Deterministic, varied surface peaking around mid-day mid-week.
            let value = 10.0
                + ((r as f64) * 7.0)
                + ((c as f64) * 11.0)
                + (((r + c) % 3) as f64) * 9.0;
            cells.push(Value::Map(vec![
                (Value::Text("row_id".into()), Value::Text((*row_id).into())),
                (Value::Text("col_id".into()), Value::Text((*col_id).into())),
                (Value::Text("value".into()), Value::F64(value.min(100.0))),
            ]));
        }
    }
    Value::Array(cells)
}

// =============================================================================
// Wire type-string sampling
// =============================================================================

/// Synthesize a sample Value for one wire type-string (see schema/types.rs
/// grammar). `field` drives small heuristics (numeric-looking BindRefs,
/// heading level range).
fn sample_value(wire: &str, field: &str, depth: u32, ctr: &mut u64) -> Value {
    if let Some(inner) = strip_generic(wire, "Option<") {
        return sample_value(inner, field, depth, ctr);
    }
    if let Some(inner) = strip_generic(wire, "Array<") {
        return Value::Array(vec![sample_value(inner, field, depth, ctr)]);
    }
    if let Some(name) = strip_generic(wire, "Enum<") {
        return enum_sample(name, field);
    }
    if let Some(name) = strip_generic(wire, "Inline<") {
        return inline_sample(name, depth, ctr);
    }
    if let Some(names) = strip_generic(wire, "ComponentRef<") {
        let first = names.split('|').next().unwrap_or(names);
        let mut comp = component_by_name(first)
            .map(|m| sample_component(m, depth + 1, ctr))
            .unwrap_or_else(|| text_leaf(first, ctr));
        // Buttons embedded by reference (Table.row_actions, card actions...)
        // must carry a backend handler — renderers reject inert buttons.
        if first == "Button" {
            comp.handlers = Some(HandlerMap(vec![(
                EventKind::Click,
                Handler::Backend {
                    action_id: "refresh".into(),
                    params: CborMap(vec![]),
                    optimistic: None,
                    on_failure: FailurePolicy::Toast,
                },
            )]));
        }
        return encode_to_value(&comp).unwrap_or(Value::Null);
    }
    match wire {
        "BindRef" => {
            let bind = BindRef::Literal(bind_literal(field));
            encode_to_value(&bind).unwrap_or(Value::Null)
        }
        "StatePath" => {
            let path = StatePath::new(vec![
                PathSegment::Key("demo".into()),
                PathSegment::Key(field.into()),
            ]);
            encode_to_value(&path).unwrap_or(Value::Null)
        }
        "tstr" => Value::Text(text_sample(field)),
        // Combobox requires searchable=true (catalog §5 0x0305); FormGroup
        // allows `expanded` (sampled as Option<BindRef>) only when collapsible.
        "bool" => Value::Bool(matches!(field, "searchable" | "collapsible")),
        "u8" | "u16" | "u32" | "u64" => Value::U64(uint_sample(field)),
        "i32" | "i64" => Value::I64(1),
        "f64" => Value::F64(float_sample(field)),
        "Component" => {
            let comp = text_leaf("nested content", ctr);
            encode_to_value(&comp).unwrap_or(Value::Null)
        }
        "Value" => Value::Text("demo".into()),
        "CborMap" => Value::Map(Vec::new()),
        // Unknown type-string — keep the payload decodable.
        _ => Value::Null,
    }
}

fn strip_generic<'a>(wire: &'a str, prefix: &str) -> Option<&'a str> {
    wire.strip_prefix(prefix)?.strip_suffix('>')
}

/// Literal payload for BindRef sample — numeric for fields whose name implies
/// a number, text otherwise.
fn bind_literal(field: &str) -> Value {
    const NUMERIC_HINTS: &[&str] = &[
        "value", "current", "count", "percent", "progress", "rating", "total", "step",
    ];
    if NUMERIC_HINTS.iter().any(|h| field.contains(h)) {
        Value::U64(42)
    } else {
        Value::Text(text_sample(field))
    }
}

fn text_sample(field: &str) -> String {
    // `*_id` fields (template ids, action ids...) are grammar-validated to
    // [a-z0-9_-]; everything else gets a human-readable sample.
    if field == "id" || field.ends_with("_id") || field.ends_with("_ids") {
        return "demo-id".into();
    }
    match field {
        // MentionInput.trigger_chars entries must each be a single character
        // (renderer validates `t.length === 1`); a human-readable sample would
        // abort the whole Form tab render.
        "trigger_chars" => "@".into(),
        // CodeBlock validates the language tag grammar.
        "language" => "rust".into(),
        // CalendarMonth.month is validated as YYYY-MM by its renderer.
        "month" => "2026-06".into(),
        // Image sources must actually load in the browser (a dead https URL
        // produces ERR_NAME_NOT_RESOLVED console noise) — use an inline 1x1
        // PNG, which the asset-src validators accept.
        "src" | "ref_" => concat!(
            "data:image/png;base64,",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkY",
            "PhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
        )
        .into(),
        // Plain links are not fetched by the browser.
        "url" | "href" => "https://example.invalid/demo".into(),
        // Validated as ISO 4217 by the CurrencyInput renderer.
        "currency_code" => "USD".into(),
        _ => format!("Sample {}", field.replace('_', " ")),
    }
}

/// Range-validated float fields (Slider/Heatmap scale): min must stay < max.
fn float_sample(field: &str) -> f64 {
    if field.contains("min") {
        0.0
    } else if field.contains("max") {
        100.0
    } else {
        0.5
    }
}

fn uint_sample(field: &str) -> u64 {
    match field {
        // Heading.level and friends are range-validated 1..=6.
        "level" => 2,
        "k" | "columns" | "cols" | "span" => 2,
        "max" | "total" => 100,
        _ => 1,
    }
}

// =============================================================================
// Enum / inline-struct / tagged-union sampling
// =============================================================================

fn enum_sample(name: &str, field: &str) -> Value {
    // LiveRegion's first variant is "off", but the LiveRegion component
    // renderer only accepts polite/assertive for politeness.
    if name == "LiveRegion" {
        return Value::Text("polite".into());
    }
    // FabPosition's first variant is "bottom_right", which pins the FAB to a
    // fixed screen corner — in the inline catalog that escapes the sample slot
    // and floats over the page. Render it in-flow instead.
    if name == "FabPosition" && field == "position" {
        return Value::Text("inline".into());
    }
    ALL_ENUMS
        .iter()
        .find(|e| e.name == name)
        .and_then(|e| e.variants.first())
        .map(|(_, wire)| Value::Text((*wire).into()))
        .unwrap_or(Value::Null)
}

/// `Inline<X>` wires cover both derive-encoded inline structs (integer-keyed
/// CBOR maps) and manually encoded tagged unions (tstr-keyed maps with a
/// discriminator). Look up unions first, then inline structs.
fn inline_sample(name: &str, depth: u32, ctr: &mut u64) -> Value {
    if let Some(u) = ALL_TAGGED_UNIONS.iter().find(|u| u.name == name) {
        let variant = match u.variants.first() {
            Some(v) => v,
            None => return Value::Map(Vec::new()),
        };
        let mut entries: Vec<(Value, Value)> = vec![(
            Value::Text(u.discriminator_key.into()),
            Value::Text(variant.wire_kind.into()),
        )];
        for f in variant.fields {
            if f.wire.starts_with("Option<") {
                continue;
            }
            // Schema field names keep the Rust keyword-escape underscore
            // (e.g. `ref_`), but manual encoders emit the bare name (`ref`).
            entries.push((
                Value::Text(f.name.trim_end_matches('_').into()),
                sample_value(f.wire, f.name, depth, ctr),
            ));
        }
        return canon_map(entries);
    }
    if let Some(i) = ALL_INLINE_STRUCTS.iter().find(|i| i.name == name) {
        let mut entries: Vec<(Value, Value)> = Vec::new();
        for f in i.fields {
            // Optional fields are included only for BindRefs: several
            // renderers require at least one display field (e.g.
            // SegmentOption demands label or icon), and an optional BindRef
            // is always a safe text sample.
            if f.wire.starts_with("Option<") && f.wire != "Option<BindRef>" {
                continue;
            }
            entries.push((
                Value::U64(f.key as u64),
                sample_value(f.wire, f.name, depth, ctr),
            ));
        }
        return canon_map(entries);
    }
    Value::Map(Vec::new())
}

fn component_by_name(name: &str) -> Option<&'static ComponentMeta> {
    ALL_COMPONENTS.iter().find(|m| m.name == name).copied()
}

/// Sort map entries by the byte representation of their encoded keys so the
/// emitted CBOR stays canonical (RFC 8949 deterministic key order).
fn canon_map(mut entries: Vec<(Value, Value)>) -> Value {
    entries.sort_by_cached_key(|(k, _)| {
        let mut buf = Vec::new();
        let _ = minicbor::encode(k, &mut buf);
        buf
    });
    Value::Map(entries)
}
