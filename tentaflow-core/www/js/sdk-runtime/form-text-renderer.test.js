// =============================================================================
// File: sdk-runtime/form-text-renderer.test.js
// Description: Tests for Input (0x0301) / Textarea (0x0302) rendered through
// the <tf-input> / <tf-textarea> web components. Components are imported so
// happy-dom upgrades them on mount; tf-input needs MutationObserver bridged
// before import. The renderer intercepts raw events and re-emits synthetic
// `__tfReemit` events with the SDK { value, kind } payload.
// =============================================================================

import './_dom-test-harness.js';
import { window as domWindow } from './_dom-test-harness.js';
if (!globalThis.MutationObserver) globalThis.MutationObserver = domWindow.MutationObserver;
import '../components/tf-input.js';
import '../components/tf-textarea.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { INPUT_TAG, TEXTAREA_TAG } from './form-text-renderer.js';

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
// tf-input/tf-textarea build their light DOM in connectedCallback, so tests
// that touch the inner native control mount the rendered element first.
function mount(el) {
  document.body.appendChild(el);
  return el;
}

/// Helper: minimal Input. Requires type+bind_path+validators(=[])+size.
function inputFields({ type = 'text', path = PATH('q'), validators = [], size = 'md', ...rest } = {}) {
  const f = [[0, type], [1, path], [9, validators], [18, size]];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}
function textareaFields({ path = PATH('body'), validators = [], size = 'md', rows, autoresize = false, monospace = false, ...rest } = {}) {
  const f = [[0, path], [4, validators], [10, size], [12, autoresize], [14, monospace]];
  if (rows != null) f.push([11, rows]);
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}

// ============================================================================
// Input (0x0301)
// ============================================================================

test('Input renders <tf-input> with type attr + store value on inner input', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'hello' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    type: 'email',
    3: { kind: 'literal', value: 'Email' },
  }))));
  assertEq(el.tagName.toLowerCase(), 'tf-input');
  assert(el.classList.contains('tf-input--size-md'));
  assert(el.classList.contains('tf-input--type-email'));
  assertEq(el.getAttribute('type'), 'email');
  assertEq(el.getAttribute('label'), 'Email');
  const input = el.querySelector('input');
  assertEq(input.type, 'email');
  assertEq(input.value, 'hello');
  assertEq(el.querySelector('.tf-label').textContent, 'Email');
});

test('Input phone maps to HTML type=tel', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    type: 'phone',
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.getAttribute('type'), 'tel');
  assertEq(el.querySelector('input').type, 'tel');
});

test('Input reacts to store push (one-way read)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: '' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  assertEq(input.value, '');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'world' } }],
  });
  assertEq(input.value, 'world');
});

test('Input typing re-emits input+change with SDK { value, kind: tstr }', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  const evs = [];
  el.addEventListener('input', (e) => evs.push(['input', e.detail]));
  el.addEventListener('change', (e) => evs.push(['change', e.detail]));
  input.value = 'abc';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(evs, [
    ['input', { value: 'abc', kind: 'tstr' }],
    ['change', { value: 'abc', kind: 'tstr' }],
  ]);
});

test('Input bubbled native events do not duplicate SDK input/change', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  const evs = [];
  el.addEventListener('input', (e) => evs.push(['input', e.detail]));
  el.addEventListener('change', (e) => evs.push(['change', e.detail]));
  // Real browsers bubble the inner native control's events to the host
  // ALONGSIDE the component CustomEvent — each action must still produce
  // exactly one SDK event, and the raw event must never carry through.
  input.value = 'abc';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(evs, [
    ['input', { value: 'abc', kind: 'tstr' }],
    ['change', { value: 'abc', kind: 'tstr' }],
  ]);
});

test('Input focusin/focusout re-emit exactly one SDK focus/blur each', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  const evs = [];
  el.addEventListener('focus', (e) => evs.push(['focus', e.detail]));
  el.addEventListener('blur', (e) => evs.push(['blur', e.detail]));
  // Native focus/blur don't bubble — the renderer listens to focusin/focusout
  // and re-emits SDK focus/blur. Pre-fix the re-emit re-entered its own
  // listener (same event name on the same host) and recursed; one bubbled
  // focusin/focusout must complete with exactly one SDK event.
  input.dispatchEvent(new (globalThis.Event)('focusin', { bubbles: true }));
  input.dispatchEvent(new (globalThis.Event)('focusout', { bubbles: true }));
  assertEq(evs, [['focus', null], ['blur', null]]);
});

