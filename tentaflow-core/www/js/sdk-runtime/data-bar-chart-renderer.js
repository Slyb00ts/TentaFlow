// =============================================================================
// Plik: sdk-runtime/data-bar-chart-renderer.js
// Opis: Renderery BarChart (0x0217) + StackedBar (0x021A) — chunk 3.3d-10.
//
// BarChart: 3 stacking modes (none/stacked/percent), 2 orientations
// (vertical/horizontal), grouped bars dla stacking=none (każda seria w
// osobnym sub-slocie per category), shared chart-shared infrastructure
// (axes, legend). Data shape per series: Array<{x: string|number, y: number}>.
//
// StackedBar: pojedynczy poziomy bar 100% szerokości z segmentami
// proportionalnymi do value/total. show_percentages: per-segment label %.
// total: BindRef resolved jako liczba; jeśli sum(values) > total → bar
// wypełniony do total (overflow ignorowany).
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs
// BarChart (7 pól) + StackedBar (5 pól).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { subscribeBindRef, resolveBindRef } from './bind-resolver.js';
import {
  SVG_NS, TONES,
  requireEnum, requireBool, requireU16, requireString, assertOnlyKnownFields,
  parseChartSeries, parseChartAxis, parseChartLegend,
  computeDomains, scaleY,
  renderXAxis, renderYAxis, renderGridlinesY,
  buildLegend,
  ID_RE,
} from './data-chart-shared.js';

export const BAR_CHART_TAG = 0x0217;
export const STACKED_BAR_TAG = 0x021A;
const BAR_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);
const STACKED_BAR_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
const STACK_SEGMENT_KEYS = new Set([0, 1, 2, 3]);
const CHART_ORIENTATIONS = new Set(['vertical', 'horizontal']);
const BAR_STACKING_MODES = new Set(['none', 'stacked', 'percent']);
const PLOT_MARGIN = { top: 12, right: 16, bottom: 36, left: 48 };
const BAR_GROUP_PADDING_FRACTION = 0.2;  // 20% padding między grupami kategorii

// =============================================================================
// BarChart (0x0217)
// =============================================================================

