// =============================================================================
// Plik: sdk-runtime/data-chart-shared.js
// Opis: Współdzielone helpery dla rendererów charts (LineChart/AreaChart/
// BarChart/StackedBar). Parsing inline structs (ChartSeries, ChartAxis,
// ChartLegend, ChartTooltip), generacja ticków, build axes/legend/tooltip
// DOM. Eksport używany przez data-{line,area,bar,stacked}-chart-renderer.js.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/inline.rs (chart family) +
// tokens.rs (chart enums).
// =============================================================================

import { resolveBindRef, subscribeBindRef, formatValue } from './bind-resolver.js';

export const SVG_NS = 'http://www.w3.org/2000/svg';
export const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);
export const CHART_SERIES_STYLES = new Set(['solid', 'dashed', 'dotted']);
export const CHART_AXIS_SCALES = new Set(['linear', 'log', 'time', 'category']);
export const CHART_LEGEND_POSITIONS = new Set(['top', 'bottom', 'left', 'right', 'none']);
export const CHART_LEGEND_ALIGNS = new Set(['start', 'center', 'end']);
export const CHART_ZOOM_MODES = new Set(['none', 'x', 'y', 'both']);
export const VALUE_FORMAT_KINDS = new Set([
  'number', 'currency', 'percent', 'bytes', 'duration',
  'date', 'time', 'datetime', 'relative', 'plain',
]);
// Mirror Rust value_format.rs enum variants — kazdy wariant ma exact key set.
// Currency: tylko `code` (NIE decimals). Walidator odrzuca obce klucze.
export const VALUE_FORMAT_VARIANT_KEYS = {
  plain:    new Set(['kind']),
  number:   new Set(['kind', 'decimals', 'thousands_sep']),
  currency: new Set(['kind', 'code']),
  percent:  new Set(['kind', 'decimals']),
  bytes:    new Set(['kind', 'base']),
  duration: new Set(['kind', 'style']),
  date:     new Set(['kind', 'style']),
  time:     new Set(['kind', 'style']),
  datetime: new Set(['kind', 'style']),
  relative: new Set(['kind']),
};
export const ID_RE = /^[a-z0-9_-]{1,64}$/;
const CHART_SERIES_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const CHART_AXIS_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const CHART_LEGEND_KEYS = new Set([0, 1]);
const CHART_TOOLTIP_KEYS = new Set([0, 1]);

export function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
export function requireBool(v, ctx) {
  if (typeof v !== 'boolean') throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  return v;
}
export function requireU8(v, ctx) {
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
export function requireU16(v, ctx) {
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) throw new TypeError(`${ctx}: expected u16, got ${v}`);
  return v;
}
export function requireF64(v, ctx) {
  if (typeof v !== 'number' || !Number.isFinite(v)) throw new TypeError(`${ctx}: expected finite f64, got ${v}`);
  return v;
}
export function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
export function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
export function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

export function assertValueFormat(fmt, ctx, locale) {
  if (fmt == null) return;
  if (typeof fmt !== 'object' || Array.isArray(fmt)) {
    throw new TypeError(`${ctx}: ValueFormat must be object`);
  }
  if (typeof fmt.kind !== 'string' || !VALUE_FORMAT_KINDS.has(fmt.kind)) {
    throw new TypeError(`${ctx}: ValueFormat.kind invalid: ${fmt.kind}`);
  }
  const allowed = VALUE_FORMAT_VARIANT_KEYS[fmt.kind];
  for (const k of Object.keys(fmt)) {
    if (!allowed.has(k)) throw new TypeError(`${ctx}: unexpected key '${k}' for kind=${fmt.kind}`);
  }
  try { formatValue(0, fmt, locale); }
  catch (err) {
    throw new TypeError(`${ctx}: invalid ValueFormat — ${err && err.message ? err.message : err}`);
  }
}

