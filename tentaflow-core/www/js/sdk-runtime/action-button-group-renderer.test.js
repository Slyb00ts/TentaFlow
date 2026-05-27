// =============================================================================
// Plik: sdk-runtime/action-button-group-renderer.test.js
// Opis: Testy ButtonGroup (tag 0x0403) — chunk 3.3b-4.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { BUTTON_TAG } from './action-button-renderer.js';
import { ICON_BUTTON_TAG } from './action-icon-button-renderer.js';
import { BUTTON_GROUP_TAG } from './action-button-group-renderer.js';

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

function makeEngine() {
  return new ComponentRenderer({
    store: new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n }),
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

function btn(label, extra = {}) {
  return comp(BUTTON_TAG, [
    [0, 'primary'], [1, 'neutral'],
    [2, { kind: 'literal', value: label }],
    [5, 'md'], [6, false], [9, 'default'],
  ], extra);
}

// ============================================================================

test('ButtonGroup renders div role=group z orientation class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BUTTON_GROUP_TAG, [
      [0, [btn('A'), btn('B'), btn('C')]],
      [1, 'horizontal'],
      [2, false],
    ])
  );
  assertEq(el.tagName, 'DIV');
  assertEq(el.getAttribute('role'), 'group');
  assert(el.classList.contains('tf-button-group'));
  assert(el.classList.contains('tf-button-group--orientation-horizontal'));
  assert(!el.classList.contains('tf-button-group--attached'));
  assertEq(el.querySelectorAll('.tf-button').length, 3);
});

test('ButtonGroup vertical attached=true', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BUTTON_GROUP_TAG, [
      [0, [btn('X')]],
      [1, 'vertical'],
      [2, true],
    ])
  );
  assert(el.classList.contains('tf-button-group--orientation-vertical'));
  assert(el.classList.contains('tf-button-group--attached'));
});

test('ButtonGroup rejects non-Button child', () => {
  setup();
  const engine = makeEngine();
  // IconButton (tag 0x0402) ≠ Button (0x0401) — spec wymusza Button TAG.
  const iconBtn = comp(ICON_BUTTON_TAG, [
    [0, { kind: 'named', name: 'star' }],
    [1, 'primary'], [2, 'neutral'], [3, 'md'], [4, 'Star it'],
  ]);
  assertThrows(() =>
    engine.render(
      comp(BUTTON_GROUP_TAG, [
        [0, [btn('A'), iconBtn]],
        [1, 'horizontal'], [2, false],
      ])
    )
  );
});

test('ButtonGroup rejects empty/missing required fields', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BUTTON_GROUP_TAG, [])));
  // Brak orientation.
  assertThrows(() =>
    engine.render(comp(BUTTON_GROUP_TAG, [[0, [btn('A')]], [2, false]]))
  );
  // Brak attached.
  assertThrows(() =>
    engine.render(comp(BUTTON_GROUP_TAG, [[0, [btn('A')]], [1, 'horizontal']]))
  );
});

test('ButtonGroup empty buttons array is valid (empty group)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BUTTON_GROUP_TAG, [[0, []], [1, 'horizontal'], [2, false]])
  );
  assertEq(el.querySelectorAll('.tf-button').length, 0);
});

test('ButtonGroup rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BUTTON_GROUP_TAG, [
        [0, [btn('A')]], [1, 'horizontal'], [2, false], [99, 'rogue'],
      ])
    )
  );
});

test('ButtonGroup rejects invalid orientation', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BUTTON_GROUP_TAG, [
        [0, [btn('A')]], [1, 'diagonal'], [2, false],
      ])
    )
  );
});

test('ButtonGroup propaguje handlers + bind do dzieci Button', () => {
  setup();
  const dispatched = [];
  const store = new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
  const engine = new ComponentRenderer({
    store,
    eventDispatcher: { emit(e) { dispatched.push(e); } },
    locale: 'en-US',
  });
  const buttonWithHandler = comp(BUTTON_TAG, [
    [0, 'primary'], [1, 'neutral'],
    [2, { kind: 'literal', value: 'Save' }],
    [5, 'md'], [6, false], [9, 'default'],
  ], { id: 'btn-save', handlers: [['click', { kind: 'backend', operation_id: 'save_op' }]] });
  const el = engine.render(
    comp(BUTTON_GROUP_TAG, [
      [0, [buttonWithHandler]],
      [1, 'horizontal'], [2, false],
    ])
  );
  el.querySelector('.tf-button').click();
  assertEq(dispatched.length, 1);
  assertEq(dispatched[0].source_id, 'btn-save');
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
