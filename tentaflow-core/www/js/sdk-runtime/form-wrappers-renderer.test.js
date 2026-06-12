// =============================================================================
// File: sdk-runtime/form-wrappers-renderer.test.js
// Description: Tests for FormField/FormGroup/FormSection/Form. Children render
// through the tf-input web component, so it is imported for happy-dom upgrade.
// =============================================================================

import './_dom-test-harness.js';
import { window as domWindow } from './_dom-test-harness.js';
// tf-input observes child mutations; the harness does not export the
// observer, so it is bridged here before the component is imported.
if (!globalThis.MutationObserver) globalThis.MutationObserver = domWindow.MutationObserver;
import '../components/tf-input.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  FORM_FIELD_TAG, FORM_GROUP_TAG, FORM_SECTION_TAG, FORM_TAG,
} from './form-wrappers-renderer.js';
import { INPUT_TAG } from './form-text-renderer.js';

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
// tf-input builds its light DOM in connectedCallback, so tests that touch the
// inner native <input> mount the rendered tree first.
function mount(el) {
  document.body.appendChild(el);
  return el;
}

/// Minimal Input used as a child component.
function dummyInput(id = 'child') {
  return comp(INPUT_TAG, [
    [0, 'text'], [1, PATH('q')], [9, []], [18, 'md'],
    [3, { kind: 'literal', value: 'Lbl' }],
  ], { id });
}

// ============================================================================
// FormField
// ============================================================================

test('FormField renderuje label + child + aria-labelledby', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_FIELD_TAG, [
    [0, { kind: 'literal', value: 'Imię' }],
    [3, false],
    [4, dummyInput('inp')],
    [5, 'stacked'],
  ]));
  const lbl = el.querySelector('.tf-form-field__label');
  const child = el.querySelector('.tf-form-field__child');
  assertEq(lbl.textContent, 'Imię');
  assertEq(child.getAttribute('aria-labelledby'), lbl.id);
});

test('FormField required dodaje aria-required na child + star markę', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_FIELD_TAG, [
    [0, { kind: 'literal', value: 'X' }],
    [3, true], [4, dummyInput('i')], [5, 'stacked'],
  ]));
  const child = el.querySelector('.tf-form-field__child');
  assertEq(child.getAttribute('aria-required'), 'true');
  assert(el.querySelector('.tf-form-field__required-mark') != null);
});

test('FormField hint renderuje + dodaje aria-describedby', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_FIELD_TAG, [
    [0, { kind: 'literal', value: 'X' }],
    [1, { kind: 'literal', value: 'pomoc' }],
    [3, false], [4, dummyInput('i')], [5, 'stacked'],
  ]));
  const hint = el.querySelector('.tf-form-field__hint');
  assertEq(hint.textContent, 'pomoc');
  const child = el.querySelector('.tf-form-field__child');
  assertEq(child.getAttribute('aria-describedby'), hint.id);
});

test('FormField reactive error BindRef set i clear', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('err'), value: 'Wymagane' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(FORM_FIELD_TAG, [
    [0, { kind: 'literal', value: 'X' }],
    [2, { kind: 'bound', path: PATH('err') }],
    [3, false], [4, dummyInput('i')], [5, 'stacked'],
  ]));
  const err = el.querySelector('.tf-form-field__error');
  assertEq(err.textContent, 'Wymagane');
  assertEq(err.hidden, false);
  assert(el.classList.contains('tf-form-field--invalid'));
  const child = el.querySelector('.tf-form-field__child');
  assertEq(child.getAttribute('aria-invalid'), 'true');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('err'), op: { kind: 'set', value: '' } }],
  });
  assertEq(err.hidden, true);
  assertEq(child.hasAttribute('aria-invalid'), false);
});

test('FormField bez label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_FIELD_TAG, [
    [3, false], [4, dummyInput('i')], [5, 'stacked'],
  ])));
});

test('FormField layout=horizontal ustawia klasę', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_FIELD_TAG, [
    [0, { kind: 'literal', value: 'X' }],
    [3, false], [4, dummyInput('i')], [5, 'horizontal'],
  ]));
  assert(el.classList.contains('tf-form-field--layout-horizontal'));
});

test('FormField error clear usuwa errId z aria-describedby (zostaje hint)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('err'), value: 'Bad' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(FORM_FIELD_TAG, [
    [0, { kind: 'literal', value: 'X' }],
    [1, { kind: 'literal', value: 'hint' }],
    [2, { kind: 'bound', path: PATH('err') }],
    [3, false], [4, dummyInput('i')], [5, 'stacked'],
  ]));
  const child = el.querySelector('.tf-form-field__child');
  const hintId = el.querySelector('.tf-form-field__hint').id;
  const errId = el.querySelector('.tf-form-field__error').id;
  const initial = child.getAttribute('aria-describedby');
  assert(initial.includes(hintId));
  assert(initial.includes(errId));
  // Clear error.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('err'), op: { kind: 'set', value: '' } }],
  });
  const after = child.getAttribute('aria-describedby');
  assertEq(after, hintId);  // tylko hint zostaje
});