// =============================================================================
// ChartSeries / ChartAxis / ChartLegend / ChartTooltip parsing
// =============================================================================

export function parseChartSeries(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: ChartSeries must be FieldMap`);
  const seen = new Set();
  const s = { id: null, name: null, data_path: null, tone: null, style: null, show_in_legend: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!CHART_SERIES_KEYS.has(k)) throw new TypeError(`${ctx}: unknown ChartSeries key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: {
        const id = requireString(v, `${ctx}.id`);
        if (!ID_RE.test(id)) throw new TypeError(`${ctx}.id: invalid grammar`);
        s.id = id;
        break;
      }
      case 1: s.name = v; break;
      case 2: s.data_path = requirePath(v, `${ctx}.data_path`); break;
      case 3: if (v != null) s.tone = requireEnum(v, TONES, `${ctx}.tone`); break;
      case 4: s.style = requireEnum(v, CHART_SERIES_STYLES, `${ctx}.style`); break;
      case 5: s.show_in_legend = requireBool(v, `${ctx}.show_in_legend`); break;
    }
  }
  if (s.id == null) throw new TypeError(`${ctx}: id required`);
  if (s.name == null) throw new TypeError(`${ctx}: name required`);
  if (s.data_path == null) throw new TypeError(`${ctx}: data_path required`);
  if (s.style == null) throw new TypeError(`${ctx}: style required`);
  if (s.show_in_legend == null) throw new TypeError(`${ctx}: show_in_legend required`);
  return s;
}

export function parseChartAxis(raw, ctx, locale) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: ChartAxis must be FieldMap`);
  const seen = new Set();
  const a = { label: null, format: null, min: null, max: null, ticks: null, scale: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!CHART_AXIS_KEYS.has(k)) throw new TypeError(`${ctx}: unknown ChartAxis key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: if (v != null) a.label = v; break;
      case 1: if (v != null) { assertValueFormat(v, `${ctx}.format`, locale); a.format = v; } break;
      case 2: if (v != null) a.min = requireF64(v, `${ctx}.min`); break;
      case 3: if (v != null) a.max = requireF64(v, `${ctx}.max`); break;
      case 4: if (v != null) a.ticks = requireU8(v, `${ctx}.ticks`); break;
      case 5: a.scale = requireEnum(v, CHART_AXIS_SCALES, `${ctx}.scale`); break;
    }
  }
  if (a.scale == null) throw new TypeError(`${ctx}: scale required`);
  if (a.min != null && a.max != null && a.min >= a.max) {
    throw new TypeError(`${ctx}: min must be < max`);
  }
  return a;
}

export function parseChartLegend(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: ChartLegend must be FieldMap`);
  const seen = new Set();
  const l = { position: null, alignment: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!CHART_LEGEND_KEYS.has(k)) throw new TypeError(`${ctx}: unknown ChartLegend key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) l.position = requireEnum(v, CHART_LEGEND_POSITIONS, `${ctx}.position`);
    else l.alignment = requireEnum(v, CHART_LEGEND_ALIGNS, `${ctx}.alignment`);
  }
  if (l.position == null) throw new TypeError(`${ctx}: position required`);
  if (l.alignment == null) throw new TypeError(`${ctx}: alignment required`);
  return l;
}

export function parseChartTooltip(raw, ctx, locale) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: ChartTooltip must be FieldMap`);
  const seen = new Set();
  const t = { enabled: null, format: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!CHART_TOOLTIP_KEYS.has(k)) throw new TypeError(`${ctx}: unknown ChartTooltip key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) t.enabled = requireBool(v, `${ctx}.enabled`);
    else if (v != null) { assertValueFormat(v, `${ctx}.format`, locale); t.format = v; }
  }
  if (t.enabled == null) throw new TypeError(`${ctx}: enabled required`);
  return t;
}

// =============================================================================
// Tick generation
// =============================================================================

