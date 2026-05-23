// =============================================================================
// Plik: sdk-runtime/data-heatmap-gauge-renderer.test.js
// Opis: Testy Heatmap (0x021B) + Gauge (0x021C) — chunk 3.3d-12.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { HEATMAP_TAG, GAUGE_TAG } from './data-heatmap-gauge-renderer.js';

const results = [];
function test(name, fn) {
  try { fn(); results.push({ name, ok: true }); }
  catch (err) { results.push({ name, ok: false, err }); }
}
function assertEq(a, e, m) {
  const aj = JSON.stringify(a, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  const ej = JSON.stringify(e, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  if (aj !== ej) throw new Error(`${m || 'assertEq'}: expected ${ej}, got ${aj}`);
}
function assert(cond, m) { if (!cond) throw new Error(m || 'assert failed'); }
function assertThrows(fn, m) {
  let t = false; try { fn(); } catch { t = true; }
  if (!t) throw new Error(m || 'expected throw');
}

const PATH = (...segs) => segs.map((s) =>
  typeof s === 'number' ? { kind: 'index', value: s } : { kind: 'key', value: s });

function makeStore() { return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n }); }
function makeEngine(store) {
  return new ComponentRenderer({ store: store || makeStore(), eventDispatcher: { emit() {} }, locale: 'en-US' });
}
function comp(tag, fields, extra = {}) {
  return {
    tag, id: extra.id ?? 'c1', fields,
    handlers: extra.handlers ?? null,
    bind: extra.bind ?? null,
    a11y: extra.a11y ?? null,
    visibility: extra.visibility ?? null,
    test_id: extra.test_id ?? null,
  };
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

// ============================================================================
// Heatmap
// ============================================================================

const LIT = (value) => ({ kind: 'literal', value });
function heatmapRow(id, label) { return [[0, id], [1, LIT(label)]]; }
function heatmapFields({
  rows = [heatmapRow('r1', 'Row 1'), heatmapRow('r2', 'Row 2')],
  columns = [heatmapRow('c1', 'Col 1'), heatmapRow('c2', 'Col 2')],
  cellsPath = PATH('cells'),
  scale = { kind: 'linear', min: 0, max: 100, color_from: 'muted', color_to: 'primary' },
  legendPosition = 'bottom',
  cellSizePx = 32,
  tooltip = true,
} = {}) {
  return [
    [0, rows], [1, columns], [2, cellsPath], [3, scale],
    [4, legendPosition], [5, cellSizePx], [6, tooltip],
  ];
}

test('Heatmap renderuje rect per cell', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('cells'), value: [
      { row_id: 'r1', col_id: 'c1', value: 25 },
      { row_id: 'r1', col_id: 'c2', value: 75 },
      { row_id: 'r2', col_id: 'c1', value: 50 },
      { row_id: 'r2', col_id: 'c2', value: 100 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(HEATMAP_TAG, heatmapFields()));
  document.body.appendChild(el);
  const cells = el.querySelectorAll('rect.tf-heatmap__cell');
  assertEq(cells.length, 4);
  // Cell ma data-attrs.
  assertEq(cells[0].getAttribute('data-row-id'), 'r1');
  assertEq(cells[0].getAttribute('data-col-id'), 'c1');
});

test('Heatmap pomija unknown row_id/col_id', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('cells'), value: [
      { row_id: 'r1', col_id: 'c1', value: 10 },
      { row_id: 'unknown', col_id: 'c1', value: 20 },
      { row_id: 'r1', col_id: 'unknown', value: 30 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(HEATMAP_TAG, heatmapFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('rect.tf-heatmap__cell').length, 1);
});

test('Heatmap legend categorical renderuje per-bucket swatch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('cells'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const fields = heatmapFields({ scale: {
    kind: 'categorical',
    buckets: [
      [[0, 10], [1, 'success']],
      [[0, 50], [1, 'warning']],
      [[0, 100], [1, 'critical']],
    ],
  }});
  const el = engine.render(comp(HEATMAP_TAG, fields));
  document.body.appendChild(el);
  const buckets = el.querySelectorAll('.tf-heatmap__legend-bucket');
  assertEq(buckets.length, 3);
});

test('Heatmap legend=none nie renderuje legendy', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(HEATMAP_TAG, heatmapFields({ legendPosition: 'none' })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-heatmap__legend') == null);
});

test('Heatmap reaguje na zmianę cells_path', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [{ row_id: 'r1', col_id: 'c1', value: 10 }] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(HEATMAP_TAG, heatmapFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('rect.tf-heatmap__cell').length, 1);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('cells'), op: { kind: 'set', value: [
      { row_id: 'r1', col_id: 'c1', value: 5 },
      { row_id: 'r2', col_id: 'c2', value: 80 },
    ] } }],
  });
  assertEq(el.querySelectorAll('rect.tf-heatmap__cell').length, 2);
});

