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

test('Sparkline line variant renderuje <polyline>', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 5, 3, 8, 2] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields()));
  const svg = el.querySelector('svg');
  assertEq(svg.getAttribute('width'), '100');
  assertEq(svg.getAttribute('height'), '30');
  assert(svg.querySelector('polyline.tf-sparkline__line') != null);
});

test('Sparkline area variant renderuje <polygon> + <polyline>', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [0, 1, 2, 3] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'area' })));
  const svg = el.querySelector('svg');
  assert(svg.querySelector('polygon.tf-sparkline__area') != null);
  assert(svg.querySelector('polyline.tf-sparkline__line') != null);
});

test('Sparkline bar variant renderuje N rectangles', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 2, 3, 4, 5] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  assertEq(el.querySelectorAll('rect.tf-sparkline__bar').length, 5);
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

test('Sparkline reactive: data update rerenders SVG', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 2] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  assertEq(el.querySelectorAll('rect').length, 2);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('data'), op: { kind: 'set', value: [1, 2, 3, 4, 5] } }],
  });
  assertEq(el.querySelectorAll('rect').length, 5);
});

test('Sparkline empty data nie crashuje', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields()));
  assertEq(el.querySelectorAll('polyline').length, 0);
});

test('Sparkline nieliczbowe wartości w array są filtrowane', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 'foo', null, 2, NaN, 3] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  assertEq(el.querySelectorAll('rect').length, 3);
});

test('Sparkline wszystkie wartości równe — bez crash przez div/0', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [5, 5, 5, 5] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields()));
  assert(el.querySelector('polyline') != null);
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

test('Sparkline SVG ma role=img + aria-label', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields()));
  const svg = el.querySelector('svg');
  assertEq(svg.getAttribute('role'), 'img');
  assert(svg.hasAttribute('aria-label'));
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