/// Nice round number for linear ticks (algorytm D3-style). Wraca 1/2/5×10^k.
function niceNumber(x, round) {
  const exp = Math.floor(Math.log10(x));
  const f = x / Math.pow(10, exp);
  let nf;
  if (round) {
    nf = f < 1.5 ? 1 : f < 3 ? 2 : f < 7 ? 5 : 10;
  } else {
    nf = f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10;
  }
  return nf * Math.pow(10, exp);
}

/// Generuje tick'i linear: [min..max] z ~count tick'ami round'owanymi.
export function generateLinearTicks(min, max, count) {
  if (!Number.isFinite(min) || !Number.isFinite(max) || min === max) {
    return [min];
  }
  const range = niceNumber(max - min, false);
  const step = niceNumber(range / Math.max(1, count - 1), true);
  const niceMin = Math.floor(min / step) * step;
  const niceMax = Math.ceil(max / step) * step;
  const ticks = [];
  for (let v = niceMin; v <= niceMax + step / 2; v += step) {
    // Floating point safety — round to step precision.
    const rounded = Math.round(v / step) * step;
    ticks.push(rounded);
  }
  return ticks;
}

/// Tick generation dla log scale (potęgi 10 między min i max).
export function generateLogTicks(min, max) {
  if (min <= 0 || max <= 0 || min >= max) return [];
  const logMin = Math.floor(Math.log10(min));
  const logMax = Math.ceil(Math.log10(max));
  const ticks = [];
  for (let exp = logMin; exp <= logMax; exp++) {
    ticks.push(Math.pow(10, exp));
  }
  return ticks;
}

/// Format wartości ticka. Time scale → date string, format ValueFormat lub
/// number fallback.
export function formatTick(value, axis, locale) {
  if (axis.format) {
    try { return formatValue(value, axis.format, locale); }
    catch { /* fall through */ }
  }
  if (axis.scale === 'time') {
    try {
      return new Intl.DateTimeFormat(locale, {
        month: 'short', day: 'numeric',
      }).format(new Date(value));
    } catch { return String(value); }
  }
  if (axis.scale === 'category') return String(value);
  // Linear/log: nice number formatting.
  if (Number.isFinite(value)) {
    if (Number.isInteger(value)) return String(value);
    const abs = Math.abs(value);
    if (abs >= 1000) return new Intl.NumberFormat(locale).format(value);
    return new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(value);
  }
  return String(value);
}

// =============================================================================
// SVG axis rendering
// =============================================================================

/// Renderuje X axis (na dole plot area).
/// `range`: { x0, x1 } pixel range. `domain`: { min, max } data domain
/// (linear/log) lub null dla category (wtedy `categories` jest user-supplied).
/// `axis`: parsed ChartAxis. `y` pixel position osi.
export function renderXAxis(parent, axis, domain, categories, x0, x1, y, locale) {
  const group = document.createElementNS(SVG_NS, 'g');
  group.classList.add('tf-chart__axis');
  group.classList.add('tf-chart__axis--x');
  group.setAttribute('transform', `translate(0, ${y})`);
  // Axis line.
  const line = document.createElementNS(SVG_NS, 'line');
  line.setAttribute('x1', String(x0));
  line.setAttribute('x2', String(x1));
  line.setAttribute('y1', '0');
  line.setAttribute('y2', '0');
  line.classList.add('tf-chart__axis-line');
  group.appendChild(line);
  // Ticks.
  let tickValues;
  if (axis.scale === 'category') {
    tickValues = categories || [];
  } else if (axis.scale === 'log') {
    tickValues = generateLogTicks(domain.min, domain.max);
  } else if (axis.scale === 'time') {
    tickValues = generateLinearTicks(domain.min, domain.max, axis.ticks || 6);
  } else {
    tickValues = generateLinearTicks(domain.min, domain.max, axis.ticks || 6);
  }
  for (const t of tickValues) {
    let px;
    if (axis.scale === 'category') {
      const idx = categories.indexOf(t);
      if (idx < 0) continue;
      const step = (x1 - x0) / Math.max(1, categories.length);
      px = x0 + step * (idx + 0.5);
    } else if (axis.scale === 'log') {
      const logMin = Math.log10(domain.min);
      const logMax = Math.log10(domain.max);
      px = x0 + ((Math.log10(t) - logMin) / (logMax - logMin)) * (x1 - x0);
    } else {
      px = x0 + ((t - domain.min) / (domain.max - domain.min)) * (x1 - x0);
    }
    if (!Number.isFinite(px) || px < x0 - 0.5 || px > x1 + 0.5) continue;
    const tick = document.createElementNS(SVG_NS, 'line');
    tick.setAttribute('x1', String(px));
    tick.setAttribute('x2', String(px));
    tick.setAttribute('y1', '0');
    tick.setAttribute('y2', '4');
    tick.classList.add('tf-chart__axis-tick');
    group.appendChild(tick);
    const label = document.createElementNS(SVG_NS, 'text');
    label.setAttribute('x', String(px));
    label.setAttribute('y', '16');
    label.setAttribute('text-anchor', 'middle');
    label.classList.add('tf-chart__axis-label');
    label.textContent = formatTick(t, axis, locale);
    group.appendChild(label);
  }
  parent.appendChild(group);
  return group;
}