test('Heatmap odrzuca scale.linear z min>=max', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(HEATMAP_TAG, heatmapFields({
    scale: { kind: 'linear', min: 100, max: 100, color_from: 'muted', color_to: 'primary' },
  }))));
});

test('Heatmap odrzuca scale.logarithmic z min<=0', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(HEATMAP_TAG, heatmapFields({
    scale: { kind: 'logarithmic', min: 0, max: 100, base: 10 },
  }))));
});

test('Heatmap odrzuca cell_size_px=0', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(HEATMAP_TAG, heatmapFields({ cellSizePx: 0 }))));
});

test('Heatmap odrzuca duplicate row id', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(HEATMAP_TAG, heatmapFields({
    rows: [heatmapRow('r1', 'A'), heatmapRow('r1', 'B')],
  }))));
});

test('Heatmap odrzuca HeatmapBucket.label który nie jest BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  // legend_position=none, ale walidacja shape musi i tak zadziałać.
  assertThrows(() => engine.render(comp(HEATMAP_TAG, heatmapFields({
    legendPosition: 'none',
    scale: { kind: 'categorical', buckets: [
      [[0, 10], [1, 'success'], [2, 'just a string']],
    ]},
  }))));
});

test('Heatmap odrzuca HeatmapRow.label który nie jest BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cells'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(HEATMAP_TAG, heatmapFields({
    rows: [[[0, 'r1'], [1, 'plain string']]],
  }))));
});

// ============================================================================
// Gauge
// ============================================================================

function gaugeThresh(value, tone) { return [[0, value], [1, tone]]; }
function gaugeFields({
  valueBind = { kind: 'bound', path: PATH('val') },
  min = 0, max = 100,
  thresholds = [gaugeThresh(50, 'warning'), gaugeThresh(80, 'critical')],
  variant = 'circular',
  label = null, format = null, sizePx = 160,
} = {}) {
  return [
    [0, valueBind], [1, min], [2, max], [3, thresholds],
    [4, variant], [5, label], [6, format], [7, sizePx],
  ];
}

test('Gauge renderuje track + value arc + thresholds', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 35 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields()));
  document.body.appendChild(el);
  assert(el.querySelector('path.tf-gauge__track') != null);
  assert(el.querySelector('path.tf-gauge__value-arc') != null);
  assertEq(el.querySelectorAll('line.tf-gauge__threshold').length, 2);
});

test('Gauge variant=semi ma klasę tf-gauge--variant-semi', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 50 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields({ variant: 'semi' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-gauge--variant-semi'));
});

test('Gauge reaguje na zmianę value przez BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 10 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('text.tf-gauge__value-text').textContent, '10');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('val'), op: { kind: 'set', value: 90 } }],
  });
  assertEq(el.querySelector('text.tf-gauge__value-text').textContent, '90');
});

test('Gauge clampuje value do [min, max]', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 150 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields()));
  document.body.appendChild(el);
  // Tone z najwyższego threshold (80 critical) — value clamped do 100.
  const arc = el.querySelector('path.tf-gauge__value-arc');
  assert(arc.getAttribute('class').includes('tone-critical'));
});

test('Gauge odrzuca min>=max', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 0 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(GAUGE_TAG, gaugeFields({ min: 100, max: 0 }))));
});

test('Gauge odrzuca size_px=0', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 0 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(GAUGE_TAG, gaugeFields({ sizePx: 0 }))));
});

test('Gauge runtime patch do Infinity → visible error state (— + aria-invalid)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 25 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('text.tf-gauge__value-text').textContent, '25');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('val'), op: { kind: 'set', value: Infinity } }],
  });
  assertEq(el.querySelector('text.tf-gauge__value-text').textContent, '—');
  const svg = el.querySelector('svg.tf-gauge__svg');
  assertEq(svg.getAttribute('aria-invalid'), 'true');
});

test('Gauge value=null renderuje empty arc + dash', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('text.tf-gauge__value-text').textContent, '—');
});

test('Gauge odrzuca GaugeThreshold.label który nie jest BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 10 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(GAUGE_TAG, gaugeFields({
    thresholds: [[[0, 50], [1, 'warning'], [2, 'plain']]],
  }))));
});

test('Gauge value=0 renderuje pusty arc bez błędu', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 0 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields()));
  document.body.appendChild(el);
  const arc = el.querySelector('path.tf-gauge__value-arc');
  assert(arc.getAttribute('d').startsWith('M '));
});

test('Gauge variant=circular full value to pełny okrąg (dwa łuki)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('val'), value: 100 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GAUGE_TAG, gaugeFields()));
  document.body.appendChild(el);
  const arc = el.querySelector('path.tf-gauge__value-arc');
  const arcCount = (arc.getAttribute('d').match(/A /g) || []).length;
  assertEq(arcCount, 2);
});

// ============================================================================
// Report
// ============================================================================

const failed = results.filter((r) => !r.ok);
console.log(`heatmap+gauge tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
