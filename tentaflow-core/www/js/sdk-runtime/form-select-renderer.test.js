// =============================================================================
// Plik: sdk-runtime/form-select-renderer.test.js
// Opis: Testy Select (0x0303) — chunk 3.3c-3a.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { SELECT_TAG } from './form-select-renderer.js';

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

/// Helper: SelectOption jako FieldMap [[key, value], ...].
function opt({ value, label, icon, disabled = false, groupId, description } = {}) {
  const f = [[0, value], [1, label], [3, disabled]];
  if (icon != null) f.push([2, icon]);
  if (groupId != null) f.push([4, groupId]);
  if (description != null) f.push([5, description]);
  return f;
}
function tstrOpt(v, lbl) {
  return opt({
    value: { kind: 'tstr', value: v },
    label: { kind: 'literal', value: lbl },
  });
}
function selectFields({ path = PATH('sel'), options = [], searchable = false, clearable = false, virtualize = false, size = 'md', ...rest } = {}) {
  const f = [
    [0, path], [1, options],
    [4, searchable], [5, clearable], [6, virtualize], [8, size],
  ];
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
// Render + selected value
// ============================================================================

test('Select trigger to <div role=combobox> (NIE <button>) by uniknąć button-in-button', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')], clearable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  const trigger = el.querySelector('.tf-select__trigger');
  assertEq(trigger.tagName, 'DIV');
  assertEq(trigger.getAttribute('role'), 'combobox');
  assertEq(trigger.getAttribute('tabindex'), '0');
});

test('Select renderuje trigger combobox + placeholder gdy brak wartości', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'Frukty' },
    2: { kind: 'literal', value: 'Wybierz...' },
  })));
  const trigger = el.querySelector('.tf-select__trigger');
  assertEq(trigger.getAttribute('role'), 'combobox');
  assertEq(trigger.getAttribute('aria-haspopup'), 'listbox');
  assertEq(trigger.getAttribute('aria-expanded'), 'false');
  const lbl = el.querySelector('.tf-select__trigger-label');
  assertEq(lbl.textContent, 'Wybierz...');
  assert(lbl.classList.contains('tf-select__trigger-label--placeholder'));
});

test('Select pokazuje label aktualnie wybranej opcji', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'b' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'F' },
  })));
  assertEq(el.querySelector('.tf-select__trigger-label').textContent, 'Banana');
});

test('Select reactive label po store push', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'F' },
  })));
  assertEq(el.querySelector('.tf-select__trigger-label').textContent, 'Apple');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: 'b' } }],
  });
  assertEq(el.querySelector('.tf-select__trigger-label').textContent, 'Banana');
});

// ============================================================================
// Open/close popover
// ============================================================================

test('Select trigger click otwiera popover + aria-expanded=true', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-select__trigger');
  const popover = el.querySelector('.tf-select__popover');
  assertEq(popover.hidden, true);
  trigger.click();
  assertEq(popover.hidden, false);
  assertEq(trigger.getAttribute('aria-expanded'), 'true');
});

test('Select Escape zamyka popover + aria-expanded=false', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();
  assertEq(el.querySelector('.tf-select__popover').hidden, false);
  keydown(trigger, 'Escape');
  assertEq(el.querySelector('.tf-select__popover').hidden, true);
  assertEq(trigger.getAttribute('aria-expanded'), 'false');
});

test('Select outside click zamyka popover', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  el.querySelector('.tf-select__trigger').click();
  assertEq(el.querySelector('.tf-select__popover').hidden, false);
  // Symulujemy click na document poza wrapper'em.
  const outside = document.createElement('div');
  document.body.appendChild(outside);
  outside.click();
  assertEq(el.querySelector('.tf-select__popover').hidden, true);
});

// ============================================================================
// Keyboard nav + select
// ============================================================================

test('Select ArrowDown po openie ustawia aria-activedescendant na pierwszą opcję', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-select__trigger');
  keydown(trigger, 'ArrowDown');  // opens
  // Po open, aria-activedescendant wskazuje na pierwszą opcję.
  const first = el.querySelector('.tf-select__option');
  assertEq(trigger.getAttribute('aria-activedescendant'), first.id);
});

