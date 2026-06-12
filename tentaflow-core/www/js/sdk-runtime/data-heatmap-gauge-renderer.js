// =============================================================================
// File: sdk-runtime/data-heatmap-gauge-renderer.js
// Description: Renderers for Heatmap (0x021B) using <tf-heatmap> and Gauge
//              (0x021C) using <tf-gauge> web components. The renderers only
//              validate the CBOR FieldMap and map BindRefs to component
//              attributes/properties; all drawing lives in the components.
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/gauge.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, formatValue, assertBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requireU16, requireF64, requireString,
  requirePath, assertOnlyKnownFields, assertValueFormat,
} from './data-chart-shared.js';

// =============================================================================
// Heatmap (0x021B) — uses <tf-heatmap> web component
// =============================================================================

export const HEATMAP_TAG = 0x021B;
const HEATMAP_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const HEATMAP_LEGEND_POSITIONS = new Set(['top_right', 'bottom', 'none']);
const HEATMAP_ROW_KEYS = new Set([0, 1]);
const HEATMAP_COL_KEYS = new Set([0, 1]);
const HEATMAP_BUCKET_KEYS = new Set([0, 1, 2]);
const HEATMAP_SCALE_KINDS = new Set(['linear', 'logarithmic', 'categorical']);
const ID_RE = /^[a-z0-9_-]{1,64}$/;

function parseHeatmapRow(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: HeatmapRow must be FieldMap`);
  const seen = new Set();
  let id, label;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!HEATMAP_ROW_KEYS.has(k)) throw new TypeError(`${ctx}: unknown HeatmapRow key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) {
      id = requireString(v, `${ctx}.id`);
      if (!ID_RE.test(id)) throw new TypeError(`${ctx}.id: invalid grammar`);
    } else {
      assertBindRef(v, `${ctx}.label`);
      label = v;
    }
  }
  if (id == null) throw new TypeError(`${ctx}: id required`);
  if (label == null) throw new TypeError(`${ctx}: label required`);
  return { id, label };
}
function parseHeatmapColumn(raw, ctx) {
  return parseHeatmapRow(raw, ctx);
}

function parseHeatmapBucket(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: HeatmapBucket must be FieldMap`);
  const seen = new Set();
  let threshold, tone, label = null;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!HEATMAP_BUCKET_KEYS.has(k)) throw new TypeError(`${ctx}: unknown HeatmapBucket key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) threshold = requireF64(v, `${ctx}.threshold`);
    else if (k === 1) tone = requireEnum(v, TONES, `${ctx}.tone`);
    else if (v != null) { assertBindRef(v, `${ctx}.label`); label = v; }
  }
  if (threshold == null) throw new TypeError(`${ctx}: threshold required`);
  if (tone == null) throw new TypeError(`${ctx}: tone required`);
  return { threshold, tone, label };
}