/// Renderuje Y axis (po lewej plot area).
export function renderYAxis(parent, axis, domain, x, y0, y1, locale) {
  const group = document.createElementNS(SVG_NS, 'g');
  group.classList.add('tf-chart__axis');
  group.classList.add('tf-chart__axis--y');
  group.setAttribute('transform', `translate(${x}, 0)`);
  const line = document.createElementNS(SVG_NS, 'line');
  line.setAttribute('x1', '0');
  line.setAttribute('x2', '0');
  line.setAttribute('y1', String(y0));
  line.setAttribute('y2', String(y1));
  line.classList.add('tf-chart__axis-line');
  group.appendChild(line);
  const tickValues = axis.scale === 'log'
    ? generateLogTicks(domain.min, domain.max)
    : generateLinearTicks(domain.min, domain.max, axis.ticks || 6);
  for (const t of tickValues) {
    let py;
    if (axis.scale === 'log') {
      const logMin = Math.log10(domain.min);
      const logMax = Math.log10(domain.max);
      py = y1 - ((Math.log10(t) - logMin) / (logMax - logMin)) * (y1 - y0);
    } else {
      py = y1 - ((t - domain.min) / (domain.max - domain.min)) * (y1 - y0);
    }
    if (!Number.isFinite(py) || py < y0 - 0.5 || py > y1 + 0.5) continue;
    const tick = document.createElementNS(SVG_NS, 'line');
    tick.setAttribute('x1', '-4');
    tick.setAttribute('x2', '0');
    tick.setAttribute('y1', String(py));
    tick.setAttribute('y2', String(py));
    tick.classList.add('tf-chart__axis-tick');
    group.appendChild(tick);
    const label = document.createElementNS(SVG_NS, 'text');
    label.setAttribute('x', '-6');
    label.setAttribute('y', String(py));
    label.setAttribute('text-anchor', 'end');
    label.setAttribute('dominant-baseline', 'middle');
    label.classList.add('tf-chart__axis-label');
    label.textContent = formatTick(t, axis, locale);
    group.appendChild(label);
  }
  parent.appendChild(group);
  return group;
}

