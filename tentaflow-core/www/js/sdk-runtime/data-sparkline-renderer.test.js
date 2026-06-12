// =============================================================================
// Plik: sdk-runtime/data-sparkline-renderer.test.js
// Opis: Testy Sparkline (0x0215) — chunk 3.3d-7.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { SPARKLINE_TAG } from './data-sparkline-renderer.js';

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

function sparklineFields({
  data = PATH('data'), variant = 'line', tone = 'primary',
  w = 100, h = 30, showMinMax = false,
} = {}) {
  return [
    [0, data], [1, variant], [2, tone],
    [3, w], [4, h], [5, showMinMax],
  ];
}

// tf-sparkline draws on <canvas> (no 2D context in happy-dom), so the
// component module is intentionally NOT imported: <tf-sparkline> stays an
// un-upgraded element and the renderer's property writes (.points, .color,
// .fill, .height) are asserted directly as the renderer→component contract.

test('Sparkline line variant binds points + sizing to tf-sparkline', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 5, 3, 8, 2] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields()));
  assert(el.classList.contains('tf-sparkline--variant-line'));
  const spark = el.querySelector('tf-sparkline');
  assert(spark != null, 'tf-sparkline must exist');
  assertEq(spark.points, [1, 5, 3, 8, 2]);
  assertEq(spark.fill, false);
  assertEq(spark.height, 30);
  assertEq(spark.style.width, '100px');
  assertEq(spark.color, 'primary');
});

test('Sparkline area variant sets fill=true', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [0, 1, 2, 3] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'area' })));
  assert(el.classList.contains('tf-sparkline--variant-area'));
  assertEq(el.querySelector('tf-sparkline').fill, true);
});

test('Sparkline bar variant keeps fill=false and sets variant class', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 2, 3, 4, 5] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  assert(el.classList.contains('tf-sparkline--variant-bar'));
  const spark = el.querySelector('tf-sparkline');
  assertEq(spark.fill, false);
  assertEq(spark.points.length, 5);
});

test('Sparkline show_min_max renderuje min/max badges', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [3, 1, 9, 5] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ showMinMax: true })));
  assertEq(el.querySelector('.tf-sparkline__min').textContent, '1');
  assertEq(el.querySelector('.tf-sparkline__max').textContent, '9');
});

test('Sparkline z fractional values format z 2 miejscami', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [0.123, 0.456] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ showMinMax: true })));
  assertEq(el.querySelector('.tf-sparkline__min').textContent, '0.12');
  assertEq(el.querySelector('.tf-sparkline__max').textContent, '0.46');
});

test('Sparkline reactive: store patch updates bound points', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 2] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  const spark = el.querySelector('tf-sparkline');
  assertEq(spark.points, [1, 2]);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('data'), op: { kind: 'set', value: [1, 2, 3, 4, 5] } }],
  });
  assertEq(spark.points, [1, 2, 3, 4, 5]);
});

test('Sparkline empty data binds empty points without crash', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields()));
  assertEq(el.querySelector('tf-sparkline').points, []);
});

test('Sparkline non-numeric values are filtered out of points', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 'foo', null, 2, NaN, 3] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  assertEq(el.querySelector('tf-sparkline').points, [1, 2, 3]);
});

test('Sparkline all-equal values bind unchanged (min/max badges equal)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [5, 5, 5, 5] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ showMinMax: true })));
  assertEq(el.querySelector('tf-sparkline').points, [5, 5, 5, 5]);
  assertEq(el.querySelector('.tf-sparkline__min').textContent, '5');
  assertEq(el.querySelector('.tf-sparkline__max').textContent, '5');
});

test('Sparkline tone klasy', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ tone: 'critical' })));
  assert(el.classList.contains('tf-sparkline--tone-critical'));
});

test('Sparkline invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'pie' }))));
});

test('Sparkline width_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SPARKLINE_TAG, sparklineFields({ w: 0 }))));
});

test('Sparkline height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SPARKLINE_TAG, sparklineFields({ h: 0 }))));
});

test('Sparkline width_px/height_px accept BigInt u16, tone maps to color role', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ w: 120n, h: 40n, tone: 'critical' })));
  const spark = el.querySelector('tf-sparkline');
  assertEq(spark.style.width, '120px');
  assertEq(spark.height, 40);
  assertEq(spark.color, 'danger');
});

test('Sparkline unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SPARKLINE_TAG, [
    ...sparklineFields(), [99, 'x'],
  ])));
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