test('Select ArrowDown przechodzi do kolejnej opcji', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B'), tstrOpt('c', 'C')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();  // open + active=0
  keydown(trigger, 'ArrowDown');
  const second = el.querySelectorAll('.tf-select__option')[1];
  assertEq(trigger.getAttribute('aria-activedescendant'), second.id);
});

test('Select ArrowUp wraps z pierwszej na ostatnią', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();  // active=0
  keydown(trigger, 'ArrowUp');  // wrap → idx=1
  const last = el.querySelectorAll('.tf-select__option')[1];
  assertEq(trigger.getAttribute('aria-activedescendant'), last.id);
});

test('Select Home/End jumpują na pierwszą/ostatnią opcję', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B'), tstrOpt('c', 'C')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();
  keydown(trigger, 'End');
  const opts = el.querySelectorAll('.tf-select__option');
  assertEq(trigger.getAttribute('aria-activedescendant'), opts[2].id);
  keydown(trigger, 'Home');
  assertEq(trigger.getAttribute('aria-activedescendant'), opts[0].id);
});

test('Select Enter commituje aktywną opcję + emituje change z SelectValue', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();  // active=0
  keydown(trigger, 'ArrowDown');  // active=1
  keydown(trigger, 'Enter');
  assertEq(got, { value: 'b', kind: 'tstr' });
  // Popover zamknięty po commit.
  assertEq(el.querySelector('.tf-select__popover').hidden, true);
});

test('Select mousedown na opcji commituje wartość', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-select__trigger').click();
  const second = el.querySelectorAll('.tf-select__option')[1];
  second.dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true }));
  assertEq(got, { value: 'b', kind: 'tstr' });
});

test('Select disabled opcja nie jest commitowalna', () => {
  setup();
  const engine = makeEngine();
  const optDisabled = [
    [0, { kind: 'tstr', value: 'x' }],
    [1, { kind: 'literal', value: 'X' }],
    [3, true],
  ];
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [optDisabled, tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();
  // Active descendant powinno być na pierwszej widocznej NIE-disabled opcji.
  const opts = el.querySelectorAll('.tf-select__option');
  assertEq(trigger.getAttribute('aria-activedescendant'), opts[1].id);
  keydown(trigger, 'Enter');
  assertEq(got, { value: 'b', kind: 'tstr' });
});

// ============================================================================
// Searchable filter
// ============================================================================

test('Select searchable=true renderuje search input w popover', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple')],
    searchable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  assert(el.querySelector('.tf-select__search') != null);
});

test('Select search filter ukrywa nie-pasujące opcje', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana'), tstrOpt('c', 'Cherry')],
    searchable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  el.querySelector('.tf-select__trigger').click();
  const search = el.querySelector('.tf-select__search');
  search.value = 'an';
  search.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  const opts = el.querySelectorAll('.tf-select__option');
  assertEq(opts[0].hidden, true);   // Apple
  assertEq(opts[1].hidden, false);  // Banana
  assertEq(opts[2].hidden, true);   // Cherry
});

// ============================================================================
// Clearable
// ============================================================================

test('Select clearable=true z wybraną wartością renderuje clear button', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  const clear = el.querySelector('.tf-select__clear');
  assertEq(clear.hidden, false);
});

test('Select clear button emituje change z null value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-select__clear').click();
  assertEq(got, { value: null, kind: null });
});

test('Select clearable=false NIE renderuje clear button', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    clearable: false,
    3: { kind: 'literal', value: 'L' },
  })));
  assertEq(el.querySelector('.tf-select__clear'), null);
});

// ============================================================================
// Groups
// ============================================================================

test('Select groups renderuje group headers + zgrupowane opcje', () => {
  setup();
  const engine = makeEngine();
  const grp1 = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const grp2 = [[0, 'vg'], [1, { kind: 'literal', value: 'Warzywa' }]];
  const apple = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'Apple' }],
    [3, false], [4, 'fr'],
  ];
  const carrot = [
    [0, { kind: 'tstr', value: 'c' }],
    [1, { kind: 'literal', value: 'Carrot' }],
    [3, false], [4, 'vg'],
  ];
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [apple, carrot],
    9: [grp1, grp2],
    3: { kind: 'literal', value: 'L' },
  })));
  const headers = el.querySelectorAll('.tf-select__group-header');
  assertEq(headers.length, 2);
  assertEq(headers[0].textContent, 'Owoce');
  assertEq(headers[1].textContent, 'Warzywa');
});