test('Input disabled BindRef blocks input/change events', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    15: { kind: 'bound', path: PATH('lock') },
  }))));
  assertEq(el.hasAttribute('disabled'), true);
  const input = el.querySelector('input');
  assertEq(input.disabled, true);
  let got = null;
  el.addEventListener('input', (e) => { got = e.detail; });
  input.value = 'x';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  assertEq(got, null);
});

test('Input readonly BindRef blocks events but renders value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'X' }, { path: PATH('ro'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    16: { kind: 'bound', path: PATH('ro') },
  }))));
  assertEq(el.hasAttribute('readonly'), true);
  assertEq(el.querySelector('input').value, 'X');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('input').dispatchEvent(
    new (globalThis.Event)('change', { bubbles: false })
  );
  assertEq(got, null);
});

test('Input error BindRef sets error attr + renders .tf-error-text', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('err'), value: 'Wymagane' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    17: { kind: 'bound', path: PATH('err') },
  }))));
  assertEq(el.getAttribute('error'), 'Wymagane');
  assertEq(el.querySelector('.tf-error-text').textContent, 'Wymagane');
  assert(el.querySelector('input').classList.contains('tf-input-error'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('err'), op: { kind: 'set', value: '' } }],
  });
  assertEq(el.hasAttribute('error'), false);
  assertEq(el.querySelector('.tf-error-text').textContent, '');
  assert(!el.querySelector('input').classList.contains('tf-input-error'));
});

test('Input passes maxlength/minlength/pattern/autocomplete/inputmode to inner input', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    10: 64, 11: 2, 12: '^[a-z]+$',
    13: 'current_password', 14: 'numeric',
  }))));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('maxlength'), '64');
  assertEq(input.getAttribute('minlength'), '2');
  assertEq(input.getAttribute('pattern'), '^[a-z]+$');
  assertEq(input.getAttribute('autocomplete'), 'current-password');
  assertEq(input.getAttribute('inputmode'), 'numeric');
});

test('Input max_length accepts BigInt u16 and rejects out-of-range', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    10: 64n,
  }))));
  assertEq(el.getAttribute('maxlength'), '64');
  assertThrows(() => engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    10: 0x10000n,
  }))));
});

test('Input with ValidationRule::Required sets required on inner input', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    validators: [{ kind: 'required' }],
  }))));
  assertEq(el.hasAttribute('required'), true);
  assertEq(el.querySelector('input').hasAttribute('required'), true);
});

test('Input leading_icon sets icon attr and inner icon hook', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    5: { kind: 'named', name: 'search' },
  }))));
  assertEq(el.getAttribute('icon'), 'search');
  assert(el.querySelector('.tf-input-wrap').classList.contains('tf-input-wrap-has-icon'));
  assertEq(el.querySelector('use').getAttribute('href'), '#i-search');
});

test('Input trailing_icon sets trailing-icon attr + inner trailing icon hook', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    6: { kind: 'named', name: 'check' },
  }))));
  assertEq(el.getAttribute('trailing-icon'), 'check');
  const trailing = el.querySelector('.tf-input-icon-trailing');
  assert(trailing != null, 'trailing icon element must exist');
  assertEq(trailing.style.display, '');
  assertEq(trailing.querySelector('use').getAttribute('href'), '#i-check');
  assert(el.querySelector('.tf-input-wrap').classList.contains('tf-input-wrap-has-trailing-icon'));
});

test('Input leading + trailing icons coexist', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    5: { kind: 'named', name: 'search' },
    6: { kind: 'named', name: 'close' },
  }))));
  assertEq(el.getAttribute('icon'), 'search');
  assertEq(el.getAttribute('trailing-icon'), 'close');
  assertEq(el.querySelector('.tf-input-icon:not(.tf-input-icon-trailing)').querySelector('use').getAttribute('href'), '#i-search');
  assertEq(el.querySelector('.tf-input-icon-trailing').querySelector('use').getAttribute('href'), '#i-close');
});

