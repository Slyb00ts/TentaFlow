// =============================================================================
// Plik: components/tf-area-chart.js
// Opis: Wykres warstwowy SVG (AreaChart 0x0218) ze stackingiem
// none/stacked/percent i opacity. Dziedziczy layout/legendę/tooltip/brush
// z TfCartesianChart (tf-line-chart.js).
//
// Kontrakt danych: jak tf-line-chart (`series` z points {x, y}), plus:
//   `stacking`: 'none'|'stacked'|'percent'
//   `opacity`:  number 0..1 (fill-opacity poligonów)
// Eventy: 'series-toggle', 'point-hover' (y = wartość oryginalna, nie
// stackowana), 'range-select' — jak w tf-line-chart.
// =============================================================================

import {
  SVG_NS, TfCartesianChart,
  computeDomains, scaleX, scaleY, applyNarrow, animateLineEnter,
  renderXAxis, renderYAxis, renderGridlinesY,
} from './tf-line-chart.js';

class TfAreaChart extends TfCartesianChart {
  constructor() {
    super();
    this._stacking = 'none';
    this._opacity = 0.4;
  }

  set stacking(value) { this._stacking = typeof value === 'string' ? value : 'none'; this._requestRender(); }
  set opacity(value) { const n = Number(value); if (Number.isFinite(n)) this._opacity = n; this._requestRender(); }

  _hostClasses() { return ['tf-chart--area', `tf-chart--stacking-${this._stacking}`]; }
  _ariaLabel() { return 'Area chart'; }
  _tooltipShowsTotal() { return this._stacking === 'stacked'; }

  /// Stacked data — each point becomes {x, y0, y1, originalY} (baseline and
  /// top). Stacked: y0 = sum of previous series at this X. Percent: per-X
  /// total normalised to 100. Series are joined on unique X values.
  _buildStackedData(visible, seriesPoints) {
    const allXs = [];
    const seenX = new Set();
    for (const pts of seriesPoints) {
      for (const p of pts) {
        const key = typeof p.x === 'string' ? `s:${p.x}` : `n:${p.x}`;
        if (!seenX.has(key)) {
          seenX.add(key);
          allXs.push(p.x);
        }
      }
    }
    // Numeric X sorted ascending (string X keeps occurrence order).
    if (allXs.every((x) => typeof x === 'number')) {
      allXs.sort((a, b) => a - b);
    }
    const lookups = seriesPoints.map((pts) => {
      const m = new Map();
      for (const p of pts) m.set(p.x, p.y);
      return m;
    });
    const stacks = visible.map(() => []);
    const baselines = new Array(allXs.length).fill(0);
    const totals = new Array(allXs.length).fill(0);
    if (this._stacking === 'percent') {
      for (let xi = 0; xi < allXs.length; xi++) {
        const x = allXs[xi];
        for (const lookup of lookups) {
          const v = lookup.get(x);
          if (typeof v === 'number' && v > 0) totals[xi] += v;
        }
      }
    }
    for (let si = 0; si < visible.length; si++) {
      const lookup = lookups[si];
      for (let xi = 0; xi < allXs.length; xi++) {
        const x = allXs[xi];
        const v = lookup.get(x);
        // Only positive values stack — zero adds no height, negatives would
        // break the additive semantics (spec: positive-only).
        if (typeof v !== 'number' || !Number.isFinite(v) || v <= 0) {
          continue;
        }
        let displayVal = v;
        if (this._stacking === 'percent') {
          if (totals[xi] === 0) continue;
          displayVal = (v / totals[xi]) * 100;
        }
        const y0 = baselines[xi];
        const y1 = y0 + displayVal;
        stacks[si].push({ x, y0, y1, originalY: v });
        baselines[xi] = y1;
      }
    }
    return stacks;
  }

  /// stacking=none: each series alone with baseline y0=0; negative values
  /// are allowed (area renders below the baseline).
  _buildUnstackedData(visible, seriesPoints) {
    return visible.map((_, si) => seriesPoints[si].map((p) => ({ x: p.x, y0: 0, y1: p.y, originalY: p.y })));
  }

