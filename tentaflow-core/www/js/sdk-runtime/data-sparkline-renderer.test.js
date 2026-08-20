// =============================================================================
// Plik: sdk-runtime/data-sparkline-renderer.test.js
// Opis: Testy Sparkline (0x0215) — chunk 3.3d-7.
// =============================================================================

import './_dom-test-harness.js';
import { TfSparkline } from '../components/tf-sparkline.js';
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

// tf-sparkline is imported so <tf-sparkline> upgrades and its property
// accessors (.points/.color/.fill/.height/.variant) round-trip. Rendered
// wrappers are NOT mounted, so the component stays disconnected and never hits
// the (null) happy-dom canvas 2D context. The bar-vs-line DRAW difference is
// proven separately below with a recording 2D-context stub.

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

test('Sparkline bar variant keeps fill=false, sets variant class + property', () => {
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
  // The component variant property distinguishes bar from line — previously the
  // renderer only flipped a CSS class and both drew an identical line.
  assertEq(spark.variant, 'bar');
});

test('Sparkline line/area variant properties differ from bar', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 2, 3] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const line = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'line' })));
  const area = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'area' })));
  const bar = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  assertEq(line.querySelector('tf-sparkline').variant, 'line');
  assertEq(area.querySelector('tf-sparkline').variant, 'area');
  assertEq(bar.querySelector('tf-sparkline').variant, 'bar');
});

test('Sparkline sets role=img and a descriptive aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [1, 5, 3] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  const spark = el.querySelector('tf-sparkline');
  assertEq(spark.getAttribute('role'), 'img');
  const label = spark.getAttribute('aria-label');
  assert(label && label.includes('bar'), 'aria-label mentions variant');
  assert(label.includes('min 1') && label.includes('max 5'), 'aria-label has min/max');
});

test('Sparkline a11y.label overrides synthesized aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('data'), value: [1, 2, 3] },
      { path: PATH('lbl'), value: 'CPU usage trend' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields(), {
    a11y: { label: { kind: 'bound', path: PATH('lbl') } },
  }));
  assertEq(el.querySelector('tf-sparkline').getAttribute('aria-label'), 'CPU usage trend');
});

test('Sparkline empty data still has aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('data'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPARKLINE_TAG, sparklineFields({ variant: 'bar' })));
  const label = el.querySelector('tf-sparkline').getAttribute('aria-label');
  assert(label && label.includes('no data'), 'empty aria-label present');
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

// ============================================================================
// Component draw — bar variant draws bars, line variant strokes a path
// ============================================================================

// Recording 2D context stub: counts the calls that distinguish a bar chart
// (fillRect per point) from a line chart (single beginPath/stroke).
function recordingCtx() {
  const calls = { fillRect: 0, stroke: 0, beginPath: 0, lineTo: 0, moveTo: 0 };
  const rects = [];
  return {
    calls, rects,
    clearRect() {},
    setTransform() {},
    quadraticCurveTo() { calls.lineTo++; },
    beginPath() { calls.beginPath++; },
    moveTo() { calls.moveTo++; },
    lineTo() { calls.lineTo++; },
    stroke() { calls.stroke++; },
    closePath() {},
    arc() {},
    fill() {},
    fillRect(x, y, w, h) { calls.fillRect++; rects.push({ x, y, w, h }); },
    set strokeStyle(_v) {}, set lineWidth(_v) {},
    set fillStyle(_v) {}, set globalAlpha(_v) {},
  };
}

function drawWith(variant, points) {
  const el = new TfSparkline();
  const ctx = recordingCtx();
  // Inject a canvas whose 2D context is our recorder (happy-dom returns null).
  el._canvas = {
    width: 0, height: 0, style: {},
    getContext() { return ctx; },
  };
  Object.defineProperty(el, 'clientWidth', { value: 120, configurable: true });
  el._variant = variant;
  el._points = points;
  el._render();
  return ctx;
}

test('Sparkline component bar variant draws one fillRect per point', () => {
  const ctx = drawWith('bar', [1, 2, 3, 4]);
  assertEq(ctx.calls.fillRect, 4);
  assertEq(ctx.calls.stroke, 0);
});

test('Sparkline component line variant strokes a path, no fillRect', () => {
  const ctx = drawWith('line', [1, 2, 3, 4]);
  assertEq(ctx.calls.fillRect, 0);
  assert(ctx.calls.stroke >= 1, 'line variant must stroke');
  assert(ctx.calls.lineTo >= 1, 'line variant must build a path');
});

test('Sparkline component bar and line draws differ measurably', () => {
  const bar = drawWith('bar', [5, 1, 9, 3]);
  const line = drawWith('line', [5, 1, 9, 3]);
  assert(bar.calls.fillRect > 0 && line.calls.fillRect === 0,
    'bar uses fillRect, line does not');
  assert(line.calls.stroke > 0 && bar.calls.stroke === 0,
    'line strokes, bar does not');
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
