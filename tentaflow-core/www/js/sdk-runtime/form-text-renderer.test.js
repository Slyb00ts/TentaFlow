// =============================================================================
// Plik: sdk-runtime/form-text-renderer.test.js
// Opis: Testy Input/Textarea — chunk 3.3c-2.
// =============================================================================

import './_dom-test-harness.js';
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

/// Helper: minimalny Input bez optional pól. Wymaga type+bind_path+
/// validators(=[])+size; reszta opcjonalna.
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

test('Input renderuje <input> z type+aktualną wartością ze store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'hello' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    type: 'email', label: undefined,
    3: { kind: 'literal', value: 'Email' },
  })));
  const input = el.querySelector('input');
  assertEq(input.tagName, 'INPUT');
  assertEq(input.getAttribute('type'), 'email');
  assertEq(input.value, 'hello');
  // <label> for-attr powiązany z input.id.
  const label = el.querySelector('label');
  assertEq(label.getAttribute('for'), input.getAttribute('id'));
});

test('Input phone mapuje na HTML type=tel', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({
    type: 'phone',
    3: { kind: 'literal', value: 'L' },
  })));
  assertEq(el.querySelector('input').getAttribute('type'), 'tel');
});

test('Input reaguje na store push (one-way read)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: '' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  })));
  const input = el.querySelector('input');
  assertEq(input.value, '');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'world' } }],
  });
  assertEq(input.value, 'world');
});

test('Input typing dispatchuje input+change z aktualnym tekstem', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  })));
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

test('Input disabled BindRef blokuje input/change events', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    15: { kind: 'bound', path: PATH('lock') },
  })));
  const input = el.querySelector('input');
  assertEq(input.hasAttribute('disabled'), true);
  let got = null;
  el.addEventListener('input', (e) => { got = e.detail; });
  input.value = 'x';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  assertEq(got, null);
});

test('Input readonly BindRef blokuje events ale renderuje value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'X' }, { path: PATH('ro'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    16: { kind: 'bound', path: PATH('ro') },
  })));
  const input = el.querySelector('input');
  assertEq(input.value, 'X');
  assertEq(input.hasAttribute('readonly'), true);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, null);
});

test('Input error BindRef ustawia aria-invalid + renderuje .tf-input__error', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('err'), value: 'Wymagane' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    17: { kind: 'bound', path: PATH('err') },
  })));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('aria-invalid'), 'true');
  const err = el.querySelector('.tf-input__error');
  assertEq(err.textContent, 'Wymagane');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('err'), op: { kind: 'set', value: '' } }],
  });
  assertEq(input.hasAttribute('aria-invalid'), false);
  assertEq(el.querySelector('.tf-input__error'), null);
});

test('Input ustawia HTML attrs: maxlength, minlength, pattern, autocomplete, inputmode', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    10: 64, 11: 2, 12: '^[a-z]+$',
    13: 'current_password', 14: 'numeric',
  })));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('maxlength'), '64');
  assertEq(input.getAttribute('minlength'), '2');
  assertEq(input.getAttribute('pattern'), '^[a-z]+$');
  assertEq(input.getAttribute('autocomplete'), 'current-password');
  assertEq(input.getAttribute('inputmode'), 'numeric');
});

test('Input z ValidationRule::Required ustawia required+aria-required', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    validators: [{ kind: 'required' }],
  })));
  const input = el.querySelector('input');
  assertEq(input.hasAttribute('required'), true);
  assertEq(input.getAttribute('aria-required'), 'true');
});

test('Input leading_icon + trailing_icon renderowane jako .tf-input__icon', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    5: { name: 'search' }, 6: { name: 'x' },
  })));
  const lead = el.querySelector('.tf-input__icon--leading');
  const trail = el.querySelector('.tf-input__icon--trailing');
  assertEq(lead.getAttribute('data-icon-name'), 'search');
  assertEq(trail.getAttribute('data-icon-name'), 'x');
});

test('Input prefix/suffix renderowane jako reactive .tf-input__affix', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('px'), value: '$' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('px') },
    8: { kind: 'literal', value: 'USD' },
  })));
  const px = el.querySelector('.tf-input__affix--prefix');
  const sx = el.querySelector('.tf-input__affix--suffix');
  assertEq(px.textContent, '$');
  assertEq(sx.textContent, 'USD');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('px'), op: { kind: 'set', value: '€' } }],
  });
  assertEq(px.textContent, '€');
});

test('Input bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(INPUT_TAG, inputFields({}))));
});

test('Input bez label akceptuje a11y.label', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({}), {
    a11y: { label: { kind: 'literal', value: 'Search' } },
  }));
  assertEq(el.querySelector('input').tagName, 'INPUT');
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

test('Input bez label ustawia aria-label na <input> z a11y.label (mirror)', () => {
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
  const input = el.querySelector('input');
  assertEq(input.getAttribute('aria-label'), 'Wyszukaj');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'Search' } }],
  });
  assertEq(input.getAttribute('aria-label'), 'Search');
});

test('Input Enter dispatchuje submit + preventDefault na keydown', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  })));
  const input = el.querySelector('input');
  let submit = null;
  el.addEventListener('submit', (e) => { submit = e.detail; });
  input.value = 'query';
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key: 'Enter', bubbles: false, cancelable: true,
  });
  input.dispatchEvent(ev);
  assertEq(submit, { value: 'query', kind: 'tstr' });
  assertEq(ev.defaultPrevented, true);
});

