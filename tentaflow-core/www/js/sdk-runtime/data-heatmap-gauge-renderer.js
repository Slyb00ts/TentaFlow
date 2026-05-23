// =============================================================================
// Plik: sdk-runtime/data-heatmap-gauge-renderer.js
// Opis: Renderery Heatmap (0x021B) + Gauge (0x021C) — chunk 3.3d-12.
//
// Heatmap: SVG grid rows×columns. Cells colored przez HeatmapScale
// (linear/logarithmic/categorical). Linear: interpolacja koloru między
// from→to. Logarithmic: log mapping. Categorical: bucket lookup po
// threshold. Hover → cell_hover event, click → cell_click event.
//
// Gauge: SVG arc (circular = pełny krąg, arc = 270°, semi = 180°). Value
// rysowane jako fill arc do current value. Thresholds: tick marks na arc.
// Center label + value text z optional ValueFormat.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/gauge.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, formatValue, assertBindRef } from './bind-resolver.js';
import {
  SVG_NS, TONES,
  requireEnum, requireBool, requireU16, requireF64, requireString,
  requirePath, assertOnlyKnownFields, assertValueFormat,
} from './data-chart-shared.js';

// =============================================================================
// Heatmap (0x021B)
// =============================================================================

export const HEATMAP_TAG = 0x021B;
const HEATMAP_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const HEATMAP_LEGEND_POSITIONS = new Set(['top_right', 'bottom', 'none']);
const HEATMAP_ROW_KEYS = new Set([0, 1]);
const HEATMAP_COL_KEYS = new Set([0, 1]);
const HEATMAP_BUCKET_KEYS = new Set([0, 1, 2]);
const HEATMAP_SCALE_KINDS = new Set(['linear', 'logarithmic', 'categorical']);
const ID_RE = /^[a-z0-9_-]{1,64}$/;
// Tone → fallback hex dla interpolacji koloru w Heatmap linear scale.
const TONE_TO_HEX = {
  neutral: '#374151',
  primary: '#1e40af',
  info: '#1e40af',
  success: '#15803d',
  warning: '#b45309',
  critical: '#b91c1c',
  muted: '#9ca3af',
};

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
  // Buckets sorted po threshold ascending — spec'owa konwencja dla
  // deterministic lookup.
  for (let i = 1; i < buckets.length; i++) {
    if (buckets[i].threshold < buckets[i - 1].threshold) {
      throw new TypeError(`${ctx}.buckets must be sorted by threshold ascending`);
    }
  }
  return { kind: 'categorical', buckets };
}