function renderBarChart(component, ctx) {
  assertOnlyKnownFields(component.fields, BAR_CHART_FIELD_KEYS, 'BarChart');

  const seriesRaw = ctx.readField(component.fields, 0);
  if (!Array.isArray(seriesRaw) || seriesRaw.length === 0) {
    throw new TypeError('BarChart.series: expected non-empty Array<ChartSeries>');
  }
  const series = seriesRaw.map((s, i) => parseChartSeries(s, `BarChart.series[${i}]`));
  const seenIds = new Set();
  for (const s of series) {
    if (seenIds.has(s.id)) throw new TypeError(`BarChart.series: duplicate id '${s.id}'`);
    seenIds.add(s.id);
  }
  const xAxisRaw = ctx.readField(component.fields, 1);
  if (xAxisRaw == null) throw new TypeError('BarChart.x_axis is required');
  const xAxis = parseChartAxis(xAxisRaw, 'BarChart.x_axis', ctx.locale);
  const yAxisRaw = ctx.readField(component.fields, 2);
  if (yAxisRaw == null) throw new TypeError('BarChart.y_axis is required');
  const yAxis = parseChartAxis(yAxisRaw, 'BarChart.y_axis', ctx.locale);
  const orientation = requireEnum(ctx.readField(component.fields, 3), CHART_ORIENTATIONS, 'BarChart.orientation');
  const stacking = requireEnum(ctx.readField(component.fields, 4), BAR_STACKING_MODES, 'BarChart.stacking');
  const legendRaw = ctx.readField(component.fields, 5);
  if (legendRaw == null) throw new TypeError('BarChart.legend is required');
  const legend = parseChartLegend(legendRaw, 'BarChart.legend');
  const heightPx = requireU16(ctx.readField(component.fields, 6), 'BarChart.height_px');
  if (heightPx === 0) throw new TypeError('BarChart.height_px must be > 0');
  if ((stacking === 'stacked' || stacking === 'percent') && yAxis.scale === 'log') {
    throw new TypeError(`BarChart.stacking=${stacking} incompatible with y_axis.scale=log`);
  }
  if (stacking === 'percent' && yAxis.scale !== 'linear') {
    throw new TypeError('BarChart.stacking=percent requires y_axis.scale=linear');
  }
  // BarChart wymaga category x-axis (bars są per category) — log/time też
  // formalnie się sprawdza, ale category jest natural. Pozwalamy linear ale
  // wówczas każdy X traktowany jako osobna kategoria (po unique values).

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-chart');
  wrapper.classList.add('tf-chart--bar');
  wrapper.classList.add(`tf-chart--orientation-${orientation}`);
  wrapper.classList.add(`tf-chart--stacking-${stacking}`);
  wrapper.classList.add(`tf-chart--legend-${legend.position}`);
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
  svg.setAttribute('aria-label', 'Bar chart');
  svg.classList.add('tf-chart__svg');
  plot.appendChild(svg);

  const legendEl = buildLegend(legend, series.map((s) => ({ series: s, hidden: false })), ctx, (sid) => onLegendToggle(sid));
  if (legend.position === 'top' && legendEl) root.appendChild(legendEl);
  if (legend.position === 'left' && legendEl) root.appendChild(legendEl);
  root.appendChild(plot);
  if (legend.position === 'right' && legendEl) root.appendChild(legendEl);
  if (legend.position === 'bottom' && legendEl) root.appendChild(legendEl);

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

    // Zbierz wszystkie unikalne kategorie X w kolejności wystąpienia.
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

    // Compute Y domain z stacking-aware logic.
    let yDomain;
    if (stacking === 'none') {
      const baseDomain = computeDomains(seriesPoints, { ...xAxis, scale: 'category' }, yAxis);
      yDomain = baseDomain.ys;
    } else {
      // Stacked / percent: total per category.
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
      const max = stacking === 'percent' ? 100 : Math.max(0, ...totals);
      yDomain = { min: 0, max };
      if (yAxis.min != null) yDomain.min = yAxis.min;
      if (yAxis.max != null) yDomain.max = yAxis.max;
      if (yDomain.min === yDomain.max) yDomain.max = yDomain.min + 1;
    }

    if (orientation === 'vertical') {
      // X axis = category, Y axis = value.
      renderGridlinesY(svg, yAxis, yDomain, x0, x1, y0, y1);
      renderXAxis(svg, { ...xAxis, scale: 'category' }, null, categories, x0, x1, y1, ctx.locale);
      renderYAxis(svg, yAxis, yDomain, x0, y0, y1, ctx.locale);

      const groupWidth = (x1 - x0) / Math.max(1, categories.length);
      for (let ci = 0; ci < categories.length; ci++) {
        const cat = categories[ci];
        const groupX = x0 + ci * groupWidth + groupWidth * BAR_GROUP_PADDING_FRACTION / 2;
        const innerW = groupWidth * (1 - BAR_GROUP_PADDING_FRACTION);
        if (stacking === 'none') {
          // Grouped: per-series sub-bar w obrębie group.
          const barW = innerW / Math.max(1, visibleSeries.length);
          for (let si = 0; si < visibleSeries.length; si++) {
            const s = visibleSeries[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt) continue;
            const py = scaleY(pt.y, yAxis, yDomain, y0, y1);
            const yBase = scaleY(Math.max(0, yDomain.min), yAxis, yDomain, y0, y1);
            if (py == null || yBase == null) continue;
            const rectY = Math.min(py, yBase);
            const rectH = Math.abs(py - yBase);
            const rect = document.createElementNS(SVG_NS, 'rect');
            rect.setAttribute('x', String(groupX + si * barW));
            rect.setAttribute('y', String(rectY));
            rect.setAttribute('width', String(barW));
            rect.setAttribute('height', String(rectH));
            rect.classList.add('tf-chart__bar');
            if (s.tone) rect.classList.add(`tf-chart__bar--tone-${s.tone}`);
            rect.setAttribute('data-series-id', s.id);
            svg.appendChild(rect);
          }
        } else {
          // Stacked / percent: baseline per kategoria.
          let baselineY = scaleY(0, yAxis, yDomain, y0, y1);
          let accum = 0;
          let total = 0;
          if (stacking === 'percent') {
            for (let si = 0; si < visibleSeries.length; si++) {
              const pt = seriesPoints[si].find((p) => p.x === cat);
              if (pt && pt.y > 0) total += pt.y;
            }
            if (total === 0) continue;
          }
          for (let si = 0; si < visibleSeries.length; si++) {
            const s = visibleSeries[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt || pt.y <= 0) continue;
            const displayVal = stacking === 'percent' ? (pt.y / total) * 100 : pt.y;
            const topY = scaleY(accum + displayVal, yAxis, yDomain, y0, y1);
            const botY = scaleY(accum, yAxis, yDomain, y0, y1);
            if (topY == null || botY == null) { accum += displayVal; continue; }
            const rect = document.createElementNS(SVG_NS, 'rect');
            rect.setAttribute('x', String(groupX));
            rect.setAttribute('y', String(topY));
            rect.setAttribute('width', String(innerW));
            rect.setAttribute('height', String(Math.abs(botY - topY)));
            rect.classList.add('tf-chart__bar');
            if (s.tone) rect.classList.add(`tf-chart__bar--tone-${s.tone}`);
            rect.setAttribute('data-series-id', s.id);
            svg.appendChild(rect);
            accum += displayVal;
          }
        }
      }
    } else {
      // Horizontal: X axis = value (gridlines vertical), Y axis = category.
      renderGridlinesY(svg, yAxis, yDomain, x0, x1, y0, y1);  // vertical gridlines (re-use, takes Y but renders X-spaced ticks)
      // Dla horizontal X axis renderuje wartości na dole; Y axis category po lewej.
      renderXAxis(svg, yAxis, yDomain, null, x0, x1, y1, ctx.locale);
      renderYAxis(svg, { ...xAxis, scale: 'category' }, { min: 0, max: 1 }, x0, y0, y1, ctx.locale);
      // Renderuj kategorie po lewej (manual tick labels w renderYAxis może
      // nie obsługiwać category; quick fix: append per-category labels manually).
      // Pomiń auto-axis dla Y i zrób custom.
      const lastY = svg.querySelector('.tf-chart__axis--y');
      if (lastY) lastY.remove();
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
        if (stacking === 'none') {
          const barH = innerH / Math.max(1, visibleSeries.length);
          for (let si = 0; si < visibleSeries.length; si++) {
            const s = visibleSeries[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt) continue;
            const pxRight = scaleAlongValueAxis(pt.y, yAxis, yDomain, x0, x1);
            const baselineVal = yAxis.scale === 'log' ? yDomain.min : Math.max(0, yDomain.min);
            const pxLeft = scaleAlongValueAxis(baselineVal, yAxis, yDomain, x0, x1);
            if (pxRight == null || pxLeft == null) continue;
            const rectX = Math.min(pxRight, pxLeft);
            const rectW = Math.abs(pxRight - pxLeft);
            const rect = document.createElementNS(SVG_NS, 'rect');
            rect.setAttribute('x', String(rectX));
            rect.setAttribute('y', String(groupY + si * barH));
            rect.setAttribute('width', String(rectW));
            rect.setAttribute('height', String(barH));
            rect.classList.add('tf-chart__bar');
            if (s.tone) rect.classList.add(`tf-chart__bar--tone-${s.tone}`);
            rect.setAttribute('data-series-id', s.id);
            svg.appendChild(rect);
          }
        } else {
          let accum = 0;
          let total = 0;
          if (stacking === 'percent') {
            for (let si = 0; si < visibleSeries.length; si++) {
              const pt = seriesPoints[si].find((p) => p.x === cat);
              if (pt && pt.y > 0) total += pt.y;
            }
            if (total === 0) continue;
          }
          for (let si = 0; si < visibleSeries.length; si++) {
            const s = visibleSeries[si];
            const pt = seriesPoints[si].find((p) => p.x === cat);
            if (!pt || pt.y <= 0) continue;
            const displayVal = stacking === 'percent' ? (pt.y / total) * 100 : pt.y;
            const pxFrom = scaleAlongValueAxis(accum, yAxis, yDomain, x0, x1);
            const pxTo = scaleAlongValueAxis(accum + displayVal, yAxis, yDomain, x0, x1);
            if (pxFrom == null || pxTo == null) { accum += displayVal; continue; }
            const rect = document.createElementNS(SVG_NS, 'rect');
            rect.setAttribute('x', String(Math.min(pxFrom, pxTo)));
            rect.setAttribute('y', String(groupY));
            rect.setAttribute('width', String(Math.abs(pxTo - pxFrom)));
            rect.setAttribute('height', String(innerH));
            rect.classList.add('tf-chart__bar');
            if (s.tone) rect.classList.add(`tf-chart__bar--tone-${s.tone}`);
            rect.setAttribute('data-series-id', s.id);
            svg.appendChild(rect);
            accum += displayVal;
          }
        }
      }
    }
  };

  rebuild();
  for (const s of series) {
    ctx.registerCleanup(ctx.store.subscribe(s.data_path, rebuild));
  }
  if (xAxis.label) ctx.registerCleanup(subscribeBindRef(xAxis.label, ctx.store, rebuild));
  if (yAxis.label) ctx.registerCleanup(subscribeBindRef(yAxis.label, ctx.store, rebuild));

  if (typeof globalThis.ResizeObserver === 'function') {
    const ro = new globalThis.ResizeObserver(() => rebuild());
    ro.observe(plot);
    ctx.registerCleanup(() => ro.disconnect());
  }

  return wrapper;
}

