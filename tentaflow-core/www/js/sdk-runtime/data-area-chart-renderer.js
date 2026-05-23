// =============================================================================
// Plik: sdk-runtime/data-area-chart-renderer.js
// Opis: Renderer AreaChart (0x0218) — chunk 3.3d-9. Real SVG area chart z
// 3 trybami stacking (none/stacked/percent), opacity, plus pełna chart-
// shared infrastruktura (axes, legend, tooltip, zoom, brush) z LineChart.
//
// Stacking modes:
//   none    — każda seria jako osobny area pod krzywą, bazowo do y=0
//   stacked — series stack'owane (każda kolejna jest baselined na poprzedniej)
//   percent — stacked z normalizacją per-x do 100%
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs AreaChart (10 pól).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { subscribeBindRef, resolveBindRef, formatValue } from './bind-resolver.js';
import {
  SVG_NS, CHART_ZOOM_MODES,
  requireEnum, requireBool, requireU16, requireF64, assertOnlyKnownFields,
  parseChartSeries, parseChartAxis, parseChartLegend, parseChartTooltip,
  computeDomains, scaleX, scaleY,
  renderXAxis, renderYAxis, renderGridlinesY,
  buildLegend,
} from './data-chart-shared.js';

export const AREA_CHART_TAG = 0x0218;
const AREA_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
const AREA_STACKING_MODES = new Set(['none', 'stacked', 'percent']);
const PLOT_MARGIN = { top: 12, right: 16, bottom: 36, left: 48 };

