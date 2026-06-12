// =============================================================================
// Plik: sdk-runtime/data-chart-shared.js
// Opis: Współdzielone walidatory CBOR dla rendererów (charts i nie tylko):
// require* primitives, parsing inline structs (ChartSeries, ChartAxis,
// ChartLegend, ChartTooltip), ValueFormat assert. Rysowanie (osie, ticki,
// domeny, legenda) żyje w components/tf-line-chart.js.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/inline.rs (chart family) +
// tokens.rs (chart enums).
// =============================================================================

import { formatValue } from './bind-resolver.js';

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
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFn) throw new TypeError(`${ctx}: expected u8, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
export function requireU16(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFn) throw new TypeError(`${ctx}: expected u16, got ${v}`);
    return Number(v);
  }
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