/// Renderuje gridlines na osi Y (poziome linie przez plot area).
export function renderGridlinesY(parent, axis, domain, x0, x1, y0, y1) {
  const group = document.createElementNS(SVG_NS, 'g');
  group.classList.add('tf-chart__gridlines');
  group.classList.add('tf-chart__gridlines--y');
  const tickValues = axis.scale === 'log'
    ? generateLogTicks(domain.min, domain.max)
    : generateLinearTicks(domain.min, domain.max, axis.ticks || 6);
  for (const t of tickValues) {
    let py;
    if (axis.scale === 'log') {
      const logMin = Math.log10(domain.min);
      const logMax = Math.log10(domain.max);
      py = y1 - ((Math.log10(t) - logMin) / (logMax - logMin)) * (y1 - y0);
    } else {
      py = y1 - ((t - domain.min) / (domain.max - domain.min)) * (y1 - y0);
    }
    if (!Number.isFinite(py) || py < y0 - 0.5 || py > y1 + 0.5) continue;
    const line = document.createElementNS(SVG_NS, 'line');
    line.setAttribute('x1', String(x0));
    line.setAttribute('x2', String(x1));
    line.setAttribute('y1', String(py));
    line.setAttribute('y2', String(py));
    line.classList.add('tf-chart__gridline');
    group.appendChild(line);
  }
  parent.appendChild(group);
  return group;
}

// =============================================================================
// Legend DOM
// =============================================================================

/// Buduje legend DOM. `seriesEntries` to lista `{series, hidden}` z opcjonalnym
/// initialnym state. Emit'uje `series_toggle` event z `{series_id, hidden}`.
export function buildLegend(legendCfg, seriesEntries, ctx, onToggle) {
  if (legendCfg.position === 'none') return null;
  const wrap = document.createElement('div');
  wrap.classList.add('tf-chart__legend');
  wrap.classList.add(`tf-chart__legend--position-${legendCfg.position}`);
  wrap.classList.add(`tf-chart__legend--align-${legendCfg.alignment}`);
  wrap.setAttribute('role', 'list');
  for (const entry of seriesEntries) {
    const { series } = entry;
    if (!series.show_in_legend) continue;
    const item = document.createElement('button');
    item.setAttribute('type', 'button');
    item.classList.add('tf-chart__legend-item');
    item.setAttribute('role', 'listitem');
    item.setAttribute('data-series-id', series.id);
    if (entry.hidden) item.classList.add('tf-chart__legend-item--hidden');
    const sw = document.createElement('span');
    sw.classList.add('tf-chart__legend-swatch');
    if (series.tone) sw.classList.add(`tf-chart__legend-swatch--tone-${series.tone}`);
    item.appendChild(sw);
    const label = document.createElement('span');
    label.classList.add('tf-chart__legend-label');
    const apply = () => {
      const v = resolveBindRef(series.name, ctx.store);
      label.textContent = v == null ? series.id : String(v);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(series.name, ctx.store, apply));
    item.appendChild(label);
    const onClick = (e) => {
      e.preventDefault();
      onToggle(series.id);
    };
    item.addEventListener('click', onClick);
    ctx.registerCleanup(() => item.removeEventListener('click', onClick));
    wrap.appendChild(item);
  }
  return wrap;
}

// =============================================================================
// Data domain computation
// =============================================================================

