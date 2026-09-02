// =============================================================================
// Plik: components/tf-stream-chart.js
// Opis: Wykres strumieniowy SVG (okno czasu przesuwane w lewo) na bazie
// TfCartesianChart (tf-line-chart.js). Nowe próbki wchodzą przez `push()`,
// a nie przez ponowne przypisanie `series`: istniejące polilinie są
// re-projektowane w miejscu i cała warstwa serii przesuwa się transformem
// CSS o szerokość jednej próbki — bez pełnego re-renderu.
// Light DOM, klasy .tf-chart__* z controls.css.
//
// Kontrakt danych (property `series`) — jak tf-line-chart:
//   Array<{ id, name, tone, style, showInLegend,
//           points: Array<{x: number (epoch ms), y: number}> }>   // seed
// Property `window`: number sekund widocznych (domyślnie 300).
// Property `yAxis`: { min (domyślnie 0), max (null → auto, zaokrąglone do
//   ładnego ticka; skala rośnie natychmiast, maleje dopiero gdy maksimum
//   spadnie poniżej połowy), ticks, format }.
// Property `xAxis`: { format: ((secondsAgo: number) => string) | null }
//   — etykiety osi X są względne (-300 … 0 s), domyślnie "-4m" / "-30s" / "0".
// Property `fill`: boolean — wypełnienie pod linią (domyślnie true).
// Property `tooltip`/`legend`/`height`/`locale`/`animate`: jak w bazie.
//
// Metoda `push(x, values)`: `x` epoch ms, `values` = { [seriesId]: y }.
// Serie nieobecne w `values` nie dostają próbki. Punkty starsze niż okno
// (plus jedna próbka zapasu, żeby linia dochodziła do lewej krawędzi) są
// odrzucane.
//
// Eventy (bubbles:false): 'series-toggle', 'point-hover' — jak w bazie.
// =============================================================================

import {
  SVG_NS, TfCartesianChart,
  scaleX, scaleY, generateLinearTicks,
  renderXAxis, renderYAxis, renderGridlinesY,
} from './tf-line-chart.js';

const DEFAULT_WINDOW_SECS = 300;
const DEFAULT_FILL_OPACITY = 0.14;
// Slide duration ceiling: a long polling interval must not turn into a
// slow crawl that lags behind the next sample.
const MAX_SLIDE_MS = 2000;

let clipCounter = 0;

/// Default relative label: whole minutes when the offset is a multiple of
/// 60 s, seconds otherwise, and a bare "0" at the right edge.
function formatRelative(secs) {
  if (secs === 0) return '0';
  const abs = Math.abs(secs);
  if (abs >= 60 && abs % 60 === 0) return `-${abs / 60}m`;
  return `-${abs}s`;
}

/// The highest "nice" tick covering `max` — the top of the auto Y domain.
function niceCeiling(max, ticks) {
  if (!(max > 0)) return 1;
  const generated = generateLinearTicks(0, max, ticks);
  return generated[generated.length - 1];
}

class TfStreamChart extends TfCartesianChart {
  constructor() {
    super();
    this._windowSecs = DEFAULT_WINDOW_SECS;
    this._fill = true;
    this._yAxis.min = 0;
    this._lastX = null;
    this._prevX = null;
    this._usedYMax = null;
    this._layer = null;
    this._seriesEls = new Map();
    this._clipId = `tf-stream-clip-${++clipCounter}`;
    this._slideRaf = 0;
  }

  set series(value) {
    const list = Array.isArray(value) ? value : [];
    // Own copies: push() mutates the point arrays.
    this._series = list.map((s) => ({
      ...s,
      points: (s.points || [])
        .filter((p) => typeof p.x === 'number' && Number.isFinite(p.x) && typeof p.y === 'number' && Number.isFinite(p.y))
        .map((p) => ({ x: p.x, y: p.y }))
        .sort((a, b) => a.x - b.x),
    }));
    this._lastX = null;
    for (const s of this._series) {
      const last = s.points[s.points.length - 1];
      if (last && (this._lastX == null || last.x > this._lastX)) this._lastX = last.x;
    }
    this._prevX = null;
    this._usedYMax = null;
    if (this._animate) this._animPending = true;
    this._requestRender();
  }

  set window(value) {
    const n = Number(value);
    if (Number.isFinite(n) && n > 0) this._windowSecs = n;
    this._requestRender();
  }

