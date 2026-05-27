// =============================================================================
// Plik: sdk-runtime/action-bars-renderer.test.js
// Opis: Testy ActionBar/SegmentedControl/FilterChips/WizardFooter (3.3b-6).
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
import {
  ACTION_BAR_TAG, SEGMENTED_CONTROL_TAG, FILTER_CHIPS_TAG, WIZARD_FOOTER_TAG,
} from './action-bars-renderer.js';

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
function btn(label, extra = {}) {
  return comp(BUTTON_TAG, [
    [0, 'primary'], [1, 'neutral'],
    [2, { kind: 'literal', value: label }],
    [5, 'md'], [6, false], [9, 'default'],
  ], extra);
}

// ============================================================================
// ActionBar
// ============================================================================

test('ActionBar renders leading + divider + trailing slots', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(ACTION_BAR_TAG, [
      [0, [btn('Save', { id: 's1' })]],
      [1, [btn('Cancel', { id: 'c1' }), btn('OK', { id: 'o1' })]],
      [2, true],
      [3, false],
    ])
  );
  assertEq(el.getAttribute('role'), 'toolbar');
  assertEq(el.querySelectorAll('.tf-action-bar__leading .tf-button').length, 1);
  assertEq(el.querySelectorAll('.tf-action-bar__trailing .tf-button').length, 2);
  assert(el.querySelector('.tf-action-bar__divider') != null);
});

test('ActionBar sticky=true adds modifier class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(ACTION_BAR_TAG, [[0, []], [1, []], [2, false], [3, true]])
  );
  assert(el.classList.contains('tf-action-bar--sticky'));
  assertEq(el.querySelector('.tf-action-bar__divider'), null);
});

test('ActionBar rejects non-Button child', () => {
  setup();
  const engine = makeEngine();
  const iconBtn = comp(ICON_BUTTON_TAG, [
    [0, { kind: 'named', name: 'star' }],
    [1, 'primary'], [2, 'neutral'], [3, 'md'], [4, 'X'],
  ]);
  assertThrows(() =>
    engine.render(
      comp(ACTION_BAR_TAG, [[0, [iconBtn]], [1, []], [2, false], [3, false]])
    )
  );
});

// ============================================================================
// SegmentedControl
// ============================================================================

test('SegmentedControl renders radiogroup z opcjami i ustawia aria-checked po store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('view'), value: 'list' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SEGMENTED_CONTROL_TAG, [
      [0, PATH('view')],
      [1, [
        { value: { kind: 'tstr', value: 'list' }, label: { kind: 'literal', value: 'Lista' } },
        { value: { kind: 'tstr', value: 'grid' }, label: { kind: 'literal', value: 'Siatka' } },
      ]],
      [2, 'md'],
      [3, false],
    ])
  );
  assertEq(el.getAttribute('role'), 'radiogroup');
  const opts = el.querySelectorAll('.tf-segmented__option');
  assertEq(opts[0].getAttribute('aria-checked'), 'true');
  assertEq(opts[1].getAttribute('aria-checked'), 'false');
  // Patch w store przesuwa selection.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('view'), op: { kind: 'set', value: 'grid' } }],
  });
  assertEq(opts[0].getAttribute('aria-checked'), 'false');
  assertEq(opts[1].getAttribute('aria-checked'), 'true');
});

test('SegmentedControl click dispatches change z detail.value+kind', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('mode'), value: 1 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SEGMENTED_CONTROL_TAG, [
      [0, PATH('mode')],
      [1, [
        { value: { kind: 'u32', value: 0 }, label: { kind: 'literal', value: 'A' } },
        { value: { kind: 'u32', value: 1 }, label: { kind: 'literal', value: 'B' } },
      ]],
      [2, 'sm'], [3, false],
    ])
  );
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  el.querySelectorAll('.tf-segmented__option')[0].click();
  assertEq(received, { value: 0, kind: 'u32' });
});

test('SegmentedControl rejects empty options', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SEGMENTED_CONTROL_TAG, [[0, PATH('x')], [1, []], [2, 'md'], [3, false]])
    )
  );
});

test('SegmentedControl rejects option bez label i bez icon', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SEGMENTED_CONTROL_TAG, [
        [0, PATH('x')],
        [1, [{ value: { kind: 'bool', value: true } }]],
        [2, 'md'], [3, false],
      ])
    )
  );
});

test('SegmentedControl rejects SelectValue z unknown kind', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SEGMENTED_CONTROL_TAG, [
        [0, PATH('x')],
        [1, [{ value: { kind: 'float', value: 3.14 }, label: { kind: 'literal', value: 'X' } }]],
        [2, 'md'], [3, false],
      ])
    )
  );
});

// ============================================================================
// FilterChips
// ============================================================================

test('SegmentedControl icon-only z named icon bez label rejected (a11y)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SEGMENTED_CONTROL_TAG, [
        [0, PATH('x')],
        [1, [{
          value: { kind: 'tstr', value: 'a' },
          icon: { kind: 'named', name: 'star' },
        }]],
        [2, 'md'], [3, false],
      ])
    )
  );
});

