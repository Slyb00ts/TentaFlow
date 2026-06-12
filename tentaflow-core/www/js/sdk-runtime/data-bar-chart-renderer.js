// =============================================================================
// Plik: sdk-runtime/data-bar-chart-renderer.js
// Opis: Renderery BarChart (0x0217) + StackedBar (0x021A) — mappery na
// komponent <tf-bar-chart>. BarChart = mode='chart' (orientacje vertical/
// horizontal, stacking none/stacked/percent); StackedBar = mode='single'
// (jeden poziomy pasek z segmentami do total). Walidacja CBOR i resolve
// BindRefów w rendererze; całe rysowanie w tf-bar-chart.js.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs
// BarChart (7 pól) + StackedBar (5 pól).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { subscribeBindRef, resolveBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requireU16, requireString, assertOnlyKnownFields,
  parseChartAxis, parseChartLegend,
  ID_RE,
} from './data-chart-shared.js';
import {
  toComponentAxis, bindSeriesProperty, bridgeChartEvents, parseLineChartLikeSeries,
} from './data-line-chart-renderer.js';

export const BAR_CHART_TAG = 0x0217;
export const STACKED_BAR_TAG = 0x021A;
const BAR_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const STACKED_BAR_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const STACK_SEGMENT_KEYS = new Set([0, 1, 2, 3]);
const CHART_ORIENTATIONS = new Set(['vertical', 'horizontal']);
const BAR_STACKING_MODES = new Set(['none', 'stacked', 'percent']);

// =============================================================================
// BarChart (0x0217) — uses <tf-bar-chart mode='chart'>
// =============================================================================

function renderBarChart(component, ctx) {
  assertOnlyKnownFields(component.fields, BAR_CHART_FIELD_KEYS, 'BarChart');

  const series = parseLineChartLikeSeries(component, ctx, 'BarChart');
  const xAxisRaw = ctx.readField(component.fields, 1);
  if (xAxisRaw == null) throw new TypeError('BarChart.x_axis is required');
  const xAxis = parseChartAxis(xAxisRaw, 'BarChart.x_axis', ctx.locale);
  const yAxisRaw = ctx.readField(component.fields, 2);
  if (yAxisRaw == null) throw new TypeError('BarChart.y_axis is required');
  const yAxis = parseChartAxis(yAxisRaw, 'BarChart.y_axis', ctx.locale);
  const orientation = requireEnum(ctx.readField(component.fields, 3), CHART_ORIENTATIONS, 'BarChart.orientation');
  const stacking = requireEnum(ctx.readField(component.fields, 4), BAR_STACKING_MODES, 'BarChart.stacking');
  const legendRaw = ctx.readField(component.fields, 5);
  if (legendRaw == null) throw new TypeError('BarChart.legend is required');
  const legend = parseChartLegend(legendRaw, 'BarChart.legend');
  const heightPx = requireU16(ctx.readField(component.fields, 6), 'BarChart.height_px');
  if (heightPx === 0) throw new TypeError('BarChart.height_px must be > 0');
  if ((stacking === 'stacked' || stacking === 'percent') && yAxis.scale === 'log') {
    throw new TypeError(`BarChart.stacking=${stacking} incompatible with y_axis.scale=log`);
  }
  if (stacking === 'percent' && yAxis.scale !== 'linear') {
    throw new TypeError('BarChart.stacking=percent requires y_axis.scale=linear');
  }
  // Bars are drawn per category; a linear/time/log X axis is treated as a
  // category list of its unique values inside the component.

  const el = document.createElement('tf-bar-chart');
  el.locale = ctx.locale;
  el.xAxis = toComponentAxis(xAxis, ctx.locale);
  el.yAxis = toComponentAxis(yAxis, ctx.locale);
  el.legend = legend;
  el.orientation = orientation;
  el.stacking = stacking;
  el.height = heightPx;

  const sync = bindSeriesProperty(el, series, ctx);
  if (xAxis.label) ctx.registerCleanup(subscribeBindRef(xAxis.label, ctx.store, sync));
  if (yAxis.label) ctx.registerCleanup(subscribeBindRef(yAxis.label, ctx.store, sync));

  bridgeChartEvents(el, [['series-toggle', 'series_toggle']]);

  return el;
}

