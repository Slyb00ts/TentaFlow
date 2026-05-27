// =============================================================================
// Plik: sdk-runtime/form-multiselect-renderer.test.js
// Opis: Testy MultiSelect (0x0304) — chunk 3.3c-3b.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { MULTISELECT_TAG } from './form-multiselect-renderer.js';

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

function tstrOpt(v, lbl, opts = {}) {
  const f = [
    [0, { kind: 'tstr', value: v }],
    [1, { kind: 'literal', value: lbl }],
    [3, opts.disabled === true],
  ];
  if (opts.groupId != null) f.push([4, opts.groupId]);
  return f;
}
function msFields({
  path = PATH('sel'), options = [], searchable = false, clearable = false,
  virtualize = false, size = 'md', showSelectAll = false, ...rest
} = {}) {
  const f = [
    [0, path], [1, options],
    [4, searchable], [5, clearable], [6, virtualize], [8, size],
    [11, showSelectAll],
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
// Render
// ============================================================================

test('MultiSelect trigger to <div role=combobox> (NIE <button>) by uniknąć button-in-button', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')], clearable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  const trigger = el.querySelector('.tf-multiselect__trigger');
  assertEq(trigger.tagName, 'DIV');
  assertEq(trigger.getAttribute('role'), 'combobox');
  assertEq(trigger.getAttribute('tabindex'), '0');
});

test('MultiSelect renderuje trigger combobox + placeholder gdy brak zaznaczeń', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: [] }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'Owoce' },
    2: { kind: 'literal', value: 'Wybierz...' },
  })));
  const trigger = el.querySelector('.tf-multiselect__trigger');
  assertEq(trigger.getAttribute('role'), 'combobox');
  const ph = el.querySelector('.tf-multiselect__placeholder');
  assertEq(ph.textContent, 'Wybierz...');
});

test('MultiSelect listbox ma aria-multiselectable=true', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  const lb = el.querySelector('[role="listbox"]');
  assertEq(lb.getAttribute('aria-multiselectable'), 'true');
});

test('MultiSelect renderuje chipy dla wszystkich zaznaczonych', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [{ kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'c' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana'), tstrOpt('c', 'Cherry')],
    3: { kind: 'literal', value: 'L' },
  })));
  const chips = el.querySelectorAll('.tf-multiselect__chip');
  assertEq(chips.length, 2);
  assertEq(chips[0].querySelector('.tf-multiselect__chip-label').textContent, 'Apple');
  assertEq(chips[1].querySelector('.tf-multiselect__chip-label').textContent, 'Cherry');
});

test('MultiSelect akceptuje raw primitive values w store (bez SelectValue wrap)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a', 'b'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B'), tstrOpt('c', 'C')],
    3: { kind: 'literal', value: 'L' },
  })));
  assertEq(el.querySelectorAll('.tf-multiselect__chip').length, 2);
});

// ============================================================================
// Open + toggle
// ============================================================================

test('MultiSelect trigger click otwiera popover', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-multiselect__trigger');
  const popover = el.querySelector('.tf-multiselect__popover');
  trigger.click();
  assertEq(popover.hidden, false);
  assertEq(trigger.getAttribute('aria-expanded'), 'true');
});

test('MultiSelect option mousedown TOGGLE w/ change=array, popover zostaje otwarty', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const trigger = el.querySelector('.tf-multiselect__trigger');
  trigger.click();
  const opts = el.querySelectorAll('.tf-multiselect__option');
  opts[0].dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true }));
  assertEq(got, { value: [{ kind: 'tstr', value: 'a' }], kind: 'array' });
  // Popover MUSI zostać otwarty.
  assertEq(el.querySelector('.tf-multiselect__popover').hidden, false);
});

test('MultiSelect click drugi raz na zaznaczonej opcji odznacza ją', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a', 'b'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect__trigger').click();
  // Odznacz 'a' — w detail powinno zostać tylko 'b'.
  el.querySelectorAll('.tf-multiselect__option')[0].dispatchEvent(
    new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true })
  );
  assertEq(got, { value: [{ kind: 'tstr', value: 'b' }], kind: 'array' });
});

test('MultiSelect aria-selected odzwierciedla zaznaczenie', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['b'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  const opts = el.querySelectorAll('.tf-multiselect__option');
  assertEq(opts[0].hasAttribute('aria-selected'), false);
  assertEq(opts[1].getAttribute('aria-selected'), 'true');
  assertEq(opts[1].querySelector('.tf-multiselect__option-check').textContent, '✓');
});

// ============================================================================
// max_selections
// ============================================================================

test('MultiSelect max_selections blokuje add powyżej limitu', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a', 'b'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B'), tstrOpt('c', 'C')],
    3: { kind: 'literal', value: 'L' },
    10: 2,  // max_selections=2
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect__trigger').click();
  // Próba dodania 'c' powinna być zignorowana.
  el.querySelectorAll('.tf-multiselect__option')[2].dispatchEvent(
    new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true })
  );
  assertEq(got, null);
});

test('MultiSelect max_selections=0 throws przy renderze', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    10: 0,
  }))));
});

// ============================================================================
// Chip remove
// ============================================================================

test('MultiSelect chip × usuwa konkretną wartość', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a', 'b', 'c'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B'), tstrOpt('c', 'C')],
    3: { kind: 'literal', value: 'L' },
  })));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  // Drugi chip (B) — × na nim.
  const removes = el.querySelectorAll('.tf-multiselect__chip-remove');
  removes[1].click();
  assertEq(got, {
    value: [{ kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'c' }],
    kind: 'array',
  });
});

