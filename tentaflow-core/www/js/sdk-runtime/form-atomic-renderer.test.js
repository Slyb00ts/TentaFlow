// =============================================================================
// Plik: sdk-runtime/form-atomic-renderer.test.js
// Opis: Testy Toggle/Checkbox/Radio — chunk 3.3c-1.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { TOGGLE_TAG, CHECKBOX_TAG, RADIO_TAG } from './form-atomic-renderer.js';

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

function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}
function makeEngine(store) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: { emit() {} },
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

// ============================================================================
// Toggle (0x030A)
// ============================================================================

test('Toggle renders <button role=switch> z reactive aria-checked po store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')],
      [1, { kind: 'literal', value: 'Powiadomienia' }],
      [3, 'md'],
      [6, 'trailing'],
    ])
  );
  const switchEl = el.querySelector('.tf-toggle__switch');
  assertEq(switchEl.tagName, 'BUTTON');
  assertEq(switchEl.getAttribute('role'), 'switch');
  assertEq(switchEl.getAttribute('aria-checked'), 'false');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('on'), op: { kind: 'set', value: true } }],
  });
  assertEq(switchEl.getAttribute('aria-checked'), 'true');
  assert(switchEl.classList.contains('tf-toggle__switch--on'));
});

test('Toggle click dispatches change z negated value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')],
      [1, { kind: 'literal', value: 'X' }],
      [3, 'sm'], [6, 'leading'],
    ])
  );
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  el.querySelector('.tf-toggle__switch').click();
  assertEq(received, { value: true, kind: 'bool' });
});

test('Toggle uses default tone=primary when field absent', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
      [3, 'md'], [6, 'trailing'],
    ])
  );
  assert(el.classList.contains('tf-toggle--tone-primary'));
});

test('Toggle disabled BindRef blocks click + sets aria-disabled', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('on'), value: false },
      { path: PATH('locked'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
      [3, 'md'], [5, { kind: 'bound', path: PATH('locked') }], [6, 'trailing'],
    ])
  );
  const sw = el.querySelector('.tf-toggle__switch');
  assertEq(sw.getAttribute('aria-disabled'), 'true');
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  sw.click();
  assertEq(received, null);
});

test('Toggle label_position=leading places label before switch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'L' }],
      [3, 'md'], [6, 'leading'],
    ])
  );
  // Label powinien być pierwszym dzieckiem.
  assertEq(el.children[0].classList.contains('tf-toggle__label'), true);
});

test('Toggle bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(TOGGLE_TAG, [[0, PATH('on')], [3, 'md'], [6, 'trailing']]))
  );
});

test('Toggle rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(TOGGLE_TAG, [
        [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
        [3, 'md'], [6, 'trailing'], [99, 'rogue'],
      ])
    )
  );
});

// ============================================================================
// Checkbox (0x030B)
// ============================================================================

test('Checkbox renders <input type=checkbox> z reactive checked', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('chk'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')],
      [1, { kind: 'literal', value: 'Zgoda' }],
      [5, 'md'],
    ])
  );
  const box = el.querySelector('input[type=checkbox]');
  assertEq(box.checked, false);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('chk'), op: { kind: 'set', value: true } }],
  });
  assertEq(box.checked, true);
});

test('Checkbox change event dispatches z value=bool', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('chk'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')], [1, { kind: 'literal', value: 'X' }], [5, 'md'],
    ])
  );
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  const box = el.querySelector('input');
  box.checked = true;
  box.dispatchEvent(new window.Event('change'));
  assertEq(received, { value: true, kind: 'bool' });
});

test('Checkbox indeterminate BindRef reactive', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('chk'), value: false },
      { path: PATH('ind'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')], [1, { kind: 'literal', value: 'X' }],
      [3, { kind: 'bound', path: PATH('ind') }], [5, 'md'],
    ])
  );
  const box = el.querySelector('input');
  assertEq(box.indeterminate, true);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('ind'), op: { kind: 'set', value: false } }],
  });
  assertEq(box.indeterminate, false);
});

test('Checkbox bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(CHECKBOX_TAG, [[0, PATH('chk')], [5, 'md']]))
  );
});

// ============================================================================
// Radio (0x030C)
// ============================================================================

