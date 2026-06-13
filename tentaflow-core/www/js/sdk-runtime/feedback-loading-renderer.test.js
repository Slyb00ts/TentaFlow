// =============================================================================
// File: sdk-runtime/feedback-loading-renderer.test.js
// Description: Tests for Skeleton (0x0506), Spinner (0x0507),
// LoadingBar (0x0508) — chunk 3.3e-2.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  SKELETON_TAG, SPINNER_TAG, LOADING_BAR_TAG,
} from './feedback-loading-renderer.js';

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
const LIT = (value) => ({ kind: 'literal', value });
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
// Skeleton
// ============================================================================

function skeletonFields({
  variant = 'text', width = null, height = null,
  animate = true, lines = 3,
} = {}) {
  const f = [[0, variant]];
  if (width != null) f.push([1, width]);
  if (height != null) f.push([2, height]);
  f.push([3, animate], [4, lines]);
  return f;
}

test('Skeleton text variant renders correct number of lines', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({ variant: 'text', lines: 5 })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-skeleton'));
  assert(el.classList.contains('tf-skeleton--text'));
  assertEq(el.querySelectorAll('.tf-skeleton__line').length, 5);
});

test('Skeleton animate=true adds shimmer class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({ animate: true })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-skeleton--animate'));
});

test('Skeleton animate=false omits shimmer class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({ animate: false })));
  document.body.appendChild(el);
  assert(!el.classList.contains('tf-skeleton--animate'));
});

test('Skeleton circle variant renders circle element', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({
    variant: 'circle',
    width: { kind: 'px', value: 48 },
  })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-skeleton--circle'));
  assert(el.querySelector('.tf-skeleton__circle') != null);
});

test('Skeleton rectangle variant renders rect element', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({ variant: 'rectangle' })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-skeleton__rect') != null);
});

test('Skeleton card variant renders header rect + 3 body lines', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({ variant: 'card' })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-skeleton__card-header') != null);
  assertEq(el.querySelectorAll('.tf-skeleton__card-body .tf-skeleton__line').length, 3);
});

test('Skeleton table_row variant renders 4 cells', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({ variant: 'table_row' })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('.tf-skeleton__table-cell').length, 4);
});

test('Skeleton applies width DimensionToken', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields({
    variant: 'rectangle',
    width: { kind: 'px', value: 200 },
  })));
  document.body.appendChild(el);
  assertEq(el.style.width, '200px');
});

test('Skeleton rejects invalid variant', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SKELETON_TAG, skeletonFields({ variant: 'blob' }))));
});

test('Skeleton rejects unknown field key', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SKELETON_TAG, [[0, 'text'], [3, true], [4, 1], [9, 'bad']])));
});

test('Skeleton has aria-hidden=true', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SKELETON_TAG, skeletonFields()));
  document.body.appendChild(el);
  assertEq(el.getAttribute('aria-hidden'), 'true');
});

// ============================================================================
// Spinner
// ============================================================================

function spinnerFields({
  size = 'md', tone = 'primary', label = null, variant = 'default',
} = {}) {
  const f = [[0, size], [1, tone]];
  if (label != null) f.push([2, label]);
  f.push([3, variant]);
  return f;
}

test('Spinner renders <tf-spinner> with size and tone attributes', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SPINNER_TAG, spinnerFields({ size: 'lg', tone: 'success' })));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-SPINNER');
  assertEq(el.getAttribute('size'), 'lg');
  assertEq(el.getAttribute('tone'), 'success');
});

test('Spinner variant maps to a variant attribute on the host', () => {
  setup();
  const engine = makeEngine(makeStore());
  for (const variant of ['default', 'ring', 'dots', 'bars']) {
    const el = engine.render(comp(SPINNER_TAG, spinnerFields({ variant })));
    document.body.appendChild(el);
    assertEq(el.getAttribute('variant'), variant, `variant ${variant}`);
  }
});

test('Spinner label sets aria-label on the host', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SPINNER_TAG, spinnerFields({ label: LIT('Loading data') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('aria-label'), 'Loading data');
});

test('Spinner label reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sl'), value: 'Wait' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SPINNER_TAG, spinnerFields({ label: BOUND('sl') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('aria-label'), 'Wait');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('sl'), op: { kind: 'set', value: 'Almost done' } }] });
  assertEq(el.getAttribute('aria-label'), 'Almost done');
});

test('Spinner without label has no aria-label', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SPINNER_TAG, spinnerFields()));
  document.body.appendChild(el);
  assertEq(el.getAttribute('aria-label'), null);
});

test('Spinner rejects non-BindRef label', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SPINNER_TAG, spinnerFields({ label: 'raw-string' }))));
});

test('Spinner rejects invalid size', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SPINNER_TAG, spinnerFields({ size: 'huge' }))));
});

test('Spinner rejects invalid variant', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SPINNER_TAG, spinnerFields({ variant: 'spiral' }))));
});

test('Spinner rejects unknown field key', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SPINNER_TAG, [[0, 'md'], [1, 'primary'], [3, 'default'], [7, 'x']])));
});

// ============================================================================
// LoadingBar
// ============================================================================

function loadingBarFields({
  visible = LIT(true), progress = null, tone = 'primary',
} = {}) {
  const f = [[0, visible]];
  if (progress != null) f.push([1, progress]);
  f.push([2, tone]);
  return f;
}

test('LoadingBar renders with tone class and role=progressbar', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ tone: 'info' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-loading-bar'));
  assert(el.classList.contains('tf-loading-bar--tone-info'));
  assertEq(el.getAttribute('role'), 'progressbar');
});

test('LoadingBar indeterminate when no progress', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields()));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-loading-bar--indeterminate'));
  assert(!el.classList.contains('tf-loading-bar--determinate'));
});

test('LoadingBar determinate with progress BindRef', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ progress: LIT(0.5) })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-loading-bar--determinate'));
  const track = el.querySelector('.tf-loading-bar__track');
  assertEq(track.style.width, '50%');
  assertEq(el.getAttribute('aria-valuenow'), '50');
});

test('LoadingBar visible=false hides element', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ visible: LIT(false) })));
  document.body.appendChild(el);
  assertEq(el.style.display, 'none');
});

test('LoadingBar visible reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('vis'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ visible: BOUND('vis') })));
  document.body.appendChild(el);
  assert(el.style.display !== 'none');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('vis'), op: { kind: 'set', value: false } }] });
  assertEq(el.style.display, 'none');
});

test('LoadingBar progress reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('pg'), value: 0.3 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ progress: BOUND('pg') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-loading-bar__track').style.width, '30%');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('pg'), op: { kind: 'set', value: 0.8 } }] });
  assertEq(el.querySelector('.tf-loading-bar__track').style.width, '80%');
});

test('LoadingBar progress clamps to 0..1', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ progress: LIT(1.5) })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-loading-bar__track').style.width, '100%');
});

test('LoadingBar rejects missing visible', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(LOADING_BAR_TAG, [[2, 'primary']])));
});

test('LoadingBar rejects invalid tone', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ tone: 'nope' }))));
});

test('LoadingBar rejects unknown field key', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(LOADING_BAR_TAG, [[0, LIT(true)], [2, 'primary'], [5, 'x']])));
});

test('LoadingBar has aria-valuemin/max when determinate', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LOADING_BAR_TAG, loadingBarFields({ progress: LIT(0.25) })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('aria-valuemin'), '0');
  assertEq(el.getAttribute('aria-valuemax'), '100');
});

// ============================================================================
const failed = results.filter((r) => !r.ok);
console.log(`feedback-loading tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
