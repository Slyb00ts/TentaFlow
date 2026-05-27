// =============================================================================
// Plik: sdk-runtime/data-pie-chart-renderer.js
// Opis: Renderer PieChart (0x0219) — chunk 3.3d-11. Real SVG pie/donut z
// slices generowanymi przez <path d="M cx cy L x0 y0 A r r 0 largeArc 1
// x1 y1 Z">. Donut variant ma inner radius (annulus path).
//
// Data shape: Array<{id?: string, label: string, value: number, tone?: Tone}>.
// id opcjonalny — używany do stable klucz w legend (fallback do label).
// max_segments: gdy data.length > max, ostatnie agregowane w "Other".
// show_labels: tekst label + % na slice'ach (powyżej threshold ~3%).
// show_legend: lista per slice z label + value + percent.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs PieChart (6 pól).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { subscribeBindRef } from './bind-resolver.js';
import {
  SVG_NS, TONES,
  requireEnum, requireBool, requireU8, requireU16,
  requirePath, assertOnlyKnownFields,
} from './data-chart-shared.js';

export const PIE_CHART_TAG = 0x0219;
const PIE_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const PIE_VARIANTS = new Set(['pie', 'donut']);
// Tone cycle dla slices bez explicite tone (rotuje po liście).
const TONE_CYCLE = ['primary', 'success', 'warning', 'critical', 'info', 'muted', 'neutral'];
const SLICE_LABEL_THRESHOLD = 0.03;  // 3% — mniejsze slice'y bez tekstu na nich

