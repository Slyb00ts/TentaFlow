// =============================================================================
// Plik: sdk-runtime/data-pie-chart-renderer.test.js
// Opis: Testy PieChart (0x0219) — chunk 3.3d-11.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { PIE_CHART_TAG } from './data-pie-chart-renderer.js';
import '../components/tf-pie-chart.js';

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

function pieFields({
  dataPath = PATH('data'), variant = 'pie',
  showLabels = false, showLegend = false,
  maxSegments = 8, heightPx = 200,
} = {}) {
  return [
    [0, dataPath], [1, variant], [2, showLabels],
    [3, showLegend], [4, maxSegments], [5, heightPx],
  ];
}

// ============================================================================

test('PieChart renderuje N <path> per slice', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'A', value: 30 },
      { label: 'B', value: 50 },
      { label: 'C', value: 20 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('path.tf-pie-chart__slice').length, 3);
});

test('PieChart donut variant ma inner radius (donut path)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'A', value: 50 },
      { label: 'B', value: 50 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields({ variant: 'donut' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-pie-chart--variant-donut'));
  // Donut path zawiera dwa "A" commands (outer + inner arc).
  const path = el.querySelector('path.tf-pie-chart__slice');
  const d = path.getAttribute('d');
  const arcCount = (d.match(/A /g) || []).length;
  assertEq(arcCount, 2);
});

test('PieChart pie variant single slice = circle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [{ label: 'Only', value: 100 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  // Single slice rendered as <circle>.
  assert(el.querySelector('circle.tf-pie-chart__slice') != null);
});

test('PieChart donut single slice = circle + hole', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [{ label: 'Only', value: 100 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields({ variant: 'donut' })));
  document.body.appendChild(el);
  assert(el.querySelector('circle.tf-pie-chart__slice') != null);
  assert(el.querySelector('circle.tf-pie-chart__hole') != null);
});

test('PieChart slice color cycle gdy bez explicit tone', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'A', value: 33 },
      { label: 'B', value: 33 },
      { label: 'C', value: 34 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  const slices = el.querySelectorAll('.tf-pie-chart__slice');
  // Pierwsza slice primary, kolejne success, warning.
  assert(slices[0].classList.contains('tf-pie-chart__slice--tone-primary'));
  assert(slices[1].classList.contains('tf-pie-chart__slice--tone-success'));
});

test('PieChart slice explicit tone respektowany', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'A', value: 50, tone: 'critical' },
      { label: 'B', value: 50, tone: 'success' },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-pie-chart__slice--tone-critical') != null);
  assert(el.querySelector('.tf-pie-chart__slice--tone-success') != null);
});

test('PieChart max_segments aggregates overflow do "Other"', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'A', value: 30 },
      { label: 'B', value: 25 },
      { label: 'C', value: 20 },
      { label: 'D', value: 15 },
      { label: 'E', value: 10 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields({ maxSegments: 3, showLegend: true })));
  document.body.appendChild(el);
  // 3 slices: A, B, Other (C+D+E = 45).
  assertEq(el.querySelectorAll('.tf-pie-chart__slice').length, 3);
  const lastLegend = el.querySelectorAll('.tf-pie-chart__legend-label');
  assertEq(lastLegend[lastLegend.length - 1].textContent, 'Other');
});

test('PieChart show_labels=true renderuje % na slice', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'A', value: 60 },
      { label: 'B', value: 40 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields({ showLabels: true })));
  document.body.appendChild(el);
  const labels = el.querySelectorAll('.tf-pie-chart__slice-label');
  assertEq(labels.length, 2);
  assertEq(labels[0].textContent, '60%');
});

test('PieChart show_legend renderuje listę z label+value+percent', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'Sales', value: 75 },
      { label: 'Returns', value: 25 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields({ showLegend: true })));
  const items = el.querySelectorAll('.tf-pie-chart__legend-item');
  assertEq(items.length, 2);
  assertEq(items[0].querySelector('.tf-pie-chart__legend-label').textContent, 'Sales');
  assertEq(items[0].querySelector('.tf-pie-chart__legend-value').textContent, '75 (75.0%)');
});

test('PieChart NaN/negative values filtered', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'A', value: 50 },
      { label: 'B', value: NaN },
      { label: 'C', value: -10 },
      { label: 'D', value: 50 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  // Tylko A i D są valid.
  assertEq(el.querySelectorAll('.tf-pie-chart__slice').length, 2);
});

test('PieChart reactive: data update rebuilds slices', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [{ label: 'A', value: 50 }, { label: 'B', value: 50 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('.tf-pie-chart__slice').length, 2);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('data'), op: { kind: 'set', value: [
      { label: 'X', value: 25 },
      { label: 'Y', value: 25 },
      { label: 'Z', value: 50 },
    ] } }],
  });
  assertEq(el.querySelectorAll('.tf-pie-chart__slice').length, 3);
});

test('PieChart empty data nie crashuje', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('.tf-pie-chart__slice').length, 0);
});

test('PieChart slice ma aria-label z label+value+%', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [
      { label: 'Quarter 1', value: 25 },
      { label: 'Quarter 2', value: 75 },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields()));
  document.body.appendChild(el);
  const slice = el.querySelector('.tf-pie-chart__slice');
  assertEq(slice.getAttribute('aria-label'), 'Quarter 1: 25 (25.0%)');
});

test('PieChart invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(PIE_CHART_TAG, pieFields({ variant: 'square' }))));
});

test('PieChart max_segments=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(PIE_CHART_TAG, pieFields({ maxSegments: 0 }))));
});

test('PieChart height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(PIE_CHART_TAG, pieFields({ heightPx: 0 }))));
});

test('PieChart unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(PIE_CHART_TAG, [
    ...pieFields(), [99, 'x'],
  ])));
});

test('PieChart SVG ma role=img + aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [{ label: 'A', value: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PIE_CHART_TAG, pieFields({ variant: 'donut' })));
  const svg = el.querySelector('svg');
  assertEq(svg.getAttribute('role'), 'img');
  assertEq(svg.getAttribute('aria-label'), 'Donut chart');
});

// ---- report ----
function reportResults() {
  let pass = 0, fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) { pass++; lines.push(`✓ ${r.name}`); }
    else { fail++; lines.push(`✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`); }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  return { pass, fail, text: lines.join('\n') };
}
if (typeof process !== 'undefined') {
  const r = reportResults();
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}
export { reportResults };
