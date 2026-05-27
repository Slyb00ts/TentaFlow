// =============================================================================
// Plik: sdk-runtime/form-combobox-renderer.test.js
// Opis: Testy Combobox (0x0305) + Autocomplete (0x0306) — chunk 3.3c-3c.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { COMBOBOX_TAG, AUTOCOMPLETE_TAG } from './form-combobox-renderer.js';

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
function tstrOpt(v, lbl, opts = {}) {
  const f = [
    [0, { kind: 'tstr', value: v }],
    [1, { kind: 'literal', value: lbl }],
    [3, opts.disabled === true],
  ];
  if (opts.groupId != null) f.push([4, opts.groupId]);
  return f;
}
function cbFields({
  path = PATH('q'), options = [], clearable = false, virtualize = false,
  size = 'md', freeInput = false, minChars = 0, remoteSearch = false, ...rest
} = {}) {
  const f = [
    [0, path], [1, options], [4, true], [5, clearable], [6, virtualize],
    [8, size], [10, freeInput], [11, minChars], [12, remoteSearch],
  ];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}
function acFields({
  path = PATH('q'), actionId = 'do_search', minChars = 1, debounceMs = 100, ...rest
} = {}) {
  const f = [[0, path], [1, actionId], [3, minChars], [4, debounceMs]];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}
function keydown(el, key, mods = {}) {
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key, bubbles: false, cancelable: true, ...mods,
  });
  el.dispatchEvent(ev);
  return ev;
}

// ============================================================================
// Combobox (0x0305)
// ============================================================================

test('Combobox renderuje <input role=combobox> z aria-autocomplete=list', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    3: { kind: 'literal', value: 'F' },
  })));
  const input = el.querySelector('input');
  assertEq(input.tagName, 'INPUT');
  assertEq(input.getAttribute('role'), 'combobox');
  assertEq(input.getAttribute('aria-autocomplete'), 'list');
  assertEq(input.getAttribute('aria-expanded'), 'false');
});

test('Combobox searchable=false throws', () => {
  setup();
  const engine = makeEngine();
  // Manualnie wymuszamy searchable=false (override).
  const fields = cbFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  });
  // Znajdź klucz 4 i zamień na false.
  for (const e of fields) if (e[0] === 4) e[1] = false;
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, fields)));
});

test('Combobox pokazuje label aktualnie wybranej opcji w input.value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'b' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  })));
  assertEq(el.querySelector('input').value, 'Banana');
});

test('Combobox typing filtruje opcje + otwiera popover', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana'), tstrOpt('c', 'Cherry')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const input = el.querySelector('input');
  input.value = 'an';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  const popover = el.querySelector('.tf-combobox__popover');
  assertEq(popover.hidden, false);
  const opts = el.querySelectorAll('.tf-combobox__option');
  assertEq(opts[0].hidden, true);   // Apple
  assertEq(opts[1].hidden, false);  // Banana
  assertEq(opts[2].hidden, true);   // Cherry
});

test('Combobox min_search_chars gate — popover NIE otwiera się przed limit', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    minChars: 3,
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const input = el.querySelector('input');
  input.value = 'ap';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  assertEq(el.querySelector('.tf-combobox__popover').hidden, true);
  input.value = 'app';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  assertEq(el.querySelector('.tf-combobox__popover').hidden, false);
});

test('Combobox Enter na aktywnej opcji commituje + zamyka', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  keydown(input, 'ArrowDown');  // open + active=0
  keydown(input, 'ArrowDown');  // active=1
  keydown(input, 'Enter');
  assertEq(got, { value: 'b', kind: 'tstr' });
  assertEq(el.querySelector('.tf-combobox__popover').hidden, true);
});

test('Combobox free_input=true Enter z tekstem nie-z-opcji emituje change tstr', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    freeInput: true,
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  // Najpierw zamknij popover, żeby Enter nie wybrał aktywnej opcji.
  input.value = 'xyz';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  keydown(input, 'Escape');  // close
  keydown(input, 'Enter');
  assertEq(got, { value: 'xyz', kind: 'tstr' });
});

test('Combobox free_input=false Enter z raw text BEZ commit', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    freeInput: false,
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = 'xyz';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  keydown(input, 'Escape');
  keydown(input, 'Enter');
  assertEq(got, null);
});

test('Combobox remote_search=true wymaga remote_action_id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [],
    remoteSearch: true,
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Combobox remote_search emituje "search" event po debounce', (done) => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [],
    remoteSearch: true,
    minChars: 2,
    3: { kind: 'literal', value: 'L' },
    13: 'remote_query',
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('search', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = 'foo';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  // 300ms debounce + buffer
  setTimeout(() => {
    try {
      assertEq(got, { query: 'foo', action_id: 'remote_query' });
    } catch (e) {
      results.push({ name: 'Combobox remote_search emituje search [async]', ok: false, err: e });
      return;
    }
    results.push({ name: 'Combobox remote_search emituje search [async]', ok: true });
  }, 450);
});