function renderPieChart(component, ctx) {
  assertOnlyKnownFields(component.fields, PIE_CHART_FIELD_KEYS, 'PieChart');

  const dataPath = requirePath(ctx.readField(component.fields, 0), 'PieChart.data_path');
  const variant = requireEnum(ctx.readField(component.fields, 1), PIE_VARIANTS, 'PieChart.variant');
  const showLabels = requireBool(ctx.readField(component.fields, 2), 'PieChart.show_labels');
  const showLegend = requireBool(ctx.readField(component.fields, 3), 'PieChart.show_legend');
  const maxSegments = requireU8(ctx.readField(component.fields, 4), 'PieChart.max_segments');
  if (maxSegments === 0) throw new TypeError('PieChart.max_segments must be > 0');
  const heightPx = requireU16(ctx.readField(component.fields, 5), 'PieChart.height_px');
  if (heightPx === 0) throw new TypeError('PieChart.height_px must be > 0');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-chart');
  wrapper.classList.add('tf-pie-chart');
  wrapper.classList.add(`tf-pie-chart--variant-${variant}`);
  wrapper.style.height = `${heightPx}px`;

  const layout = document.createElement('div');
  layout.classList.add('tf-pie-chart__layout');
  wrapper.appendChild(layout);

  const plot = document.createElement('div');
  plot.classList.add('tf-pie-chart__plot');
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('width', '100%');
  svg.setAttribute('height', '100%');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', variant === 'donut' ? 'Donut chart' : 'Pie chart');
  svg.classList.add('tf-pie-chart__svg');
  plot.appendChild(svg);
  layout.appendChild(plot);

  let legendEl = null;
  if (showLegend) {
    legendEl = document.createElement('ul');
    legendEl.classList.add('tf-pie-chart__legend');
    legendEl.setAttribute('role', 'list');
    layout.appendChild(legendEl);
  }

  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const getPixelDimensions = () => {
    const rect = plot.getBoundingClientRect ? plot.getBoundingClientRect() : null;
    const w = (rect && rect.width > 0) ? rect.width : heightPx;
    const h = (rect && rect.height > 0) ? rect.height : heightPx;
    return { w, h };
  };

  const readDataSlices = () => {
    let arr;
    try { arr = ctx.store.read(dataPath); } catch { arr = undefined; }
    if (!Array.isArray(arr)) return [];
    const out = [];
    for (let i = 0; i < arr.length; i++) {
      const item = arr[i];
      if (item == null || typeof item !== 'object') continue;
      const v = item.value;
      if (typeof v !== 'number' || !Number.isFinite(v) || v <= 0) continue;
      const label = typeof item.label === 'string' ? item.label : String(i);
      const id = typeof item.id === 'string' ? item.id : null;
      const tone = typeof item.tone === 'string' && TONES.has(item.tone) ? item.tone : null;
      out.push({ id, label, value: v, tone });
    }
    return out;
  };

  const rebuild = () => {
    runRebuildCleanups();
    svg.replaceChildren();
    if (legendEl) legendEl.replaceChildren();
    const { w, h } = getPixelDimensions();
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    const cx = w / 2;
    const cy = h / 2;
    const radius = Math.min(w, h) / 2 * 0.85;
    const innerRadius = variant === 'donut' ? radius * 0.55 : 0;

    let slices = readDataSlices();
    if (slices.length === 0) return;
    // max_segments aggregation: zachowaj pierwsze (max-1), reszta → "Other".
    if (slices.length > maxSegments) {
      const kept = slices.slice(0, maxSegments - 1);
      const rest = slices.slice(maxSegments - 1);
      const restValue = rest.reduce((acc, s) => acc + s.value, 0);
      kept.push({ id: '__other__', label: 'Other', value: restValue, tone: 'muted' });
      slices = kept;
    }
    const total = slices.reduce((acc, s) => acc + s.value, 0);
    if (total <= 0) return;

    let startAngle = -Math.PI / 2;  // 12 o'clock
    for (let i = 0; i < slices.length; i++) {
      const slice = slices[i];
      const fraction = slice.value / total;
      const sweepAngle = fraction * Math.PI * 2;
      const endAngle = startAngle + sweepAngle;
      const tone = slice.tone || TONE_CYCLE[i % TONE_CYCLE.length];

      // Build SVG path. Pie: M cx cy → L outer_start → A outer_arc → Z.
      // Donut: M outer_start → A outer_arc → L inner_end → A inner_arc → Z.
      const ox0 = cx + Math.cos(startAngle) * radius;
      const oy0 = cy + Math.sin(startAngle) * radius;
      const ox1 = cx + Math.cos(endAngle) * radius;
      const oy1 = cy + Math.sin(endAngle) * radius;
      const largeArc = sweepAngle > Math.PI ? 1 : 0;
      let d;
      if (innerRadius > 0) {
        const ix0 = cx + Math.cos(endAngle) * innerRadius;
        const iy0 = cy + Math.sin(endAngle) * innerRadius;
        const ix1 = cx + Math.cos(startAngle) * innerRadius;
        const iy1 = cy + Math.sin(startAngle) * innerRadius;
        d = [
          `M ${ox0} ${oy0}`,
          `A ${radius} ${radius} 0 ${largeArc} 1 ${ox1} ${oy1}`,
          `L ${ix0} ${iy0}`,
          `A ${innerRadius} ${innerRadius} 0 ${largeArc} 0 ${ix1} ${iy1}`,
          'Z',
        ].join(' ');
      } else {
        d = [
          `M ${cx} ${cy}`,
          `L ${ox0} ${oy0}`,
          `A ${radius} ${radius} 0 ${largeArc} 1 ${ox1} ${oy1}`,
          'Z',
        ].join(' ');
      }
      // Specjalny przypadek: pojedynczy slice = 100% → arc nie zamyka się
      // sam (M=start=end). Renderuj jako circle dla pie, annulus dla donut.
      if (slices.length === 1 || fraction >= 0.9999) {
        const circle = document.createElementNS(SVG_NS, 'circle');
        circle.setAttribute('cx', String(cx));
        circle.setAttribute('cy', String(cy));
        circle.setAttribute('r', String(radius));
        circle.classList.add('tf-pie-chart__slice');
        circle.classList.add(`tf-pie-chart__slice--tone-${tone}`);
        circle.setAttribute('data-slice-id', slice.id || slice.label);
        circle.setAttribute('data-value', String(slice.value));
        const a11yLabel = `${slice.label}: ${slice.value} (${(fraction * 100).toFixed(1)}%)`;
        circle.setAttribute('aria-label', a11yLabel);
        if (innerRadius > 0) {
          // Donut single slice: hole w środku przez mask lub osobny circle.
          // Najprostsze: render path z dwoma kręgami (outer pie + inner cut).
          svg.appendChild(circle);
          const hole = document.createElementNS(SVG_NS, 'circle');
          hole.setAttribute('cx', String(cx));
          hole.setAttribute('cy', String(cy));
          hole.setAttribute('r', String(innerRadius));
          hole.classList.add('tf-pie-chart__hole');
          svg.appendChild(hole);
        } else {
          svg.appendChild(circle);
        }
      } else {
        const path = document.createElementNS(SVG_NS, 'path');
        path.setAttribute('d', d);
        path.classList.add('tf-pie-chart__slice');
        path.classList.add(`tf-pie-chart__slice--tone-${tone}`);
        path.setAttribute('data-slice-id', slice.id || slice.label);
        path.setAttribute('data-value', String(slice.value));
        const a11yLabel = `${slice.label}: ${slice.value} (${(fraction * 100).toFixed(1)}%)`;
        path.setAttribute('aria-label', a11yLabel);
        svg.appendChild(path);
      }

      // Label na slice'ach >= threshold (ale tylko gdy show_labels=true).
      if (showLabels && fraction >= SLICE_LABEL_THRESHOLD) {
        const midAngle = startAngle + sweepAngle / 2;
        const labelRadius = innerRadius > 0 ? (radius + innerRadius) / 2 : radius * 0.65;
        const lx = cx + Math.cos(midAngle) * labelRadius;
        const ly = cy + Math.sin(midAngle) * labelRadius;
        const text = document.createElementNS(SVG_NS, 'text');
        text.setAttribute('x', String(lx));
        text.setAttribute('y', String(ly));
        text.setAttribute('text-anchor', 'middle');
        text.setAttribute('dominant-baseline', 'middle');
        text.classList.add('tf-pie-chart__slice-label');
        text.textContent = `${(fraction * 100).toFixed(fraction >= 0.1 ? 0 : 1)}%`;
        svg.appendChild(text);
      }

      // Legend item.
      if (legendEl) {
        const li = document.createElement('li');
        li.classList.add('tf-pie-chart__legend-item');
        const sw = document.createElement('span');
        sw.classList.add('tf-pie-chart__legend-swatch');
        sw.classList.add(`tf-pie-chart__legend-swatch--tone-${tone}`);
        li.appendChild(sw);
        const labelEl2 = document.createElement('span');
        labelEl2.classList.add('tf-pie-chart__legend-label');
        labelEl2.textContent = slice.label;
        li.appendChild(labelEl2);
        const valEl = document.createElement('span');
        valEl.classList.add('tf-pie-chart__legend-value');
        valEl.textContent = `${slice.value} (${(fraction * 100).toFixed(1)}%)`;
        li.appendChild(valEl);
        legendEl.appendChild(li);
      }

      startAngle = endAngle;
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(dataPath, rebuild));

  if (typeof globalThis.ResizeObserver === 'function') {
    const ro = new globalThis.ResizeObserver(() => rebuild());
    ro.observe(plot);
    ctx.registerCleanup(() => ro.disconnect());
  }

  return wrapper;
}

export function registerDataPieChartRenderer() {
  if (!lookupComponentRenderer(PIE_CHART_TAG)) registerComponentRenderer(PIE_CHART_TAG, renderPieChart);
}