test('Input prefix + suffix adornments render reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('q'), value: '' },
      { path: PATH('sfx'), value: 'kg' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'literal', value: '$' },
    8: { kind: 'bound', path: PATH('sfx') },
  }))));
  const prefix = el.querySelector('.tf-input-prefix');
  const suffix = el.querySelector('.tf-input-suffix');
  assert(prefix != null && suffix != null, 'prefix/suffix elements must exist');
  assertEq(el.getAttribute('prefix'), '$');
  assertEq(prefix.textContent, '$');
  assertEq(prefix.style.display, '');
  assertEq(suffix.textContent, 'kg');
  assertEq(suffix.style.display, '');
  assert(el.querySelector('.tf-input-wrap').classList.contains('tf-input-wrap-has-prefix'));
  assert(el.querySelector('.tf-input-wrap').classList.contains('tf-input-wrap-has-suffix'));
  // Suffix is reactive — a store push updates the adornment text.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sfx'), op: { kind: 'set', value: 'lb' } }],
  });
  assertEq(suffix.textContent, 'lb');
});

test('Input without prefix/suffix/trailing-icon keeps adornments hidden', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.hasAttribute('prefix'), false);
  assertEq(el.hasAttribute('suffix'), false);
  assertEq(el.hasAttribute('trailing-icon'), false);
  assertEq(el.querySelector('.tf-input-prefix').style.display, 'none');
  assertEq(el.querySelector('.tf-input-suffix').style.display, 'none');
  assertEq(el.querySelector('.tf-input-icon-trailing').style.display, 'none');
});

test('Input placeholder + hint attrs are reactive', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('ph'), value: 'Szukaj...' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    2: { kind: 'bound', path: PATH('ph') },
    4: { kind: 'literal', value: 'pomocniczy' },
  }))));
  assertEq(el.getAttribute('placeholder'), 'Szukaj...');
  assertEq(el.querySelector('input').placeholder, 'Szukaj...');
  assertEq(el.getAttribute('hint'), 'pomocniczy');
  assertEq(el.querySelector('.tf-hint').textContent, 'pomocniczy');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('ph'), op: { kind: 'set', value: 'Search...' } }],
  });
  assertEq(el.querySelector('input').placeholder, 'Search...');
});

test('Input without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(INPUT_TAG, inputFields({}))));
});

test('Input without label accepts a11y.label (reactive aria-label mirror)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('lbl'), value: 'Wyszukaj' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({}), {
    a11y: { label: { kind: 'bound', path: PATH('lbl') } },
  }));
  assertEq(el.getAttribute('aria-label'), 'Wyszukaj');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'Search' } }],
  });
  assertEq(el.getAttribute('aria-label'), 'Search');
});

test('Input variant "ghost" adds class, default is outlined, bad value throws', () => {
  setup();
  const engine = makeEngine();
  const ghost = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    19: 'ghost',
  }))));
  assert(ghost.classList.contains('tf-input--variant-ghost'));

  const plain = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  assert(plain.classList.contains('tf-input--variant-outlined'));

  assertThrows(() => engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    19: 'framed',
  }))));
});

test('Input unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(INPUT_TAG, [
    [0, 'text'], [1, PATH('q')], [9, []], [18, 'md'],
    [3, { kind: 'literal', value: 'L' }],
    [99, 'oops'],
  ])));
});

test('Input invalid InputType throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(INPUT_TAG, inputFields({
    type: 'invalid',
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Input invalid InputSize throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(INPUT_TAG, inputFields({
    size: 'xl',
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Input invalid validators (non-array) throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(INPUT_TAG, [
    [0, 'text'], [1, PATH('q')], [9, 'not-an-array'], [18, 'md'],
    [3, { kind: 'literal', value: 'L' }],
  ])));
});

test('Input Enter on host re-emits submit + preventDefault', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  let submit = null;
  el.addEventListener('submit', (e) => { submit = e.detail; });
  el.value = 'query';
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key: 'Enter', bubbles: false, cancelable: true,
  });
  el.dispatchEvent(ev);
  assertEq(submit, { value: 'query', kind: 'tstr' });
  assertEq(ev.defaultPrevented, true);
});