  _drawPlot(svg, box, enter) {
    const { x0, x1, y0, y1 } = box;
    const visible = this._visibleSeries();
    let seriesPoints = visible.map((s) => s.points || []);

    let baseDomain = computeDomains(seriesPoints, this._xAxis, this._yAxis);
    let { xs, categories } = baseDomain;
    if (categories) {
      const sliced = applyNarrow(categories, x1 - x0, this._narrow);
      if (sliced !== categories) {
        const keep = new Set(sliced);
        seriesPoints = seriesPoints.map((pts) => pts.filter((p) => keep.has(p.x)));
        baseDomain = computeDomains(seriesPoints, this._xAxis, this._yAxis);
        xs = baseDomain.xs;
        categories = sliced;
      }
    }
    let ys = baseDomain.ys;

    let stacks;
    if (this._stacking === 'none') {
      stacks = this._buildUnstackedData(visible, seriesPoints);
    } else {
      stacks = this._buildStackedData(visible, seriesPoints);
      // ys.max must reflect the top of the stacked range.
      let stackMax = 0;
      for (const stack of stacks) {
        for (const pt of stack) if (pt.y1 > stackMax) stackMax = pt.y1;
      }
      ys = { min: 0, max: this._stacking === 'percent' ? 100 : Math.max(stackMax, ys.max) };
      if (this._yAxis.min != null) ys.min = this._yAxis.min;
      if (this._yAxis.max != null) ys.max = this._yAxis.max;
      if (ys.min === ys.max) ys.max = ys.min + 1;
    }
    this._lastDomain = { xs, ys, categories };

    renderGridlinesY(svg, this._yAxis, ys, x0, x1, y0, y1);
    renderXAxis(svg, this._xAxis, xs, categories, x0, x1, y1, this._locale);
    renderYAxis(svg, this._yAxis, ys, x0, y0, y1, this._locale);

    // Areas bottom-up. Each area is a polygon: top edge (L→R over y1) +
    // bottom edge (R→L over y0).
    for (let si = 0; si < visible.length; si++) {
      const s = visible[si];
      const stack = stacks[si];
      if (stack.length === 0) continue;
      const topCoords = [];
      const bottomCoords = [];
      for (const pt of stack) {
        const px = scaleX(pt.x, this._xAxis, xs, categories, x0, x1);
        const pyTop = scaleY(pt.y1, this._yAxis, ys, y0, y1);
        const pyBottom = scaleY(pt.y0, this._yAxis, ys, y0, y1);
        if (px == null || pyTop == null || pyBottom == null) continue;
        topCoords.push([px, pyTop]);
        bottomCoords.push(`${px},${pyBottom}`);
        this._hoverItems.push({ seriesId: s.id, seriesName: s.name, tone: s.tone, x: pt.x, y: pt.originalY, display: pt.originalY, px, py: pyTop });
      }
      if (topCoords.length === 0) continue;
      const topStr = topCoords.map((c) => `${c[0]},${c[1]}`);
      const pts = [...topStr, ...bottomCoords.reverse()].join(' ');
      const area = document.createElementNS(SVG_NS, 'polygon');
      area.setAttribute('points', pts);
      area.setAttribute('fill-opacity', String(this._opacity));
      area.classList.add('tf-chart__area');
      if (enter) area.classList.add('tf-chart__area--enter');
      if (s.tone) area.classList.add(`tf-chart__area--tone-${s.tone}`);
      area.setAttribute('data-series-id', s.id);
      svg.appendChild(area);

      // Top edge line for readability (matching tone color).
      const line = document.createElementNS(SVG_NS, 'polyline');
      line.setAttribute('points', topStr.join(' '));
      line.classList.add('tf-chart__series-line');
      line.classList.add(`tf-chart__series-line--style-${s.style}`);
      if (s.tone) line.classList.add(`tf-chart__series-line--tone-${s.tone}`);
      if (enter) animateLineEnter(line, topCoords, si * 80);
      svg.appendChild(line);

      // Points overlay.
      const g = document.createElementNS(SVG_NS, 'g');
      g.classList.add('tf-chart__series-points');
      if (enter) g.classList.add('tf-chart__series-points--enter');
      g.setAttribute('data-series-id', s.id);
      for (const pt of stack) {
        const px = scaleX(pt.x, this._xAxis, xs, categories, x0, x1);
        const py = scaleY(pt.y1, this._yAxis, ys, y0, y1);
        if (px == null || py == null) continue;
        const c = document.createElementNS(SVG_NS, 'circle');
        c.setAttribute('cx', String(px));
        c.setAttribute('cy', String(py));
        c.setAttribute('r', '2.5');
        c.classList.add('tf-chart__series-point');
        if (s.tone) c.classList.add(`tf-chart__series-point--tone-${s.tone}`);
        g.appendChild(c);
      }
      svg.appendChild(g);
    }
  }
}

if (!customElements.get('tf-area-chart')) {
  customElements.define('tf-area-chart', TfAreaChart);
}

export { TfAreaChart };