function renderAreaChart(component, ctx) {
  assertOnlyKnownFields(component.fields, AREA_CHART_FIELD_KEYS, 'AreaChart');

  const seriesRaw = ctx.readField(component.fields, 0);
  if (!Array.isArray(seriesRaw) || seriesRaw.length === 0) {
    throw new TypeError('AreaChart.series: expected non-empty Array<ChartSeries>');
  }
  const series = seriesRaw.map((s, i) => parseChartSeries(s, `AreaChart.series[${i}]`));
  const seenIds = new Set();
  for (const s of series) {
    if (seenIds.has(s.id)) throw new TypeError(`AreaChart.series: duplicate id '${s.id}'`);
    seenIds.add(s.id);
  }
  const xAxisRaw = ctx.readField(component.fields, 1);
  if (xAxisRaw == null) throw new TypeError('AreaChart.x_axis is required');
  const xAxis = parseChartAxis(xAxisRaw, 'AreaChart.x_axis', ctx.locale);
  const yAxisRaw = ctx.readField(component.fields, 2);
  if (yAxisRaw == null) throw new TypeError('AreaChart.y_axis is required');
  const yAxis = parseChartAxis(yAxisRaw, 'AreaChart.y_axis', ctx.locale);
  const legendRaw = ctx.readField(component.fields, 3);
  if (legendRaw == null) throw new TypeError('AreaChart.legend is required');
  const legend = parseChartLegend(legendRaw, 'AreaChart.legend');
  const tooltipRaw = ctx.readField(component.fields, 4);
  if (tooltipRaw == null) throw new TypeError('AreaChart.tooltip is required');
  const tooltip = parseChartTooltip(tooltipRaw, 'AreaChart.tooltip', ctx.locale);
  const zoom = requireEnum(ctx.readField(component.fields, 5), CHART_ZOOM_MODES, 'AreaChart.zoom');
  const brush = requireBool(ctx.readField(component.fields, 6), 'AreaChart.brush');
  const heightPx = requireU16(ctx.readField(component.fields, 7), 'AreaChart.height_px');
  if (heightPx === 0) throw new TypeError('AreaChart.height_px must be > 0');
  const stacking = requireEnum(ctx.readField(component.fields, 8), AREA_STACKING_MODES, 'AreaChart.stacking');
  // §4 0x0218 default: opacity = 0.4.
  const opacityRaw = ctx.readField(component.fields, 9);
  const opacity = opacityRaw === undefined ? 0.4 : requireF64(opacityRaw, 'AreaChart.opacity');
  if (opacity < 0 || opacity > 1) {
    throw new TypeError('AreaChart.opacity must be in 0.0..=1.0');
  }
  // Stacking percent wymusza y_axis.scale=linear (nie ma sensu na log/category).
  if (stacking === 'percent' && yAxis.scale !== 'linear') {
    throw new TypeError('AreaChart.stacking=percent requires y_axis.scale=linear');
  }
  // Stacked / percent + log scale niezgodne (bazowanie zwykle wymaga dodawania
  // wartości — log nie nadaje się; spec'owy wybór, ale walidujemy).
  if ((stacking === 'stacked' || stacking === 'percent') && yAxis.scale === 'log') {
    throw new TypeError(`AreaChart.stacking=${stacking} incompatible with y_axis.scale=log`);
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-chart');
  wrapper.classList.add('tf-chart--area');
  wrapper.classList.add(`tf-chart--legend-${legend.position}`);
  wrapper.classList.add(`tf-chart--stacking-${stacking}`);
  wrapper.style.height = `${heightPx}px`;

  const hiddenSet = new Set();

  const root = document.createElement('div');
  root.classList.add('tf-chart__layout');
  root.classList.add(`tf-chart__layout--legend-${legend.position}`);
  wrapper.appendChild(root);

  const plot = document.createElement('div');
  plot.classList.add('tf-chart__plot');
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('width', '100%');
  svg.setAttribute('height', '100%');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', 'Area chart');
  svg.classList.add('tf-chart__svg');
  plot.appendChild(svg);

  let tooltipEl = null;
  if (tooltip.enabled) {
    tooltipEl = document.createElement('div');
    tooltipEl.classList.add('tf-chart__tooltip');
    tooltipEl.hidden = true;
    plot.appendChild(tooltipEl);
  }

  const legendEl = buildLegend(legend, series.map((s) => ({ series: s, hidden: false })), ctx, (sid) => onLegendToggle(sid));
  if (legend.position === 'top' && legendEl) root.appendChild(legendEl);
  if (legend.position === 'left' && legendEl) root.appendChild(legendEl);
  root.appendChild(plot);
  if (legend.position === 'right' && legendEl) root.appendChild(legendEl);
  if (legend.position === 'bottom' && legendEl) root.appendChild(legendEl);

  let brushStart = null;
  let brushRect = null;

  const readSeriesData = (s) => {
    let arr;
    try { arr = ctx.store.read(s.data_path); } catch { arr = undefined; }
    if (!Array.isArray(arr)) return [];
    const out = [];
    for (const p of arr) {
      if (p == null || typeof p !== 'object') continue;
      const x = p.x;
      const y = p.y;
      if (typeof y !== 'number' || !Number.isFinite(y)) continue;
      out.push({ x, y });
    }
    return out;
  };

  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const getPixelDimensions = () => {
    const rect = plot.getBoundingClientRect ? plot.getBoundingClientRect() : null;
    const w = (rect && rect.width > 0) ? rect.width : heightPx * 1.5;
    const h = (rect && rect.height > 0) ? rect.height : heightPx;
    return { w, h };
  };

  let lastDomain = null;
  let lastPlotBox = null;
  let lastVisibleSeries = [];
  let lastStackedData = null;  // dla tooltip lookup

  /// Buduje stacked data — każdy punkt ma {x, y0, y1} (baseline i top).
  /// Stacked: y0 = sum poprzednich serii w tym X; y1 = y0 + value.
  /// Percent: per-X total, każda seria proportional.
  /// Wymaga że wszystkie series mają TE SAME X (lub align po unikalnych X
  /// wartościach). Stosujemy join na unique X.
  function buildStackedData(visibleSeries, seriesPoints) {
    // Zbierz wszystkie unikalne X w kolejności pierwszego wystąpienia.
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
    // Sort numeric X ascending (string X zostaje w kolejności wystąpienia).
    if (allXs.every((x) => typeof x === 'number')) {
      allXs.sort((a, b) => a - b);
    }
    // Per series, lookup table x → value.
    const lookups = seriesPoints.map((pts) => {
      const m = new Map();
      for (const p of pts) m.set(p.x, p.y);
      return m;
    });
    // Build stacks.
    const stacks = visibleSeries.map(() => []);
    const baselines = new Array(allXs.length).fill(0);
    // For percent, najpierw compute totals per x.
    const totals = new Array(allXs.length).fill(0);
    if (stacking === 'percent') {
      for (let xi = 0; xi < allXs.length; xi++) {
        const x = allXs[xi];
        for (const lookup of lookups) {
          const v = lookup.get(x);
          if (typeof v === 'number' && v > 0) totals[xi] += v;
        }
      }
    }
    for (let si = 0; si < visibleSeries.length; si++) {
      const lookup = lookups[si];
      for (let xi = 0; xi < allXs.length; xi++) {
        const x = allXs[xi];
        const v = lookup.get(x);
        // Stacked/percent: tylko POSITIVE values stack'ują (v=0 nie
        // kontrybuuje wysokości; v<0 ignorowane bo łamałoby additive
        // semantykę). Spec: positive-only.
        if (typeof v !== 'number' || !Number.isFinite(v) || v <= 0) {
          continue;
        }
        let displayVal = v;
        if (stacking === 'percent') {
          if (totals[xi] === 0) continue;
          displayVal = (v / totals[xi]) * 100;
        }
        const y0 = baselines[xi];
        const y1 = y0 + displayVal;
        stacks[si].push({ x, y0, y1, originalY: v });
        baselines[xi] = y1;
      }
    }
    return { allXs, stacks };
  }

  function buildUnstackedData(visibleSeries, seriesPoints) {
    // Każda seria osobno z baseline y0=0. Stacking=none akceptuje też
    // wartości ujemne (area renderuje się pod baseline'em) — czyszczenie
    // NaN/non-numeric robi już readSeriesData.
    const stacks = visibleSeries.map((_, si) => {
      const pts = seriesPoints[si];
      return pts.map((p) => ({ x: p.x, y0: 0, y1: p.y, originalY: p.y }));
    });
    return { allXs: null, stacks };
  }

  const rebuild = () => {
    runRebuildCleanups();
    svg.replaceChildren();
    const { w, h } = getPixelDimensions();
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    const x0 = PLOT_MARGIN.left;
    const x1 = w - PLOT_MARGIN.right;
    const y0 = PLOT_MARGIN.top;
    const y1 = h - PLOT_MARGIN.bottom;
    if (x1 <= x0 || y1 <= y0) return;

    const visibleSeries = series.filter((s) => !hiddenSet.has(s.id));
    const seriesPoints = visibleSeries.map(readSeriesData);

    // Compute domain BEZ stacking (uses raw y values). Dla stacking
    // ys.max powinno odzwierciedlać szczyt stackowanego zakresu.
    const baseDomain = computeDomains(seriesPoints, xAxis, yAxis);
    const { xs, categories } = baseDomain;
    let ys = baseDomain.ys;

    let stackedData;
    if (stacking === 'none') {
      stackedData = buildUnstackedData(visibleSeries, seriesPoints);
    } else {
      stackedData = buildStackedData(visibleSeries, seriesPoints);
      // Adjust ys.max do max(y1) ze stacked data.
      let stackMax = 0;
      for (const stack of stackedData.stacks) {
        for (const pt of stack) if (pt.y1 > stackMax) stackMax = pt.y1;
      }
      ys = { min: 0, max: stacking === 'percent' ? 100 : Math.max(stackMax, ys.max) };
      if (yAxis.min != null) ys.min = yAxis.min;
      if (yAxis.max != null) ys.max = yAxis.max;
      if (ys.min === ys.max) ys.max = ys.min + 1;
    }
    lastDomain = { xs, ys, categories };
    lastPlotBox = { x0, x1, y0, y1 };
    lastVisibleSeries = visibleSeries.map((s, i) => ({ series: s, points: seriesPoints[i] }));
    lastStackedData = stackedData;

    renderGridlinesY(svg, yAxis, ys, x0, x1, y0, y1);
    renderXAxis(svg, xAxis, xs, categories, x0, x1, y1, ctx.locale);
    renderYAxis(svg, yAxis, ys, x0, y0, y1, ctx.locale);

    // Render areas (od dołu stack'u). Każda area to polygon:
    // top edge (lewa→prawa po y1) + bottom edge (prawa→lewa po y0).
    for (let si = 0; si < visibleSeries.length; si++) {
      const s = visibleSeries[si];
      const stack = stackedData.stacks[si];
      if (stack.length === 0) continue;
      const topCoords = [];
      const bottomCoords = [];
      for (const pt of stack) {
        const px = scaleX(pt.x, xAxis, xs, categories, x0, x1);
        const pyTop = scaleY(pt.y1, yAxis, ys, y0, y1);
        const pyBottom = scaleY(pt.y0, yAxis, ys, y0, y1);
        if (px == null || pyTop == null || pyBottom == null) continue;
        topCoords.push(`${px},${pyTop}`);
        bottomCoords.push(`${px},${pyBottom}`);
      }
      if (topCoords.length === 0) continue;
      // Polygon: top L→R + bottom R→L.
      const pts = [...topCoords, ...bottomCoords.reverse()].join(' ');
      const area = document.createElementNS(SVG_NS, 'polygon');
      area.setAttribute('points', pts);
      area.setAttribute('fill-opacity', String(opacity));
      area.classList.add('tf-chart__area');
      if (s.tone) area.classList.add(`tf-chart__area--tone-${s.tone}`);
      area.setAttribute('data-series-id', s.id);
      svg.appendChild(area);

      // Linia top edge dla lepszej czytelności (matching tone color).
      const line = document.createElementNS(SVG_NS, 'polyline');
      line.setAttribute('points', topCoords.join(' '));
      line.classList.add('tf-chart__series-line');
      line.classList.add(`tf-chart__series-line--style-${s.style}`);
      if (s.tone) line.classList.add(`tf-chart__series-line--tone-${s.tone}`);
      svg.appendChild(line);

      // Points overlay.
      const g = document.createElementNS(SVG_NS, 'g');
      g.classList.add('tf-chart__series-points');
      g.setAttribute('data-series-id', s.id);
      for (let pi = 0; pi < stack.length; pi++) {
        const pt = stack[pi];
        const px = scaleX(pt.x, xAxis, xs, categories, x0, x1);
        const py = scaleY(pt.y1, yAxis, ys, y0, y1);
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

    if (brush) {
      brushRect = document.createElementNS(SVG_NS, 'rect');
      brushRect.classList.add('tf-chart__brush');
      brushRect.setAttribute('y', String(y0));
      brushRect.setAttribute('height', String(y1 - y0));
      brushRect.hidden = true;
      svg.appendChild(brushRect);
    }
  };

  const onLegendToggle = (sid) => {
    if (hiddenSet.has(sid)) hiddenSet.delete(sid);
    else hiddenSet.add(sid);
    if (legendEl) {
      const item = legendEl.querySelector(`[data-series-id="${sid}"]`);
      if (item) item.classList.toggle('tf-chart__legend-item--hidden', hiddenSet.has(sid));
    }
    rebuild();
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('series_toggle', {
        bubbles: false,
        detail: { series_id: sid, hidden: hiddenSet.has(sid) },
      })
    );
  };

  rebuild();
  for (const s of series) {
    ctx.registerCleanup(ctx.store.subscribe(s.data_path, rebuild));
  }
  if (xAxis.label) ctx.registerCleanup(subscribeBindRef(xAxis.label, ctx.store, rebuild));
  if (yAxis.label) ctx.registerCleanup(subscribeBindRef(yAxis.label, ctx.store, rebuild));

  if (tooltip.enabled) {
    const onMove = (e) => {
      if (!lastPlotBox || lastVisibleSeries.length === 0) { tooltipEl.hidden = true; return; }
      const svgRect = svg.getBoundingClientRect();
      const mx = e.clientX - svgRect.left;
      const my = e.clientY - svgRect.top;
      const { x0, x1, y0, y1 } = lastPlotBox;
      if (mx < x0 || mx > x1 || my < y0 || my > y1) { tooltipEl.hidden = true; return; }
      let best = null;
      for (let si = 0; si < lastVisibleSeries.length; si++) {
        const entry = lastVisibleSeries[si];
        const stack = lastStackedData.stacks[si];
        for (const pt of stack) {
          const px = scaleX(pt.x, xAxis, lastDomain.xs, lastDomain.categories, x0, x1);
          const py = scaleY(pt.y1, yAxis, lastDomain.ys, y0, y1);
          if (px == null || py == null) continue;
          const dx = px - mx;
          const dy = py - my;
          const d2 = dx * dx + dy * dy;
          if (best == null || d2 < best.d2) {
            best = { d2, series: entry.series, point: pt, px, py };
          }
        }
      }
      if (!best || best.d2 > 32 * 32) { tooltipEl.hidden = true; return; }
      const seriesName = resolveBindRef(best.series.name, ctx.store);
      const valToShow = best.point.originalY;
      const yLabel = tooltip.format
        ? (() => { try { return formatValue(valToShow, tooltip.format, ctx.locale); }
                  catch { return String(valToShow); } })()
        : String(valToShow);
      tooltipEl.replaceChildren();
      const seriesEl = document.createElement('div');
      seriesEl.classList.add('tf-chart__tooltip-series');
      seriesEl.textContent = seriesName == null ? best.series.id : String(seriesName);
      tooltipEl.appendChild(seriesEl);
      const valEl = document.createElement('div');
      valEl.classList.add('tf-chart__tooltip-value');
      valEl.textContent = yLabel;
      tooltipEl.appendChild(valEl);
      tooltipEl.hidden = false;
      tooltipEl.style.left = `${best.px + 8}px`;
      tooltipEl.style.top = `${best.py - 8}px`;
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('point_hover', {
          bubbles: false,
          detail: { series_id: best.series.id, x: best.point.x, y: best.point.originalY },
        })
      );
    };
    const onLeave = () => { tooltipEl.hidden = true; };
    svg.addEventListener('mousemove', onMove);
    svg.addEventListener('mouseleave', onLeave);
    ctx.registerCleanup(() => {
      svg.removeEventListener('mousemove', onMove);
      svg.removeEventListener('mouseleave', onLeave);
    });
  }

  if (brush || zoom !== 'none') {
    const onDown = (e) => {
      if (!lastPlotBox) return;
      const svgRect = svg.getBoundingClientRect();
      const mx = e.clientX - svgRect.left;
      const my = e.clientY - svgRect.top;
      const { x0, x1, y0, y1 } = lastPlotBox;
      if (mx < x0 || mx > x1 || my < y0 || my > y1) return;
      e.preventDefault();
      brushStart = { mx, my };
      if (brushRect) {
        brushRect.setAttribute('x', String(mx));
        brushRect.setAttribute('width', '0');
        brushRect.hidden = false;
      }
    };
    const onUp = (e) => {
      if (brushStart == null || !lastPlotBox) return;
      const svgRect = svg.getBoundingClientRect();
      const mx = e.clientX - svgRect.left;
      const my = e.clientY - svgRect.top;
      const { x0, x1, y0, y1 } = lastPlotBox;
      const clampedMx = Math.max(x0, Math.min(x1, mx));
      const clampedMy = Math.max(y0, Math.min(y1, my));
      const dx = Math.abs(clampedMx - brushStart.mx);
      const dy = Math.abs(clampedMy - brushStart.my);
      if (dx > 4 || dy > 4) {
        const xMin = Math.min(brushStart.mx, clampedMx);
        const xMax = Math.max(brushStart.mx, clampedMx);
        const yMin = Math.min(brushStart.my, clampedMy);
        const yMax = Math.max(brushStart.my, clampedMy);
        const dataXMin = pixelToData(xMin, xAxis, lastDomain.xs, x0, x1);
        const dataXMax = pixelToData(xMax, xAxis, lastDomain.xs, x0, x1);
        const dataYMax = pixelToData(yMin, yAxis, lastDomain.ys, y0, y1, true);
        const dataYMin = pixelToData(yMax, yAxis, lastDomain.ys, y0, y1, true);
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('range_select', {
            bubbles: false,
            detail: {
              x: { min: dataXMin, max: dataXMax },
              y: { min: dataYMin, max: dataYMax },
              zoom_mode: zoom,
              brush,
            },
          })
        );
      }
      brushStart = null;
      if (brushRect) brushRect.hidden = true;
    };
    const onMoveBrush = (e) => {
      if (brushStart == null || !brushRect || !lastPlotBox) return;
      const svgRect = svg.getBoundingClientRect();
      const mx = e.clientX - svgRect.left;
      const { x0, x1 } = lastPlotBox;
      const clamped = Math.max(x0, Math.min(x1, mx));
      const left = Math.min(brushStart.mx, clamped);
      const width = Math.abs(clamped - brushStart.mx);
      brushRect.setAttribute('x', String(left));
      brushRect.setAttribute('width', String(width));
    };
    svg.addEventListener('mousedown', onDown);
    svg.addEventListener('mousemove', onMoveBrush);
    globalThis.document.addEventListener('mouseup', onUp);
    ctx.registerCleanup(() => {
      svg.removeEventListener('mousedown', onDown);
      svg.removeEventListener('mousemove', onMoveBrush);
      globalThis.document.removeEventListener('mouseup', onUp);
    });
  }

  if (typeof globalThis.ResizeObserver === 'function') {
    const ro = new globalThis.ResizeObserver(() => rebuild());
    ro.observe(plot);
    ctx.registerCleanup(() => ro.disconnect());
  }

  return wrapper;
}

function pixelToData(px, axis, domain, p0, p1, invertY = false) {
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

export function registerDataAreaChartRenderer() {
  if (!lookupComponentRenderer(AREA_CHART_TAG)) registerComponentRenderer(AREA_CHART_TAG, renderAreaChart);
}
