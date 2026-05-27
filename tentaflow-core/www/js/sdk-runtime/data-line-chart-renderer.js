// =============================================================================
// Plik: sdk-runtime/data-line-chart-renderer.js
// Opis: Renderer LineChart (0x0216) — chunk 3.3d-8. Real SVG chart z
// axes (X/Y), gridlines, polylines per series z tone+style (solid/dashed/
// dotted), legend (top/bottom/left/right/none + alignment z toggle),
// tooltip on mousemove (nearest point per series), zoom (none/x/y/both)
// + brush emit `range_select` event.
//
// Data shape per series.data_path: Array<{x: number|string, y: number}>.
// Time scale: x = Unix ms. Category scale: x = string.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs LineChart.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { subscribeBindRef, resolveBindRef, formatValue } from './bind-resolver.js';
import {
  SVG_NS, CHART_ZOOM_MODES,
  requireEnum, requireBool, requireU16, assertOnlyKnownFields,
  parseChartSeries, parseChartAxis, parseChartLegend, parseChartTooltip,
  computeDomains, scaleX, scaleY,
  renderXAxis, renderYAxis, renderGridlinesY,
  buildLegend,
} from './data-chart-shared.js';

export const LINE_CHART_TAG = 0x0216;
const LINE_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

// Margins do osi + labels.
const PLOT_MARGIN = { top: 12, right: 16, bottom: 36, left: 48 };

