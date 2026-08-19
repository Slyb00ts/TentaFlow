// =============================================================================
// Plik: sdk-runtime/data-line-chart-renderer.js
// Opis: Renderer LineChart (0x0216) — mapper na komponent <tf-line-chart>.
// Waliduje CBOR FieldMap (series/axes/legend/tooltip/zoom/brush/height),
// resolves BindRefy (nazwy serii) i dane z store, ustawia properties
// komponentu i mostkuje eventy komponentu na eventy SDK (series_toggle,
// point_hover, range_select). Całe rysowanie żyje w tf-line-chart.js.
//
// Data shape per series.data_path: Array<{x: number|string, y: number}>.
// Time scale: x = Unix ms. Category scale: x = string.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs LineChart.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { subscribeBindRef, resolveBindRef, formatValue } from './bind-resolver.js';
import {
  CHART_ZOOM_MODES,
  requireEnum, requireBool, requireU16, assertOnlyKnownFields,
  parseChartSeries, parseChartAxis, parseChartLegend, parseChartTooltip,
} from './data-chart-shared.js';

export const LINE_CHART_TAG = 0x0216;
const LINE_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

/// Wraps a parsed ValueFormat into a plain formatter callable usable by the
/// component (which must not know about ValueFormat/locale plumbing).
export function makeValueFormatter(format, locale) {
  if (format == null) return null;
  return (value) => formatValue(value, format, locale);
}

/// Maps a parsed ChartAxis to the component axis property shape.
export function toComponentAxis(axis, locale) {
  return {
    scale: axis.scale,
    min: axis.min,
    max: axis.max,
    ticks: axis.ticks,
    format: makeValueFormatter(axis.format, locale),
  };
}

/// Reads + sanitises series points from the store (finite numeric y only).
export function readSeriesPoints(store, dataPath) {
  let arr;
  try { arr = store.read(dataPath); } catch { arr = undefined; }
  if (!Array.isArray(arr)) return [];
  const out = [];
  for (const p of arr) {
    if (p == null || typeof p !== 'object') continue;
    const x = p.x;
    const y = p.y;
    if (typeof y !== 'number' || !Number.isFinite(y)) continue;
    out.push({ x, y });
  }
  return out;
}

/// Builds the component `series` payload (resolved names + store data) and
/// wires store subscriptions so updates re-push the property.
export function bindSeriesProperty(el, series, ctx) {
  const sync = () => {
    el.series = series.map((s) => {
      const name = resolveBindRef(s.name, ctx.store);
      return {
        id: s.id,
        name: name == null ? s.id : String(name),
        tone: s.tone,
        style: s.style,
        showInLegend: s.show_in_legend,
        points: readSeriesPoints(ctx.store, s.data_path),
      };
    });
  };
  sync();
  for (const s of series) {
    ctx.registerCleanup(ctx.store.subscribe(s.data_path, sync));
    ctx.registerCleanup(subscribeBindRef(s.name, ctx.store, sync));
  }
  return sync;
}

/// Re-dispatches component events under SDK event names with identical detail.
export function bridgeChartEvents(el, pairs) {
  for (const [from, to] of pairs) {
    el.addEventListener(from, (e) => {
      el.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)(to, {
        bubbles: false,
        detail: e.detail,
      }));
    });
  }
}

export function parseLineChartLikeSeries(component, ctx, name) {
  const seriesRaw = ctx.readField(component.fields, 0);
  if (!Array.isArray(seriesRaw) || seriesRaw.length === 0) {
    throw new TypeError(`${name}.series: expected non-empty Array<ChartSeries>`);
  }
  const series = seriesRaw.map((s, i) => parseChartSeries(s, `${name}.series[${i}]`));
  const seenIds = new Set();
  for (const s of series) {
    if (seenIds.has(s.id)) throw new TypeError(`${name}.series: duplicate id '${s.id}'`);
    seenIds.add(s.id);
  }
  return series;
}

function renderLineChart(component, ctx) {
  assertOnlyKnownFields(component.fields, LINE_CHART_FIELD_KEYS, 'LineChart');

  const series = parseLineChartLikeSeries(component, ctx, 'LineChart');
  const xAxisRaw = ctx.readField(component.fields, 1);
  if (xAxisRaw == null) throw new TypeError('LineChart.x_axis is required');
  const xAxis = parseChartAxis(xAxisRaw, 'LineChart.x_axis', ctx.locale);
  const yAxisRaw = ctx.readField(component.fields, 2);
  if (yAxisRaw == null) throw new TypeError('LineChart.y_axis is required');
  const yAxis = parseChartAxis(yAxisRaw, 'LineChart.y_axis', ctx.locale);
  const legendRaw = ctx.readField(component.fields, 3);
  if (legendRaw == null) throw new TypeError('LineChart.legend is required');
  const legend = parseChartLegend(legendRaw, 'LineChart.legend');
  const tooltipRaw = ctx.readField(component.fields, 4);
  if (tooltipRaw == null) throw new TypeError('LineChart.tooltip is required');
  const tooltip = parseChartTooltip(tooltipRaw, 'LineChart.tooltip', ctx.locale);
  const zoom = requireEnum(ctx.readField(component.fields, 5), CHART_ZOOM_MODES, 'LineChart.zoom');
  const brush = requireBool(ctx.readField(component.fields, 6), 'LineChart.brush');
  const heightPx = requireU16(ctx.readField(component.fields, 7), 'LineChart.height_px');
  if (heightPx === 0) throw new TypeError('LineChart.height_px must be > 0');

  const el = document.createElement('tf-line-chart');
  el.locale = ctx.locale;
  el.xAxis = toComponentAxis(xAxis, ctx.locale);
  el.yAxis = toComponentAxis(yAxis, ctx.locale);
  el.legend = legend;
  el.tooltip = { enabled: tooltip.enabled, valueFormat: makeValueFormatter(tooltip.format, ctx.locale) };
  el.zoom = zoom;
  el.brush = brush;
  el.height = heightPx;

  const sync = bindSeriesProperty(el, series, ctx);
  // Axis labels are BindRefs in the spec; re-push data when they change.
  if (xAxis.label) ctx.registerCleanup(subscribeBindRef(xAxis.label, ctx.store, sync));
  if (yAxis.label) ctx.registerCleanup(subscribeBindRef(yAxis.label, ctx.store, sync));

  bridgeChartEvents(el, [
    ['series-toggle', 'series_toggle'],
    ['point-hover', 'point_hover'],
    ['range-select', 'range_select'],
  ]);

  return el;
}

export function registerDataLineChartRenderer() {
  if (!lookupComponentRenderer(LINE_CHART_TAG)) registerComponentRenderer(LINE_CHART_TAG, renderLineChart);
}