  set fill(value) { this._fill = Boolean(value); this._requestRender(); }

  set yAxis(value) {
    super.yAxis = { min: 0, ...(value || {}) };
    this._usedYMax = null;
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this._slideRaf && typeof globalThis.cancelAnimationFrame === 'function') {
      globalThis.cancelAnimationFrame(this._slideRaf);
      this._slideRaf = 0;
    }
  }

  _hostClasses() { return ['tf-chart--stream']; }
  _ariaLabel() { return 'Streaming chart'; }

  _formatXLabel(x) {
    try {
      return new Intl.DateTimeFormat(this._locale, { hour: '2-digit', minute: '2-digit', second: '2-digit' }).format(new Date(x));
    } catch { return String(x); }
  }

  // ---- domains ----------------------------------------------------------------

  /// Right edge = newest sample (or "now" before any sample), left edge =
  /// right edge minus the window.
  _xDomain() {
    const max = this._lastX == null ? Date.now() : this._lastX;
    return { min: max - this._windowSecs * 1000, max };
  }

  _yDomain(visible, xs) {
    let max = 0;
    for (const s of visible) {
      for (const p of s.points) {
        if (p.x < xs.min) continue;
        if (p.y > max) max = p.y;
      }
    }
    const min = this._yAxis.min == null ? 0 : this._yAxis.min;
    if (this._yAxis.max != null) return { min, max: Math.max(this._yAxis.max, min + 1) };
    const ticks = this._yAxis.ticks || 4;
    let top = niceCeiling(max, ticks);
    // Hysteresis: keep the current scale while the signal stays above half
    // of it, so a brief dip does not make the whole plot jump.
    if (this._usedYMax != null && top < this._usedYMax && max >= this._usedYMax / 2) top = this._usedYMax;
    if (top <= min) top = min + 1;
    return { min, max: top };
  }

  // ---- drawing ------------------------------------------------------------------

  _drawPlot(svg, box, enter) {
    const { x0, x1, y0, y1 } = box;
    const visible = this._visibleSeries();
    const xs = this._xDomain();
    const ys = this._yDomain(visible, xs);
    this._usedYMax = ys.max;
    this._lastDomain = { xs, ys, categories: null };

    const relAxis = {
      scale: 'linear', min: -this._windowSecs, max: 0, ticks: this._xAxis.ticks || 6,
      format: typeof this._xAxis.format === 'function' ? this._xAxis.format : formatRelative,
    };
    renderGridlinesY(svg, this._yAxis, ys, x0, x1, y0, y1);
    renderXAxis(svg, relAxis, { min: -this._windowSecs, max: 0 }, null, x0, x1, y1, this._locale);
    renderYAxis(svg, this._yAxis, ys, x0, y0, y1, this._locale);

    const defs = document.createElementNS(SVG_NS, 'defs');
    const clip = document.createElementNS(SVG_NS, 'clipPath');
    clip.setAttribute('id', this._clipId);
    const rect = document.createElementNS(SVG_NS, 'rect');
    rect.setAttribute('x', String(x0));
    rect.setAttribute('y', String(y0));
    rect.setAttribute('width', String(x1 - x0));
    rect.setAttribute('height', String(y1 - y0));
    clip.appendChild(rect);
    defs.appendChild(clip);
    svg.appendChild(defs);

    this._layer = document.createElementNS(SVG_NS, 'g');
    this._layer.classList.add('tf-chart__stream-layer');
    this._layer.setAttribute('clip-path', `url(#${this._clipId})`);
    svg.appendChild(this._layer);

    this._seriesEls = new Map();
    for (const s of visible) {
      const els = { area: null, line: null };
      if (this._fill) {
        els.area = document.createElementNS(SVG_NS, 'polygon');
        els.area.classList.add('tf-chart__area');
        if (s.tone) els.area.classList.add(`tf-chart__area--tone-${s.tone}`);
        els.area.setAttribute('fill-opacity', String(DEFAULT_FILL_OPACITY));
        els.area.setAttribute('data-series-id', s.id);
        if (enter) els.area.classList.add('tf-chart__area--enter');
        this._layer.appendChild(els.area);
      }
      els.line = document.createElementNS(SVG_NS, 'polyline');
      els.line.classList.add('tf-chart__series-line');
      els.line.classList.add(`tf-chart__series-line--style-${s.style || 'solid'}`);
      if (s.tone) els.line.classList.add(`tf-chart__series-line--tone-${s.tone}`);
      els.line.setAttribute('data-series-id', s.id);
      this._layer.appendChild(els.line);
      this._seriesEls.set(s.id, els);
    }
    this._project(box, xs, ys);
  }

  /// Re-computes every visible series' pixel coordinates against the given
  /// domains and writes them into the existing SVG nodes.
  _project(box, xs, ys) {
    const { x0, x1, y0, y1 } = box;
    this._hoverItems = [];
    for (const s of this._visibleSeries()) {
      const els = this._seriesEls.get(s.id);
      if (!els) continue;
      const coords = [];
      for (const p of s.points) {
        const px = scaleX(p.x, this._xAxis, xs, null, x0, x1);
        const py = scaleY(p.y, this._yAxis, ys, y0, y1);
        if (px == null || py == null) continue;
        coords.push(`${px},${py}`);
        if (px >= x0 - 0.5) {
          this._hoverItems.push({ seriesId: s.id, seriesName: s.name, tone: s.tone, x: p.x, y: p.y, display: p.y, px, py });
        }
      }
      els.line.setAttribute('points', coords.join(' '));
      if (els.area) {
        if (coords.length < 2) {
          els.area.setAttribute('points', '');
        } else {
          const first = coords[0].split(',')[0];
          const last = coords[coords.length - 1].split(',')[0];
          els.area.setAttribute('points', `${first},${y1} ${coords.join(' ')} ${last},${y1}`);
        }
      }
    }
  }

  // ---- streaming ------------------------------------------------------------------

  push(x, values) {
    if (typeof x !== 'number' || !Number.isFinite(x) || !values) return;
    const keepFrom = x - this._windowSecs * 1000;
    let touched = false;
    for (const s of this._series) {
      const y = values[s.id];
      if (typeof y === 'number' && Number.isFinite(y)) {
        s.points.push({ x, y });
        touched = true;
      }
      // Keep one point left of the window so the line reaches the edge.
      while (s.points.length > 1 && s.points[1].x < keepFrom) s.points.shift();
    }
    if (!touched) return;
    this._prevX = this._lastX;
    this._lastX = x;
    if (!this._svg || !this._lastPlotBox || !this._layer) return;

    const box = this._lastPlotBox;
    const visible = this._visibleSeries();
    const xs = this._xDomain();
    const ys = this._yDomain(visible, xs);
    if (ys.max !== this._usedYMax || ys.min !== this._lastDomain?.ys.min) {
      // Scale change: axes and gridlines have to be redrawn anyway.
      this._animPending = false;
      this._renderPlot(true);
      return;
    }
    this._lastDomain = { xs, ys, categories: null };
    this._project(box, xs, ys);
    this._hideHover();
    this._slide(box, xs);
  }

  /// One-sample slide: the layer is placed where the previous frame's
  /// points were and eased back to zero, so the eye sees a continuous
  /// leftward motion instead of a jump.
  _slide(box, xs) {
    const layer = this._layer;
    if (!layer || this._prevX == null || !this._motionAllowed()) return;
    const deltaMs = this._lastX - this._prevX;
    if (!(deltaMs > 0)) return;
    const dx = (deltaMs / (xs.max - xs.min)) * (box.x1 - box.x0);
    if (!(dx > 0)) return;
    const duration = Math.min(MAX_SLIDE_MS, deltaMs);
    layer.style.transition = 'none';
    layer.style.transform = `translateX(${dx}px)`;
    if (typeof globalThis.requestAnimationFrame !== 'function') {
      layer.style.transform = '';
      return;
    }
    if (this._slideRaf) globalThis.cancelAnimationFrame(this._slideRaf);
    this._slideRaf = globalThis.requestAnimationFrame(() => {
      this._slideRaf = 0;
      // Force style flush so the transition starts from the offset position.
      void layer.getBoundingClientRect();
      layer.style.transition = `transform ${duration}ms linear`;
      layer.style.transform = 'translateX(0)';
    });
  }
}

if (!customElements.get('tf-stream-chart')) {
  customElements.define('tf-stream-chart', TfStreamChart);
}

export { TfStreamChart };
