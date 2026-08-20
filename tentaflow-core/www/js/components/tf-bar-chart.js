// =============================================================================
// Plik: components/tf-bar-chart.js
// Opis: Wykres słupkowy SVG (BarChart 0x0217) — orientacje vertical/
// horizontal, stacking none/stacked/percent — oraz tryb pojedynczego paska
// segmentowego (StackedBar 0x021A) jako mode='single'. Dziedziczy layout/
// legendę z TfCartesianChart (tf-line-chart.js).
//
// Tryb wykresu (mode='chart', domyślny) — kontrakt jak tf-line-chart:
//   `series` Array<{id, name, tone, showInLegend, points: [{x, y}]}>,
//   `xAxis`/`yAxis`, `legend`, `tooltip`, `crosshair`, `animate`, `narrow`,
//   `orientation`: 'vertical'|'horizontal', `stacking`: 'none'|'stacked'|'percent',
//   `maxBarWidth`: number px (domyślnie 34, szerokość pojedynczego słupka),
//   `height`, `locale`. Słupki pionowe: górny segment stosu to <path> z
//   zaokrąglonym szczytem, niższe segmenty i słupki poziome to <rect>.
//   Event 'series-toggle' detail {series_id, hidden}, 'point-hover'.
//
// Tryb mode='single' (StackedBar) — jeden poziomy pasek 100% szerokości:
//   `segments`: Array<{id: string, label: string, value: number, tone}>,
//   `total`: number (>0; 0 = pasek pusty), `showLegend`, `showPercentages`,
//   `height`. Overflow ponad total jest przycinany.
// =============================================================================

import {
  SVG_NS, TfCartesianChart, BAR_STAGGER_MS,
  computeDomains, scaleY, applyNarrow, generateLinearTicks,
  renderXAxis, renderYAxis, renderGridlinesY,
} from './tf-line-chart.js';

const BAR_GROUP_PADDING_FRACTION = 0.3;  // padding between category groups
const DEFAULT_MAX_BAR_WIDTH = 34;        // px — keeps sparse charts readable
const BAR_TOP_RADIUS = 3;

/// Path of a bar whose top corners are rounded (radius capped by size).
export function roundedTopBarPath(x, y, w, h, radius = BAR_TOP_RADIUS) {
  const r = Math.max(0, Math.min(radius, w / 2, h));
  const x1 = x + w;
  const y1 = y + h;
  return `M${x},${y1}V${y + r}Q${x},${y} ${x + r},${y}H${x1 - r}Q${x1},${y} ${x1},${y + r}V${y1}Z`;
}

/// Scales a value along the value axis (linear/log) to pixels — used in
/// horizontal layout where X carries the value.
function scaleAlongValueAxis(value, axis, domain, p0, p1) {
  if (typeof value !== 'number' || !Number.isFinite(value)) return null;
  if (axis.scale === 'log') {
    if (value <= 0 || domain.min <= 0 || domain.max <= 0) return null;
    const lm = Math.log10(domain.min);
    const lx = Math.log10(domain.max);
    return p0 + ((Math.log10(value) - lm) / (lx - lm)) * (p1 - p0);
  }
  return p0 + ((value - domain.min) / (domain.max - domain.min)) * (p1 - p0);
}

class TfBarChart extends TfCartesianChart {
  constructor() {
    super();
    this._mode = 'chart';
    this._orientation = 'vertical';
    this._stacking = 'none';
    this._maxBarWidth = DEFAULT_MAX_BAR_WIDTH;
    // mode='single' state.
    this._segments = [];
    this._total = 0;
    this._showLegend = false;
    this._showPercentages = false;
  }

  set mode(value) { this._mode = value === 'single' ? 'single' : 'chart'; this._requestRender(); }
  set orientation(value) { this._orientation = typeof value === 'string' ? value : 'vertical'; this._requestRender(); }
  set stacking(value) { this._stacking = typeof value === 'string' ? value : 'none'; this._requestRender(); }
  set maxBarWidth(value) {
    const n = Number(value);
    this._maxBarWidth = Number.isFinite(n) && n > 0 ? n : DEFAULT_MAX_BAR_WIDTH;
    this._requestRender();
  }
  set segments(value) { this._segments = Array.isArray(value) ? value : []; this._requestRender(); }
  set total(value) { const n = Number(value); this._total = Number.isFinite(n) && n > 0 ? n : 0; this._requestRender(); }
  set showLegend(value) { this._showLegend = Boolean(value); this._requestRender(); }
  set showPercentages(value) { this._showPercentages = Boolean(value); this._requestRender(); }