test('Input Enter with Shift does NOT submit', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  let submit = null;
  el.addEventListener('submit', (e) => { submit = e.detail; });
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key: 'Enter', shiftKey: true, bubbles: false, cancelable: true,
  });
  el.dispatchEvent(ev);
  assertEq(submit, null);
  assertEq(ev.defaultPrevented, false);
});

test('Input readonly Enter preventDefaults but does NOT submit', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'X' }, { path: PATH('ro'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    16: { kind: 'bound', path: PATH('ro') },
  }))));
  let submit = null;
  el.addEventListener('submit', (e) => { submit = e.detail; });
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key: 'Enter', bubbles: false, cancelable: true,
  });
  el.dispatchEvent(ev);
  assertEq(submit, null);
  // preventDefault MUST run regardless of muted state — a native parent form
  // would otherwise submit outside host control.
  assertEq(ev.defaultPrevented, true);
});

test('Input disabled suppresses renderer focus/blur re-emission', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    15: { kind: 'bound', path: PATH('lock') },
  }))));
  // Muted renderer must NOT re-emit SDK focus/blur for the bubbled
  // focusin/focusout edges of a disabled control.
  const evs = [];
  el.addEventListener('focus', () => evs.push('focus'));
  el.addEventListener('blur', () => evs.push('blur'));
  const input = el.querySelector('input');
  input.dispatchEvent(new (globalThis.Event)('focusin', { bubbles: true }));
  input.dispatchEvent(new (globalThis.Event)('focusout', { bubbles: true }));
  assertEq(evs, []);
});

test('Input destroy unbinds store subscription + listeners', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  engine.destroy(el);
  // After destroy a store update must no longer reach the inner input.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'CHANGED' } }],
  });
  assertEq(input.value, 'a');
});

// ============================================================================
// Textarea (0x0302)
// ============================================================================

test('Textarea renders <tf-textarea> with rows default=3', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('body'), value: 'hi' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'Body' },
  }))));
  assertEq(el.tagName.toLowerCase(), 'tf-textarea');
  assert(el.classList.contains('tf-textarea--size-md'));
  assertEq(el.getAttribute('rows'), '3');
  assertEq(el.getAttribute('label'), 'Body');
  const ta = el.querySelector('textarea');
  assertEq(ta.value, 'hi');
  assertEq(Number(ta.rows), 3);
});

test('Textarea explicit rows respected', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    rows: 8, 2: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.getAttribute('rows'), '8');
  assertEq(Number(el.querySelector('textarea').rows), 8);
});

test('Textarea typing re-emits input+change with SDK payload', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  }))));
  const ta = el.querySelector('textarea');
  const evs = [];
  el.addEventListener('input', (e) => evs.push(['input', e.detail]));
  el.addEventListener('change', (e) => evs.push(['change', e.detail]));
  ta.value = 'multi\nline';
  ta.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  ta.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(evs, [
    ['input', { value: 'multi\nline', kind: 'tstr' }],
    ['change', { value: 'multi\nline', kind: 'tstr' }],
  ]);
});

test('Textarea bubbled native events do not duplicate SDK input/change', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  }))));
  const ta = el.querySelector('textarea');
  const evs = [];
  el.addEventListener('input', (e) => evs.push(['input', e.detail]));
  el.addEventListener('change', (e) => evs.push(['change', e.detail]));
  // Real-browser bubbling: native event reaches the host alongside the
  // component CustomEvent — exactly one SDK event per action.
  ta.value = 'xyz';
  ta.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  ta.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(evs, [
    ['input', { value: 'xyz', kind: 'tstr' }],
    ['change', { value: 'xyz', kind: 'tstr' }],
  ]);
});

test('Textarea focusin/focusout re-emit exactly one SDK focus/blur each', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  }))));
  const ta = el.querySelector('textarea');
  const evs = [];
  el.addEventListener('focus', (e) => evs.push(['focus', e.detail]));
  el.addEventListener('blur', (e) => evs.push(['blur', e.detail]));
  ta.dispatchEvent(new (globalThis.Event)('focusin', { bubbles: true }));
  ta.dispatchEvent(new (globalThis.Event)('focusout', { bubbles: true }));
  assertEq(evs, [['focus', null], ['blur', null]]);
});