// =============================================================================
// StackedBar (0x021A) — uses <tf-bar-chart mode='single'>
// =============================================================================

function parseStackSegment(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: StackSegment must be FieldMap`);
  const seen = new Set();
  const s = { id: null, value: null, label: null, tone: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!STACK_SEGMENT_KEYS.has(k)) throw new TypeError(`${ctx}: unknown StackSegment key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: {
        const id = requireString(v, `${ctx}.id`);
        if (!ID_RE.test(id)) throw new TypeError(`${ctx}.id: invalid grammar`);
        s.id = id;
        break;
      }
      case 1: s.value = v; break;
      case 2: if (v != null) s.label = v; break;
      case 3: s.tone = requireEnum(v, TONES, `${ctx}.tone`); break;
    }
  }
  if (s.id == null) throw new TypeError(`${ctx}: id required`);
  if (s.value == null) throw new TypeError(`${ctx}: value required`);
  if (s.tone == null) throw new TypeError(`${ctx}: tone required`);
  return s;
}

function renderStackedBar(component, ctx) {
  assertOnlyKnownFields(component.fields, STACKED_BAR_FIELD_KEYS, 'StackedBar');

  const segmentsRaw = ctx.readField(component.fields, 0);
  if (!Array.isArray(segmentsRaw) || segmentsRaw.length === 0) {
    throw new TypeError('StackedBar.segments: expected non-empty Array<StackSegment>');
  }
  const segments = segmentsRaw.map((s, i) => parseStackSegment(s, `StackedBar.segments[${i}]`));
  const seenIds = new Set();
  for (const seg of segments) {
    if (seenIds.has(seg.id)) throw new TypeError(`StackedBar.segments: duplicate id '${seg.id}'`);
    seenIds.add(seg.id);
  }
  const totalBind = ctx.readField(component.fields, 1);
  if (totalBind == null) throw new TypeError('StackedBar.total is required (BindRef)');
  const showLegend = requireBool(ctx.readField(component.fields, 2), 'StackedBar.show_legend');
  const showPercentages = requireBool(ctx.readField(component.fields, 3), 'StackedBar.show_percentages');
  const heightPx = requireU16(ctx.readField(component.fields, 4), 'StackedBar.height_px');
  if (heightPx === 0) throw new TypeError('StackedBar.height_px must be > 0');

  const el = document.createElement('tf-bar-chart');
  el.mode = 'single';
  el.showLegend = showLegend;
  el.showPercentages = showPercentages;
  el.height = heightPx;

  const sync = () => {
    const total = resolveBindRef(totalBind, ctx.store);
    el.total = typeof total === 'number' && Number.isFinite(total) && total > 0 ? total : 0;
    el.segments = segments.map((seg) => {
      const value = resolveBindRef(seg.value, ctx.store);
      const label = seg.label != null ? resolveBindRef(seg.label, ctx.store) : null;
      return {
        id: seg.id,
        label: label == null || label === '' ? seg.id : String(label),
        value: typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : 0,
        tone: seg.tone,
      };
    });
  };
  sync();
  ctx.registerCleanup(subscribeBindRef(totalBind, ctx.store, sync));
  for (const seg of segments) {
    ctx.registerCleanup(subscribeBindRef(seg.value, ctx.store, sync));
    if (seg.label != null) ctx.registerCleanup(subscribeBindRef(seg.label, ctx.store, sync));
  }

  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerDataBarChartRenderers() {
  if (!lookupComponentRenderer(BAR_CHART_TAG)) registerComponentRenderer(BAR_CHART_TAG, renderBarChart);
  if (!lookupComponentRenderer(STACKED_BAR_TAG)) registerComponentRenderer(STACKED_BAR_TAG, renderStackedBar);
}