function parseHeatmapScale(raw, ctx) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new TypeError(`${ctx}: HeatmapScale must be object`);
  }
  if (typeof raw.kind !== 'string' || !HEATMAP_SCALE_KINDS.has(raw.kind)) {
    throw new TypeError(`${ctx}.kind invalid: ${raw.kind}`);
  }
  if (raw.kind === 'linear') {
    for (const k of Object.keys(raw)) {
      if (!['kind', 'min', 'max', 'color_from', 'color_to'].includes(k)) {
        throw new TypeError(`${ctx}: unexpected key '${k}' for kind=linear`);
      }
    }
    const min = requireF64(raw.min, `${ctx}.min`);
    const max = requireF64(raw.max, `${ctx}.max`);
    if (min >= max) throw new TypeError(`${ctx}: min must be < max`);
    const color_from = requireEnum(raw.color_from, TONES, `${ctx}.color_from`);
    const color_to = requireEnum(raw.color_to, TONES, `${ctx}.color_to`);
    return { kind: 'linear', min, max, color_from, color_to };
  }
  if (raw.kind === 'logarithmic') {
    for (const k of Object.keys(raw)) {
      if (!['kind', 'min', 'max', 'base'].includes(k)) {
        throw new TypeError(`${ctx}: unexpected key '${k}' for kind=logarithmic`);
      }
    }
    const min = requireF64(raw.min, `${ctx}.min`);
    const max = requireF64(raw.max, `${ctx}.max`);
    const base = requireF64(raw.base, `${ctx}.base`);
    if (min <= 0 || max <= 0) throw new TypeError(`${ctx}: min/max must be > 0 for logarithmic`);
    if (min >= max) throw new TypeError(`${ctx}: min must be < max`);
    if (base <= 1) throw new TypeError(`${ctx}.base must be > 1`);
    return { kind: 'logarithmic', min, max, base };
  }
  // categorical
  for (const k of Object.keys(raw)) {
    if (!['kind', 'buckets'].includes(k)) {
      throw new TypeError(`${ctx}: unexpected key '${k}' for kind=categorical`);
    }
  }
  if (!Array.isArray(raw.buckets) || raw.buckets.length === 0) {
    throw new TypeError(`${ctx}.buckets must be non-empty array`);
  }
  const buckets = raw.buckets.map((b, i) => parseHeatmapBucket(b, `${ctx}.buckets[${i}]`));
  for (let i = 1; i < buckets.length; i++) {
    if (buckets[i].threshold < buckets[i - 1].threshold) {
      throw new TypeError(`${ctx}.buckets must be sorted by threshold ascending`);
    }
  }
  return { kind: 'categorical', buckets };
}