test('Radio renders <input type=radio> z reactive checked po SelectValue', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('view'), value: 'list' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(RADIO_TAG, [
      [0, PATH('view')],
      [1, { kind: 'tstr', value: 'list' }],
      [2, { kind: 'literal', value: 'Lista' }],
    ])
  );
  const radio = el.querySelector('input[type=radio]');
  assertEq(radio.checked, true);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('view'), op: { kind: 'set', value: 'grid' } }],
  });
  assertEq(radio.checked, false);
});

test('Radio change dispatches z value+kind z SelectValue', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('n'), value: 0 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(RADIO_TAG, [
      [0, PATH('n')],
      [1, { kind: 'u32', value: 5 }],
      [2, { kind: 'literal', value: 'Pięć' }],
    ])
  );
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  const radio = el.querySelector('input');
  radio.checked = true;
  radio.dispatchEvent(new window.Event('change'));
  assertEq(received, { value: 5, kind: 'u32' });
});

test('Radio rejects missing required value/label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_TAG, [[0, PATH('x')]])));
  assertThrows(() =>
    engine.render(comp(RADIO_TAG, [[0, PATH('x')], [1, { kind: 'tstr', value: 'a' }]]))
  );
});

test('Radio rejects SelectValue z unknown kind', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(RADIO_TAG, [
        [0, PATH('x')],
        [1, { kind: 'float', value: 1.5 }],
        [2, { kind: 'literal', value: 'X' }],
      ])
    )
  );
});

test('Toggle bez label propaguje a11y.label jako aria-label na switch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [3, 'md'], [6, 'trailing'],
    ], { a11y: { label: { kind: 'literal', value: 'Powiadomienia' } } })
  );
  const sw = el.querySelector('.tf-toggle__switch');
  assertEq(sw.getAttribute('aria-label'), 'Powiadomienia');
});

test('Checkbox bez label propaguje a11y.label jako aria-label na input', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('chk'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(CHECKBOX_TAG, [[0, PATH('chk')], [5, 'md']], {
      a11y: { label: { kind: 'literal', value: 'Zgoda RODO' } },
    })
  );
  assertEq(el.querySelector('input').getAttribute('aria-label'), 'Zgoda RODO');
});

test('Toggle a11y.label rejects whitespace-only initial value', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(TOGGLE_TAG, [
        [0, PATH('on')], [3, 'md'], [6, 'trailing'],
      ], { a11y: { label: { kind: 'literal', value: '   ' } } })
    )
  );
});

test('Radio name nie koliduje dla key=a__i_1 vs [a, 1]', () => {
  setup();
  const engine = makeEngine();
  // PathSegment z key 'a__i_1' powinno mieć INNĄ name niż [key('a'), index(1)].
  const r1 = engine.render(
    comp(RADIO_TAG, [
      [0, [{ kind: 'key', value: 'a__i_1' }]],
      [1, { kind: 'tstr', value: 'x' }],
      [2, { kind: 'literal', value: 'X' }],
    ], { id: 'r1' })
  );
  const r2 = engine.render(
    comp(RADIO_TAG, [
      [0, [{ kind: 'key', value: 'a' }, { kind: 'index', value: 1 }]],
      [1, { kind: 'tstr', value: 'y' }],
      [2, { kind: 'literal', value: 'Y' }],
    ], { id: 'r2' })
  );
  const n1 = r1.querySelector('input').getAttribute('name');
  const n2 = r2.querySelector('input').getAttribute('name');
  assert(n1 !== n2, `path serialization collision: ${n1}`);
});

test('Radio name attr is deterministic from bind_path', () => {
  setup();
  const engine = makeEngine();
  const path1 = PATH('cfg', 'view');
  const radio1 = engine.render(
    comp(RADIO_TAG, [
      [0, path1], [1, { kind: 'tstr', value: 'a' }],
      [2, { kind: 'literal', value: 'A' }],
    ], { id: 'r1' })
  );
  const radio2 = engine.render(
    comp(RADIO_TAG, [
      [0, path1], [1, { kind: 'tstr', value: 'b' }],
      [2, { kind: 'literal', value: 'B' }],
    ], { id: 'r2' })
  );
  const n1 = radio1.querySelector('input').getAttribute('name');
  const n2 = radio2.querySelector('input').getAttribute('name');
  assertEq(n1, n2);
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