test('FormGroup toggle event detail zawiera bind (expandedBind) dla write-back', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('exp'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const expBind = { kind: 'bound', path: PATH('exp') };
  const el = engine.render(comp(FORM_GROUP_TAG, [
    [0, { kind: 'literal', value: 'S' }],
    [2, true], [3, expBind],
    [4, [dummyInput('i')]], [5, 'sm'],
  ]));
  let got = null;
  el.addEventListener('toggle', (e) => { got = e.detail; });
  el.querySelector('.tf-form-group__toggle').click();
  assertEq(got.value, false);
  assertEq(got.kind, 'bool');
  assertEq(got.bind, expBind);
});

test('FormField unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_FIELD_TAG, [
    [0, { kind: 'literal', value: 'X' }],
    [3, false], [4, dummyInput('i')], [5, 'stacked'], [99, 'x'],
  ])));
});

// ============================================================================
// FormGroup
// ============================================================================

test('FormGroup renderuje children + spacing class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_GROUP_TAG, [
    [2, false], [4, [dummyInput('i1'), dummyInput('i2')]],
    [5, 'md'],
  ]));
  assertEq(el.querySelectorAll('tf-input').length, 2);
  assert(el.classList.contains('tf-form-group--spacing-md'));
});

test('FormGroup collapsible=true z toggle button + aria-expanded', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_GROUP_TAG, [
    [0, { kind: 'literal', value: 'Sekcja' }],
    [2, true], [4, [dummyInput('i1')]], [5, 'sm'],
  ]));
  const toggle = el.querySelector('.tf-form-group__toggle');
  assert(toggle != null);
  assertEq(toggle.getAttribute('aria-expanded'), 'true');
});

test('FormGroup collapsible bez expanded BindRef → local toggle', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_GROUP_TAG, [
    [0, { kind: 'literal', value: 'S' }],
    [2, true], [4, [dummyInput('i1')]], [5, 'sm'],
  ]));
  const toggle = el.querySelector('.tf-form-group__toggle');
  const body = el.querySelector('.tf-form-group__body');
  assertEq(body.hidden, false);
  toggle.click();
  assertEq(body.hidden, true);
  assertEq(toggle.getAttribute('aria-expanded'), 'false');
});

test('FormGroup collapsible z expanded BindRef → emit toggle event', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('exp'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(FORM_GROUP_TAG, [
    [0, { kind: 'literal', value: 'S' }],
    [2, true], [3, { kind: 'bound', path: PATH('exp') }],
    [4, [dummyInput('i1')]], [5, 'sm'],
  ]));
  let got = null;
  el.addEventListener('toggle', (e) => { got = e.detail; });
  el.querySelector('.tf-form-group__toggle').click();
  assertEq(got.value, false);
  assertEq(got.kind, 'bool');
});

test('FormGroup expanded BindRef bez collapsible throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_GROUP_TAG, [
    [2, false], [3, { kind: 'literal', value: true }],
    [4, [dummyInput('i')]], [5, 'sm'],
  ])));
});

test('FormGroup pusta children throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_GROUP_TAG, [
    [2, false], [4, []], [5, 'sm'],
  ])));
});

test('FormGroup unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_GROUP_TAG, [
    [2, false], [4, [dummyInput('i')]], [5, 'sm'], [99, 'x'],
  ])));
});

// ============================================================================
// FormSection
// ============================================================================

test('FormSection renderuje title + children + default spacing=lg + divider_top=true', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_SECTION_TAG, [
    [0, { kind: 'literal', value: 'Sekcja' }],
    [2, [dummyInput('i1')]],
  ]));
  assertEq(el.querySelector('.tf-form-section__title').textContent, 'Sekcja');
  assert(el.classList.contains('tf-form-section--spacing-lg'));
  assert(el.classList.contains('tf-form-section--divider-top'));
});

test('FormSection divider_top=false NIE ustawia klasy', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_SECTION_TAG, [
    [0, { kind: 'literal', value: 'S' }],
    [2, [dummyInput('i')]],
    [4, false],
  ]));
  assert(!el.classList.contains('tf-form-section--divider-top'));
});

test('FormSection bez title throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_SECTION_TAG, [
    [2, [dummyInput('i')]],
  ])));
});

test('FormSection pusta children throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_SECTION_TAG, [
    [0, { kind: 'literal', value: 'S' }],
    [2, []],
  ])));
});

test('FormSection unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_SECTION_TAG, [
    [0, { kind: 'literal', value: 'S' }],
    [2, [dummyInput('i')]], [99, 'x'],
  ])));
});

