// =============================================================================
// File: sdk-runtime/data-progress-rating-renderer.test.js
// Description: Tests for ProgressBar (0x021D) + RatingDisplay (0x021E)
// renderers backed by the <tf-progress-bar> and <tf-rating> web components.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-progress-bar.js';
import '../components/tf-rating.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { PROGRESS_BAR_TAG, RATING_DISPLAY_TAG } from './data-progress-rating-renderer.js';

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
const BOUND = (...segs) => ({ kind: 'bound', path: PATH(...segs) });

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
// ProgressBar
// ============================================================================

function progressFields({
  value = BOUND('v'), max = 100,
  variant = 'default', tone = 'primary',
  showLabel = true, label = null, size = 'md', orientation = null,
} = {}) {
  const f = [[0, value], [1, max], [2, variant], [3, tone], [4, showLabel]];
  if (label != null) f.push([5, label]);
  f.push([6, size]);
  if (orientation != null) f.push([7, orientation]);
  return f;
}

test('ProgressBar renderuje fill na podstawie value/max', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 40 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields()));
  document.body.appendChild(el);
  const fill = el.querySelector('.tf-progress-bar-fill');
  assertEq(fill.style.width, '40%');
  assertEq(el.getAttribute('aria-valuenow'), '40');
});

test('ProgressBar orientation=vertical wypełnia height, nie width', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 40 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields({ orientation: 'vertical' })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('orientation'), 'vertical');
  const fill = el.querySelector('.tf-progress-bar-fill');
  assertEq(fill.style.height, '40%');
  assertEq(fill.style.width, '');
  assert(el.querySelector('.tf-progress-bar').classList.contains('vertical'));
});

test('ProgressBar orientation domyślnie horizontal (brak klucza 7)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 40 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields()));
  document.body.appendChild(el);
  assert(el.getAttribute('orientation') == null, 'no orientation attr');
  const fill = el.querySelector('.tf-progress-bar-fill');
  assertEq(fill.style.width, '40%');
  assert(!el.querySelector('.tf-progress-bar').classList.contains('vertical'));
});

test('ProgressBar odrzuca nieznaną orientację', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(PROGRESS_BAR_TAG, progressFields({ orientation: 'diagonal' }))));
});

test('ProgressBar clampuje value do [0, max]', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 200 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-progress-bar-fill').style.width, '100%');
});

test('ProgressBar variant=indeterminate ignoruje value, brak aria-valuenow', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 50 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields({ variant: 'indeterminate' })));
  document.body.appendChild(el);
  assert(el.getAttribute('aria-valuenow') == null);
  assert(el.classList.contains('tf-progress-bar--variant-indeterminate'));
});

test('ProgressBar reaguje na patch value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 10 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-progress-bar-fill').style.width, '10%');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('v'), op: { kind: 'set', value: 75 } }] });
  assertEq(el.querySelector('.tf-progress-bar-fill').style.width, '75%');
  assertEq(el.querySelector('.tf-progress-bar-label').textContent, '75%');
});

test('ProgressBar label BindRef override standard %', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('v'), value: 50 }, { path: PATH('lbl'), value: '3/6 plików' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields({ label: BOUND('lbl') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-progress-bar-label').textContent, '3/6 plików');
});

test('ProgressBar value=NaN → aria-invalid + label —', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 10 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PROGRESS_BAR_TAG, progressFields()));
  document.body.appendChild(el);
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('v'), op: { kind: 'set', value: NaN } }] });
  assertEq(el.getAttribute('aria-invalid'), 'true');
  assertEq(el.querySelector('.tf-progress-bar-label').textContent, '—');
});

test('ProgressBar odrzuca max<=0', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 1 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(PROGRESS_BAR_TAG, progressFields({ max: 0 }))));
});

test('ProgressBar default max=1.0 gdy nie podano', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 0.5 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const fields = [
    [0, BOUND('v')],
    [2, 'default'], [3, 'primary'], [4, true], [6, 'md'],
  ];
  const el = engine.render(comp(PROGRESS_BAR_TAG, fields));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-progress-bar-fill').style.width, '50%');
});