  _hostClasses() {
    return ['tf-chart--bar', `tf-chart--orientation-${this._orientation}`, `tf-chart--stacking-${this._stacking}`];
  }
  _ariaLabel() { return 'Bar chart'; }
  _tooltipShowsTotal() { return this._stacking === 'stacked'; }
  _formatXLabel(x) { return String(x); }
  _hoverAxis() { return this._orientation === 'horizontal' ? 'y' : 'x'; }

  _render() {
    if (this._mode === 'single') {
      this._renderSingle();
      return;
    }
    super._render();
  }

  // ---- mode='chart' -----------------------------------------------------------

  _drawPlot(svg, box, enter) {
    const { x0, x1, y0, y1 } = box;
    const visible = this._visibleSeries();
    let seriesPoints = visible.map((s) => s.points || []);

    // Unique X categories in occurrence order; numeric-only sets sorted.
    let categories = [];
    const seenCat = new Set();
    for (const pts of seriesPoints) {
      for (const p of pts) {
        const key = typeof p.x === 'string' ? `s:${p.x}` : `n:${p.x}`;
        if (!seenCat.has(key)) {
          seenCat.add(key);
          categories.push(p.x);
        }
      }
    }
    if (categories.every((x) => typeof x === 'number')) {
      categories.sort((a, b) => a - b);
    }
    if (this._orientation === 'vertical') {
      const sliced = applyNarrow(categories, x1 - x0, this._narrow);
      if (sliced !== categories) {
        const keep = new Set(sliced);
        seriesPoints = seriesPoints.map((pts) => pts.filter((p) => keep.has(p.x)));
        categories = sliced;
      }
    }

    // Stacking-aware Y domain.
    let yDomain;
    if (this._stacking === 'none') {
      const baseDomain = computeDomains(seriesPoints, { ...this._xAxis, scale: 'category' }, this._yAxis);
      yDomain = baseDomain.ys;
      // Bars encode magnitude from a zero baseline, so the axis must include 0
      // (unlike line/area, where computeDomains may start at the data min).
      // Without this, bars shorter than the auto-computed min render with zero/
      // negative height and vanish. Honor an explicit yAxis.min/max override.
      if (this._yAxis.min == null && yDomain.min > 0) yDomain.min = 0;
      if (this._yAxis.max == null && yDomain.max < 0) yDomain.max = 0;
    } else {
      const totals = new Array(categories.length).fill(0);
      const catIdx = new Map(categories.map((c, i) => [typeof c === 'string' ? `s:${c}` : `n:${c}`, i]));
      for (const pts of seriesPoints) {
        for (const p of pts) {
          const k = typeof p.x === 'string' ? `s:${p.x}` : `n:${p.x}`;
          const i = catIdx.get(k);
          if (i == null) continue;
          if (typeof p.y === 'number' && p.y > 0) totals[i] += p.y;
        }
      }
      const max = this._stacking === 'percent' ? 100 : Math.max(0, ...totals);
      yDomain = { min: 0, max };
      if (this._yAxis.min != null) yDomain.min = this._yAxis.min;
      if (this._yAxis.max != null) yDomain.max = this._yAxis.max;
      if (yDomain.min === yDomain.max) yDomain.max = yDomain.min + 1;
    }
    // Without an explicit max the axis ends on a round tick with ~10 %
    // headroom above the tallest bar, so the top gridline always clears the
    // data instead of being grazed by it.
    if (this._yAxis.max == null && this._yAxis.scale !== 'log' && yDomain.max > yDomain.min) {
      const target = yDomain.min + (yDomain.max - yDomain.min) * 1.1;
      const ticks = generateLinearTicks(yDomain.min, target, this._yAxis.ticks || 6);
      const top = ticks[ticks.length - 1];
      if (Number.isFinite(top) && top >= yDomain.max) yDomain.max = top;
    }
    this._lastDomain = { xs: { min: 0, max: 1 }, ys: yDomain, categories };

    if (this._orientation === 'vertical') {
      renderGridlinesY(svg, this._yAxis, yDomain, x0, x1, y0, y1);
      renderXAxis(svg, { ...this._xAxis, scale: 'category' }, null, categories, x0, x1, y1, this._locale);
      renderYAxis(svg, this._yAxis, yDomain, x0, y0, y1, this._locale);

      const groupWidth = (x1 - x0) / Math.max(1, categories.length);
      const barCap = this._maxBarWidth * (this._stacking === 'none' ? Math.max(1, visible.length) : 1);
      for (let ci = 0; ci < categories.length; ci++) {
        const cat = categories[ci];
        // Cap the bar width and centre it in its band: with one or two
        // categories an uncapped bar stretches across half the chart and stops
        // reading as a bar.
        const innerW = Math.min(groupWidth * (1 - BAR_GROUP_PADDING_FRACTION), barCap);
        const groupX = x0 + ci * groupWidth + (groupWidth - innerW) / 2;
        const centerX = x0 + groupWidth * (ci + 0.5);
        const yBase = scaleY(Math.max(0, yDomain.min), this._yAxis, yDomain, y0, y1);
        // Bars of one category share the stagger delay and grow from the axis.
        const anim = enter ? { delay: ci * BAR_STAGGER_MS, originY: yBase == null ? y1 : yBase } : null;
        if (this._stacking === 'none') {
          // Grouped: per-series sub-bar inside the category group.
          const barW = innerW / Math.max(1, visible.length);
          for (let si = 0; si < visible.length; si++) {
            const s = visible[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt) continue;
            const py = scaleY(pt.y, this._yAxis, yDomain, y0, y1);
            if (py == null || yBase == null) continue;
            this._appendBar(svg, s, groupX + si * barW, Math.min(py, yBase), barW, Math.abs(py - yBase), { top: py <= yBase, anim });
            this._hoverItems.push({ seriesId: s.id, seriesName: s.name, tone: s.tone, x: cat, y: pt.y, display: pt.y, px: centerX, py: Math.min(py, yBase) });
          }
        } else {
          // Stacked / percent: per-category baseline.
          let accum = 0;
          let total = 0;
          if (this._stacking === 'percent') {
            for (let si = 0; si < visible.length; si++) {
              const pt = seriesPoints[si].find((p) => p.x === cat);
              if (pt && pt.y > 0) total += pt.y;
            }
            if (total === 0) continue;
          }
          let topIndex = -1;
          for (let si = visible.length - 1; si >= 0; si--) {
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (pt && pt.y > 0) { topIndex = si; break; }
          }
          for (let si = 0; si < visible.length; si++) {
            const s = visible[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt || pt.y <= 0) continue;
            const displayVal = this._stacking === 'percent' ? (pt.y / total) * 100 : pt.y;
            const topY = scaleY(accum + displayVal, this._yAxis, yDomain, y0, y1);
            const botY = scaleY(accum, this._yAxis, yDomain, y0, y1);
            if (topY == null || botY == null) { accum += displayVal; continue; }
            this._appendBar(svg, s, groupX, topY, innerW, Math.abs(botY - topY), { top: si === topIndex, anim });
            this._hoverItems.push({ seriesId: s.id, seriesName: s.name, tone: s.tone, x: cat, y: pt.y, display: displayVal, px: centerX, py: topY });
            accum += displayVal;
          }
        }
      }
    } else {
      // Horizontal: X axis carries the value, Y axis lists categories.
      renderGridlinesY(svg, this._yAxis, yDomain, x0, x1, y0, y1);
      renderXAxis(svg, this._yAxis, yDomain, null, x0, x1, y1, this._locale);
      const yGroup = document.createElementNS(SVG_NS, 'g');
      yGroup.classList.add('tf-chart__axis');
      yGroup.classList.add('tf-chart__axis--y');
      yGroup.setAttribute('transform', `translate(${x0}, 0)`);
      const ylineEl = document.createElementNS(SVG_NS, 'line');
      ylineEl.setAttribute('x1', '0'); ylineEl.setAttribute('x2', '0');
      ylineEl.setAttribute('y1', String(y0)); ylineEl.setAttribute('y2', String(y1));
      ylineEl.classList.add('tf-chart__axis-line');
      yGroup.appendChild(ylineEl);
      const groupHeight = (y1 - y0) / Math.max(1, categories.length);
      for (let ci = 0; ci < categories.length; ci++) {
        const cy = y0 + ci * groupHeight + groupHeight / 2;
        const txt = document.createElementNS(SVG_NS, 'text');
        txt.setAttribute('x', '-6');
        txt.setAttribute('y', String(cy));
        txt.setAttribute('text-anchor', 'end');
        txt.setAttribute('dominant-baseline', 'middle');
        txt.classList.add('tf-chart__axis-label');
        txt.textContent = String(categories[ci]);
        yGroup.appendChild(txt);
      }
      svg.appendChild(yGroup);

      for (let ci = 0; ci < categories.length; ci++) {
        const cat = categories[ci];
        const groupY = y0 + ci * groupHeight + groupHeight * BAR_GROUP_PADDING_FRACTION / 2;
        const innerH = groupHeight * (1 - BAR_GROUP_PADDING_FRACTION);
        if (this._stacking === 'none') {
          const barH = innerH / Math.max(1, visible.length);
          for (let si = 0; si < visible.length; si++) {
            const s = visible[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt) continue;
            const pxRight = scaleAlongValueAxis(pt.y, this._yAxis, yDomain, x0, x1);
            const baselineVal = this._yAxis.scale === 'log' ? yDomain.min : Math.max(0, yDomain.min);
            const pxLeft = scaleAlongValueAxis(baselineVal, this._yAxis, yDomain, x0, x1);
            if (pxRight == null || pxLeft == null) continue;
            this._appendBar(svg, s, Math.min(pxRight, pxLeft), groupY + si * barH, Math.abs(pxRight - pxLeft), barH);
            this._hoverItems.push({ seriesId: s.id, seriesName: s.name, tone: s.tone, x: cat, y: pt.y, display: pt.y, px: pxRight, py: groupY + innerH / 2 });
          }
        } else {
          let accum = 0;
          let total = 0;
          if (this._stacking === 'percent') {
            for (let si = 0; si < visible.length; si++) {
              const pt = seriesPoints[si].find((p) => p.x === cat);
              if (pt && pt.y > 0) total += pt.y;
            }
            if (total === 0) continue;
          }
          for (let si = 0; si < visible.length; si++) {
            const s = visible[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt || pt.y <= 0) continue;
            const displayVal = this._stacking === 'percent' ? (pt.y / total) * 100 : pt.y;
            const pxFrom = scaleAlongValueAxis(accum, this._yAxis, yDomain, x0, x1);
            const pxTo = scaleAlongValueAxis(accum + displayVal, this._yAxis, yDomain, x0, x1);
            if (pxFrom == null || pxTo == null) { accum += displayVal; continue; }
            this._appendBar(svg, s, Math.min(pxFrom, pxTo), groupY, Math.abs(pxTo - pxFrom), innerH);
            this._hoverItems.push({ seriesId: s.id, seriesName: s.name, tone: s.tone, x: cat, y: pt.y, display: displayVal, px: pxTo, py: groupY + innerH / 2 });
            accum += displayVal;
          }
        }
      }
    }
  }

