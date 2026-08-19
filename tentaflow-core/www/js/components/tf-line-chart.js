// =============================================================================
// Plik: components/tf-line-chart.js
// Opis: Wykres liniowy SVG (LineChart 0x0216) + bazowa klasa TfCartesianChart
// i helpery rysowania (osie, ticki, domeny, legenda, tooltip, crosshair,
// animacja wejścia) współdzielone przez tf-area-chart i tf-bar-chart.
// Light DOM, klasy .tf-chart__* z controls.css.
//
// Kontrakt danych (property `series`):
//   Array<{
//     id: string,                 // stabilny identyfikator serii
//     name: string,               // etykieta do legendy (już zresolvowana)
//     tone: string|null,          // neutral/primary/accent/success/warning/critical/info/muted
//     style: 'solid'|'dashed'|'dotted',
//     showInLegend: boolean,
//     points: Array<{x: number|string, y: number}>,  // y zawsze skończona liczba
//   }>
// Property `xAxis`/`yAxis`: { scale: 'linear'|'log'|'time'|'category',
//   min: number|null, max: number|null, ticks: number|null,
//   format: ((value) => string)|null }  // formatter może rzucić → fallback;
//   null na osi liniowej/log → fmtCompact z utils.js
// Property `legend`: { position: 'top'|'bottom'|'left'|'right'|'none',
//   alignment: 'start'|'center'|'end' } | null
// Property `tooltip`: { enabled: boolean (domyślnie true),
//   format: ((x, items) => string|Element)|null,   // pełna treść tooltipa
//   valueFormat: ((y) => string)|null,             // wartość w wierszu (domyślnie fmtExact)
//   totalLabel: string }                           // wiersz sumy przy stacking='stacked'
//   items = [{ seriesId, seriesName, y, tone }]
// Property `crosshair`: boolean (pionowa linia na najbliższej kategorii);
// `animate`: boolean (jednorazowa animacja wejścia po ustawieniu `series`);
// `narrow`: { breakpoint: 560, maxPoints: 10 } (wąski plot → ostatnie N kategorii);
// `zoom`: 'none'|'x'|'y'|'both'; `brush`: boolean; `height`: number px;
// `locale`: string (Intl dla domyślnego formatowania ticków).
//
// Rendering: każdy setter renderuje synchronicznie (DOM jest gotowy od razu
// po przypisaniu, także przed podłączeniem do dokumentu).
//
// Eventy (bubbles:false, na hoście):
//   'series-toggle' detail {series_id, hidden}
//   'point-hover'   detail {series_id, x, y}
//   'range-select'  detail {x:{min,max}, y:{min,max}, zoom_mode, brush}
// =============================================================================

import { fmtCompact, fmtExact } from '../utils.js';

export const SVG_NS = 'http://www.w3.org/2000/svg';

const PLOT_MARGIN = { top: 12, right: 16, bottom: 36, left: 48 };
// Estimated glyph width of the 10 px axis font — used to thin category labels.
const LABEL_CHAR_PX = 6.5;
const LABEL_GAP_PX = 8;
// Upper bound on category labels even when they would fit (mockup density).
const MAX_CATEGORY_LABELS = 14;
const BAR_STAGGER_MS = 12;

// =============================================================================
// Tick generation
// =============================================================================

/// Nice round number for linear ticks (D3-style). Returns 1/2/5×10^k.
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

/// Linear ticks: [min..max] with ~count rounded ticks.
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

/// Log-scale ticks (powers of 10 between min and max).
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

