// =============================================================================
// Plik: components/tf-bar-chart.js
// Opis: Wykres słupkowy SVG (BarChart 0x0217) — orientacje vertical/
// horizontal, stacking none/stacked/percent — oraz tryb pojedynczego paska
// segmentowego (StackedBar 0x021A) jako mode='single'. Dziedziczy layout/
// legendę z TfCartesianChart (tf-line-chart.js).
//
// Tryb wykresu (mode='chart', domyślny) — kontrakt jak tf-line-chart:
//   `series` Array<{id, name, tone, showInLegend, points: [{x, y}]}>,
//   `xAxis`/`yAxis`, `legend`, `orientation`: 'vertical'|'horizontal',
//   `stacking`: 'none'|'stacked'|'percent', `height`, `locale`.
//   Event 'series-toggle' detail {series_id, hidden}.
//
// Tryb mode='single' (StackedBar) — jeden poziomy pasek 100% szerokości:
//   `segments`: Array<{id: string, label: string, value: number, tone}>,
//   `total`: number (>0; 0 = pasek pusty), `showLegend`, `showPercentages`,
//   `height`. Overflow ponad total jest przycinany.
// =============================================================================

import {
  SVG_NS, TfCartesianChart,
  computeDomains, scaleY,
  renderXAxis, renderYAxis, renderGridlinesY,
} from './tf-line-chart.js';

const BAR_GROUP_PADDING_FRACTION = 0.2;  // 20% padding between category groups
const MAX_BAR_GROUP_WIDTH = 56;          // px — keeps sparse charts readable

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
    // mode='single' state.
    this._segments = [];
    this._total = 0;
    this._showLegend = false;
    this._showPercentages = false;
  }

  set mode(value) { this._mode = value === 'single' ? 'single' : 'chart'; this._render(); }
  set orientation(value) { this._orientation = typeof value === 'string' ? value : 'vertical'; this._render(); }
  set stacking(value) { this._stacking = typeof value === 'string' ? value : 'none'; this._render(); }
  set segments(value) { this._segments = Array.isArray(value) ? value : []; this._render(); }
  set total(value) { const n = Number(value); this._total = Number.isFinite(n) && n > 0 ? n : 0; this._render(); }
  set showLegend(value) { this._showLegend = Boolean(value); this._render(); }
  set showPercentages(value) { this._showPercentages = Boolean(value); this._render(); }

  _hostClasses() {
    return ['tf-chart--bar', `tf-chart--orientation-${this._orientation}`, `tf-chart--stacking-${this._stacking}`];
  }
  _ariaLabel() { return 'Bar chart'; }

  _render() {
    if (this._mode === 'single') {
      this._renderSingle();
      return;
    }
    super._render();
  }

  // ---- mode='chart' -----------------------------------------------------------

  _drawPlot(svg, box) {
    const { x0, x1, y0, y1 } = box;
    const visible = this._visibleSeries();
    const seriesPoints = visible.map((s) => s.points || []);

    // Unique X categories in occurrence order; numeric-only sets sorted.
    const categories = [];
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
    this._lastDomain = { xs: { min: 0, max: 1 }, ys: yDomain, categories };

    if (this._orientation === 'vertical') {
      renderGridlinesY(svg, this._yAxis, yDomain, x0, x1, y0, y1);
      renderXAxis(svg, { ...this._xAxis, scale: 'category' }, null, categories, x0, x1, y1, this._locale);
      renderYAxis(svg, this._yAxis, yDomain, x0, y0, y1, this._locale);

      const groupWidth = (x1 - x0) / Math.max(1, categories.length);
      for (let ci = 0; ci < categories.length; ci++) {
        const cat = categories[ci];
        // Cap the bar width and centre it in its band: with one or two
        // categories an uncapped bar stretches across half the chart and stops
        // reading as a bar.
        const innerW = Math.min(groupWidth * (1 - BAR_GROUP_PADDING_FRACTION), MAX_BAR_GROUP_WIDTH);
        const groupX = x0 + ci * groupWidth + (groupWidth - innerW) / 2;
        if (this._stacking === 'none') {
          // Grouped: per-series sub-bar inside the category group.
          const barW = innerW / Math.max(1, visible.length);
          for (let si = 0; si < visible.length; si++) {
            const s = visible[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt) continue;
            const py = scaleY(pt.y, this._yAxis, yDomain, y0, y1);
            const yBase = scaleY(Math.max(0, yDomain.min), this._yAxis, yDomain, y0, y1);
            if (py == null || yBase == null) continue;
            this._appendBar(svg, s, groupX + si * barW, Math.min(py, yBase), barW, Math.abs(py - yBase));
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
          for (let si = 0; si < visible.length; si++) {
            const s = visible[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt || pt.y <= 0) continue;
            const displayVal = this._stacking === 'percent' ? (pt.y / total) * 100 : pt.y;
            const topY = scaleY(accum + displayVal, this._yAxis, yDomain, y0, y1);
            const botY = scaleY(accum, this._yAxis, yDomain, y0, y1);
            if (topY == null || botY == null) { accum += displayVal; continue; }
            this._appendBar(svg, s, groupX, topY, innerW, Math.abs(botY - topY));
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
            accum += displayVal;
          }
        }
      }
    }
  }

  _appendBar(svg, series, x, y, w, h) {
    const rect = document.createElementNS(SVG_NS, 'rect');
    rect.setAttribute('x', String(x));
    rect.setAttribute('y', String(y));
    rect.setAttribute('width', String(w));
    rect.setAttribute('height', String(h));
    rect.classList.add('tf-chart__bar');
    if (series.tone) rect.classList.add(`tf-chart__bar--tone-${series.tone}`);
    rect.setAttribute('data-series-id', series.id);
    svg.appendChild(rect);
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