  /// `opts.top` → <path> with rounded top corners (the visible top of a
  /// stack), otherwise a plain <rect>. `opts.anim` = {delay, originY} marks
  /// the element for the scaleY entry animation anchored at the axis.
  _appendBar(svg, series, x, y, w, h, opts = null) {
    let el;
    if (opts && opts.top) {
      el = document.createElementNS(SVG_NS, 'path');
      el.setAttribute('d', roundedTopBarPath(x, y, w, h));
    } else {
      el = document.createElementNS(SVG_NS, 'rect');
      el.setAttribute('x', String(x));
      el.setAttribute('y', String(y));
      el.setAttribute('width', String(w));
      el.setAttribute('height', String(h));
    }
    el.classList.add('tf-chart__bar');
    if (series.tone) el.classList.add(`tf-chart__bar--tone-${series.tone}`);
    el.setAttribute('data-series-id', series.id);
    if (opts && opts.anim) {
      el.classList.add('tf-chart__bar--enter');
      el.style.transformOrigin = `0 ${opts.anim.originY}px`;
      el.style.animationDelay = `${opts.anim.delay}ms`;
    }
    svg.appendChild(el);
  }

  // ---- mode='single' (StackedBar) ----------------------------------------------

  _renderSingle() {
    for (const c of this._appliedClasses) this.classList.remove(c);
    this._appliedClasses = ['tf-stacked-bar'];
    this.classList.add('tf-stacked-bar');
    this.style.height = '';
    this.style.minHeight = `${this._height}px`;
    this._svg = null;
    this._plot = null;
    this._lastPlotBox = null;

    const barWrap = document.createElement('div');
    barWrap.classList.add('tf-stacked-bar__bar');
    barWrap.setAttribute('role', 'img');
    barWrap.setAttribute('aria-label', 'Stacked bar');
    barWrap.style.height = `${this._height}px`;

    let legendEl = null;
    if (this._showLegend) {
      legendEl = document.createElement('ul');
      legendEl.classList.add('tf-stacked-bar__legend');
      legendEl.setAttribute('role', 'list');
    }

    const total = this._total;
    let accumValue = 0;
    for (const seg of this._segments) {
      const value = typeof seg.value === 'number' && Number.isFinite(seg.value) && seg.value >= 0 ? seg.value : 0;
      // Clamp to remaining capacity (overflow above total is dropped).
      const usableValue = total > 0 ? Math.min(value, Math.max(0, total - accumValue)) : 0;
      const percent = total > 0 ? (usableValue / total) * 100 : 0;
      accumValue += usableValue;

      const segEl = document.createElement('div');
      segEl.classList.add('tf-stacked-bar__segment');
      segEl.classList.add(`tf-stacked-bar__segment--tone-${seg.tone}`);
      segEl.setAttribute('data-segment-id', seg.id);
      segEl.style.width = `${percent}%`;
      const labelText = seg.label != null && seg.label !== '' ? String(seg.label) : seg.id;
      segEl.setAttribute('title', `${labelText}: ${value}`);
      if (this._showPercentages && percent >= 5) {
        const pct = document.createElement('span');
        pct.classList.add('tf-stacked-bar__segment-percent');
        pct.textContent = `${percent.toFixed(percent >= 10 ? 0 : 1)}%`;
        segEl.appendChild(pct);
      }
      barWrap.appendChild(segEl);

      if (legendEl) {
        const li = document.createElement('li');
        li.classList.add('tf-stacked-bar__legend-item');
        const sw = document.createElement('span');
        sw.classList.add('tf-stacked-bar__legend-swatch');
        sw.classList.add(`tf-stacked-bar__legend-swatch--tone-${seg.tone}`);
        li.appendChild(sw);
        const labelEl = document.createElement('span');
        labelEl.classList.add('tf-stacked-bar__legend-label');
        labelEl.textContent = labelText;
        li.appendChild(labelEl);
        const valueEl = document.createElement('span');
        valueEl.classList.add('tf-stacked-bar__legend-value');
        valueEl.textContent = this._showPercentages ? `${percent.toFixed(1)}%` : String(value);
        li.appendChild(valueEl);
        legendEl.appendChild(li);
      }
    }
    // Remaining unfilled space (sum(values) < total).
    const usedPercent = (accumValue / Math.max(1, total)) * 100;
    if (total > 0 && usedPercent < 100) {
      const rest = document.createElement('div');
      rest.classList.add('tf-stacked-bar__segment-rest');
      rest.style.width = `${100 - usedPercent}%`;
      barWrap.appendChild(rest);
    }

    const children = [barWrap];
    if (legendEl) children.push(legendEl);
    this.replaceChildren(...children);
  }
}

if (!customElements.get('tf-bar-chart')) {
  customElements.define('tf-bar-chart', TfBarChart);
}

export { TfBarChart };
