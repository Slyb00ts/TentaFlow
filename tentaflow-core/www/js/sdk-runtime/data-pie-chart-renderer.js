// =============================================================================
// Plik: sdk-runtime/data-pie-chart-renderer.js
// Opis: Renderer PieChart (0x0219) — mapper na komponent <tf-pie-chart>.
// Waliduje CBOR FieldMap (data_path/variant/show_labels/show_legend/
// max_segments/height_px), czyta i sanityzuje slices ze store i ustawia
// properties komponentu. Rysowanie (slices, "Other", legenda) w
// tf-pie-chart.js.
//
// Data shape: Array<{id?: string, label: string, value: number, tone?: Tone}>.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/charts.rs PieChart (6 pól).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import {
  TONES,
  requireEnum, requireBool, requireU8, requireU16,
  requirePath, assertOnlyKnownFields,
} from './data-chart-shared.js';

export const PIE_CHART_TAG = 0x0219;
const PIE_CHART_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const PIE_VARIANTS = new Set(['pie', 'donut']);

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

  const el = document.createElement('tf-pie-chart');
  el.variant = variant;
  el.showLabels = showLabels;
  el.showLegend = showLegend;
  el.maxSegments = maxSegments;
  el.height = heightPx;

  const sync = () => {
    let arr;
    try { arr = ctx.store.read(dataPath); } catch { arr = undefined; }
    const slices = [];
    if (Array.isArray(arr)) {
      for (let i = 0; i < arr.length; i++) {
        const item = arr[i];
        if (item == null || typeof item !== 'object') continue;
        const v = item.value;
        if (typeof v !== 'number' || !Number.isFinite(v) || v <= 0) continue;
        slices.push({
          id: typeof item.id === 'string' ? item.id : null,
          label: typeof item.label === 'string' ? item.label : String(i),
          value: v,
          tone: typeof item.tone === 'string' && TONES.has(item.tone) ? item.tone : null,
        });
      }
    }
    el.slices = slices;
  };
  sync();
  ctx.registerCleanup(ctx.store.subscribe(dataPath, sync));

  return el;
}

export function registerDataPieChartRenderer() {
  if (!lookupComponentRenderer(PIE_CHART_TAG)) registerComponentRenderer(PIE_CHART_TAG, renderPieChart);
}
