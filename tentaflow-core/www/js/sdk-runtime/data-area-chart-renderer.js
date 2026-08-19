// =============================================================================
// Plik: sdk-runtime/data-area-chart-renderer.js
// Opis: Renderer AreaChart (0x0218) — mapper na komponent <tf-area-chart>.
// Waliduje CBOR FieldMap (series/axes/legend/tooltip/zoom/brush/height/
// stacking/opacity), resolves dane z store i mostkuje eventy SDK. Stacking
// (none/stacked/percent) i rysowanie żyją w tf-area-chart.js.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs AreaChart (10 pól).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { subscribeBindRef } from './bind-resolver.js';
import {
  CHART_ZOOM_MODES,
  requireEnum, requireBool, requireU16, requireF64, assertOnlyKnownFields,
  parseChartAxis, parseChartLegend, parseChartTooltip,
} from './data-chart-shared.js';
import {
  makeValueFormatter, toComponentAxis,
  bindSeriesProperty, bridgeChartEvents, parseLineChartLikeSeries,
} from './data-line-chart-renderer.js';

export const AREA_CHART_TAG = 0x0218;
const AREA_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
const AREA_STACKING_MODES = new Set(['none', 'stacked', 'percent']);

function renderAreaChart(component, ctx) {
  assertOnlyKnownFields(component.fields, AREA_CHART_FIELD_KEYS, 'AreaChart');

  const series = parseLineChartLikeSeries(component, ctx, 'AreaChart');
  const xAxisRaw = ctx.readField(component.fields, 1);
  if (xAxisRaw == null) throw new TypeError('AreaChart.x_axis is required');
  const xAxis = parseChartAxis(xAxisRaw, 'AreaChart.x_axis', ctx.locale);
  const yAxisRaw = ctx.readField(component.fields, 2);
  if (yAxisRaw == null) throw new TypeError('AreaChart.y_axis is required');
  const yAxis = parseChartAxis(yAxisRaw, 'AreaChart.y_axis', ctx.locale);
  const legendRaw = ctx.readField(component.fields, 3);
  if (legendRaw == null) throw new TypeError('AreaChart.legend is required');
  const legend = parseChartLegend(legendRaw, 'AreaChart.legend');
  const tooltipRaw = ctx.readField(component.fields, 4);
  if (tooltipRaw == null) throw new TypeError('AreaChart.tooltip is required');
  const tooltip = parseChartTooltip(tooltipRaw, 'AreaChart.tooltip', ctx.locale);
  const zoom = requireEnum(ctx.readField(component.fields, 5), CHART_ZOOM_MODES, 'AreaChart.zoom');
  const brush = requireBool(ctx.readField(component.fields, 6), 'AreaChart.brush');
  const heightPx = requireU16(ctx.readField(component.fields, 7), 'AreaChart.height_px');
  if (heightPx === 0) throw new TypeError('AreaChart.height_px must be > 0');
  const stacking = requireEnum(ctx.readField(component.fields, 8), AREA_STACKING_MODES, 'AreaChart.stacking');
  // §4 0x0218 default: opacity = 0.4.
  const opacityRaw = ctx.readField(component.fields, 9);
  const opacity = opacityRaw === undefined ? 0.4 : requireF64(opacityRaw, 'AreaChart.opacity');
  if (opacity < 0 || opacity > 1) {
    throw new TypeError('AreaChart.opacity must be in 0.0..=1.0');
  }
  // Percent stacking only makes sense on a linear value axis.
  if (stacking === 'percent' && yAxis.scale !== 'linear') {
    throw new TypeError('AreaChart.stacking=percent requires y_axis.scale=linear');
  }
  // Stacking is additive — incompatible with a log value axis.
  if ((stacking === 'stacked' || stacking === 'percent') && yAxis.scale === 'log') {
    throw new TypeError(`AreaChart.stacking=${stacking} incompatible with y_axis.scale=log`);
  }

  const el = document.createElement('tf-area-chart');
  el.locale = ctx.locale;
  el.xAxis = toComponentAxis(xAxis, ctx.locale);
  el.yAxis = toComponentAxis(yAxis, ctx.locale);
  el.legend = legend;
  el.tooltip = { enabled: tooltip.enabled, valueFormat: makeValueFormatter(tooltip.format, ctx.locale) };
  el.zoom = zoom;
  el.brush = brush;
  el.height = heightPx;
  el.stacking = stacking;
  el.opacity = opacity;

  const sync = bindSeriesProperty(el, series, ctx);
  if (xAxis.label) ctx.registerCleanup(subscribeBindRef(xAxis.label, ctx.store, sync));
  if (yAxis.label) ctx.registerCleanup(subscribeBindRef(yAxis.label, ctx.store, sync));

  bridgeChartEvents(el, [
    ['series-toggle', 'series_toggle'],
    ['point-hover', 'point_hover'],
    ['range-select', 'range_select'],
  ]);

  return el;
}

export function registerDataAreaChartRenderer() {
  if (!lookupComponentRenderer(AREA_CHART_TAG)) registerComponentRenderer(AREA_CHART_TAG, renderAreaChart);
}