/// Tick label. axis.format is a callable formatter (may throw → fallback),
/// time scale → date string, linear/log → fmtCompact for integers and large
/// magnitudes, plain Intl formatting for fractional ticks.
export function formatTick(value, axis, locale) {
  if (typeof axis.format === 'function') {
    try { return axis.format(value); }
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
  if (Number.isFinite(value)) {
    if (Number.isInteger(value) || Math.abs(value) >= 1e4) return fmtCompact(value, locale);
    return new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(value);
  }
  return String(value);
}

/// Picks which category indices get a label so neighbouring labels never
/// overlap (and at most MAX_CATEGORY_LABELS are shown). Keeps the first and
/// the last category; the last one wins over a regular-step label that would
/// collide with it.
export function thinCategoryIndices(labels, stepPx) {
  const n = labels.length;
  if (n === 0) return [];
  const longest = labels.reduce((m, l) => Math.max(m, String(l).length), 0);
  const labelPx = longest * LABEL_CHAR_PX + LABEL_GAP_PX;
  const every = Math.max(1, Math.ceil(labelPx / Math.max(1, stepPx)), Math.ceil(n / MAX_CATEGORY_LABELS));
  const picked = [];
  for (let i = 0; i < n; i += every) picked.push(i);
  const last = n - 1;
  if (picked[picked.length - 1] !== last) {
    if ((last - picked[picked.length - 1]) * stepPx < labelPx) picked.pop();
    picked.push(last);
  }
  return picked;
}

// =============================================================================
// SVG axis rendering
// =============================================================================

/// X axis at the bottom of the plot area. `domain`: {min, max} for
/// linear/log/time, null for category (then `categories` supplies ticks).
export function renderXAxis(parent, axis, domain, categories, x0, x1, y, locale) {
  const group = document.createElementNS(SVG_NS, 'g');
  group.classList.add('tf-chart__axis');
  group.classList.add('tf-chart__axis--x');
  group.setAttribute('transform', `translate(0, ${y})`);
  const line = document.createElementNS(SVG_NS, 'line');
  line.setAttribute('x1', String(x0));
  line.setAttribute('x2', String(x1));
  line.setAttribute('y1', '0');
  line.setAttribute('y2', '0');
  line.classList.add('tf-chart__axis-line');
  group.appendChild(line);
  let tickValues;
  if (axis.scale === 'category') {
    const cats = categories || [];
    const step = (x1 - x0) / Math.max(1, cats.length);
    const labels = cats.map((c) => formatTick(c, axis, locale));
    tickValues = thinCategoryIndices(labels, step).map((i) => cats[i]);
  } else if (axis.scale === 'log') {
    tickValues = generateLogTicks(domain.min, domain.max);
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

/// Y axis at the left of the plot area.
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

/// Horizontal gridlines across the plot area (Y axis ticks).
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
// Data domain computation + scaling
// =============================================================================

/// Computes X/Y data domains from series points. Log scale keeps only
/// values > 0 (log10 undefined otherwise); user min/max override for log
/// scale MUST be > 0 — throws otherwise. Category: unique values in order
/// of first occurrence.
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
        // Log scale: skip non-positive values (cannot be in the domain).
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
  // User overrides from axis.min/max.
  if (xAxis.min != null) xs.min = xAxis.min;
  if (xAxis.max != null) xs.max = xAxis.max;
  if (yAxis.min != null) ys.min = yAxis.min;
  if (yAxis.max != null) ys.max = yAxis.max;
  // Defensive defaults when the domain is empty.
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

/// Scales an X value to pixels. Category uses the index in `categories`.
/// Returns null when the value cannot be scaled.
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

/// Scales Y to pixels (SVG Y grows downwards, hence the invert).
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

/// Converts a pixel coordinate back to the data domain. `invertY` for the
/// Y axis (SVG Y inverted). Category has no inverse mapping → null.
export function pixelToData(px, axis, domain, p0, p1, invertY = false) {
  if (axis.scale === 'category') return null;
  if (axis.scale === 'log') {
    const lm = Math.log10(domain.min);
    const lx = Math.log10(domain.max);
    const ratio = invertY ? (p1 - px) / (p1 - p0) : (px - p0) / (p1 - p0);
    return Math.pow(10, lm + ratio * (lx - lm));
  }
  const ratio = invertY ? (p1 - px) / (p1 - p0) : (px - p0) / (p1 - p0);
  return domain.min + ratio * (domain.max - domain.min);
}

/// Keeps only the last `maxPoints` categories when the plot is narrower than
/// `breakpoint`; returns the (possibly) sliced category list.
export function applyNarrow(categories, plotWidth, narrow) {
  if (!Array.isArray(categories) || !narrow) return categories;
  if (plotWidth >= narrow.breakpoint || categories.length <= narrow.maxPoints) return categories;
  return categories.slice(-narrow.maxPoints);
}

/// Polyline length in user units — drives the stroke-dashoffset draw-in.
export function polylineLength(coords) {
  let len = 0;
  for (let i = 1; i < coords.length; i++) {
    const dx = coords[i][0] - coords[i - 1][0];
    const dy = coords[i][1] - coords[i - 1][1];
    len += Math.hypot(dx, dy);
  }
  return len;
}

/// Marks a polyline for the draw-in animation; inline dash styles are dropped
/// once the animation ends so dashed/dotted line styles are restored.
export function animateLineEnter(polyline, coords, delayMs = 0) {
  const len = Math.ceil(polylineLength(coords)) + 2;
  polyline.style.strokeDasharray = `${len}`;
  polyline.style.strokeDashoffset = `${len}`;
  if (delayMs) polyline.style.animationDelay = `${delayMs}ms`;
  polyline.classList.add('tf-chart__series-line--enter');
  polyline.addEventListener('animationend', () => {
    polyline.style.strokeDasharray = '';
    polyline.style.strokeDashoffset = '';
    polyline.style.animationDelay = '';
    polyline.classList.remove('tf-chart__series-line--enter');
  }, { once: true });
}

function mediaMatches(query) {
  if (typeof globalThis.matchMedia !== 'function') return false;
  try { return globalThis.matchMedia(query).matches; } catch { return false; }
}

// =============================================================================
// TfCartesianChart — shared host/layout/legend/tooltip/brush machinery
// =============================================================================

const DEFAULT_AXIS = { scale: 'linear', min: null, max: null, ticks: null, format: null };
const DEFAULT_TOOLTIP = { enabled: true, format: null, valueFormat: null, totalLabel: 'Σ' };
const DEFAULT_NARROW = { breakpoint: 560, maxPoints: 10 };

export class TfCartesianChart extends HTMLElement {
  constructor() {
    super();
    this._series = [];
    this._xAxis = { ...DEFAULT_AXIS };
    this._yAxis = { ...DEFAULT_AXIS };
    this._legend = null;
    this._tooltip = { ...DEFAULT_TOOLTIP };
    this._crosshair = true;
    this._animate = true;
    this._narrow = { ...DEFAULT_NARROW };
    this._zoom = 'none';
    this._brush = false;
    this._height = 200;
    this._locale = undefined;
    this._hidden = new Set();
    this._appliedClasses = [];
    this._svg = null;
    this._plot = null;
    this._legendEl = null;
    this._tooltipEl = null;
    this._crosshairEl = null;
    this._hoverItems = [];
    this._animPending = false;
    this._animClearScheduled = false;
    this._brushStart = null;
    this._brushRect = null;
    this._lastDomain = null;
    this._lastPlotBox = null;
    this._lastSize = null;
    this._ro = null;
    this._onDocUp = (e) => this._handleBrushUp(e);
  }

  set series(value) {
    this._series = Array.isArray(value) ? value : [];
    if (this._animate) this._animPending = true;
    this._requestRender();
  }
  set xAxis(value) { this._xAxis = { ...DEFAULT_AXIS, ...(value || {}) }; this._requestRender(); }
  set yAxis(value) { this._yAxis = { ...DEFAULT_AXIS, ...(value || {}) }; this._requestRender(); }
  set legend(value) { this._legend = value || null; this._requestRender(); }
  set tooltip(value) { this._tooltip = { ...DEFAULT_TOOLTIP, ...(value || {}) }; this._requestRender(); }
  set crosshair(value) { this._crosshair = Boolean(value); this._requestRender(); }
  set animate(value) { this._animate = Boolean(value); if (!this._animate) this._animPending = false; }
  set narrow(value) {
    this._narrow = value ? { ...DEFAULT_NARROW, ...value } : null;
    this._requestRender();
  }
  set zoom(value) { this._zoom = typeof value === 'string' ? value : 'none'; this._requestRender(); }
  set brush(value) { this._brush = Boolean(value); this._requestRender(); }
  set height(value) { const n = Number(value); if (Number.isFinite(n) && n > 0) this._height = n; this._requestRender(); }
  set locale(value) { this._locale = value || undefined; this._requestRender(); }

  connectedCallback() {
    document.addEventListener('mouseup', this._onDocUp);
    if (typeof globalThis.ResizeObserver === 'function' && !this._ro) {
      this._ro = new globalThis.ResizeObserver(() => this._renderPlot(false));
      if (this._plot) this._ro.observe(this._plot);
    }
    if (!this._svg) this._render();
  }

  disconnectedCallback() {
    document.removeEventListener('mouseup', this._onDocUp);
    if (this._ro) { this._ro.disconnect(); this._ro = null; }
  }

  // ---- subclass hooks -------------------------------------------------------

  /// Host classes for the current configuration (without 'tf-chart').
  _hostClasses() { return []; }

  /// aria-label for the <svg>.
  _ariaLabel() { return 'Chart'; }

  /// Draws everything inside the svg for the given plot box. Must set
  /// `this._lastDomain` ({xs, ys, categories}) when brush/tooltip apply and
  /// push hover candidates into `this._hoverItems`:
  /// {seriesId, seriesName, tone, x, y, display, px, py}.
  /// `enter` = true on the first paint after `series` changed (animation).
  _drawPlot(_svg, _box, _enter) {}

  /// Whether the default tooltip appends a total row.
  _tooltipShowsTotal() { return false; }

  /// Tooltip title for the hovered x value.
  _formatXLabel(x) { return formatTick(x, this._xAxis, this._locale); }

  /// Axis along which hover candidates are grouped ('x' columns, 'y' rows).
  _hoverAxis() { return 'x'; }

  // ---- rendering ------------------------------------------------------------

  _visibleSeries() { return this._series.filter((s) => !this._hidden.has(s.id)); }

  _requestRender() { this._render(); }

  _hoverEnabled() {
    return this._tooltip.enabled && !mediaMatches('(hover: none)');
  }

  _motionAllowed() {
    return !mediaMatches('(prefers-reduced-motion: reduce)');
  }

  _render() {
    // Host classes: replace the previously applied managed set.
    for (const c of this._appliedClasses) this.classList.remove(c);
    const legendPos = this._legend ? this._legend.position : null;
    const classes = ['tf-chart', ...this._hostClasses()];
    if (legendPos) classes.push(`tf-chart--legend-${legendPos}`);
    for (const c of classes) this.classList.add(c);
    this._appliedClasses = classes;
    this.style.height = `${this._height}px`;

    const root = document.createElement('div');
    root.classList.add('tf-chart__layout');
    if (legendPos) root.classList.add(`tf-chart__layout--legend-${legendPos}`);

    this._plot = document.createElement('div');
    this._plot.classList.add('tf-chart__plot');
    this._svg = document.createElementNS(SVG_NS, 'svg');
    this._svg.setAttribute('width', '100%');
    this._svg.setAttribute('height', '100%');
    this._svg.setAttribute('role', 'img');
    this._svg.setAttribute('aria-label', this._ariaLabel());
    this._svg.classList.add('tf-chart__svg');
    this._plot.appendChild(this._svg);

    this._tooltipEl = null;
    if (this._hoverEnabled()) {
      this._tooltipEl = document.createElement('div');
      this._tooltipEl.classList.add('tf-chart__tooltip');
      this._tooltipEl.hidden = true;
      this._plot.appendChild(this._tooltipEl);
    }

    this._legendEl = this._buildLegend();
    if ((legendPos === 'top' || legendPos === 'left') && this._legendEl) root.appendChild(this._legendEl);
    root.appendChild(this._plot);
    if ((legendPos === 'right' || legendPos === 'bottom') && this._legendEl) root.appendChild(this._legendEl);

    this.replaceChildren(root);

    this._attachSvgListeners();
    if (this._ro) { this._ro.disconnect(); this._ro.observe(this._plot); }
    this._renderPlot(true);
  }

  _pixelDimensions() {
    const rect = this._plot && this._plot.getBoundingClientRect ? this._plot.getBoundingClientRect() : null;
    // happy-dom returns width=0 for unmounted nodes; fall back to height-based box.
    const w = (rect && rect.width > 0) ? rect.width : this._height * 1.5;
    const h = (rect && rect.height > 0) ? rect.height : this._height;
    return { w, h };
  }

  /// `force=false` (ResizeObserver) skips the rebuild when the measured box
  /// did not change — the observer fires once right after observe(), which
  /// would otherwise discard the entry animation of the first paint.
  _renderPlot(force = true) {
    const svg = this._svg;
    if (!svg) return;
    const { w, h } = this._pixelDimensions();
    if (!force && this._lastSize && this._lastSize.w === w && this._lastSize.h === h) return;
    this._lastSize = { w, h };
    svg.replaceChildren();
    this._brushRect = null;
    this._crosshairEl = null;
    this._hoverItems = [];
    if (this._tooltipEl) this._tooltipEl.hidden = true;
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    const box = {
      x0: PLOT_MARGIN.left,
      x1: w - PLOT_MARGIN.right,
      y0: PLOT_MARGIN.top,
      y1: h - PLOT_MARGIN.bottom,
    };
    if (box.x1 <= box.x0 || box.y1 <= box.y0) { this._lastPlotBox = null; return; }
    this._lastPlotBox = box;
    const enter = this._animPending && this._motionAllowed();
    this._drawPlot(svg, box, enter);
    if (this._animPending && !this._animClearScheduled && typeof globalThis.requestAnimationFrame === 'function') {
      // Any rebuild inside the same frame still animates; the flag clears
      // only once the first animated paint is on screen.
      this._animClearScheduled = true;
      globalThis.requestAnimationFrame(() => {
        this._animPending = false;
        this._animClearScheduled = false;
      });
    }
    if (this._crosshair && this._hoverEnabled()) {
      this._crosshairEl = document.createElementNS(SVG_NS, 'line');
      this._crosshairEl.classList.add('tf-chart__crosshair');
      this._crosshairEl.setAttribute('y1', String(box.y0));
      this._crosshairEl.setAttribute('y2', String(box.y1));
      this._crosshairEl.setAttribute('x1', String(box.x0));
      this._crosshairEl.setAttribute('x2', String(box.x0));
      this._crosshairEl.style.display = 'none';
      svg.appendChild(this._crosshairEl);
    }
    if (this._brush) {
      this._brushRect = document.createElementNS(SVG_NS, 'rect');
      this._brushRect.classList.add('tf-chart__brush');
      this._brushRect.setAttribute('y', String(box.y0));
      this._brushRect.setAttribute('height', String(box.y1 - box.y0));
      this._brushRect.style.display = 'none';
      svg.appendChild(this._brushRect);
    }
  }

  // ---- legend ----------------------------------------------------------------

  _buildLegend() {
    const legend = this._legend;
    if (!legend || legend.position === 'none') return null;
    const wrap = document.createElement('div');
    wrap.classList.add('tf-chart__legend');
    wrap.classList.add(`tf-chart__legend--position-${legend.position}`);
    wrap.classList.add(`tf-chart__legend--align-${legend.alignment}`);
    wrap.setAttribute('role', 'list');
    for (const s of this._series) {
      if (!s.showInLegend) continue;
      const item = document.createElement('button');
      item.setAttribute('type', 'button');
      item.classList.add('tf-chart__legend-item');
      item.setAttribute('role', 'listitem');
      item.setAttribute('data-series-id', s.id);
      if (this._hidden.has(s.id)) item.classList.add('tf-chart__legend-item--hidden');
      const sw = document.createElement('span');
      sw.classList.add('tf-chart__legend-swatch');
      if (s.tone) sw.classList.add(`tf-chart__legend-swatch--tone-${s.tone}`);
      item.appendChild(sw);
      const label = document.createElement('span');
      label.classList.add('tf-chart__legend-label');
      label.textContent = s.name == null ? s.id : String(s.name);
      item.appendChild(label);
      item.addEventListener('click', (e) => {
        e.preventDefault();
        this._toggleSeries(s.id);
      });
      wrap.appendChild(item);
    }
    return wrap;
  }

  _toggleSeries(sid) {
    if (this._hidden.has(sid)) this._hidden.delete(sid);
    else this._hidden.add(sid);
    if (this._legendEl) {
      const item = this._legendEl.querySelector(`[data-series-id="${sid}"]`);
      if (item) item.classList.toggle('tf-chart__legend-item--hidden', this._hidden.has(sid));
    }
    this._renderPlot(true);
    this.dispatchEvent(new CustomEvent('series-toggle', {
      bubbles: false,
      detail: { series_id: sid, hidden: this._hidden.has(sid) },
    }));
  }

  // ---- tooltip + crosshair + brush listeners -----------------------------------

  _attachSvgListeners() {
    const svg = this._svg;
    if (this._hoverEnabled()) {
      svg.addEventListener('mousemove', (e) => this._handleTooltipMove(e));
      svg.addEventListener('mouseleave', () => this._hideHover());
    }
    if (this._brush || this._zoom !== 'none') {
      svg.addEventListener('mousedown', (e) => this._handleBrushDown(e));
      svg.addEventListener('mousemove', (e) => this._handleBrushMove(e));
    }
  }

  _hideHover() {
    if (this._tooltipEl) this._tooltipEl.hidden = true;
    if (this._crosshairEl) this._crosshairEl.style.display = 'none';
  }

  /// Nearest x column to the pointer → crosshair + one tooltip listing every
  /// series at that x. The whole plot box is the hit area, so the tooltip
  /// also works in the gaps between bars.
  _handleTooltipMove(e) {
    const tooltipEl = this._tooltipEl;
    if (!tooltipEl) return;
    const box = this._lastPlotBox;
    if (!box) { this._hideHover(); return; }
    const svgRect = this._svg.getBoundingClientRect();
    const mx = e.clientX - svgRect.left;
    const my = e.clientY - svgRect.top;
    if (mx < box.x0 || mx > box.x1 || my < box.y0 || my > box.y1) { this._hideHover(); return; }
    const byRow = this._hoverAxis() === 'y';
    const key = byRow ? 'py' : 'px';
    const m = byRow ? my : mx;
    let best = null;
    for (const cand of this._hoverItems) {
      if (best == null || Math.abs(cand[key] - m) < Math.abs(best - m)) best = cand[key];
    }
    if (best == null) { this._hideHover(); return; }
    const group = this._hoverItems.filter((c) => Math.abs(c[key] - best) < 0.5);
    if (group.length === 0) { this._hideHover(); return; }
    const bestPx = byRow ? Math.max(...group.map((c) => c.px)) : best;

    const xValue = group[0].x;
    const items = group.map((c) => ({ seriesId: c.seriesId, seriesName: c.seriesName, y: c.display, tone: c.tone }));
    tooltipEl.replaceChildren();
    let custom = null;
    if (typeof this._tooltip.format === 'function') {
      try { custom = this._tooltip.format(xValue, items); } catch { custom = null; }
    }
    if (custom instanceof Element) {
      tooltipEl.appendChild(custom);
    } else if (typeof custom === 'string') {
      tooltipEl.innerHTML = custom;
    } else {
      this._buildDefaultTooltip(tooltipEl, xValue, items);
    }
    tooltipEl.hidden = false;

    // Clamp inside the plot box: flip to the left of the crosshair when the
    // right side has no room, keep the top edge within the plot vertically.
    const tw = tooltipEl.offsetWidth || 0;
    const th = tooltipEl.offsetHeight || 0;
    let left = bestPx + 12;
    if (left + tw > box.x1) left = Math.max(box.x0, bestPx - 12 - tw);
    let top = my - 10;
    if (top + th > box.y1) top = Math.max(box.y0, box.y1 - th);
    if (top < box.y0) top = box.y0;
    tooltipEl.style.left = `${left}px`;
    tooltipEl.style.top = `${top}px`;

    if (this._crosshairEl) {
      if (byRow) {
        this._crosshairEl.setAttribute('x1', String(box.x0));
        this._crosshairEl.setAttribute('x2', String(box.x1));
        this._crosshairEl.setAttribute('y1', String(best));
        this._crosshairEl.setAttribute('y2', String(best));
      } else {
        this._crosshairEl.setAttribute('x1', String(bestPx));
        this._crosshairEl.setAttribute('x2', String(bestPx));
      }
      this._crosshairEl.style.display = '';
    }

    let nearest = group[0];
    const other = byRow ? 'px' : 'py';
    const mo = byRow ? mx : my;
    for (const c of group) if (Math.abs(c[other] - mo) < Math.abs(nearest[other] - mo)) nearest = c;
    this.dispatchEvent(new CustomEvent('point-hover', {
      bubbles: false,
      detail: { series_id: nearest.seriesId, x: nearest.x, y: nearest.y },
    }));
  }

  _formatTooltipValue(y) {
    if (typeof this._tooltip.valueFormat === 'function') {
      try { return String(this._tooltip.valueFormat(y)); } catch { /* fall through */ }
    }
    return fmtExact(y, this._locale);
  }

  _buildDefaultTooltip(tooltipEl, xValue, items) {
    const title = document.createElement('div');
    title.classList.add('tf-chart__tooltip-title');
    title.textContent = this._formatXLabel(xValue);
    tooltipEl.appendChild(title);
    const addRow = (name, value, tone, extraClass) => {
      const row = document.createElement('div');
      row.classList.add('tf-chart__tooltip-row');
      if (extraClass) row.classList.add(extraClass);
      const nameEl = document.createElement('span');
      nameEl.classList.add('tf-chart__tooltip-name');
      if (tone) {
        const sw = document.createElement('span');
        sw.classList.add('tf-chart__legend-swatch', `tf-chart__legend-swatch--tone-${tone}`);
        nameEl.appendChild(sw);
      }
      nameEl.appendChild(document.createTextNode(name));
      row.appendChild(nameEl);
      const valueEl = document.createElement('span');
      valueEl.classList.add('tf-chart__tooltip-value');
      valueEl.textContent = value;
      row.appendChild(valueEl);
      tooltipEl.appendChild(row);
    };
    let total = 0;
    // Stacked charts list the top segment first — the reading order of the stack.
    const ordered = this._tooltipShowsTotal() ? [...items].reverse() : items;
    for (const it of ordered) {
      addRow(it.seriesName == null ? it.seriesId : String(it.seriesName), this._formatTooltipValue(it.y), it.tone, null);
      if (typeof it.y === 'number' && Number.isFinite(it.y)) total += it.y;
    }
    if (this._tooltipShowsTotal() && items.length > 1) {
      addRow(String(this._tooltip.totalLabel ?? ''), this._formatTooltipValue(total), null, 'tf-chart__tooltip-row--total');
    }
  }

  _handleBrushDown(e) {
    const box = this._lastPlotBox;
    if (!box) return;
    const svgRect = this._svg.getBoundingClientRect();
    const mx = e.clientX - svgRect.left;
    const my = e.clientY - svgRect.top;
    if (mx < box.x0 || mx > box.x1 || my < box.y0 || my > box.y1) return;
    e.preventDefault();
    this._brushStart = { mx, my };
    if (this._brushRect) {
      this._brushRect.setAttribute('x', String(mx));
      this._brushRect.setAttribute('width', '0');
      this._brushRect.style.display = '';
    }
  }

  _handleBrushMove(e) {
    if (this._brushStart == null || !this._brushRect || !this._lastPlotBox) return;
    const svgRect = this._svg.getBoundingClientRect();
    const mx = e.clientX - svgRect.left;
    const { x0, x1 } = this._lastPlotBox;
    const clamped = Math.max(x0, Math.min(x1, mx));
    const left = Math.min(this._brushStart.mx, clamped);
    const width = Math.abs(clamped - this._brushStart.mx);
    this._brushRect.setAttribute('x', String(left));
    this._brushRect.setAttribute('width', String(width));
  }

  _handleBrushUp(e) {
    if (this._brushStart == null || !this._lastPlotBox || !this._lastDomain) { this._brushStart = null; return; }
    const box = this._lastPlotBox;
    const svgRect = this._svg.getBoundingClientRect();
    const mx = e.clientX - svgRect.left;
    const my = e.clientY - svgRect.top;
    const clampedMx = Math.max(box.x0, Math.min(box.x1, mx));
    const clampedMy = Math.max(box.y0, Math.min(box.y1, my));
    const dx = Math.abs(clampedMx - this._brushStart.mx);
    const dy = Math.abs(clampedMy - this._brushStart.my);
    if (dx > 4 || dy > 4) {
      const xMin = Math.min(this._brushStart.mx, clampedMx);
      const xMax = Math.max(this._brushStart.mx, clampedMx);
      const yMin = Math.min(this._brushStart.my, clampedMy);
      const yMax = Math.max(this._brushStart.my, clampedMy);
      this.dispatchEvent(new CustomEvent('range-select', {
        bubbles: false,
        detail: {
          x: {
            min: pixelToData(xMin, this._xAxis, this._lastDomain.xs, box.x0, box.x1),
            max: pixelToData(xMax, this._xAxis, this._lastDomain.xs, box.x0, box.x1),
          },
          y: {
            min: pixelToData(yMax, this._yAxis, this._lastDomain.ys, box.y0, box.y1, true),
            max: pixelToData(yMin, this._yAxis, this._lastDomain.ys, box.y0, box.y1, true),
          },
          zoom_mode: this._zoom,
          brush: this._brush,
        },
      }));
    }
    this._brushStart = null;
    if (this._brushRect) this._brushRect.style.display = 'none';
  }
}

// =============================================================================
// TfLineChart
// =============================================================================

class TfLineChart extends TfCartesianChart {
  _hostClasses() { return ['tf-chart--line']; }
  _ariaLabel() { return 'Line chart'; }

  _drawPlot(svg, box, enter) {
    const { x0, x1, y0, y1 } = box;
    const visible = this._visibleSeries();
    let seriesPoints = visible.map((s) => s.points || []);
    let { xs, ys, categories } = computeDomains(seriesPoints, this._xAxis, this._yAxis);
    if (categories) {
      const sliced = applyNarrow(categories, x1 - x0, this._narrow);
      if (sliced !== categories) {
        const keep = new Set(sliced);
        seriesPoints = seriesPoints.map((pts) => pts.filter((p) => keep.has(p.x)));
        ({ xs, ys } = computeDomains(seriesPoints, this._xAxis, this._yAxis));
        categories = sliced;
      }
    }
    this._lastDomain = { xs, ys, categories };

    renderGridlinesY(svg, this._yAxis, ys, x0, x1, y0, y1);
    renderXAxis(svg, this._xAxis, xs, categories, x0, x1, y1, this._locale);
    renderYAxis(svg, this._yAxis, ys, x0, y0, y1, this._locale);

    for (let i = 0; i < visible.length; i++) {
      const s = visible[i];
      const points = seriesPoints[i];
      if (points.length === 0) continue;
      const coords = [];
      for (const p of points) {
        const px = scaleX(p.x, this._xAxis, xs, categories, x0, x1);
        const py = scaleY(p.y, this._yAxis, ys, y0, y1);
        if (px == null || py == null) continue;
        coords.push([px, py]);
        this._hoverItems.push({ seriesId: s.id, seriesName: s.name, tone: s.tone, x: p.x, y: p.y, display: p.y, px, py });
      }
      if (coords.length === 0) continue;
      const polyline = document.createElementNS(SVG_NS, 'polyline');
      polyline.setAttribute('points', coords.map((c) => `${c[0]},${c[1]}`).join(' '));
      polyline.classList.add('tf-chart__series-line');
      polyline.classList.add(`tf-chart__series-line--style-${s.style}`);
      if (s.tone) polyline.classList.add(`tf-chart__series-line--tone-${s.tone}`);
      polyline.setAttribute('data-series-id', s.id);
      if (enter) animateLineEnter(polyline, coords, i * 80);
      svg.appendChild(polyline);
      // Point dots overlay (hover detection target).
      const g = document.createElementNS(SVG_NS, 'g');
      g.classList.add('tf-chart__series-points');
      if (enter) g.classList.add('tf-chart__series-points--enter');
      g.setAttribute('data-series-id', s.id);
      let pi = 0;
      for (const p of points) {
        const px = scaleX(p.x, this._xAxis, xs, categories, x0, x1);
        const py = scaleY(p.y, this._yAxis, ys, y0, y1);
        if (px == null || py == null) { pi++; continue; }
        const c = document.createElementNS(SVG_NS, 'circle');
        c.setAttribute('cx', String(px));
        c.setAttribute('cy', String(py));
        c.setAttribute('r', '2.5');
        c.classList.add('tf-chart__series-point');
        if (s.tone) c.classList.add(`tf-chart__series-point--tone-${s.tone}`);
        c.setAttribute('data-point-index', String(pi));
        g.appendChild(c);
        pi++;
      }
      svg.appendChild(g);
    }
  }
}

if (!customElements.get('tf-line-chart')) {
  customElements.define('tf-line-chart', TfLineChart);
}

export { TfLineChart, BAR_STAGGER_MS };