function renderHeatmap(component, ctx) {
  assertOnlyKnownFields(component.fields, HEATMAP_FIELD_KEYS, 'Heatmap');

  const rowsRaw = ctx.readField(component.fields, 0);
  const rows = rowsRaw == null ? [] : (() => {
    if (!Array.isArray(rowsRaw)) throw new TypeError('Heatmap.rows: expected Array<HeatmapRow>');
    return rowsRaw.map((r, i) => parseHeatmapRow(r, `Heatmap.rows[${i}]`));
  })();
  const colsRaw = ctx.readField(component.fields, 1);
  const columns = colsRaw == null ? [] : (() => {
    if (!Array.isArray(colsRaw)) throw new TypeError('Heatmap.columns: expected Array<HeatmapColumn>');
    return colsRaw.map((c, i) => parseHeatmapColumn(c, `Heatmap.columns[${i}]`));
  })();
  const rowIds = new Set();
  for (const r of rows) {
    if (rowIds.has(r.id)) throw new TypeError(`Heatmap.rows: duplicate id '${r.id}'`);
    rowIds.add(r.id);
  }
  const colIds = new Set();
  for (const c of columns) {
    if (colIds.has(c.id)) throw new TypeError(`Heatmap.columns: duplicate id '${c.id}'`);
    colIds.add(c.id);
  }
  const cellsPath = requirePath(ctx.readField(component.fields, 2), 'Heatmap.cells_path');
  const scaleRaw = ctx.readField(component.fields, 3);
  if (scaleRaw == null) throw new TypeError('Heatmap.scale is required');
  const scale = parseHeatmapScale(scaleRaw, 'Heatmap.scale');
  const legendPosition = requireEnum(ctx.readField(component.fields, 4), HEATMAP_LEGEND_POSITIONS, 'Heatmap.legend_position');
  const cellSizePx = requireU16(ctx.readField(component.fields, 5), 'Heatmap.cell_size_px');
  if (cellSizePx === 0) throw new TypeError('Heatmap.cell_size_px must be > 0');
  const tooltipEnabled = requireBool(ctx.readField(component.fields, 6), 'Heatmap.tooltip');

  // <tf-heatmap> web component
  const heatmap = document.createElement('tf-heatmap');
  heatmap.showLegend = (legendPosition !== 'none');
  heatmap.rows = rows.length;
  heatmap.cols = columns.length;

  // Resolve row/column labels reactively
  const rowLabelStrs = rows.map(() => '');
  const colLabelStrs = columns.map(() => '');

  const rebuildLabels = () => {
    for (let i = 0; i < rows.length; i++) {
      const v = resolveBindRef(rows[i].label, ctx.store);
      rowLabelStrs[i] = v == null ? rows[i].id : String(v);
    }
    for (let i = 0; i < columns.length; i++) {
      const v = resolveBindRef(columns[i].label, ctx.store);
      colLabelStrs[i] = v == null ? columns[i].id : String(v);
    }
    heatmap.rowLabels = [...rowLabelStrs];
    heatmap.colLabels = [...colLabelStrs];
  };
  rebuildLabels();
  for (const row of rows) {
    ctx.registerCleanup(subscribeBindRef(row.label, ctx.store, rebuildLabels));
  }
  for (const col of columns) {
    ctx.registerCleanup(subscribeBindRef(col.label, ctx.store, rebuildLabels));
  }

  // Build row/col index maps for cell lookup
  const rowIdxMap = new Map(rows.map((r, i) => [r.id, i]));
  const colIdxMap = new Map(columns.map((c, i) => [c.id, i]));

  // Reactive cells: read from store, convert to 2D values array
  const rebuildCells = () => {
    let cellsArr;
    try { cellsArr = ctx.store.read(cellsPath); } catch { cellsArr = undefined; }
    if (!Array.isArray(cellsArr)) {
      heatmap.values = [];
      return;
    }
    // Build 2D array [row][col]
    const grid = Array.from({ length: rows.length }, () =>
      Array.from({ length: columns.length }, () => 0)
    );
    for (const cell of cellsArr) {
      if (cell == null || typeof cell !== 'object') continue;
      const rIdx = rowIdxMap.get(cell.row_id);
      const cIdx = colIdxMap.get(cell.col_id);
      if (rIdx == null || cIdx == null) continue;
      const v = typeof cell.value === 'number' && Number.isFinite(cell.value)
        ? cell.value : 0;
      // Normalize to 0..1 for tf-heatmap level buckets
      if (scale.kind === 'linear') {
        grid[rIdx][cIdx] = (v - scale.min) / (scale.max - scale.min);
      } else if (scale.kind === 'logarithmic') {
        if (v > 0) {
          const lm = Math.log(scale.min) / Math.log(scale.base);
          const lx = Math.log(scale.max) / Math.log(scale.base);
          const lv = Math.log(v) / Math.log(scale.base);
          grid[rIdx][cIdx] = (lv - lm) / (lx - lm);
        }
      } else {
        // categorical: normalize by bucket index
        let bucketIdx = 0;
        for (let b = 0; b < scale.buckets.length; b++) {
          if (v <= scale.buckets[b].threshold) { bucketIdx = b; break; }
          bucketIdx = b;
        }
        grid[rIdx][cIdx] = scale.buckets.length > 1
          ? bucketIdx / (scale.buckets.length - 1) : 0;
      }
    }
    heatmap.values = grid;
  };
  rebuildCells();
  ctx.registerCleanup(ctx.store.subscribe(cellsPath, rebuildCells));

  // Forward cell-click events (tf-heatmap calls back with a single object)
  heatmap.onCellClick = ({ row, col }) => {
    const rowId = rows[row] ? rows[row].id : String(row);
    const colId = columns[col] ? columns[col].id : String(col);
    heatmap.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('cell_click', {
        bubbles: false,
        detail: { row_id: rowId, col_id: colId },
      })
    );
  };

  return heatmap;
}

// =============================================================================
// Gauge (0x021C) — uses <tf-gauge> web component
// =============================================================================

export const GAUGE_TAG = 0x021C;
const GAUGE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);
const GAUGE_VARIANTS = new Set(['circular', 'arc', 'semi']);
const GAUGE_THRESHOLD_KEYS = new Set([0, 1, 2]);

