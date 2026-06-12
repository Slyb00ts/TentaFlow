// =============================================================================
// Plik: components/tf-line-chart.js
// Opis: Wykres liniowy SVG (LineChart 0x0216) + bazowa klasa TfCartesianChart
// i helpery rysowania (osie, ticki, domeny, legenda) współdzielone przez
// tf-area-chart i tf-bar-chart. Light DOM, klasy .tf-chart__* z controls.css.
//
// Kontrakt danych (property `series`):
//   Array<{
//     id: string,                 // stabilny identyfikator serii
//     name: string,               // etykieta do legendy (już zresolvowana)
//     tone: string|null,          // neutral/primary/success/warning/critical/info/muted
//     style: 'solid'|'dashed'|'dotted',
//     showInLegend: boolean,
//     points: Array<{x: number|string, y: number}>,  // y zawsze skończona liczba
//   }>
// Property `xAxis`/`yAxis`: { scale: 'linear'|'log'|'time'|'category',
//   min: number|null, max: number|null, ticks: number|null,
//   format: ((value) => string)|null }  // formatter może rzucić → fallback
// Property `legend`: { position: 'top'|'bottom'|'left'|'right'|'none',
//   alignment: 'start'|'center'|'end' } | null
// Property `tooltip`: { enabled: boolean, format: ((value) => string)|null }
// Property `zoom`: 'none'|'x'|'y'|'both'; `brush`: boolean; `height`: number px;
// `locale`: string (Intl dla domyślnego formatowania ticków).
//
// Eventy (bubbles:false, na hoście):
//   'series-toggle' detail {series_id, hidden}
//   'point-hover'   detail {series_id, x, y}
//   'range-select'  detail {x:{min,max}, y:{min,max}, zoom_mode, brush}
// =============================================================================

export const SVG_NS = 'http://www.w3.org/2000/svg';

const PLOT_MARGIN = { top: 12, right: 16, bottom: 36, left: 48 };

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
/// time scale → date string, otherwise number formatting.
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
    tickValues = categories || [];
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

// =============================================================================
// TfCartesianChart — shared host/layout/legend/tooltip/brush machinery
// =============================================================================

const DEFAULT_AXIS = { scale: 'linear', min: null, max: null, ticks: null, format: null };

export class TfCartesianChart extends HTMLElement {
  constructor() {
    super();
    this._series = [];
    this._xAxis = { ...DEFAULT_AXIS };
    this._yAxis = { ...DEFAULT_AXIS };
    this._legend = null;
    this._tooltip = { enabled: false, format: null };
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
    this._brushStart = null;
    this._brushRect = null;
    this._lastDomain = null;
    this._lastPlotBox = null;
    this._ro = null;
    this._onDocUp = (e) => this._handleBrushUp(e);
  }

  set series(value) { this._series = Array.isArray(value) ? value : []; this._render(); }
  set xAxis(value) { this._xAxis = { ...DEFAULT_AXIS, ...(value || {}) }; this._render(); }
  set yAxis(value) { this._yAxis = { ...DEFAULT_AXIS, ...(value || {}) }; this._render(); }
  set legend(value) { this._legend = value || null; this._render(); }
  set tooltip(value) { this._tooltip = { enabled: false, format: null, ...(value || {}) }; this._render(); }
  set zoom(value) { this._zoom = typeof value === 'string' ? value : 'none'; this._render(); }
  set brush(value) { this._brush = Boolean(value); this._render(); }
  set height(value) { const n = Number(value); if (Number.isFinite(n) && n > 0) this._height = n; this._render(); }
  set locale(value) { this._locale = value || undefined; this._render(); }

