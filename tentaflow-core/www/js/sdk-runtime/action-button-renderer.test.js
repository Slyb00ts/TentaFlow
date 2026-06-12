// =============================================================================
// Plik: sdk-runtime/action-button-renderer.test.js
// Opis: Testy Button (tag 0x0401) — chunk 3.3b-1.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { BUTTON_TAG } from './action-button-renderer.js';

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
  typeof s === 'number'
    ? { kind: 'index', value: s }
    : { kind: 'key', value: s });

function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}
function makeEngine(store, dispatcher) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: dispatcher || { emit() {} },
    locale: 'en-US',
  });
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

const VALID = [
  [0, 'primary'],
  [1, 'neutral'],
  [2, { kind: 'literal', value: 'OK' }],
  [5, 'md'],
  [6, false],
  [9, 'default'],
];

// ============================================================================
// Render basics
// ============================================================================

test('Button renders <tf-button> with mapped variant + label attribute', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BUTTON_TAG, [
      [0, 'destructive'], [1, 'success'], [2, { kind: 'literal', value: 'Save' }],
      [5, 'lg'], [6, true], [9, 'comfortable'],
    ])
  );
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-BUTTON');
  // SDK 'destructive' maps to tf-button variant="danger".
  assertEq(el.getAttribute('variant'), 'danger');
  // SDK size 'lg' has no tf-button size mapping (md/lg are default).
  assertEq(el.getAttribute('size'), null);
  assertEq(el.getAttribute('label'), 'Save');
  // full_width=true is applied as inline width on the host.
  assertEq(el.style.width, '100%');
});

test('Button maps SDK xs/sm sizes to tf-button size=sm', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BUTTON_TAG, [
      [0, 'tertiary'], [1, 'neutral'], [2, { kind: 'literal', value: 'X' }],
      [5, 'xs'], [6, false], [9, 'compact'],
    ])
  );
  // SDK 'tertiary' maps to tf-button variant="ghost".
  assertEq(el.getAttribute('variant'), 'ghost');
  assertEq(el.getAttribute('size'), 'sm');
  assertEq(el.style.width, '');
});

test('Button label updates reactively from BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'A' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(BUTTON_TAG, [
      [0, 'primary'], [1, 'neutral'],
      [2, { kind: 'bound', path: PATH('lbl') }],
      [5, 'md'], [6, false], [9, 'default'],
    ])
  );
  assertEq(el.getAttribute('label'), 'A');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'B' } }],
  });
  assertEq(el.getAttribute('label'), 'B');
});

// ============================================================================
// Disabled BindRef
// ============================================================================

test('Button disabled BindRef toggles host disabled attribute reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('d'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(BUTTON_TAG, [
      ...VALID,
      [7, { kind: 'bound', path: PATH('d') }],
    ])
  );
  assert(el.hasAttribute('disabled'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('d'), op: { kind: 'set', value: false } }],
  });
  assert(!el.hasAttribute('disabled'));
});

// ============================================================================
// Loading BindRef
// ============================================================================

test('Button loading BindRef sets aria-busy + implicit disabled', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('l'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(BUTTON_TAG, [
      ...VALID,
      [8, { kind: 'bound', path: PATH('l') }],
    ])
  );
  assertEq(el.getAttribute('aria-busy'), 'true');
  // Loading forces disabled even without a field-7 bind.
  assert(el.hasAttribute('disabled'));
});

test('Button loading=false clears aria-busy and disabled', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('l'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(BUTTON_TAG, [
      ...VALID,
      [8, { kind: 'bound', path: PATH('l') }],
    ])
  );
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('l'), op: { kind: 'set', value: false } }],
  });
  assert(!el.hasAttribute('aria-busy'));
  assert(!el.hasAttribute('disabled'));
});

test('Button: explicit disabled=true overrides loading=false', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('d'), value: true },
      { path: PATH('l'), value: false },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(BUTTON_TAG, [
      ...VALID,
      [7, { kind: 'bound', path: PATH('d') }],
      [8, { kind: 'bound', path: PATH('l') }],
    ])
  );
  assert(el.hasAttribute('disabled'));
});

// ============================================================================
// Spec compliance
// ============================================================================

// Current renderer contract: icon_leading/icon_trailing are read but neither
// rendered nor rejected (empty if-blocks in renderButton) — see bug report.
test('Button accepts but does not render icon_leading/icon_trailing', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BUTTON_TAG, [
      ...VALID,
      [3, { kind: 'named', name: 'star' }],
      [4, { kind: 'named', name: 'arrow_right' }],
    ])
  );
  assertEq(el.tagName, 'TF-BUTTON');
  assertEq(el.getAttribute('icon'), null);
  assertEq(el.children.length, 0);
});

test('Button rejects invalid tone', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BUTTON_TAG, [
        [0, 'primary'], [1, 'sparkly'], [2, { kind: 'literal', value: 'X' }],
        [5, 'md'], [6, false], [9, 'default'],
      ])
    )
  );
});

test('Button rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(BUTTON_TAG, [...VALID, [99, 'rogue']]))
  );
});

test('Button rejects invalid variant', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BUTTON_TAG, [
        [0, 'galactic'], [1, 'neutral'], [2, { kind: 'literal', value: 'X' }],
        [5, 'md'], [6, false], [9, 'default'],
      ])
    )
  );
});

test('Button rejects invalid size', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BUTTON_TAG, [
        [0, 'primary'], [1, 'neutral'], [2, { kind: 'literal', value: 'X' }],
        [5, 'huge'], [6, false], [9, 'default'],
      ])
    )
  );
});

test('Button rejects missing required fields', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BUTTON_TAG, [])));
  assertThrows(() => engine.render(comp(BUTTON_TAG, [
    [0, 'primary'], [1, 'neutral'], [2, { kind: 'literal', value: 'X' }],
  ])));
});

// ============================================================================
// Click via engine handlers
// ============================================================================

test('Button click handler emits through eventDispatcher', () => {
  setup();
  const dispatched = [];
  const engine = makeEngine(undefined, {
    emit(evt) { dispatched.push(evt); }
  });
  const el = engine.render(
    comp(BUTTON_TAG, VALID, {
      handlers: [['click', { kind: 'backend', operation_id: 'save_op' }]],
    })
  );
  el.click();
  assertEq(dispatched.length, 1);
  assertEq(dispatched[0].event_kind, 'click');
  assertEq(dispatched[0].source_id, 'c1');
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