function parseGaugeThreshold(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: GaugeThreshold must be FieldMap`);
  const seen = new Set();
  let value, tone, label = null;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!GAUGE_THRESHOLD_KEYS.has(k)) throw new TypeError(`${ctx}: unknown GaugeThreshold key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) value = requireF64(v, `${ctx}.value`);
    else if (k === 1) tone = requireEnum(v, TONES, `${ctx}.tone`);
    else if (v != null) { assertBindRef(v, `${ctx}.label`); label = v; }
  }
  if (value == null) throw new TypeError(`${ctx}: value required`);
  if (tone == null) throw new TypeError(`${ctx}: tone required`);
  return { value, tone, label };
}

function renderGauge(component, ctx) {
  assertOnlyKnownFields(component.fields, GAUGE_FIELD_KEYS, 'Gauge');

  const valueBind = ctx.readField(component.fields, 0);
  if (valueBind == null) throw new TypeError('Gauge.value is required (BindRef)');
  const min = requireF64(ctx.readField(component.fields, 1), 'Gauge.min');
  const max = requireF64(ctx.readField(component.fields, 2), 'Gauge.max');
  if (min >= max) throw new TypeError('Gauge.min must be < max');
  const thresholdsRaw = ctx.readField(component.fields, 3);
  const thresholds = thresholdsRaw == null ? [] : (() => {
    if (!Array.isArray(thresholdsRaw)) throw new TypeError('Gauge.thresholds: expected Array<GaugeThreshold>');
    return thresholdsRaw.map((t, i) => parseGaugeThreshold(t, `Gauge.thresholds[${i}]`));
  })();
  const variant = requireEnum(ctx.readField(component.fields, 4), GAUGE_VARIANTS, 'Gauge.variant');
  const labelBind = ctx.readField(component.fields, 5);
  const format = ctx.readField(component.fields, 6);
  assertValueFormat(format, 'Gauge.format', ctx.locale);
  const sizePx = requireU16(ctx.readField(component.fields, 7), 'Gauge.size_px');
  if (sizePx === 0) throw new TypeError('Gauge.size_px must be > 0');

  // <tf-gauge> web component — renderer only maps validated fields onto it.
  const el = document.createElement('tf-gauge');
  el.setAttribute('min', String(min));
  el.setAttribute('max', String(max));
  el.setAttribute('variant', variant);
  el.setAttribute('size', String(sizePx));

  // Thresholds: resolve label BindRefs to strings; re-push on label changes.
  const applyThresholds = () => {
    el.thresholds = thresholds.map((th) => {
      let label = null;
      if (th.label != null) {
        const v = resolveBindRef(th.label, ctx.store);
        label = v == null ? String(th.value) : String(v);
      }
      return { value: th.value, tone: th.tone, label };
    });
  };
  applyThresholds();
  for (const th of thresholds) {
    if (th.label != null) {
      ctx.registerCleanup(subscribeBindRef(th.label, ctx.store, applyThresholds));
    }
  }

  if (labelBind != null) {
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      el.setAttribute('label', v == null ? '' : String(v));
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
  }

  const apply = () => {
    const raw = resolveBindRef(valueBind, ctx.store);
    if (raw == null) {
      // Absent attribute = empty state (muted tone, no aria-invalid).
      el.removeAttribute('display-value');
      el.removeAttribute('value');
      return;
    }
    if (typeof raw !== 'number' || !Number.isFinite(raw)) {
      // Non-finite attribute = invalid state (critical tone + aria-invalid).
      el.removeAttribute('display-value');
      el.setAttribute('value', 'NaN');
      return;
    }
    const clamped = Math.max(min, Math.min(max, raw));
    if (format) el.setAttribute('display-value', formatValue(clamped, format, ctx.locale));
    else el.removeAttribute('display-value');
    el.setAttribute('value', String(raw));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(valueBind, ctx.store, apply));

  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerDataHeatmapGaugeRenderers() {
  if (!lookupComponentRenderer(HEATMAP_TAG)) registerComponentRenderer(HEATMAP_TAG, renderHeatmap);
  if (!lookupComponentRenderer(GAUGE_TAG)) registerComponentRenderer(GAUGE_TAG, renderGauge);
}