/// Hex color interpolation between two hex strings, t ∈ 0..1.
function interpolateHex(fromHex, toHex, t) {
  const tt = Math.max(0, Math.min(1, t));
  const f = parseHex(fromHex);
  const g = parseHex(toHex);
  if (!f || !g) return fromHex;
  const r = Math.round(f.r + (g.r - f.r) * tt);
  const g2 = Math.round(f.g + (g.g - f.g) * tt);
  const b = Math.round(f.b + (g.b - f.b) * tt);
  return `#${r.toString(16).padStart(2, '0')}${g2.toString(16).padStart(2, '0')}${b.toString(16).padStart(2, '0')}`;
}
function parseHex(h) {
  if (typeof h !== 'string') return null;
  const m = h.match(/^#?([0-9a-fA-F]{6})$/);
  if (!m) return null;
  const v = m[1];
  return { r: parseInt(v.slice(0, 2), 16), g: parseInt(v.slice(2, 4), 16), b: parseInt(v.slice(4, 6), 16) };
}

/// Wylicza kolor cell dla danej wartości w danej skali. Wraca {fill, label}.
function colorForValue(value, scale) {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return { fill: 'transparent', label: null };
  }
  if (scale.kind === 'linear') {
    const t = (value - scale.min) / (scale.max - scale.min);
    return {
      fill: interpolateHex(TONE_TO_HEX[scale.color_from], TONE_TO_HEX[scale.color_to], t),
      label: null,
    };
  }
  if (scale.kind === 'logarithmic') {
    if (value <= 0) return { fill: 'transparent', label: null };
    const lm = Math.log(scale.min) / Math.log(scale.base);
    const lx = Math.log(scale.max) / Math.log(scale.base);
    const lv = Math.log(value) / Math.log(scale.base);
    const t = (lv - lm) / (lx - lm);
    return {
      fill: interpolateHex(TONE_TO_HEX.muted, TONE_TO_HEX.primary, t),
      label: null,
    };
  }
  // categorical — najnizszy bucket którego threshold >= value (lub last gdy wszystkie mniejsze).
  let chosen = scale.buckets[0];
  for (const b of scale.buckets) {
    if (value <= b.threshold) { chosen = b; break; }
    chosen = b;
  }
  return { fill: TONE_TO_HEX[chosen.tone], label: chosen.label };
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
  // Duplicate id detection.
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-heatmap');
  wrapper.classList.add(`tf-heatmap--legend-${legendPosition}`);

  // Bottom legend renderujemy POD heatmap'em; top_right z prawej obok grid'u
  // jako overlay.
  const layoutRoot = document.createElement('div');
  layoutRoot.classList.add('tf-heatmap__layout');
  wrapper.appendChild(layoutRoot);

  const labelW = 80;
  const labelH = 20;
  const gridW = columns.length * cellSizePx;
  const gridH = rows.length * cellSizePx;
  const svgW = labelW + gridW + 4;
  const svgH = labelH + gridH + 4;
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${svgW} ${svgH}`);
  svg.setAttribute('width', String(svgW));
  svg.setAttribute('height', String(svgH));
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', 'Heatmap');
  svg.classList.add('tf-heatmap__svg');
  layoutRoot.appendChild(svg);

  let tooltipEl = null;
  if (tooltipEnabled) {
    tooltipEl = document.createElement('div');
    tooltipEl.classList.add('tf-heatmap__tooltip');
    tooltipEl.hidden = true;
    wrapper.appendChild(tooltipEl);
  }

  // Column labels (top).
  for (let ci = 0; ci < columns.length; ci++) {
    const col = columns[ci];
    const tx = labelW + ci * cellSizePx + cellSizePx / 2;
    const txt = document.createElementNS(SVG_NS, 'text');
    txt.setAttribute('x', String(tx));
    txt.setAttribute('y', String(labelH - 4));
    txt.setAttribute('text-anchor', 'middle');
    txt.classList.add('tf-heatmap__col-label');
    const apply = () => {
      const v = resolveBindRef(col.label, ctx.store);
      txt.textContent = v == null ? col.id : String(v);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(col.label, ctx.store, apply));
    svg.appendChild(txt);
  }
  // Row labels (left).
  for (let ri = 0; ri < rows.length; ri++) {
    const row = rows[ri];
    const ty = labelH + ri * cellSizePx + cellSizePx / 2;
    const txt = document.createElementNS(SVG_NS, 'text');
    txt.setAttribute('x', String(labelW - 6));
    txt.setAttribute('y', String(ty));
    txt.setAttribute('text-anchor', 'end');
    txt.setAttribute('dominant-baseline', 'middle');
    txt.classList.add('tf-heatmap__row-label');
    const apply = () => {
      const v = resolveBindRef(row.label, ctx.store);
      txt.textContent = v == null ? row.id : String(v);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(row.label, ctx.store, apply));
    svg.appendChild(txt);
  }

  // Cells group — rebuild gdy cells_path się zmieni.
  const cellsGroup = document.createElementNS(SVG_NS, 'g');
  cellsGroup.classList.add('tf-heatmap__cells');
  cellsGroup.setAttribute('transform', `translate(${labelW}, ${labelH})`);
  svg.appendChild(cellsGroup);

  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const rowIdxMap = new Map(rows.map((r, i) => [r.id, i]));
  const colIdxMap = new Map(columns.map((c, i) => [c.id, i]));

  const rebuild = () => {
    runRebuildCleanups();
    cellsGroup.replaceChildren();
    let cellsArr;
    try { cellsArr = ctx.store.read(cellsPath); } catch { cellsArr = undefined; }
    if (!Array.isArray(cellsArr)) return;
    for (const cell of cellsArr) {
      if (cell == null || typeof cell !== 'object') continue;
      const rIdx = rowIdxMap.get(cell.row_id);
      const cIdx = colIdxMap.get(cell.col_id);
      if (rIdx == null || cIdx == null) continue;
      const v = cell.value;
      const { fill } = colorForValue(v, scale);
      const rect = document.createElementNS(SVG_NS, 'rect');
      rect.setAttribute('x', String(cIdx * cellSizePx));
      rect.setAttribute('y', String(rIdx * cellSizePx));
      rect.setAttribute('width', String(cellSizePx));
      rect.setAttribute('height', String(cellSizePx));
      rect.setAttribute('fill', fill);
      rect.classList.add('tf-heatmap__cell');
      rect.setAttribute('data-row-id', cell.row_id);
      rect.setAttribute('data-col-id', cell.col_id);
      rect.setAttribute('data-value', String(v));
      const aria = `${cell.row_id} × ${cell.col_id}: ${v}`;
      rect.setAttribute('aria-label', aria);

      const onClick = (e) => {
        e.preventDefault();
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('cell_click', {
            bubbles: false,
            detail: { row_id: cell.row_id, col_id: cell.col_id, value: v },
          })
        );
      };
      rect.addEventListener('click', onClick);
      rebuildCleanups.push(() => rect.removeEventListener('click', onClick));

      if (tooltipEnabled && tooltipEl) {
        const onEnter = (e) => {
          tooltipEl.textContent = aria;
          tooltipEl.hidden = false;
          const svgRect = svg.getBoundingClientRect();
          tooltipEl.style.left = `${e.clientX - svgRect.left + 12}px`;
          tooltipEl.style.top = `${e.clientY - svgRect.top + 12}px`;
          wrapper.dispatchEvent(
            new (globalThis.CustomEvent || globalThis.Event)('cell_hover', {
              bubbles: false,
              detail: { row_id: cell.row_id, col_id: cell.col_id, value: v },
            })
          );
        };
        const onLeave = () => { tooltipEl.hidden = true; };
        rect.addEventListener('mouseenter', onEnter);
        rect.addEventListener('mouseleave', onLeave);
        rebuildCleanups.push(() => {
          rect.removeEventListener('mouseenter', onEnter);
          rect.removeEventListener('mouseleave', onLeave);
        });
      }
      cellsGroup.appendChild(rect);
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(cellsPath, rebuild));

  if (legendPosition !== 'none') {
    const legend = document.createElement('div');
    legend.classList.add('tf-heatmap__legend');
    legend.classList.add(`tf-heatmap__legend--position-${legendPosition}`);
    if (scale.kind === 'linear') {
      const grad = document.createElement('div');
      grad.classList.add('tf-heatmap__legend-gradient');
      grad.style.background = `linear-gradient(to right, ${TONE_TO_HEX[scale.color_from]}, ${TONE_TO_HEX[scale.color_to]})`;
      legend.appendChild(grad);
      const minLbl = document.createElement('span');
      minLbl.classList.add('tf-heatmap__legend-tick');
      minLbl.textContent = String(scale.min);
      legend.appendChild(minLbl);
      const maxLbl = document.createElement('span');
      maxLbl.classList.add('tf-heatmap__legend-tick');
      maxLbl.textContent = String(scale.max);
      legend.appendChild(maxLbl);
    } else if (scale.kind === 'logarithmic') {
      const grad = document.createElement('div');
      grad.classList.add('tf-heatmap__legend-gradient');
      grad.style.background = `linear-gradient(to right, ${TONE_TO_HEX.muted}, ${TONE_TO_HEX.primary})`;
      legend.appendChild(grad);
      const lbl = document.createElement('span');
      lbl.classList.add('tf-heatmap__legend-tick');
      lbl.textContent = `${scale.min}..${scale.max} (log${scale.base})`;
      legend.appendChild(lbl);
    } else {
      // categorical: per-bucket swatch.
      for (const b of scale.buckets) {
        const item = document.createElement('span');
        item.classList.add('tf-heatmap__legend-bucket');
        const sw = document.createElement('span');
        sw.classList.add('tf-heatmap__legend-swatch');
        sw.style.background = TONE_TO_HEX[b.tone];
        item.appendChild(sw);
        const txt = document.createElement('span');
        txt.classList.add('tf-heatmap__legend-bucket-label');
        if (b.label != null) {
          const apply = () => {
            const v = resolveBindRef(b.label, ctx.store);
            txt.textContent = v == null ? `≤ ${b.threshold}` : String(v);
          };
          apply();
          ctx.registerCleanup(subscribeBindRef(b.label, ctx.store, apply));
        } else {
          txt.textContent = `≤ ${b.threshold}`;
        }
        item.appendChild(txt);
        legend.appendChild(item);
      }
    }
    wrapper.appendChild(legend);
  }

  return wrapper;
}

// =============================================================================
// Gauge (0x021C)
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

/// Arc span dla każdego variant (radiany).
function gaugeArcSpan(variant) {
  if (variant === 'circular') return Math.PI * 2;
  if (variant === 'arc') return Math.PI * 1.5;  // 270°
  return Math.PI;  // semi = 180°
}

function gaugeArcStart(variant) {
  // Circular: top (-90°). Arc: 135° (bottom-left). Semi: 180° (left).
  if (variant === 'circular') return -Math.PI / 2;
  if (variant === 'arc') return Math.PI * 0.75;
  return Math.PI;
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-gauge');
  wrapper.classList.add(`tf-gauge--variant-${variant}`);
  wrapper.style.width = `${sizePx}px`;
  wrapper.style.height = `${sizePx}px`;

  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${sizePx} ${sizePx}`);
  svg.setAttribute('width', String(sizePx));
  svg.setAttribute('height', String(sizePx));
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', 'Gauge');
  svg.classList.add('tf-gauge__svg');
  wrapper.appendChild(svg);

  const cx = sizePx / 2;
  const cy = sizePx / 2;
  const radius = sizePx * 0.4;
  const strokeW = sizePx * 0.08;

  const arcSpan = gaugeArcSpan(variant);
  const arcStart = gaugeArcStart(variant);

  // Background arc (track).
  const trackPath = describeArc(cx, cy, radius, arcStart, arcStart + arcSpan);
  const track = document.createElementNS(SVG_NS, 'path');
  track.setAttribute('d', trackPath);
  track.setAttribute('fill', 'none');
  track.setAttribute('stroke-width', String(strokeW));
  track.classList.add('tf-gauge__track');
  svg.appendChild(track);

  // Value arc (foreground) — updates reactive.
  const valueArc = document.createElementNS(SVG_NS, 'path');
  valueArc.setAttribute('fill', 'none');
  valueArc.setAttribute('stroke-width', String(strokeW));
  valueArc.classList.add('tf-gauge__value-arc');
  svg.appendChild(valueArc);

  // Threshold ticks: per threshold, mała kreska na arc.
  for (const th of thresholds) {
    const ratio = (th.value - min) / (max - min);
    if (ratio < 0 || ratio > 1) continue;
    const angle = arcStart + arcSpan * ratio;
    const inner = radius - strokeW * 0.6;
    const outer = radius + strokeW * 0.6;
    const x1 = cx + Math.cos(angle) * inner;
    const y1 = cy + Math.sin(angle) * inner;
    const x2 = cx + Math.cos(angle) * outer;
    const y2 = cy + Math.sin(angle) * outer;
    const tick = document.createElementNS(SVG_NS, 'line');
    tick.setAttribute('x1', String(x1));
    tick.setAttribute('y1', String(y1));
    tick.setAttribute('x2', String(x2));
    tick.setAttribute('y2', String(y2));
    tick.classList.add('tf-gauge__threshold');
    tick.classList.add(`tf-gauge__threshold--tone-${th.tone}`);
    if (th.label != null) {
      const title = document.createElementNS(SVG_NS, 'title');
      const applyLbl = () => {
        const v = resolveBindRef(th.label, ctx.store);
        title.textContent = v == null ? `${th.value}` : `${v}`;
        tick.setAttribute('aria-label', title.textContent);
      };
      applyLbl();
      ctx.registerCleanup(subscribeBindRef(th.label, ctx.store, applyLbl));
      tick.appendChild(title);
    }
    svg.appendChild(tick);
  }

  // Center value text.
  const valueText = document.createElementNS(SVG_NS, 'text');
  valueText.setAttribute('x', String(cx));
  valueText.setAttribute('y', String(cy));
  valueText.setAttribute('text-anchor', 'middle');
  valueText.setAttribute('dominant-baseline', 'middle');
  valueText.classList.add('tf-gauge__value-text');
  svg.appendChild(valueText);

  let labelText = null;
  if (labelBind != null) {
    labelText = document.createElementNS(SVG_NS, 'text');
    labelText.setAttribute('x', String(cx));
    labelText.setAttribute('y', String(cy + sizePx * 0.12));
    labelText.setAttribute('text-anchor', 'middle');
    labelText.classList.add('tf-gauge__label');
    const apply = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      labelText.textContent = v == null ? '' : String(v);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, apply));
    svg.appendChild(labelText);
  }

  // Reactive value update.
  const apply = () => {
    const raw = resolveBindRef(valueBind, ctx.store);
    // Brak danych / non-number → empty arc + '—' (np. przed pierwszym snapshotem).
    // Brak danych albo non-finite (NaN/Infinity z addona) → visible error
    // state. Renderer NIE throw'uje w callback'u subscribe, bo StateStore
    // swallow'uje exceptions z subscriberów (only logs) — to oznaczałoby
    // stale gauge i niewidzialny błąd. Zamiast tego pokazujemy '—' z
    // critical tone, plus aria-invalid dla a11y readers.
    const invalid = (raw == null) || typeof raw !== 'number' || !Number.isFinite(raw);
    if (invalid) {
      valueArc.setAttribute('d', `M ${cx + radius * Math.cos(arcStart)} ${cy + radius * Math.sin(arcStart)}`);
      const tone = raw == null ? 'muted' : 'critical';
      valueArc.setAttribute('class', `tf-gauge__value-arc tf-gauge__value-arc--tone-${tone}`);
      valueText.textContent = '—';
      svg.setAttribute('aria-label', `— (${min}-${max})`);
      if (raw != null) svg.setAttribute('aria-invalid', 'true');
      else svg.removeAttribute('aria-invalid');
      return;
    }
    svg.removeAttribute('aria-invalid');
    const clamped = Math.max(min, Math.min(max, raw));
    const ratio = (clamped - min) / (max - min);
    const endAngle = arcStart + arcSpan * ratio;
    valueArc.setAttribute('d', describeArc(cx, cy, radius, arcStart, endAngle));
    let tone = 'primary';
    for (const th of thresholds) {
      if (clamped >= th.value) tone = th.tone;
    }
    valueArc.setAttribute('class', `tf-gauge__value-arc tf-gauge__value-arc--tone-${tone}`);
    valueText.textContent = format ? formatValue(clamped, format, ctx.locale) : String(clamped);
    svg.setAttribute('aria-label', `${valueText.textContent} (${min}-${max})`);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(valueBind, ctx.store, apply));

  return wrapper;
}