test('MultiSelect chip × NIE otwiera popover', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  el.querySelector('.tf-multiselect__chip-remove').click();
  assertEq(el.querySelector('.tf-multiselect__popover').hidden, true);
});

// ============================================================================
// Clearable
// ============================================================================

test('MultiSelect clearable=true + zaznaczone renderuje clear button', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  assertEq(el.querySelector('.tf-multiselect__clear').hidden, false);
});

test('MultiSelect clear emituje change=[]', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a', 'b'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect__clear').click();
  assertEq(got, { value: [], kind: 'array' });
});

// ============================================================================
// Select all
// ============================================================================

test('MultiSelect show_select_all=true renderuje przycisk "Select all"', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
  })));
  const btn = el.querySelector('.tf-multiselect__select-all');
  assertEq(btn.textContent, 'Select all');
  assertEq(btn.dataset.mode, 'all');
});

test('MultiSelect "Select all" emituje change ze wszystkimi enabled options', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B', { disabled: true }), tstrOpt('c', 'C')],
    showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
  })));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect__select-all').click();
  assertEq(got, {
    value: [{ kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'c' }],
    kind: 'array',
  });
});

test('MultiSelect "Select all" → "Clear all" po pełnym zaznaczeniu', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a', 'b'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
  })));
  const btn = el.querySelector('.tf-multiselect__select-all');
  assertEq(btn.textContent, 'Clear all');
  assertEq(btn.dataset.mode, 'clear');
});

// ============================================================================
// Search
// ============================================================================

test('MultiSelect search filter ukrywa nie-pasujące', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    searchable: true,
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  el.querySelector('.tf-multiselect__trigger').click();
  const search = el.querySelector('.tf-multiselect__search');
  search.value = 'app';
  search.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  const opts = el.querySelectorAll('.tf-multiselect__option');
  assertEq(opts[0].hidden, false);
  assertEq(opts[1].hidden, true);
});

// ============================================================================
// Disabled
// ============================================================================

test('MultiSelect disabled BindRef blokuje open + toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lock'), value: true }, { path: PATH('sel'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const trigger = el.querySelector('.tf-multiselect__trigger');
  assertEq(trigger.getAttribute('aria-disabled'), 'true');
  trigger.click();
  assertEq(el.querySelector('.tf-multiselect__popover').hidden, true);
});

test('MultiSelect disabled wyłącza select-all button (show_select_all=true)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [] }, { path: PATH('lock'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const btn = el.querySelector('.tf-multiselect__select-all');
  assertEq(btn.disabled, false);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: true } }],
  });
  assertEq(btn.disabled, true);
  assertEq(btn.dataset.mode, 'noop');
});

test('MultiSelect disabled wyłącza nested clear + chip remove buttons', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: ['a'] }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')], clearable: true,
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  const trigger = el.querySelector('.tf-multiselect__trigger');
  assert(!trigger.hasAttribute('tabindex'));
  assertEq(el.querySelector('.tf-multiselect__clear').disabled, true);
  assertEq(el.querySelector('.tf-multiselect__chip-remove').disabled, true);
});

test('MultiSelect disabled flip mid-open blokuje toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [] }, { path: PATH('lock'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect__trigger').click();
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: true } }],
  });
  el.querySelector('.tf-multiselect__option').dispatchEvent(
    new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true })
  );
  assertEq(got, null);
});

// ============================================================================
// Keyboard
// ============================================================================

test('MultiSelect Enter toggluje aktywną opcję bez zamykania popover', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const trigger = el.querySelector('.tf-multiselect__trigger');
  trigger.click();  // open, active=0
  keydown(trigger, 'ArrowDown');  // active=1
  keydown(trigger, 'Enter');
  assertEq(got, { value: [{ kind: 'tstr', value: 'b' }], kind: 'array' });
  assertEq(el.querySelector('.tf-multiselect__popover').hidden, false);
});

test('MultiSelect Escape zamyka popover', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  const trigger = el.querySelector('.tf-multiselect__trigger');
  trigger.click();
  keydown(trigger, 'Escape');
  assertEq(el.querySelector('.tf-multiselect__popover').hidden, true);
});

// ============================================================================
// A11y label fallback
// ============================================================================

test('MultiSelect bez label wymaga a11y.label + mirror na trigger', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'Tagi' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
  }), { a11y: { label: { kind: 'bound', path: PATH('lbl') } } }));
  assertEq(el.querySelector('.tf-multiselect__trigger').getAttribute('aria-label'), 'Tagi');
});

test('MultiSelect bez label i bez a11y.label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
  }))));
});

// ============================================================================
// Validation
// ============================================================================

test('MultiSelect unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, [
    [0, PATH('sel')], [1, []], [4, false], [5, false], [6, false], [8, 'md'], [11, false],
    [3, { kind: 'literal', value: 'L' }],
    [99, 'oops'],
  ])));
});

test('MultiSelect option z nieznanym group_id throws', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A', { groupId: 'unknown' })],
    9: [grp],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('MultiSelect destroy odpina document click listener', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  })));
  document.body.appendChild(el);
  el.querySelector('.tf-multiselect__trigger').click();
  engine.destroy(el);
  const outside = document.createElement('div');
  document.body.appendChild(outside);
  outside.click();
});

// ============================================================================
// Groups
// ============================================================================

test('MultiSelect group ma role=group + aria-labelledby na samym group elemencie', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const el = engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A', { groupId: 'fr' })],
    9: [grp],
    3: { kind: 'literal', value: 'L' },
  })));
  const group = el.querySelector('.tf-multiselect__group');
  assertEq(group.getAttribute('role'), 'group');
  const header = el.querySelector('.tf-multiselect__group-header');
  assertEq(group.getAttribute('aria-labelledby'), header.id);
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