test('Textarea monospace=true sets class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    monospace: true, 2: { kind: 'literal', value: 'L' },
  })));
  assert(el.classList.contains('tf-textarea--monospace'));
});

test('Textarea autoresize=true sets autogrow attr', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    autoresize: true, 2: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.hasAttribute('autogrow'), true);
  // Typing through the component autogrow path must not throw.
  const ta = el.querySelector('textarea');
  ta.value = 'aaa\nbbb\nccc\nddd';
  ta.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
});

test('Textarea maxlength passes through to inner textarea', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    5: 500,
  }))));
  assertEq(el.getAttribute('maxlength'), '500');
  assertEq(el.querySelector('textarea').getAttribute('maxlength'), '500');
});

test('Textarea max_rows < rows throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXTAREA_TAG, textareaFields({
    rows: 4, 13: 2, 2: { kind: 'literal', value: 'L' },
  }))));
});

test('Textarea rows=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXTAREA_TAG, textareaFields({
    rows: 0, 2: { kind: 'literal', value: 'L' },
  }))));
});

test('Textarea disabled BindRef blocks events', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('body'), value: 'x' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  assertEq(el.hasAttribute('disabled'), true);
  const ta = el.querySelector('textarea');
  assertEq(ta.disabled, true);
  let got = null;
  el.addEventListener('input', (e) => { got = e.detail; });
  ta.value = 'y';
  ta.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  assertEq(got, null);
});

test('Textarea error BindRef sets error attr + .tf-error-text', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('body'), value: '' }, { path: PATH('err'), value: 'Za krótkie' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    9: { kind: 'bound', path: PATH('err') },
  }))));
  assertEq(el.getAttribute('error'), 'Za krótkie');
  assertEq(el.querySelector('.tf-error-text').textContent, 'Za krótkie');
  assert(el.querySelector('textarea').classList.contains('tf-input-error'));
});

test('Textarea without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXTAREA_TAG, textareaFields({}))));
});

test('Textarea reactive value sync from store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('body'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  }))));
  const ta = el.querySelector('textarea');
  assertEq(ta.value, 'a');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('body'), op: { kind: 'set', value: 'b' } }],
  });
  assertEq(ta.value, 'b');
});

test('Textarea without label sets aria-label from a11y.label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('body'), value: '' }, { path: PATH('lbl'), value: 'Opis' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({}), {
    a11y: { label: { kind: 'bound', path: PATH('lbl') } },
  }));
  assertEq(el.getAttribute('aria-label'), 'Opis');
});

test('Textarea disabled suppresses renderer focus/blur re-emission', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('body'), value: '' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  // Muted renderer must NOT re-emit SDK focus/blur for the bubbled
  // focusin/focusout edges of a disabled control.
  const evs = [];
  el.addEventListener('focus', () => evs.push('focus'));
  el.addEventListener('blur', () => evs.push('blur'));
  const ta = el.querySelector('textarea');
  ta.dispatchEvent(new (globalThis.Event)('focusin', { bubbles: true }));
  ta.dispatchEvent(new (globalThis.Event)('focusout', { bubbles: true }));
  assertEq(evs, []);
});

test('Textarea destroy unbinds store subscription', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('body'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  }))));
  const ta = el.querySelector('textarea');
  engine.destroy(el);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('body'), op: { kind: 'set', value: 'CHANGED' } }],
  });
  assertEq(ta.value, 'a');
});

test('Textarea variant "ghost" adds class, default is outlined, bad value throws', () => {
  setup();
  const engine = makeEngine();
  const ghost = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    15: 'ghost',
  }))));
  assert(ghost.classList.contains('tf-textarea--variant-ghost'));

  const plain = mount(engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  }))));
  assert(plain.classList.contains('tf-textarea--variant-outlined'));

  assertThrows(() => engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    15: 'framed',
  }))));
});

test('Textarea unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXTAREA_TAG, [
    [0, PATH('body')], [4, []], [10, 'md'], [12, false], [14, false],
    [2, { kind: 'literal', value: 'L' }],
    [88, 'x'],
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