/// Wylicza data domain dla osi X i Y z serii. Dla linear/log/time: min/max
/// po wszystkich punktach. Log scale: brane są TYLKO wartości > 0 (log10
/// niezdefiniowane dla 0/negative; spec wymaga positive-only dla log scale).
/// Category: zbiera unique values w kolejności występowania.
/// User override axis.min/max dla log scale MUSI być > 0 — inaczej throws.
export function computeDomains(seriesData, xAxis, yAxis) {
  if (xAxis.scale === 'log') {
    if (xAxis.min != null && xAxis.min <= 0) throw new TypeError('ChartAxis.min must be > 0 for scale=log');
    if (xAxis.max != null && xAxis.max <= 0) throw new TypeError('ChartAxis.max must be > 0 for scale=log');
  }
  if (yAxis.scale === 'log') {
    if (yAxis.min != null && yAxis.min <= 0) throw new TypeError('ChartAxis.min must be > 0 for scale=log');
    if (yAxis.max != null && yAxis.max <= 0) throw new TypeError('ChartAxis.max must be > 0 for scale=log');
  }
  const xs = { min: Infinity, max: -Infinity };
  const ys = { min: Infinity, max: -Infinity };
  const categories = xAxis.scale === 'category' ? [] : null;
  const catSeen = new Set();
  for (const points of seriesData) {
    for (const p of points) {
      const xVal = p.x;
      const yVal = p.y;
      if (xAxis.scale === 'category') {
        if (typeof xVal === 'string' && !catSeen.has(xVal)) {
          catSeen.add(xVal);
          categories.push(xVal);
        }
      } else if (typeof xVal === 'number' && Number.isFinite(xVal)) {
        // Log scale: skip non-positive values (nie mogą być w domenie).
        if (xAxis.scale === 'log' && xVal <= 0) continue;
        if (xVal < xs.min) xs.min = xVal;
        if (xVal > xs.max) xs.max = xVal;
      }
      if (typeof yVal === 'number' && Number.isFinite(yVal)) {
        if (yAxis.scale === 'log' && yVal <= 0) continue;
        if (yVal < ys.min) ys.min = yVal;
        if (yVal > ys.max) ys.max = yVal;
      }
    }
  }
  // User overrides z axis.min/max.
  if (xAxis.min != null) xs.min = xAxis.min;
  if (xAxis.max != null) xs.max = xAxis.max;
  if (yAxis.min != null) ys.min = yAxis.min;
  if (yAxis.max != null) ys.max = yAxis.max;
  // Defensywne defaults gdy domain pusty.
  if (!Number.isFinite(xs.min)) {
    if (xAxis.scale === 'log') { xs.min = 1; xs.max = 10; }
    else { xs.min = 0; xs.max = 1; }
  }
  if (!Number.isFinite(ys.min)) {
    if (yAxis.scale === 'log') { ys.min = 1; ys.max = 10; }
    else { ys.min = 0; ys.max = 1; }
  }
  if (xs.min === xs.max) {
    xs.max = xAxis.scale === 'log' ? xs.min * 10 : xs.min + 1;
  }
  if (ys.min === ys.max) {
    ys.max = yAxis.scale === 'log' ? ys.min * 10 : ys.min + 1;
  }
  return { xs, ys, categories };
}

/// Skaluje wartość X do piksela. dla scale=category używa index w
/// `categories`. Wraca null gdy wartość nie da się skalować.
export function scaleX(value, xAxis, domain, categories, x0, x1) {
  if (xAxis.scale === 'category') {
    const idx = (categories || []).indexOf(value);
    if (idx < 0) return null;
    const step = (x1 - x0) / Math.max(1, categories.length);
    return x0 + step * (idx + 0.5);
  }
  if (xAxis.scale === 'log') {
    if (value <= 0 || domain.min <= 0 || domain.max <= 0) return null;
    const lm = Math.log10(domain.min);
    const lx = Math.log10(domain.max);
    return x0 + ((Math.log10(value) - lm) / (lx - lm)) * (x1 - x0);
  }
  if (typeof value !== 'number' || !Number.isFinite(value)) return null;
  return x0 + ((value - domain.min) / (domain.max - domain.min)) * (x1 - x0);
}

/// Skaluje Y do piksela (Y w SVG rośnie w dół, więc invert).
export function scaleY(value, yAxis, domain, y0, y1) {
  if (typeof value !== 'number' || !Number.isFinite(value)) return null;
  if (yAxis.scale === 'log') {
    if (value <= 0 || domain.min <= 0 || domain.max <= 0) return null;
    const lm = Math.log10(domain.min);
    const lx = Math.log10(domain.max);
    return y1 - ((Math.log10(value) - lm) / (lx - lm)) * (y1 - y0);
  }
  return y1 - ((value - domain.min) / (domain.max - domain.min)) * (y1 - y0);
}