test('Input Enter z Shift NIE submit', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  })));
  const input = el.querySelector('input');
  let submit = null;
  el.addEventListener('submit', (e) => { submit = e.detail; });
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key: 'Enter', shiftKey: true, bubbles: false, cancelable: true,
  });
  input.dispatchEvent(ev);
  assertEq(submit, null);
  assertEq(ev.defaultPrevented, false);
});

test('Input readonly Enter robi preventDefault ale NIE dispatchuje submit', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'X' }, { path: PATH('ro'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    16: { kind: 'bound', path: PATH('ro') },
  })));
  const input = el.querySelector('input');
  let submit = null;
  el.addEventListener('submit', (e) => { submit = e.detail; });
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key: 'Enter', bubbles: false, cancelable: true,
  });
  input.dispatchEvent(ev);
  assertEq(submit, null);
  // preventDefault MUSI być wywołany niezależnie od muted, bo natywny
  // form parent inaczej submittnie się bez kontroli host'a.
  assertEq(ev.defaultPrevented, true);
});

test('Input disabled tłumi focus/blur events renderera', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
    15: { kind: 'bound', path: PATH('lock') },
  })));
  const input = el.querySelector('input');
  const evs = [];
  el.addEventListener('focus', () => evs.push('focus'));
  el.addEventListener('blur', () => evs.push('blur'));
  input.dispatchEvent(new (globalThis.Event)('focus', { bubbles: false }));
  input.dispatchEvent(new (globalThis.Event)('blur', { bubbles: false }));
  assertEq(evs, []);
});

test('Input destroy odpina subskrypcję store + listenery', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(INPUT_TAG, inputFields({
    3: { kind: 'literal', value: 'L' },
  })));
  const input = el.querySelector('input');
  engine.destroy(el);
  // Po destroy subskrypcja powinna być zwolniona — store update nie zmienia
  // value wewnątrz input'a.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'CHANGED' } }],
  });
  assertEq(input.value, 'a');
});

// ============================================================================
// Textarea (0x0302)
// ============================================================================

test('Textarea renderuje <textarea> z rows default=3', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('body'), value: 'hi' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'Body' },
  })));
  const ta = el.querySelector('textarea');
  assertEq(ta.tagName, 'TEXTAREA');
  assertEq(ta.value, 'hi');
  assertEq(ta.getAttribute('rows'), '3');
});

test('Textarea explicit rows respektowany', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    rows: 8, 2: { kind: 'literal', value: 'L' },
  })));
  assertEq(el.querySelector('textarea').getAttribute('rows'), '8');
});

test('Textarea typing dispatchuje input+change', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  })));
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

test('Textarea monospace=true ustawia klasę', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    monospace: true, 2: { kind: 'literal', value: 'L' },
  })));
  assert(el.classList.contains('tf-textarea--monospace'));
});

test('Textarea autoresize=true ustawia klasę + odpala layout po typing', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    autoresize: true, 2: { kind: 'literal', value: 'L' },
  })));
  assert(el.classList.contains('tf-textarea--autoresize'));
  // Sam fakt nie throw'a + listener wpięty — sprawdzenie obecności
  // przez dispatch event'u.
  const ta = el.querySelector('textarea');
  ta.value = 'aaa\nbbb\nccc\nddd';
  ta.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
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

test('Textarea disabled BindRef blokuje events', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('body'), value: 'x' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const ta = el.querySelector('textarea');
  assertEq(ta.hasAttribute('disabled'), true);
  let got = null;
  el.addEventListener('input', (e) => { got = e.detail; });
  ta.value = 'y';
  ta.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  assertEq(got, null);
});

test('Textarea error BindRef ustawia aria-invalid + .tf-input__error', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('body'), value: '' }, { path: PATH('err'), value: 'Za krótkie' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    9: { kind: 'bound', path: PATH('err') },
  })));
  const ta = el.querySelector('textarea');
  assertEq(ta.getAttribute('aria-invalid'), 'true');
  assertEq(el.querySelector('.tf-input__error').textContent, 'Za krótkie');
});

test('Textarea bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXTAREA_TAG, textareaFields({}))));
});

test('Textarea reactive value sync ze store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('body'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
  })));
  const ta = el.querySelector('textarea');
  assertEq(ta.value, 'a');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('body'), op: { kind: 'set', value: 'b' } }],
  });
  assertEq(ta.value, 'b');
});

test('Textarea bez label ustawia aria-label na <textarea> z a11y.label', () => {
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
  assertEq(el.querySelector('textarea').getAttribute('aria-label'), 'Opis');
});

test('Textarea disabled tłumi focus/blur', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('body'), value: '' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    2: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const ta = el.querySelector('textarea');
  const evs = [];
  el.addEventListener('focus', () => evs.push('focus'));
  el.addEventListener('blur', () => evs.push('blur'));
  ta.dispatchEvent(new (globalThis.Event)('focus', { bubbles: false }));
  ta.dispatchEvent(new (globalThis.Event)('blur', { bubbles: false }));
  assertEq(evs, []);
});

test('Textarea destroy odpina autoresize listener', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXTAREA_TAG, textareaFields({
    autoresize: true, 2: { kind: 'literal', value: 'L' },
  })));
  const ta = el.querySelector('textarea');
  engine.destroy(el);
  // Po destroy zmiana wartości + input event nie ma już żadnego subskrybenta;
  // wywołanie nie powinno rzucić ani zmienić style.height z poziomu hook'a.
  ta.value = 'long\ncontent\nhere';
  ta.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  // Brak crashu = pass. (Layout sam w sobie nie jest weryfikowalny w
  // happy-dom — pilnujemy że cleanup nie zostawia dangling subskrypcji.)
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