test('Combobox typing → szybki backspace poniżej minChars anuluje remote search', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [],
    remoteSearch: true,
    minChars: 3,
    3: { kind: 'literal', value: 'L' },
    13: 'do_q',
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('search', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = 'foo';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  input.value = 'f';  // skasuj do 1 znaku przed debounce
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  setTimeout(() => {
    try {
      assertEq(got, null);  // search NIE powinien być wysłany
    } catch (e) {
      results.push({ name: 'Combobox backspace cancels remote search [async]', ok: false, err: e });
      return;
    }
    results.push({ name: 'Combobox backspace cancels remote search [async]', ok: true });
  }, 450);
});

test('Combobox store push z matched opcją NIE nadpisuje input.value gdy popover otwarty', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const input = el.querySelector('input');
  input.value = 'typ';  // user typing
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  // Popover teraz otwarty.
  assertEq(el.querySelector('.tf-combobox__popover').hidden, false);
  // Symulujemy late store push (matched opcja) — input.value powinien zostać 'typ'.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'b' } }],
  });
  assertEq(input.value, 'typ');
});

test('Combobox listbox mousedown na opcji commituje', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  keydown(input, 'ArrowDown');  // open
  const opts = el.querySelectorAll('.tf-combobox__option');
  opts[1].dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true }));
  assertEq(got, { value: 'b', kind: 'tstr' });
});

test('Combobox clear button emituje change=null + czyści input', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'a' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-combobox__clear').click();
  assertEq(got, { value: null, kind: null });
  assertEq(el.querySelector('input').value, '');
});

test('Combobox disabled BindRef blokuje input + clear', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'a' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const input = el.querySelector('input');
  assertEq(input.hasAttribute('disabled'), true);
  assertEq(el.querySelector('.tf-combobox__clear').disabled, true);
});

test('Combobox bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A')],
  }))));
});

test('Combobox unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, [
    [0, PATH('q')], [1, []], [4, true], [5, false], [6, false], [8, 'md'],
    [10, false], [11, 0], [12, false],
    [3, { kind: 'literal', value: 'L' }],
    [99, 'oops'],
  ])));
});

test('Combobox Escape zamyka popover', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const input = el.querySelector('input');
  keydown(input, 'ArrowDown');
  assertEq(el.querySelector('.tf-combobox__popover').hidden, false);
  keydown(input, 'Escape');
  assertEq(el.querySelector('.tf-combobox__popover').hidden, true);
});

// ============================================================================
// Autocomplete (0x0306)
// ============================================================================

test('Autocomplete renderuje <input role=combobox> z aria-autocomplete=list', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    6: { kind: 'literal', value: 'L' },
  })));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('role'), 'combobox');
  assertEq(input.getAttribute('aria-autocomplete'), 'list');
});

test('Autocomplete debounce_ms=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    debounceMs: 0,
    6: { kind: 'literal', value: 'L' },
  }))));
});

test('Autocomplete typing emituje search po debounce', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    actionId: 'find_user', minChars: 2, debounceMs: 50,
    6: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('search', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = 'ja';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  setTimeout(() => {
    try {
      assertEq(got, { query: 'ja', action_id: 'find_user', result_template_id: null });
    } catch (e) {
      results.push({ name: 'Autocomplete typing emituje search [async-2]', ok: false, err: e });
      return;
    }
    results.push({ name: 'Autocomplete typing emituje search [async-2]', ok: true });
  }, 100);
});

test('Autocomplete poniżej minChars NIE emituje search', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    actionId: 'x', minChars: 3, debounceMs: 30,
    6: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('search', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = 'ab';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  setTimeout(() => {
    try {
      assertEq(got, null);
    } catch (e) {
      results.push({ name: 'Autocomplete poniżej minChars NIE emituje [async-3]', ok: false, err: e });
      return;
    }
    results.push({ name: 'Autocomplete poniżej minChars NIE emituje [async-3]', ok: true });
  }, 80);
});

test('Autocomplete result_template_id ustawia aria-controls + data-attr', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    2: 'results-list',
    6: { kind: 'literal', value: 'L' },
  })));
  const input = el.querySelector('input');
  assert(input.getAttribute('aria-controls').endsWith('-results-list'));
  assertEq(input.getAttribute('data-result-template-id'), 'results-list');
});

test('Autocomplete bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AUTOCOMPLETE_TAG, acFields({}))));
});

test('Autocomplete typing emituje input event z value', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    6: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const got = [];
  el.addEventListener('input', (e) => got.push(e.detail));
  const input = el.querySelector('input');
  input.value = 'x';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  assertEq(got, [{ value: 'x', kind: 'tstr' }]);
});

test('Autocomplete unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AUTOCOMPLETE_TAG, [
    [0, PATH('q')], [1, 'act'], [3, 0], [4, 50],
    [6, { kind: 'literal', value: 'L' }],
    [99, 'oops'],
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
// Async testy zostawiają wpisy do `results` po opóźnieniu; raport po
// najdłuższym debounce'ie + buforze.
if (typeof process !== 'undefined') {
  setTimeout(() => {
    const r = reportResults();
    console.log(r.text);
    if (r.fail > 0) process.exit(1);
  }, 700);
}
export { reportResults };