// ============================================================================
// Form
// ============================================================================

test('Form renderuje <form novalidate> z data-scope-id', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'login'], [2, []], [3, true], [4, 'stacked'],
  ]));
  assertEq(el.tagName, 'FORM');
  assertEq(el.hasAttribute('novalidate'), true);
  assertEq(el.getAttribute('data-scope-id'), 'login');
});

test('Form invalid scope_id (uppercase) throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'Login'], [2, []], [3, true], [4, 'stacked'],
  ])));
});

test('Form scope_id length > 64 throws', () => {
  setup();
  const engine = makeEngine();
  const longId = 'a'.repeat(65);
  assertThrows(() => engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, longId], [2, []], [3, true], [4, 'stacked'],
  ])));
});

test('Form submit emit submit_form z scope_id + validators', () => {
  setup();
  const engine = makeEngine();
  const v = { kind: 'all_required', field_ids: ['a', 'b'] };
  const el = engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'login'], [2, [v]], [3, true], [4, 'stacked'],
  ]));
  let got = null;
  el.addEventListener('submit_form', (e) => { got = e.detail; });
  el.dispatchEvent(new (globalThis.Event)('submit', { bubbles: false, cancelable: true }));
  assertEq(got, { scope_id: 'login', validators: ['all_required'] });
});

test('Form prevent_default_submit=true blokuje native submit', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, []], [3, true], [4, 'stacked'],
  ]));
  const ev = new (globalThis.Event)('submit', { bubbles: false, cancelable: true });
  el.dispatchEvent(ev);
  assertEq(ev.defaultPrevented, true);
});

test('Form reset emit reset_form', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, []], [3, true], [4, 'stacked'],
  ]));
  let got = null;
  el.addEventListener('reset_form', (e) => { got = e.detail; });
  el.dispatchEvent(new (globalThis.Event)('reset', { bubbles: false, cancelable: true }));
  assertEq(got, { scope_id: 'x' });
});

test('Form validator parsing: all_required + any_required + match + custom', () => {
  setup();
  const engine = makeEngine();
  const vs = [
    { kind: 'all_required', field_ids: ['a', 'b'] },
    { kind: 'any_required', field_ids: ['c'], error_message: { kind: 'literal', value: 'X' } },
    { kind: 'match', field_a: 'pw', field_b: 'pw2' },
    { kind: 'custom', id: 'strong_pw' },
  ];
  const el = engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'login'], [2, vs], [3, true], [4, 'stacked'],
  ]));
  assertEq(el.getAttribute('data-validators-count'), '4');
  assertEq(el.getAttribute('data-validator-0'), 'all_required');
  assertEq(el.getAttribute('data-validator-2'), 'match');
});

test('Form validator match z field_a==field_b throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, [{ kind: 'match', field_a: 'a', field_b: 'a' }]],
    [3, true], [4, 'stacked'],
  ])));
});

test('Form validator unknown kind throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, [{ kind: 'nope' }]],
    [3, true], [4, 'stacked'],
  ])));
});

test('Form pusta children throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_TAG, [
    [0, []], [1, 'x'], [2, []], [3, true], [4, 'stacked'],
  ])));
});

test('Form disabled BindRef ustawia aria-disabled + tf-input host', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('lock'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, []], [3, true], [4, 'stacked'],
    [5, { kind: 'bound', path: PATH('lock') }],
  ]));
  assertEq(el.getAttribute('aria-disabled'), 'true');
  assertEq(el.querySelector('tf-input').hasAttribute('disabled'), true);
});

test('Form disabled flip OFF zdejmuje disabled z tf-input + inner input', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('lock'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, []], [3, true], [4, 'stacked'],
    [5, { kind: 'bound', path: PATH('lock') }],
  ])));
  const host = el.querySelector('tf-input');
  assertEq(host.hasAttribute('disabled'), true);
  // Component reflects the host attribute onto its internal native input.
  assertEq(el.querySelector('input').disabled, true);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: false } }],
  });
  assertEq(host.hasAttribute('disabled'), false);
  assertEq(el.querySelector('input').disabled, false);
  assertEq(el.hasAttribute('aria-disabled'), false);
});

test('Form disabled blokuje submit_form emission', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('lock'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, []], [3, true], [4, 'stacked'],
    [5, { kind: 'bound', path: PATH('lock') }],
  ]));
  let got = null;
  el.addEventListener('submit_form', (e) => { got = e.detail; });
  el.dispatchEvent(new (globalThis.Event)('submit', { bubbles: false, cancelable: true }));
  assertEq(got, null);
});

test('Form unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FORM_TAG, [
    [0, [dummyInput('i')]], [1, 'x'], [2, []], [3, true], [4, 'stacked'], [99, 'x'],
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