  connectedCallback() {
    document.addEventListener('mouseup', this._onDocUp);
    if (typeof globalThis.ResizeObserver === 'function' && !this._ro) {
      this._ro = new globalThis.ResizeObserver(() => this._renderPlot());
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
  /// `this._lastDomain` ({xs, ys, categories}) when brush/tooltip apply.
  _drawPlot(_svg, _box) {}

  /// Iterates candidate tooltip points: {seriesId, x, y, display, px, py}.
  _tooltipCandidates(_box) { return []; }

  // ---- rendering ------------------------------------------------------------

  _visibleSeries() { return this._series.filter((s) => !this._hidden.has(s.id)); }

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
    if (this._tooltip.enabled) {
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
    this._renderPlot();
  }

  _pixelDimensions() {
    const rect = this._plot && this._plot.getBoundingClientRect ? this._plot.getBoundingClientRect() : null;
    // happy-dom returns width=0 for unmounted nodes; fall back to height-based box.
    const w = (rect && rect.width > 0) ? rect.width : this._height * 1.5;
    const h = (rect && rect.height > 0) ? rect.height : this._height;
    return { w, h };
  }

  _renderPlot() {
    const svg = this._svg;
    if (!svg) return;
    svg.replaceChildren();
    this._brushRect = null;
    const { w, h } = this._pixelDimensions();
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    const box = {
      x0: PLOT_MARGIN.left,
      x1: w - PLOT_MARGIN.right,
      y0: PLOT_MARGIN.top,
      y1: h - PLOT_MARGIN.bottom,
    };
    if (box.x1 <= box.x0 || box.y1 <= box.y0) { this._lastPlotBox = null; return; }
    this._lastPlotBox = box;
    this._drawPlot(svg, box);
    if (this._brush) {
      this._brushRect = document.createElementNS(SVG_NS, 'rect');
      this._brushRect.classList.add('tf-chart__brush');
      this._brushRect.setAttribute('y', String(box.y0));
      this._brushRect.setAttribute('height', String(box.y1 - box.y0));
      this._brushRect.hidden = true;
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
    this._renderPlot();
    this.dispatchEvent(new CustomEvent('series-toggle', {
      bubbles: false,
      detail: { series_id: sid, hidden: this._hidden.has(sid) },
    }));
  }

  // ---- tooltip + brush listeners ----------------------------------------------

  _attachSvgListeners() {
    const svg = this._svg;
    if (this._tooltip.enabled) {
      svg.addEventListener('mousemove', (e) => this._handleTooltipMove(e));
      svg.addEventListener('mouseleave', () => { if (this._tooltipEl) this._tooltipEl.hidden = true; });
    }
    if (this._brush || this._zoom !== 'none') {
      svg.addEventListener('mousedown', (e) => this._handleBrushDown(e));
      svg.addEventListener('mousemove', (e) => this._handleBrushMove(e));
    }
  }

  _handleTooltipMove(e) {
    const tooltipEl = this._tooltipEl;
    if (!tooltipEl) return;
    const box = this._lastPlotBox;
    if (!box) { tooltipEl.hidden = true; return; }
    const svgRect = this._svg.getBoundingClientRect();
    const mx = e.clientX - svgRect.left;
    const my = e.clientY - svgRect.top;
    if (mx < box.x0 || mx > box.x1 || my < box.y0 || my > box.y1) { tooltipEl.hidden = true; return; }
    let best = null;
    for (const cand of this._tooltipCandidates(box)) {
      const dx = cand.px - mx;
      const dy = cand.py - my;
      const d2 = dx * dx + dy * dy;
      if (best == null || d2 < best.d2) best = { d2, ...cand };
    }
    if (!best || best.d2 > 32 * 32) { tooltipEl.hidden = true; return; }
    const yLabel = typeof this._tooltip.format === 'function'
      ? (() => { try { return this._tooltip.format(best.display); } catch { return String(best.display); } })()
      : String(best.display);
    tooltipEl.replaceChildren();
    const seriesEl = document.createElement('div');
    seriesEl.classList.add('tf-chart__tooltip-series');
    seriesEl.textContent = best.seriesName == null ? best.seriesId : String(best.seriesName);
    tooltipEl.appendChild(seriesEl);
    const valEl = document.createElement('div');
    valEl.classList.add('tf-chart__tooltip-value');
    valEl.textContent = yLabel;
    tooltipEl.appendChild(valEl);
    tooltipEl.hidden = false;
    tooltipEl.style.left = `${best.px + 8}px`;
    tooltipEl.style.top = `${best.py - 8}px`;
    this.dispatchEvent(new CustomEvent('point-hover', {
      bubbles: false,
      detail: { series_id: best.seriesId, x: best.x, y: best.y },
    }));
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
      this._brushRect.hidden = false;
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
    if (this._brushRect) this._brushRect.hidden = true;
  }
}

// =============================================================================
// TfLineChart
// =============================================================================

class TfLineChart extends TfCartesianChart {
  _hostClasses() { return ['tf-chart--line']; }
  _ariaLabel() { return 'Line chart'; }

  _drawPlot(svg, box) {
    const { x0, x1, y0, y1 } = box;
    const visible = this._visibleSeries();
    const seriesPoints = visible.map((s) => s.points || []);
    const { xs, ys, categories } = computeDomains(seriesPoints, this._xAxis, this._yAxis);
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
        coords.push(`${px},${py}`);
      }
      if (coords.length === 0) continue;
      const polyline = document.createElementNS(SVG_NS, 'polyline');
      polyline.setAttribute('points', coords.join(' '));
      polyline.classList.add('tf-chart__series-line');
      polyline.classList.add(`tf-chart__series-line--style-${s.style}`);
      if (s.tone) polyline.classList.add(`tf-chart__series-line--tone-${s.tone}`);
      polyline.setAttribute('data-series-id', s.id);
      svg.appendChild(polyline);
      // Point dots overlay (hover detection target).
      const g = document.createElementNS(SVG_NS, 'g');
      g.classList.add('tf-chart__series-points');
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

  *_tooltipCandidates(box) {
    if (!this._lastDomain) return;
    const { xs, ys, categories } = this._lastDomain;
    for (const s of this._visibleSeries()) {
      for (const p of s.points || []) {
        const px = scaleX(p.x, this._xAxis, xs, categories, box.x0, box.x1);
        const py = scaleY(p.y, this._yAxis, ys, box.y0, box.y1);
        if (px == null || py == null) continue;
        yield { seriesId: s.id, seriesName: s.name, x: p.x, y: p.y, display: p.y, px, py };
      }
    }
  }
}

if (!customElements.get('tf-line-chart')) {
  customElements.define('tf-line-chart', TfLineChart);
}

export { TfLineChart };