test('Select option z nieznanym group_id throws', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const bad = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'A' }],
    [3, false], [4, 'unknown_id'],
  ];
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [bad], 9: [grp],
    3: { kind: 'literal', value: 'L' },
  }))));
});

// ============================================================================
// Disabled
// ============================================================================

test('Select disabled BindRef blokuje open + ustawia aria-disabled', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const trigger = el.querySelector('.tf-select__trigger');
  assertEq(trigger.getAttribute('aria-disabled'), 'true');
  trigger.click();
  assertEq(el.querySelector('.tf-select__popover').hidden, true);
});

test('Select option z IconRef::Named renderuje SVG icon przez shared renderer', () => {
  setup();
  const engine = makeEngine();
  const withIcon = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'Apple' }],
    [2, { kind: 'named', name: 'check' }],
    [3, false],
  ];
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [withIcon],
    3: { kind: 'literal', value: 'L' },
  })));
  const svg = el.querySelector('.tf-select__option svg.tf-icon');
  assert(svg != null, 'expected SVG rendered by icon-renderer for IconRef::Named');
  assert(svg.classList.contains('tf-icon--name-check'));
});

test('Select option z IconRef o złym shape (brak kind) throws', () => {
  setup();
  const engine = makeEngine();
  const badIcon = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'A' }],
    [2, { name: 'check' }],  // brakuje `kind: 'named'`
    [3, false],
  ];
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [badIcon],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select group ma role=group + aria-labelledby na samym group elemencie', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const apple = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'A' }],
    [3, false], [4, 'fr'],
  ];
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [apple], 9: [grp],
    3: { kind: 'literal', value: 'L' },
  })));
  const group = el.querySelector('.tf-select__group');
  assertEq(group.getAttribute('role'), 'group');
  const header = el.querySelector('.tf-select__group-header');
  assertEq(group.getAttribute('aria-labelledby'), header.id);
});

test('Select disabled + clearable wyłącza nested clear button', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: 'a' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')], clearable: true,
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const trigger = el.querySelector('.tf-select__trigger');
  const clear = el.querySelector('.tf-select__clear');
  assert(!trigger.hasAttribute('tabindex'), 'trigger powinno stracić tabindex w disabled');
  assertEq(clear.disabled, true);
  // Flip OFF — clear znowu enabled.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: false } }],
  });
  assertEq(clear.disabled, false);
});

test('Select disabled flip mid-open blokuje commit click', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: '' }, { path: PATH('lock'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();  // open OK (lock=false)
  // Teraz przerzucamy disabled na true PODCZAS open'a.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: true } }],
  });
  const li = el.querySelector('.tf-select__option');
  li.dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true }));
  assertEq(got, null);  // commit musi być zablokowane
});

// ============================================================================
// A11y label fallback
// ============================================================================

test('Select bez label wymaga a11y.label + mirror na trigger aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'Wybierz' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
  }), { a11y: { label: { kind: 'bound', path: PATH('lbl') } } }));
  assertEq(el.querySelector('.tf-select__trigger').getAttribute('aria-label'), 'Wybierz');
});

test('Select bez label i bez a11y.label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
  }))));
});

// ============================================================================
// Validation
// ============================================================================

test('Select unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, [
    [0, PATH('sel')], [1, []], [4, false], [5, false], [6, false], [8, 'md'],
    [3, { kind: 'literal', value: 'L' }],
    [99, 'oops'],
  ])));
});

test('Select invalid size throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [], size: 'xl',
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select destroy odpina document click listener', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-select__trigger');
  trigger.click();
  engine.destroy(el);
  // Po destroy outside-click nie powinien rzucić ani błędu.
  const outside = document.createElement('div');
  document.body.appendChild(outside);
  outside.click();
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