// Skaluje wartość wzdłuż wartościowej osi do pikseli — respektuje
// linear/log scale (do horizontal layout, gdzie X to wartość). Time/category
// jako oś wartości nie jest spec'owane dla BarChart.
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

// =============================================================================
// StackedBar (0x021A)
// =============================================================================

function parseStackSegment(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: StackSegment must be FieldMap`);
  const seen = new Set();
  const s = { id: null, value: null, label: null, tone: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!STACK_SEGMENT_KEYS.has(k)) throw new TypeError(`${ctx}: unknown StackSegment key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: {
        const id = requireString(v, `${ctx}.id`);
        if (!ID_RE.test(id)) throw new TypeError(`${ctx}.id: invalid grammar`);
        s.id = id;
        break;
      }
      case 1: s.value = v; break;
      case 2: if (v != null) s.label = v; break;
      case 3: s.tone = requireEnum(v, TONES, `${ctx}.tone`); break;
    }
  }
  if (s.id == null) throw new TypeError(`${ctx}: id required`);
  if (s.value == null) throw new TypeError(`${ctx}: value required`);
  if (s.tone == null) throw new TypeError(`${ctx}: tone required`);
  return s;
}

function renderStackedBar(component, ctx) {
  assertOnlyKnownFields(component.fields, STACKED_BAR_FIELD_KEYS, 'StackedBar');

  const segmentsRaw = ctx.readField(component.fields, 0);
  if (!Array.isArray(segmentsRaw) || segmentsRaw.length === 0) {
    throw new TypeError('StackedBar.segments: expected non-empty Array<StackSegment>');
  }
  const segments = segmentsRaw.map((s, i) => parseStackSegment(s, `StackedBar.segments[${i}]`));
  const seenIds = new Set();
  for (const seg of segments) {
    if (seenIds.has(seg.id)) throw new TypeError(`StackedBar.segments: duplicate id '${seg.id}'`);
    seenIds.add(seg.id);
  }
  const totalBind = ctx.readField(component.fields, 1);
  if (totalBind == null) throw new TypeError('StackedBar.total is required (BindRef)');
  const showLegend = requireBool(ctx.readField(component.fields, 2), 'StackedBar.show_legend');
  const showPercentages = requireBool(ctx.readField(component.fields, 3), 'StackedBar.show_percentages');
  const heightPx = requireU16(ctx.readField(component.fields, 4), 'StackedBar.height_px');
  if (heightPx === 0) throw new TypeError('StackedBar.height_px must be > 0');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-stacked-bar');
  wrapper.style.minHeight = `${heightPx}px`;

  const barWrap = document.createElement('div');
  barWrap.classList.add('tf-stacked-bar__bar');
  barWrap.setAttribute('role', 'img');
  barWrap.setAttribute('aria-label', 'Stacked bar');
  barWrap.style.height = `${heightPx}px`;
  wrapper.appendChild(barWrap);

  let legendEl = null;
  if (showLegend) {
    legendEl = document.createElement('ul');
    legendEl.classList.add('tf-stacked-bar__legend');
    legendEl.setAttribute('role', 'list');
    wrapper.appendChild(legendEl);
  }

  const readSegmentValue = (seg) => {
    const v = resolveBindRef(seg.value, ctx.store);
    return typeof v === 'number' && Number.isFinite(v) && v >= 0 ? v : 0;
  };
  const readTotal = () => {
    const v = resolveBindRef(totalBind, ctx.store);
    return typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : 0;
  };

  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const rebuild = () => {
    runRebuildCleanups();
    barWrap.replaceChildren();
    if (legendEl) legendEl.replaceChildren();
    const total = readTotal();
    let accumValue = 0;
    for (const seg of segments) {
      const value = readSegmentValue(seg);
      // Clamp do remaining capacity (overflow ignorowany).
      const usableValue = total > 0 ? Math.min(value, Math.max(0, total - accumValue)) : 0;
      const percent = total > 0 ? (usableValue / total) * 100 : 0;
      accumValue += usableValue;

      const segEl = document.createElement('div');
      segEl.classList.add('tf-stacked-bar__segment');
      segEl.classList.add(`tf-stacked-bar__segment--tone-${seg.tone}`);
      segEl.setAttribute('data-segment-id', seg.id);
      segEl.style.width = `${percent}%`;
      // Tooltip-like title attribute (accessible name).
      const labelText = seg.label != null ? (resolveBindRef(seg.label, ctx.store) || seg.id) : seg.id;
      segEl.setAttribute('title', `${labelText}: ${value}`);
      if (showPercentages && percent >= 5) {
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
        valueEl.textContent = showPercentages ? `${percent.toFixed(1)}%` : String(value);
        li.appendChild(valueEl);
        legendEl.appendChild(li);
      }
    }
    // Remaining unfilled space (gdy sum(values) < total).
    const usedPercent = (accumValue / Math.max(1, total)) * 100;
    if (total > 0 && usedPercent < 100) {
      const rest = document.createElement('div');
      rest.classList.add('tf-stacked-bar__segment-rest');
      rest.style.width = `${100 - usedPercent}%`;
      barWrap.appendChild(rest);
    }
  };

  rebuild();
  ctx.registerCleanup(subscribeBindRef(totalBind, ctx.store, rebuild));
  for (const seg of segments) {
    ctx.registerCleanup(subscribeBindRef(seg.value, ctx.store, rebuild));
    if (seg.label != null) ctx.registerCleanup(subscribeBindRef(seg.label, ctx.store, rebuild));
  }

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataBarChartRenderers() {
  if (!lookupComponentRenderer(BAR_CHART_TAG)) registerComponentRenderer(BAR_CHART_TAG, renderBarChart);
  if (!lookupComponentRenderer(STACKED_BAR_TAG)) registerComponentRenderer(STACKED_BAR_TAG, renderStackedBar);
}
