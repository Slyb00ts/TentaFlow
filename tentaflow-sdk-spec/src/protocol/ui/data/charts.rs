// =============================================================================
// File: protocol/ui/data/charts.rs — Sparkline/LineChart/BarChart/AreaChart/PieChart/StackedBar (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::{ChartAxis, ChartLegend, ChartSeries, ChartTooltip, StackSegment};
use super::super::tokens::{
    AreaStacking, BarStacking, ChartOrientation, ChartZoomMode, PieVariant, SparklineVariant, Tone,
};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component {
        tag,
        id: id.into(),
        fields: FieldMap(fields),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    }
}

// -----------------------------------------------------------------------------
// 0x0215 — Sparkline
// -----------------------------------------------------------------------------

/// Inline mini chart (catalog §4 0x0215).
#[derive(Debug, Clone, PartialEq)]
pub struct Sparkline {
    pub data_path: StatePath,
    pub variant: SparklineVariant,
    pub tone: Tone,
    pub width_px: u16,
    pub height_px: u16,
    pub show_min_max: bool,
}

impl Sparkline {
    pub const TAG: u16 = 0x0215;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.data_path)?));
        e.push((1, encode_to_value(&self.variant)?));
        e.push((2, encode_to_value(&self.tone)?));
        e.push((3, encode_to_value(&self.width_px)?));
        e.push((4, encode_to_value(&self.height_px)?));
        e.push((5, encode_to_value(&self.show_min_max)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Sparkline")?;
        ensure_no_duplicate_keys("Sparkline", &c.fields.0)?;
        let mut data_path = None;
        let mut variant = None;
        let mut tone = None;
        let mut width_px = None;
        let mut height_px = None;
        let mut show_min_max = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => data_path = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => tone = Some(decode_from_value(v)?),
                3 => width_px = Some(decode_from_value(v)?),
                4 => height_px = Some(decode_from_value(v)?),
                5 => show_min_max = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Sparkline", *other)),
            }
        }
        Ok(Sparkline {
            data_path: data_path.ok_or_else(|| missing_field("Sparkline", "data_path"))?,
            variant: variant.ok_or_else(|| missing_field("Sparkline", "variant"))?,
            tone: tone.ok_or_else(|| missing_field("Sparkline", "tone"))?,
            width_px: width_px.ok_or_else(|| missing_field("Sparkline", "width_px"))?,
            height_px: height_px.ok_or_else(|| missing_field("Sparkline", "height_px"))?,
            show_min_max: show_min_max.ok_or_else(|| missing_field("Sparkline", "show_min_max"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0216 — LineChart
// -----------------------------------------------------------------------------

/// Line chart (catalog §4 0x0216). Handlers: `"point_hover"`, `"range_select"`.
#[derive(Debug, Clone, PartialEq)]
pub struct LineChart {
    pub series: Vec<ChartSeries>,
    pub x_axis: ChartAxis,
    pub y_axis: ChartAxis,
    pub legend: ChartLegend,
    pub tooltip: ChartTooltip,
    pub zoom: ChartZoomMode,
    pub brush: bool,
    pub height_px: u16,
}

impl LineChart {
    pub const TAG: u16 = 0x0216;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(8);
        e.push((0, encode_to_value(&self.series)?));
        e.push((1, encode_to_value(&self.x_axis)?));
        e.push((2, encode_to_value(&self.y_axis)?));
        e.push((3, encode_to_value(&self.legend)?));
        e.push((4, encode_to_value(&self.tooltip)?));
        e.push((5, encode_to_value(&self.zoom)?));
        e.push((6, encode_to_value(&self.brush)?));
        e.push((7, encode_to_value(&self.height_px)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "LineChart")?;
        ensure_no_duplicate_keys("LineChart", &c.fields.0)?;
        let mut series = None;
        let mut x_axis = None;
        let mut y_axis = None;
        let mut legend = None;
        let mut tooltip = None;
        let mut zoom = None;
        let mut brush = None;
        let mut height_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => series = Some(decode_from_value(v)?),
                1 => x_axis = Some(decode_from_value(v)?),
                2 => y_axis = Some(decode_from_value(v)?),
                3 => legend = Some(decode_from_value(v)?),
                4 => tooltip = Some(decode_from_value(v)?),
                5 => zoom = Some(decode_from_value(v)?),
                6 => brush = Some(decode_from_value(v)?),
                7 => height_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("LineChart", *other)),
            }
        }
        Ok(LineChart {
            series: series.unwrap_or_default(),
            x_axis: x_axis.ok_or_else(|| missing_field("LineChart", "x_axis"))?,
            y_axis: y_axis.ok_or_else(|| missing_field("LineChart", "y_axis"))?,
            legend: legend.ok_or_else(|| missing_field("LineChart", "legend"))?,
            tooltip: tooltip.ok_or_else(|| missing_field("LineChart", "tooltip"))?,
            zoom: zoom.ok_or_else(|| missing_field("LineChart", "zoom"))?,
            brush: brush.ok_or_else(|| missing_field("LineChart", "brush"))?,
            height_px: height_px.ok_or_else(|| missing_field("LineChart", "height_px"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0217 — BarChart
// -----------------------------------------------------------------------------

/// Bar chart (catalog §4 0x0217).
#[derive(Debug, Clone, PartialEq)]
pub struct BarChart {
    pub series: Vec<ChartSeries>,
    pub x_axis: ChartAxis,
    pub y_axis: ChartAxis,
    pub orientation: ChartOrientation,
    pub stacking: BarStacking,
    pub legend: ChartLegend,
    pub height_px: u16,
}

impl BarChart {
    pub const TAG: u16 = 0x0217;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.series)?));
        e.push((1, encode_to_value(&self.x_axis)?));
        e.push((2, encode_to_value(&self.y_axis)?));
        e.push((3, encode_to_value(&self.orientation)?));
        e.push((4, encode_to_value(&self.stacking)?));
        e.push((5, encode_to_value(&self.legend)?));
        e.push((6, encode_to_value(&self.height_px)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "BarChart")?;
        ensure_no_duplicate_keys("BarChart", &c.fields.0)?;
        let mut series = None;
        let mut x_axis = None;
        let mut y_axis = None;
        let mut orientation = None;
        let mut stacking = None;
        let mut legend = None;
        let mut height_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => series = Some(decode_from_value(v)?),
                1 => x_axis = Some(decode_from_value(v)?),
                2 => y_axis = Some(decode_from_value(v)?),
                3 => orientation = Some(decode_from_value(v)?),
                4 => stacking = Some(decode_from_value(v)?),
                5 => legend = Some(decode_from_value(v)?),
                6 => height_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("BarChart", *other)),
            }
        }
        Ok(BarChart {
            series: series.unwrap_or_default(),
            x_axis: x_axis.ok_or_else(|| missing_field("BarChart", "x_axis"))?,
            y_axis: y_axis.ok_or_else(|| missing_field("BarChart", "y_axis"))?,
            orientation: orientation.ok_or_else(|| missing_field("BarChart", "orientation"))?,
            stacking: stacking.ok_or_else(|| missing_field("BarChart", "stacking"))?,
            legend: legend.ok_or_else(|| missing_field("BarChart", "legend"))?,
            height_px: height_px.ok_or_else(|| missing_field("BarChart", "height_px"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0218 — AreaChart
// -----------------------------------------------------------------------------

/// Area chart (catalog §4 0x0218). Handlers: `"point_hover"`, `"range_select"`.
/// `opacity` is `f64` (catalog updated for Value-roundtrip).
#[derive(Debug, Clone, PartialEq)]
pub struct AreaChart {
    pub series: Vec<ChartSeries>,
    pub x_axis: ChartAxis,
    pub y_axis: ChartAxis,
    pub legend: ChartLegend,
    pub tooltip: ChartTooltip,
    pub zoom: ChartZoomMode,
    pub brush: bool,
    pub height_px: u16,
    pub stacking: AreaStacking,
    /// 0.0..=1.0 (validated by host validator in Krok 4).
    pub opacity: f64,
}

impl AreaChart {
    pub const TAG: u16 = 0x0218;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(10);
        e.push((0, encode_to_value(&self.series)?));
        e.push((1, encode_to_value(&self.x_axis)?));
        e.push((2, encode_to_value(&self.y_axis)?));
        e.push((3, encode_to_value(&self.legend)?));
        e.push((4, encode_to_value(&self.tooltip)?));
        e.push((5, encode_to_value(&self.zoom)?));
        e.push((6, encode_to_value(&self.brush)?));
        e.push((7, encode_to_value(&self.height_px)?));
        e.push((8, encode_to_value(&self.stacking)?));
        e.push((9, encode_to_value(&self.opacity)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "AreaChart")?;
        ensure_no_duplicate_keys("AreaChart", &c.fields.0)?;
        let mut series = None;
        let mut x_axis = None;
        let mut y_axis = None;
        let mut legend = None;
        let mut tooltip = None;
        let mut zoom = None;
        let mut brush = None;
        let mut height_px = None;
        let mut stacking = None;
        let mut opacity = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => series = Some(decode_from_value(v)?),
                1 => x_axis = Some(decode_from_value(v)?),
                2 => y_axis = Some(decode_from_value(v)?),
                3 => legend = Some(decode_from_value(v)?),
                4 => tooltip = Some(decode_from_value(v)?),
                5 => zoom = Some(decode_from_value(v)?),
                6 => brush = Some(decode_from_value(v)?),
                7 => height_px = Some(decode_from_value(v)?),
                8 => stacking = Some(decode_from_value(v)?),
                9 => opacity = Some(decode_from_value(v)?),
                other => return Err(unknown_field("AreaChart", *other)),
            }
        }
        Ok(AreaChart {
            series: series.unwrap_or_default(),
            x_axis: x_axis.ok_or_else(|| missing_field("AreaChart", "x_axis"))?,
            y_axis: y_axis.ok_or_else(|| missing_field("AreaChart", "y_axis"))?,
            legend: legend.ok_or_else(|| missing_field("AreaChart", "legend"))?,
            tooltip: tooltip.ok_or_else(|| missing_field("AreaChart", "tooltip"))?,
            zoom: zoom.ok_or_else(|| missing_field("AreaChart", "zoom"))?,
            brush: brush.ok_or_else(|| missing_field("AreaChart", "brush"))?,
            height_px: height_px.ok_or_else(|| missing_field("AreaChart", "height_px"))?,
            stacking: stacking.ok_or_else(|| missing_field("AreaChart", "stacking"))?,
            // §4 0x0218 default: opacity = 0.4.
            opacity: opacity.unwrap_or(0.4),
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0219 — PieChart
// -----------------------------------------------------------------------------

/// Pie / donut chart (catalog §4 0x0219).
#[derive(Debug, Clone, PartialEq)]
pub struct PieChart {
    pub data_path: StatePath,
    pub variant: PieVariant,
    pub show_labels: bool,
    pub show_legend: bool,
    pub max_segments: u8,
    pub height_px: u16,
}

impl PieChart {
    pub const TAG: u16 = 0x0219;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.data_path)?));
        e.push((1, encode_to_value(&self.variant)?));
        e.push((2, encode_to_value(&self.show_labels)?));
        e.push((3, encode_to_value(&self.show_legend)?));
        e.push((4, encode_to_value(&self.max_segments)?));
        e.push((5, encode_to_value(&self.height_px)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "PieChart")?;
        ensure_no_duplicate_keys("PieChart", &c.fields.0)?;
        let mut data_path = None;
        let mut variant = None;
        let mut show_labels = None;
        let mut show_legend = None;
        let mut max_segments = None;
        let mut height_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => data_path = Some(decode_from_value(v)?),
                1 => variant = Some(decode_from_value(v)?),
                2 => show_labels = Some(decode_from_value(v)?),
                3 => show_legend = Some(decode_from_value(v)?),
                4 => max_segments = Some(decode_from_value(v)?),
                5 => height_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("PieChart", *other)),
            }
        }
        Ok(PieChart {
            data_path: data_path.ok_or_else(|| missing_field("PieChart", "data_path"))?,
            variant: variant.ok_or_else(|| missing_field("PieChart", "variant"))?,
            show_labels: show_labels.ok_or_else(|| missing_field("PieChart", "show_labels"))?,
            show_legend: show_legend.ok_or_else(|| missing_field("PieChart", "show_legend"))?,
            max_segments: max_segments.ok_or_else(|| missing_field("PieChart", "max_segments"))?,
            height_px: height_px.ok_or_else(|| missing_field("PieChart", "height_px"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x021A — StackedBar
// -----------------------------------------------------------------------------

/// Horizontal stacked bar for capacity displays (catalog §4 0x021A).
#[derive(Debug, Clone, PartialEq)]
pub struct StackedBar {
    pub segments: Vec<StackSegment>,
    pub total: BindRef,
    pub show_legend: bool,
    pub show_percentages: bool,
    pub height_px: u16,
}

impl StackedBar {
    pub const TAG: u16 = 0x021A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.segments)?));
        e.push((1, encode_to_value(&self.total)?));
        e.push((2, encode_to_value(&self.show_legend)?));
        e.push((3, encode_to_value(&self.show_percentages)?));
        e.push((4, encode_to_value(&self.height_px)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "StackedBar")?;
        ensure_no_duplicate_keys("StackedBar", &c.fields.0)?;
        let mut segments = None;
        let mut total = None;
        let mut show_legend = None;
        let mut show_percentages = None;
        let mut height_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => segments = Some(decode_from_value(v)?),
                1 => total = Some(decode_from_value(v)?),
                2 => show_legend = Some(decode_from_value(v)?),
                3 => show_percentages = Some(decode_from_value(v)?),
                4 => height_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("StackedBar", *other)),
            }
        }
        Ok(StackedBar {
            segments: segments.unwrap_or_default(),
            total: total.ok_or_else(|| missing_field("StackedBar", "total"))?,
            show_legend: show_legend.ok_or_else(|| missing_field("StackedBar", "show_legend"))?,
            show_percentages: show_percentages
                .ok_or_else(|| missing_field("StackedBar", "show_percentages"))?,
            height_px: height_px.ok_or_else(|| missing_field("StackedBar", "height_px"))?,
        })
    }
}