/// Describes SVG arc path from startAngle to endAngle (radiany; 0 = positive
/// X axis, clockwise). Handles full-circle correctly (split na dwa pół-łuki).
function describeArc(cx, cy, r, startAngle, endAngle) {
  const sweepAngle = endAngle - startAngle;
  if (Math.abs(sweepAngle) < 1e-6) {
    // Empty arc — just a moveto.
    const x = cx + r * Math.cos(startAngle);
    const y = cy + r * Math.sin(startAngle);
    return `M ${x} ${y}`;
  }
  // Full circle: split into two arcs.
  if (Math.abs(sweepAngle) >= Math.PI * 2 - 1e-6) {
    const midAngle = startAngle + Math.PI;
    const x0 = cx + r * Math.cos(startAngle);
    const y0 = cy + r * Math.sin(startAngle);
    const xMid = cx + r * Math.cos(midAngle);
    const yMid = cy + r * Math.sin(midAngle);
    return `M ${x0} ${y0} A ${r} ${r} 0 1 1 ${xMid} ${yMid} A ${r} ${r} 0 1 1 ${x0} ${y0}`;
  }
  const x0 = cx + r * Math.cos(startAngle);
  const y0 = cy + r * Math.sin(startAngle);
  const x1 = cx + r * Math.cos(endAngle);
  const y1 = cy + r * Math.sin(endAngle);
  const largeArc = Math.abs(sweepAngle) > Math.PI ? 1 : 0;
  const sweepFlag = sweepAngle > 0 ? 1 : 0;
  return `M ${x0} ${y0} A ${r} ${r} 0 ${largeArc} ${sweepFlag} ${x1} ${y1}`;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataHeatmapGaugeRenderers() {
  if (!lookupComponentRenderer(HEATMAP_TAG)) registerComponentRenderer(HEATMAP_TAG, renderHeatmap);
  if (!lookupComponentRenderer(GAUGE_TAG)) registerComponentRenderer(GAUGE_TAG, renderGauge);
}