function renderLineChart(component, ctx) {
  assertOnlyKnownFields(component.fields, LINE_CHART_FIELD_KEYS, 'LineChart');

  const seriesRaw = ctx.readField(component.fields, 0);
  if (!Array.isArray(seriesRaw) || seriesRaw.length === 0) {
    throw new TypeError('LineChart.series: expected non-empty Array<ChartSeries>');
  }
  const series = seriesRaw.map((s, i) => parseChartSeries(s, `LineChart.series[${i}]`));
  const seenIds = new Set();
  for (const s of series) {
    if (seenIds.has(s.id)) throw new TypeError(`LineChart.series: duplicate id '${s.id}'`);
    seenIds.add(s.id);
  }
  const xAxisRaw = ctx.readField(component.fields, 1);
  if (xAxisRaw == null) throw new TypeError('LineChart.x_axis is required');
  const xAxis = parseChartAxis(xAxisRaw, 'LineChart.x_axis', ctx.locale);
  const yAxisRaw = ctx.readField(component.fields, 2);
  if (yAxisRaw == null) throw new TypeError('LineChart.y_axis is required');
  const yAxis = parseChartAxis(yAxisRaw, 'LineChart.y_axis', ctx.locale);
  const legendRaw = ctx.readField(component.fields, 3);
  if (legendRaw == null) throw new TypeError('LineChart.legend is required');
  const legend = parseChartLegend(legendRaw, 'LineChart.legend');
  const tooltipRaw = ctx.readField(component.fields, 4);
  if (tooltipRaw == null) throw new TypeError('LineChart.tooltip is required');
  const tooltip = parseChartTooltip(tooltipRaw, 'LineChart.tooltip', ctx.locale);
  const zoom = requireEnum(ctx.readField(component.fields, 5), CHART_ZOOM_MODES, 'LineChart.zoom');
  const brush = requireBool(ctx.readField(component.fields, 6), 'LineChart.brush');
  const heightPx = requireU16(ctx.readField(component.fields, 7), 'LineChart.height_px');
  if (heightPx === 0) throw new TypeError('LineChart.height_px must be > 0');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-chart');
  wrapper.classList.add('tf-chart--line');
  wrapper.classList.add(`tf-chart--legend-${legend.position}`);
  wrapper.style.height = `${heightPx}px`;

  // Hidden series state (per series id). Toggle przez legend click.
  const hiddenSet = new Set();

  // Layout flex root: legend top/bottom → column; left/right → row.
  let plotContainer;
  let legendEl;
  // Legend builder upgraded later after we know hidden state.

  const onLegendToggle = (sid) => {
    if (hiddenSet.has(sid)) hiddenSet.delete(sid);
    else hiddenSet.add(sid);
    // Update legend item visual state.
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

  // Plot SVG container — wymiary computed dynamicznie z resize observer'a.
  const plot = document.createElement('div');
  plot.classList.add('tf-chart__plot');
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('width', '100%');
  svg.setAttribute('height', '100%');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', 'Line chart');
  svg.classList.add('tf-chart__svg');
  plot.appendChild(svg);

  // Tooltip element (HTML overlay).
  let tooltipEl = null;
  if (tooltip.enabled) {
    tooltipEl = document.createElement('div');
    tooltipEl.classList.add('tf-chart__tooltip');
    tooltipEl.hidden = true;
    plot.appendChild(tooltipEl);
  }

  // Buduj root layout zalezny od legend position.
  const root = document.createElement('div');
  root.classList.add('tf-chart__layout');
  root.classList.add(`tf-chart__layout--legend-${legend.position}`);
  wrapper.appendChild(root);

  legendEl = buildLegend(legend, series.map((s) => ({ series: s, hidden: false })), ctx, onLegendToggle);

  if (legend.position === 'top' && legendEl) root.appendChild(legendEl);
  if (legend.position === 'left' && legendEl) root.appendChild(legendEl);
  root.appendChild(plot);
  if (legend.position === 'right' && legendEl) root.appendChild(legendEl);
  if (legend.position === 'bottom' && legendEl) root.appendChild(legendEl);
  plotContainer = plot;

  // Brush state — drag region; emit range_select przy mouseup gdy brush=true.
  let brushStart = null;
  let brushRect = null;

  // Series data cache — z store snapshot przy rebuild.
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

  // Per-rebuild cleanups (gridline/path listeners).
  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  // Pixel dimensions — calc'd przy każdym rebuildzie (responsive).
  const getPixelDimensions = () => {
    const rect = plotContainer.getBoundingClientRect ? plotContainer.getBoundingClientRect() : null;
    // happy-dom może zwrócić width=0 dla unmounted; fallback do height_px square.
    const w = (rect && rect.width > 0) ? rect.width : heightPx * 1.5;
    const h = (rect && rect.height > 0) ? rect.height : heightPx;
    return { w, h };
  };

  let lastDomain = null;
  let lastPlotBox = null;
  let lastVisibleSeries = [];

  const rebuild = () => {
    runRebuildCleanups();
    svg.replaceChildren();
    const { w, h } = getPixelDimensions();
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    const x0 = PLOT_MARGIN.left;
    const x1 = w - PLOT_MARGIN.right;
    const y0 = PLOT_MARGIN.top;
    const y1 = h - PLOT_MARGIN.bottom;
    if (x1 <= x0 || y1 <= y0) return;  // za mały container

    const visibleSeries = series.filter((s) => !hiddenSet.has(s.id));
    const seriesPoints = visibleSeries.map(readSeriesData);
    const { xs, ys, categories } = computeDomains(seriesPoints, xAxis, yAxis);
    lastDomain = { xs, ys, categories };
    lastPlotBox = { x0, x1, y0, y1 };
    lastVisibleSeries = visibleSeries.map((s, i) => ({ series: s, points: seriesPoints[i] }));

    // Gridlines.
    renderGridlinesY(svg, yAxis, ys, x0, x1, y0, y1);
    // Axes.
    renderXAxis(svg, xAxis, xs, categories, x0, x1, y1, ctx.locale);
    renderYAxis(svg, yAxis, ys, x0, y0, y1, ctx.locale);

    // Series lines.
    for (let i = 0; i < visibleSeries.length; i++) {
      const s = visibleSeries[i];
      const points = seriesPoints[i];
      if (points.length === 0) continue;
      const coords = [];
      for (const p of points) {
        const px = scaleX(p.x, xAxis, xs, categories, x0, x1);
        const py = scaleY(p.y, yAxis, ys, y0, y1);
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
      // Dots na punktach (overlay dla hover detection).
      const g = document.createElementNS(SVG_NS, 'g');
      g.classList.add('tf-chart__series-points');
      g.setAttribute('data-series-id', s.id);
      let pi = 0;
      for (const p of points) {
        const px = scaleX(p.x, xAxis, xs, categories, x0, x1);
        const py = scaleY(p.y, yAxis, ys, y0, y1);
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

    // Brush rect overlay (drag region).
    if (brush) {
      brushRect = document.createElementNS(SVG_NS, 'rect');
      brushRect.classList.add('tf-chart__brush');
      brushRect.setAttribute('y', String(y0));
      brushRect.setAttribute('height', String(y1 - y0));
      brushRect.hidden = true;
      svg.appendChild(brushRect);
    }
  };

  // Initial render — defer mikro-task żeby getBoundingClientRect zwrócił width
  // po append do parent (poza testem w happy-dom gdzie zwraca 0).
  rebuild();
  // Subscribe each series' data_path.
  for (const s of series) {
    ctx.registerCleanup(ctx.store.subscribe(s.data_path, rebuild));
  }
  // Subscribe na axis label/format zmiany.
  if (xAxis.label) ctx.registerCleanup(subscribeBindRef(xAxis.label, ctx.store, rebuild));
  if (yAxis.label) ctx.registerCleanup(subscribeBindRef(yAxis.label, ctx.store, rebuild));

  // Tooltip + hover detection.
  if (tooltip.enabled) {
    const onMove = (e) => {
      if (!lastPlotBox || lastVisibleSeries.length === 0) { tooltipEl.hidden = true; return; }
      const svgRect = svg.getBoundingClientRect();
      const mx = e.clientX - svgRect.left;
      const my = e.clientY - svgRect.top;
      const { x0, x1, y0, y1 } = lastPlotBox;
      if (mx < x0 || mx > x1 || my < y0 || my > y1) { tooltipEl.hidden = true; return; }
      // Find nearest point across all series.
      let best = null;
      for (const { series: s, points } of lastVisibleSeries) {
        for (let i = 0; i < points.length; i++) {
          const p = points[i];
          const px = scaleX(p.x, xAxis, lastDomain.xs, lastDomain.categories, x0, x1);
          const py = scaleY(p.y, yAxis, lastDomain.ys, y0, y1);
          if (px == null || py == null) continue;
          const dx = px - mx;
          const dy = py - my;
          const d2 = dx * dx + dy * dy;
          if (best == null || d2 < best.d2) {
            best = { d2, series: s, point: p, px, py };
          }
        }
      }
      if (!best || best.d2 > 32 * 32) { tooltipEl.hidden = true; return; }
      // Build tooltip content.
      const seriesName = resolveBindRef(best.series.name, ctx.store);
      const yLabel = tooltip.format
        ? (() => { try { return formatValue(best.point.y, tooltip.format, ctx.locale); }
                  catch { return String(best.point.y); } })()
        : String(best.point.y);
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
      // Emit point_hover event per spec handler.
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('point_hover', {
          bubbles: false,
          detail: { series_id: best.series.id, x: best.point.x, y: best.point.y },
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

  // Brush + zoom — both wykorzystują drag.
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
        // Convert pixel range back to data domain.
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

  // Resize observer — rebuild gdy container zmienia size.
  if (typeof globalThis.ResizeObserver === 'function') {
    const ro = new globalThis.ResizeObserver(() => rebuild());
    ro.observe(plot);
    ctx.registerCleanup(() => ro.disconnect());
  }

  return wrapper;
}

/// Convert pixel coordinate back to data domain. `invertY` = true dla Y axis
/// (SVG Y inverted).
function pixelToData(px, axis, domain, p0, p1, invertY = false) {
  if (axis.scale === 'category') return null;  // category nie ma odwrotnej mapy
  if (axis.scale === 'log') {
    const lm = Math.log10(domain.min);
    const lx = Math.log10(domain.max);
    const ratio = invertY ? (p1 - px) / (p1 - p0) : (px - p0) / (p1 - p0);
    return Math.pow(10, lm + ratio * (lx - lm));
  }
  const ratio = invertY ? (p1 - px) / (p1 - p0) : (px - p0) / (p1 - p0);
  return domain.min + ratio * (domain.max - domain.min);
}

export function registerDataLineChartRenderer() {
  if (!lookupComponentRenderer(LINE_CHART_TAG)) registerComponentRenderer(LINE_CHART_TAG, renderLineChart);
}