// ============================================================================
// RatingDisplay
// ============================================================================

function ratingFields({
  value = BOUND('r'), max = 5,
  variant = 'stars', showValue = true, precision = 'half',
} = {}) {
  return [[0, value], [1, max], [2, variant], [3, showValue], [4, precision]];
}

test('RatingDisplay renderuje N ikon SVG (max)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 3.5 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('svg.tf-rating__icon').length, 5);
});

test('RatingDisplay precision=half daje 0.5 width na 4. ikonie dla value=3.5', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 3.5 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields()));
  document.body.appendChild(el);
  const rects = el.querySelectorAll('clipPath rect');
  // Ikony 0,1,2 → full width=24; ikona 3 → half=12; ikona 4 → 0.
  assertEq(rects[0].getAttribute('width'), '24');
  assertEq(rects[2].getAttribute('width'), '24');
  assertEq(rects[3].getAttribute('width'), '12');
  assertEq(rects[4].getAttribute('width'), '0');
});

test('RatingDisplay precision=full zaokrągla 3.4 → 3', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 3.4 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields({ precision: 'full' })));
  document.body.appendChild(el);
  const rects = el.querySelectorAll('clipPath rect');
  assertEq(rects[2].getAttribute('width'), '24');
  assertEq(rects[3].getAttribute('width'), '0');
});

test('RatingDisplay precision=decimal daje partial fill 3.7 → 0.7 na 4.', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 3.7 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields({ precision: 'decimal' })));
  document.body.appendChild(el);
  const rects = el.querySelectorAll('clipPath rect');
  // 24 * 0.7 ≈ 16.8 (FP).
  const w = parseFloat(rects[3].getAttribute('width'));
  assert(Math.abs(w - 16.8) < 0.001, `expected ~16.8, got ${w}`);
});

test('RatingDisplay variant=numeric renderuje text', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 4.2 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields({ variant: 'numeric', precision: 'decimal' })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-rating__numeric').textContent, '4.2 / 5');
});

test('RatingDisplay variant=hearts dodaje klasę', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 2 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields({ variant: 'hearts' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-rating--variant-hearts'));
});

test('RatingDisplay reaguje na patch value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 1 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-rating__value').textContent, '1');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('r'), op: { kind: 'set', value: 4.5 } }] });
  assertEq(el.querySelector('.tf-rating__value').textContent, '4.5');
});

test('RatingDisplay clampuje value > max', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 99 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields({ precision: 'full' })));
  document.body.appendChild(el);
  const rects = el.querySelectorAll('clipPath rect');
  for (const r of rects) assertEq(r.getAttribute('width'), '24');
});

test('RatingDisplay numeric NaN→null sekwencja czyści aria-invalid', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 4 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields({ variant: 'numeric' })));
  document.body.appendChild(el);
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('r'), op: { kind: 'set', value: NaN } }] });
  assertEq(el.getAttribute('aria-invalid'), 'true');
  store.applyPatch({ base_revision: 1, new_revision: 2, ops: [{ path: PATH('r'), op: { kind: 'delete' } }] });
  assert(el.getAttribute('aria-invalid') == null, 'aria-invalid powinno być wyczyszczone po null');
});

test('RatingDisplay NaN/Infinity → aria-invalid', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 3 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(RATING_DISPLAY_TAG, ratingFields()));
  document.body.appendChild(el);
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('r'), op: { kind: 'set', value: NaN } }] });
  assertEq(el.getAttribute('aria-invalid'), 'true');
});

test('RatingDisplay odrzuca max=0', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 1 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(RATING_DISPLAY_TAG, ratingFields({ max: 0 }))));
});

test('RatingDisplay default max=5 gdy nie podano', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 3 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const fields = [[0, BOUND('r')], [2, 'stars'], [3, false], [4, 'full']];
  const el = engine.render(comp(RATING_DISPLAY_TAG, fields));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('svg.tf-rating__icon').length, 5);
});

// ============================================================================

const failed = results.filter((r) => !r.ok);
console.log(`progress+rating tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
