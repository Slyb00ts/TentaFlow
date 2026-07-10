// =============================================================================
// File: protocol/ui/data/gauge.rs — Heatmap/Gauge (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::{GaugeThreshold, HeatmapColumn, HeatmapRow, HeatmapScale};
use super::super::tokens::{GaugeVariant, HeatmapLegendPosition};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};
use super::super::value_format::ValueFormat;

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
// 0x021B — Heatmap
// -----------------------------------------------------------------------------

/// Grid coloured by value (catalog §4 0x021B). Handlers: `"cell_click"`, `"cell_hover"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Heatmap {
    pub rows: Vec<HeatmapRow>,
    pub columns: Vec<HeatmapColumn>,
    pub cells_path: StatePath,
    pub scale: HeatmapScale,
    pub legend_position: HeatmapLegendPosition,
    pub cell_size_px: u16,
    pub tooltip: bool,
}

impl Heatmap {
    pub const TAG: u16 = 0x021B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.rows)?));
        e.push((1, encode_to_value(&self.columns)?));
        e.push((2, encode_to_value(&self.cells_path)?));
        e.push((3, encode_to_value(&self.scale)?));
        e.push((4, encode_to_value(&self.legend_position)?));
        e.push((5, encode_to_value(&self.cell_size_px)?));
        e.push((6, encode_to_value(&self.tooltip)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Heatmap")?;
        ensure_no_duplicate_keys("Heatmap", &c.fields.0)?;
        let mut rows = None;
        let mut columns = None;
        let mut cells_path = None;
        let mut scale = None;
        let mut legend_position = None;
        let mut cell_size_px = None;
        let mut tooltip = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => rows = Some(decode_from_value(v)?),
                1 => columns = Some(decode_from_value(v)?),
                2 => cells_path = Some(decode_from_value(v)?),
                3 => scale = Some(decode_from_value(v)?),
                4 => legend_position = Some(decode_from_value(v)?),
                5 => cell_size_px = Some(decode_from_value(v)?),
                6 => tooltip = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Heatmap", *other)),
            }
        }
        Ok(Heatmap {
            rows: rows.unwrap_or_default(),
            columns: columns.unwrap_or_default(),
            cells_path: cells_path.ok_or_else(|| missing_field("Heatmap", "cells_path"))?,
            scale: scale.ok_or_else(|| missing_field("Heatmap", "scale"))?,
            legend_position: legend_position
                .ok_or_else(|| missing_field("Heatmap", "legend_position"))?,
            cell_size_px: cell_size_px.ok_or_else(|| missing_field("Heatmap", "cell_size_px"))?,
            tooltip: tooltip.ok_or_else(|| missing_field("Heatmap", "tooltip"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x021C — Gauge
// -----------------------------------------------------------------------------

/// Circular / arc / semi gauge (catalog §4 0x021C). `min` / `max` are `f64`
/// (catalog updated for Value-roundtrip compatibility).
#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    pub value: BindRef,
    pub min: f64,
    pub max: f64,
    pub thresholds: Vec<GaugeThreshold>,
    pub variant: GaugeVariant,
    pub label: Option<BindRef>,
    pub format: Option<ValueFormat>,
    pub size_px: u16,
}

impl Gauge {
    pub const TAG: u16 = 0x021C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(8);
        e.push((0, encode_to_value(&self.value)?));
        e.push((1, encode_to_value(&self.min)?));
        e.push((2, encode_to_value(&self.max)?));
        e.push((3, encode_to_value(&self.thresholds)?));
        e.push((4, encode_to_value(&self.variant)?));
        if let Some(l) = &self.label {
            e.push((5, encode_to_value(l)?));
        }
        if let Some(f) = &self.format {
            e.push((6, encode_to_value(f)?));
        }
        e.push((7, encode_to_value(&self.size_px)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Gauge")?;
        ensure_no_duplicate_keys("Gauge", &c.fields.0)?;
        let mut value = None;
        let mut min = None;
        let mut max = None;
        let mut thresholds = None;
        let mut variant = None;
        let mut label = None;
        let mut format = None;
        let mut size_px = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => value = Some(decode_from_value(v)?),
                1 => min = Some(decode_from_value(v)?),
                2 => max = Some(decode_from_value(v)?),
                3 => thresholds = Some(decode_from_value(v)?),
                4 => variant = Some(decode_from_value(v)?),
                5 => label = Some(decode_from_value(v)?),
                6 => format = Some(decode_from_value(v)?),
                7 => size_px = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Gauge", *other)),
            }
        }
        Ok(Gauge {
            value: value.ok_or_else(|| missing_field("Gauge", "value"))?,
            min: min.ok_or_else(|| missing_field("Gauge", "min"))?,
            max: max.ok_or_else(|| missing_field("Gauge", "max"))?,
            thresholds: thresholds.unwrap_or_default(),
            variant: variant.ok_or_else(|| missing_field("Gauge", "variant"))?,
            label,
            format,
            size_px: size_px.ok_or_else(|| missing_field("Gauge", "size_px"))?,
        })
    }
}
