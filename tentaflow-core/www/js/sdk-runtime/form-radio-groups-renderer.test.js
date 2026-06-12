// =============================================================================
// File: sdk-runtime/form-radio-groups-renderer.test.js
// Description: Tests for RadioGroup (0x030D) + RadioCardGroup (0x030E) rendered
// through the <tf-radio-group> / <tf-radio> web components (cards variant for
// 0x030E). Components are imported so happy-dom upgrades them on mount.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-radio.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  RADIO_GROUP_TAG, RADIO_CARD_GROUP_TAG,
} from './form-radio-groups-renderer.js';

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
// tf-radio-group builds its light DOM in connectedCallback, so tests mount.
function mount(el) {
  document.body.appendChild(el);
  return el;
}

function rOpt(v, lbl, opts = {}) {
  const f = [
    [0, { kind: 'tstr', value: v }],
    [1, { kind: 'literal', value: lbl }],
    [3, opts.disabled === true],
  ];
  if (opts.hint != null) f.push([2, opts.hint]);
  return f;
}

function rcOpt(v, title, opts = {}) {
  const f = [
    [0, { kind: 'tstr', value: v }],
    [1, opts.icon || { kind: 'named', name: 'check' }],
    [2, { kind: 'literal', value: title }],
    [5, opts.disabled === true],
  ];
  if (opts.description != null) f.push([3, opts.description]);
  if (opts.badge != null) f.push([4, opts.badge]);
  return f;
}

// ============================================================================
// RadioGroup
// ============================================================================

test('RadioGroup renders tf-radio-group with N tf-radio children + radiogroup role', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A'), rOpt('b', 'B')]],
    [2, 'vertical'], [3, { kind: 'literal', value: 'Wybór' }], [4, 'default'],
  ])));
  assertEq(el.tagName.toLowerCase(), 'tf-radio-group');
  assertEq(el.querySelectorAll('tf-radio').length, 2);
  assert(el.querySelector('[role=radiogroup]') != null, 'expected radiogroup role on list wrap');
  assertEq(el.querySelectorAll('tf-radio .tf-radio-label').length, 2);
});

test('RadioGroup sets shared name attr on the group', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A'), rOpt('b', 'B')]],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
  assertEq(el.getAttribute('name'), 'tf-radio-group-c1');
});

test('RadioGroup reactive checked sync from store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 'b' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A'), rOpt('b', 'B')]],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
  assertEq(el.getAttribute('value'), 'b');
  const radios = el.querySelectorAll('tf-radio');
  assertEq(radios[0].checked, false);
  assertEq(radios[1].checked, true);
  // Store patch flips the selection.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('r'), op: { kind: 'set', value: 'a' } }],
  });
  assertEq(el.getAttribute('value'), 'a');
  assertEq(radios[0].checked, true);
  assertEq(radios[1].checked, false);
});

test('RadioGroup click emits change with SelectValue payload', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A'), rOpt('b', 'B')]],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelectorAll('tf-radio')[1].querySelector('.tf-radio-label').click();
  assertEq(got, { value: 'b', kind: 'tstr' });
});

test('RadioGroup disabled option does not emit change', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A', { disabled: true }), rOpt('b', 'B')]],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const radios = el.querySelectorAll('tf-radio');
  assertEq(radios[0].hasAttribute('disabled'), true);
  radios[0].querySelector('.tf-radio-label').click();
  assertEq(got, null);
});

test('RadioGroup duplicate option values throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A'), rOpt('a', 'A2')]],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
});

test('RadioGroup empty options throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, []],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
});

test('RadioGroup orientation=vertical sets modifier classes + list hook', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A')]],
    [2, 'vertical'], [3, { kind: 'literal', value: 'W' }], [4, 'compact'],
  ])));
  assert(el.classList.contains('tf-radio-group--vertical'));
  assert(el.classList.contains('tf-radio-group--density-compact'));
  assert(el.querySelector('.tf-radio-group__list') != null, 'expected list hook for modifiers');
});

test('RadioGroup invalid orientation throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A')]],
    [2, 'sideways'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
});

test('RadioGroup without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A')]],
    [2, 'horizontal'], [4, 'default'],
  ])));
});

test('RadioGroup with label has aria-labelledby', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A')]],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'Lab' }], [4, 'default'],
  ])));
  const lbl = el.querySelector('.tf-radio-group__label');
  assertEq(lbl.textContent, 'Lab');
  assertEq(el.getAttribute('aria-labelledby'), lbl.id);
});

test('RadioGroup unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')], [1, [rOpt('a', 'A')]],
    [2, 'horizontal'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
    [99, 'x'],
  ])));
});