test('SegmentedControl icon-only z asset icon z alt OK', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('x'), value: 'a' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SEGMENTED_CONTROL_TAG, [
      [0, PATH('x')],
      [1, [{
        value: { kind: 'tstr', value: 'a' },
        icon: { kind: 'asset', ref: '/x.png', alt: 'Star variant' },
      }]],
      [2, 'sm'], [3, false],
    ])
  );
  assertEq(el.querySelectorAll('.tf-segmented__option').length, 1);
});

test('FilterChips multi mode reflects selected_ids array', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(FILTER_CHIPS_TAG, [
      [0, [
        { id: 'a', label: { kind: 'literal', value: 'Alpha' } },
        { id: 'b', label: { kind: 'literal', value: 'Beta' } },
      ]],
      [1, PATH('sel')],
      [2, 'multi'],
      [3, false],
    ])
  );
  const chips = el.querySelectorAll('.tf-filter-chips__chip');
  // ARIA: role=checkbox (multi) używa aria-checked, nie aria-pressed.
  assertEq(chips[0].getAttribute('aria-checked'), 'true');
  assertEq(chips[1].getAttribute('aria-checked'), 'false');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: ['a', 'b'] } }],
  });
  assertEq(chips[0].getAttribute('aria-checked'), 'true');
  assertEq(chips[1].getAttribute('aria-checked'), 'true');
});

test('FilterChips chip click dispatches change z detail.chip_id', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(FILTER_CHIPS_TAG, [
      [0, [{ id: 'x', label: { kind: 'literal', value: 'X' } }]],
      [1, PATH('sel')],
      [2, 'multi'],
      [3, false],
    ])
  );
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  el.querySelector('.tf-filter-chips__chip').click();
  assertEq(received, { chip_id: 'x' });
});

test('FilterChips clearable=true renders × button which dispatches clear', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(FILTER_CHIPS_TAG, [
      [0, [{ id: 'x', label: { kind: 'literal', value: 'X' } }]],
      [1, PATH('sel')],
      [2, 'single'],
      [3, true],
    ])
  );
  const clear = el.querySelector('.tf-filter-chips__clear');
  assert(clear != null);
  let cleared = false;
  el.addEventListener('clear', () => { cleared = true; });
  clear.click();
  assert(cleared);
});

test('FilterChips with count_path renders reactive count number', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('sel'), value: [] },
      { path: PATH('counts', 'a'), value: 42 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(FILTER_CHIPS_TAG, [
      [0, [{ id: 'a', label: { kind: 'literal', value: 'A' }, count_path: PATH('counts', 'a') }]],
      [1, PATH('sel')],
      [2, 'multi'],
      [3, false],
    ])
  );
  const count = el.querySelector('.tf-filter-chips__chip-count');
  assertEq(count.textContent, '42');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('counts', 'a'), op: { kind: 'set', value: 99 } }],
  });
  assertEq(count.textContent, '99');
});

test('FilterChips rejects duplicate chip id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(FILTER_CHIPS_TAG, [
        [0, [
          { id: 'x', label: { kind: 'literal', value: 'A' } },
          { id: 'x', label: { kind: 'literal', value: 'B' } },
        ]],
        [1, PATH('sel')], [2, 'multi'], [3, false],
      ])
    )
  );
});

// ============================================================================
// WizardFooter
// ============================================================================

test('WizardFooter renders all 4 optional buttons + extra w odpowiednich slotach', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(WIZARD_FOOTER_TAG, [
      [0, btn('Wstecz')],
      [1, btn('Dalej')],
      [2, btn('Anuluj')],
      [3, btn('Pomiń')],
      [4, [btn('Pomoc')]],
    ])
  );
  assert(el.querySelector('.tf-wizard-footer__back') != null);
  assert(el.querySelector('.tf-wizard-footer__cancel') != null);
  assert(el.querySelector('.tf-wizard-footer__next') != null);
  assert(el.querySelector('.tf-wizard-footer__skip') != null);
  assertEq(el.querySelectorAll('.tf-wizard-footer__center .tf-button').length, 1);
  // Left = back + cancel; right = skip + next.
  assertEq(el.querySelectorAll('.tf-wizard-footer__left .tf-button').length, 2);
  assertEq(el.querySelectorAll('.tf-wizard-footer__right .tf-button').length, 2);
});

test('WizardFooter wszystkie 4 actions optional — empty extra_actions OK', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(WIZARD_FOOTER_TAG, [[4, []]])
  );
  assertEq(el.getAttribute('role'), 'toolbar');
  assertEq(el.querySelectorAll('.tf-button').length, 0);
});

test('WizardFooter rejects non-Button w back_action', () => {
  setup();
  const engine = makeEngine();
  const ib = comp(ICON_BUTTON_TAG, [
    [0, { kind: 'named', name: 'star' }],
    [1, 'primary'], [2, 'neutral'], [3, 'md'], [4, 'X'],
  ]);
  assertThrows(() =>
    engine.render(comp(WIZARD_FOOTER_TAG, [[0, ib], [4, []]]))
  );
});

test('WizardFooter rejects missing extra_actions', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(WIZARD_FOOTER_TAG, [])));
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