test('RadioGroup option with hint renders .tf-radio__hint on tf-radio', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_GROUP_TAG, [
    [0, PATH('r')],
    [1, [rOpt('a', 'A', { hint: { kind: 'literal', value: 'pomocniczy' } })]],
    [2, 'vertical'], [3, { kind: 'literal', value: 'W' }], [4, 'default'],
  ])));
  const radio = el.querySelector('tf-radio');
  assertEq(radio.getAttribute('hint'), 'pomocniczy');
  assertEq(radio.querySelector('.tf-radio__hint').textContent, 'pomocniczy');
});

// ============================================================================
// RadioCardGroup
// ============================================================================

test('RadioCardGroup renders N cards + columns CSS var on cards group', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'Plan A'), rcOpt('b', 'Plan B'), rcOpt('c', 'Plan C')]],
    [2, 3], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'Plany' } } })));
  assertEq(el.tagName.toLowerCase(), 'tf-radio-group');
  assertEq(el.hasAttribute('cards'), true);
  assertEq(el.querySelectorAll('.tf-radio-card-group__card').length, 3);
  assert(el.querySelector('.tf-radio-card-group[role=radiogroup]') != null);
  assertEq(el.style.getPropertyValue('--tf-radio-card-cols'), '3');
});

test('RadioCardGroup variant=feature sets class', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A')]],
    [2, 1], [3, 'feature'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
  assert(el.classList.contains('tf-radio-card-group--variant-feature'));
});

test('RadioCardGroup reactive selected highlight + checked', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('r'), value: 'b' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A'), rcOpt('b', 'B')]],
    [2, 2], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
  const cards = el.querySelectorAll('.tf-radio-card-group__card');
  assert(cards[1].classList.contains('tf-radio-card-group__card--selected'));
  assert(!cards[0].classList.contains('tf-radio-card-group__card--selected'));
  assertEq(el.querySelectorAll('tf-radio')[1].checked, true);
});

test('RadioCardGroup click emits change with SelectValue payload', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A'), rcOpt('b', 'B')]],
    [2, 2], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelectorAll('.tf-radio-card-group__card')[1].click();
  assertEq(got, { value: 'b', kind: 'tstr' });
});

test('RadioCardGroup option with badge renders .tf-inline-badge', () => {
  setup();
  const engine = makeEngine();
  const badge = [
    [0, 'soft'], [1, 'success'],
    [2, { kind: 'literal', value: 'NEW' }],
    [5, false],
  ];
  const el = mount(engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')],
    [1, [rcOpt('a', 'A', { badge })]],
    [2, 1], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
  const bg = el.querySelector('.tf-inline-badge');
  assert(bg != null);
  assertEq(bg.querySelector('.tf-inline-badge__label').textContent, 'NEW');
  assert(bg.classList.contains('tf-inline-badge--variant-soft'));
  assert(bg.classList.contains('tf-inline-badge--tone-success'));
});

test('RadioCardGroup option with description', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')],
    [1, [rcOpt('a', 'Free', { description: { kind: 'literal', value: '0 zł/mc' } })]],
    [2, 1], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
  assertEq(el.querySelector('.tf-radio-card-group__description').textContent, '0 zł/mc');
  // Icon and body land inside the card label as direct children.
  const card = el.querySelector('.tf-radio-card-group__card');
  assert(card.querySelector(':scope > .tf-radio-card-group__icon') != null);
  assert(card.querySelector(':scope > .tf-radio-card-group__body') != null);
});

test('RadioCardGroup missing a11y.label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A')]],
    [2, 1], [3, 'default'],
  ])));
});

test('RadioCardGroup columns=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A')]],
    [2, 0], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
});

test('RadioCardGroup invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A')]],
    [2, 1], [3, 'super_fancy'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
});

test('RadioCardGroup duplicate option values throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A'), rcOpt('a', 'A2')]],
    [2, 2], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
});

test('RadioCardGroup empty options throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, []], [2, 2], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
});

test('RadioCardGroup disabled option does not emit change', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A', { disabled: true })]],
    [2, 1], [3, 'default'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const card = el.querySelector('.tf-radio-card-group__card');
  assert(card.classList.contains('tf-radio-card-group__card--disabled'));
  card.click();
  assertEq(got, null);
});

test('RadioCardGroup unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_CARD_GROUP_TAG, [
    [0, PATH('r')], [1, [rcOpt('a', 'A')]],
    [2, 1], [3, 'default'], [99, 'x'],
  ], { a11y: { label: { kind: 'literal', value: 'X' } } })));
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
