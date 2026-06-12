// =============================================================================
// File: sdk-runtime/form-multiselect-renderer.test.js
// Description: Tests for MultiSelect (0x0304) rendered through the
// tf-multiselect web component.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-multiselect.js';
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
function mount(el) {
  document.body.appendChild(el);
  return el;
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
function mousedownOn(el) {
  el.dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true }));
}
function seededEngine(selected) {
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: selected }],
    state_revision: 0, truncated: false,
  });
  return { store, engine: makeEngine(store) };
}

// ============================================================================
// Render
// ============================================================================

test('MultiSelect renders tf-multiselect with <div role=combobox> trigger (NOT <button>)', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')], clearable: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.tagName, 'TF-MULTISELECT');
  const trigger = el.querySelector('.tf-multiselect-trigger');
  assertEq(trigger.tagName, 'DIV');
  assertEq(trigger.getAttribute('role'), 'combobox');
  assertEq(trigger.getAttribute('tabindex'), '0');
  assertEq(trigger.getAttribute('aria-haspopup'), 'listbox');
});

test('MultiSelect shows placeholder when nothing is selected', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple')],
    2: { kind: 'literal', value: 'Wybierz...' },
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelector('.tf-multiselect-placeholder').textContent, 'Wybierz...');
});

test('MultiSelect popover has role=listbox + aria-multiselectable=true', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const popover = el.querySelector('.tf-multiselect-popover');
  assertEq(popover.getAttribute('role'), 'listbox');
  assertEq(popover.getAttribute('aria-multiselectable'), 'true');
});

test('MultiSelect renders chips for all selected values (SelectValue shape)', () => {
  setup();
  const { engine } = seededEngine([
    { kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'c' },
  ]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana'), tstrOpt('c', 'Cherry')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const chips = el.querySelectorAll('.tf-multiselect-chip');
  assertEq(chips.length, 2);
  assertEq(chips[0].textContent.includes('Apple'), true);
  assertEq(chips[1].textContent.includes('Cherry'), true);
});

test('MultiSelect accepts raw primitive values in the store (no SelectValue wrap)', () => {
  setup();
  const { engine } = seededEngine(['b']);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const chips = el.querySelectorAll('.tf-multiselect-chip');
  assertEq(chips.length, 1);
  assertEq(chips[0].textContent.includes('Banana'), true);
});

test('MultiSelect store push updates chips reactively', () => {
  setup();
  const { store, engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelectorAll('.tf-multiselect-chip').length, 0);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: ['a', 'b'] } }],
  });
  assertEq(el.querySelectorAll('.tf-multiselect-chip').length, 2);
});

// ============================================================================
// Open / toggle / change semantics
// ============================================================================

test('MultiSelect trigger click opens the popover', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const trigger = el.querySelector('.tf-multiselect-trigger');
  const popover = el.querySelector('.tf-multiselect-popover');
  assertEq(popover.hidden, true);
  trigger.click();
  assertEq(popover.hidden, false);
  assertEq(trigger.getAttribute('aria-expanded'), 'true');
});

test('MultiSelect option mousedown TOGGLES with change={value:[],kind:array}, popover stays open', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect-trigger').click();
  mousedownOn(el.querySelectorAll('.tf-multiselect-option')[1]);
  assertEq(got, { value: [{ kind: 'tstr', value: 'b' }], kind: 'array' });
  assertEq(el.querySelector('.tf-multiselect-popover').hidden, false);
});

test('MultiSelect second click on a selected option deselects it', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const events = [];
  el.addEventListener('change', (e) => { events.push(e.detail); });
  el.querySelector('.tf-multiselect-trigger').click();
  const optB = el.querySelectorAll('.tf-multiselect-option')[1];
  mousedownOn(optB);
  mousedownOn(optB);
  assertEq(events, [
    { value: [{ kind: 'tstr', value: 'b' }], kind: 'array' },
    { value: [], kind: 'array' },
  ]);
});

test('MultiSelect aria-selected reflects the store selection', () => {
  setup();
  const { engine } = seededEngine([{ kind: 'tstr', value: 'b' }]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const opts = el.querySelectorAll('.tf-multiselect-option');
  assert(!opts[0].hasAttribute('aria-selected'));
  assertEq(opts[1].getAttribute('aria-selected'), 'true');
  assert(opts[1].classList.contains('tf-multiselect-option--selected'));
});

test('MultiSelect disabled option is not toggleable', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('x', 'X', { disabled: true }), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect-trigger').click();
  mousedownOn(el.querySelectorAll('.tf-multiselect-option')[0]);
  assertEq(got, null);
  assertEq(el.querySelectorAll('.tf-multiselect-option')[0].getAttribute('aria-disabled'), 'true');
});

test('MultiSelect max_selections blocks adding above the limit', () => {
  setup();
  const { engine } = seededEngine([{ kind: 'tstr', value: 'a' }]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
    10: 1,
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect-trigger').click();
  mousedownOn(el.querySelectorAll('.tf-multiselect-option')[1]);
  assertEq(got, null);  // adding beyond max is rejected, no event
});

test('MultiSelect max_selections=0 throws at render', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    10: 0,
  }))));
});

test('MultiSelect chip × removes the specific value', () => {
  setup();
  const { engine } = seededEngine([
    { kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'b' },
  ]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelectorAll('.tf-multiselect-chip-remove')[0].click();
  assertEq(got, { value: [{ kind: 'tstr', value: 'b' }], kind: 'array' });
});

test('MultiSelect chip × does NOT open the popover', () => {
  setup();
  const { engine } = seededEngine([{ kind: 'tstr', value: 'a' }]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple')],
    3: { kind: 'literal', value: 'L' },
  }))));
  el.querySelector('.tf-multiselect-chip-remove').click();
  assertEq(el.querySelector('.tf-multiselect-popover').hidden, true);
});

// ============================================================================
// Clear / select-all
// ============================================================================

test('MultiSelect clearable=true + selection renders the clear button', () => {
  setup();
  const { engine } = seededEngine([{ kind: 'tstr', value: 'a' }]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple')], clearable: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelector('.tf-multiselect-clear').hidden, false);
});

test('MultiSelect clearable=false hides the clear button', () => {
  setup();
  const { engine } = seededEngine([{ kind: 'tstr', value: 'a' }]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple')], clearable: false,
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelector('.tf-multiselect-clear').hidden, true);
});

test('MultiSelect clear emits change=[]', () => {
  setup();
  const { engine } = seededEngine([
    { kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'b' },
  ]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')], clearable: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect-clear').click();
  assertEq(got, { value: [], kind: 'array' });
});

test('MultiSelect show_select_all=true renders the "Select all" button', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')], showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  const btn = el.querySelector('.tf-multiselect-select-all');
  assertEq(btn.hidden, false);
  assertEq(btn.textContent, 'Select all');
});

test('MultiSelect "Select all" emits change with all enabled options', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('x', 'X', { disabled: true }), tstrOpt('b', 'B')],
    showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect-select-all').click();
  assertEq(got, {
    value: [{ kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'b' }],
    kind: 'array',
  });
});

test('MultiSelect "Select all" → "Clear all" once everything is selected', () => {
  setup();
  const { engine } = seededEngine([
    { kind: 'tstr', value: 'a' }, { kind: 'tstr', value: 'b' },
  ]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')], showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  const btn = el.querySelector('.tf-multiselect-select-all');
  assertEq(btn.textContent, 'Clear all');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  btn.click();
  assertEq(got, { value: [], kind: 'array' });
});

test('MultiSelect select-all with max_selections < enabled count never selects all', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B'), tstrOpt('c', 'C')],
    showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
    10: 2,
  }))));
  const btn = el.querySelector('.tf-multiselect-select-all');
  // Nothing selected + cannot fit all within the limit → button is a no-op.
  assertEq(btn.disabled, true);
  assertEq(btn.dataset.mode, 'noop');
});

// ============================================================================
// Search
// ============================================================================

test('MultiSelect search filter hides non-matching options', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana'), tstrOpt('c', 'Cherry')],
    searchable: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  el.querySelector('.tf-multiselect-trigger').click();
  const search = el.querySelector('.tf-multiselect-search');
  assertEq(search.hidden, false);
  search.value = 'an';
  search.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  const opts = el.querySelectorAll('.tf-multiselect-option');
  assertEq(opts[0].hidden, true);   // Apple
  assertEq(opts[1].hidden, false);  // Banana
  assertEq(opts[2].hidden, true);   // Cherry
});

test('MultiSelect searchable=false hides the search input (no-search)', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    searchable: false,
    3: { kind: 'literal', value: 'L' },
  }))));
  assert(el.hasAttribute('no-search'));
  assertEq(el.querySelector('.tf-multiselect-search').hidden, true);
});

// ============================================================================
// Disabled
// ============================================================================

test('MultiSelect disabled BindRef blocks open + toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [] }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  const trigger = el.querySelector('.tf-multiselect-trigger');
  assertEq(trigger.getAttribute('aria-disabled'), 'true');
  assert(!trigger.hasAttribute('tabindex'));
  trigger.click();
  assertEq(el.querySelector('.tf-multiselect-popover').hidden, true);
});

test('MultiSelect disabled disables the select-all button (show_select_all=true)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [] }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')], showSelectAll: true,
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  assertEq(el.querySelector('.tf-multiselect-select-all').disabled, true);
});

test('MultiSelect disabled disables nested clear + chip-remove buttons', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('sel'), value: [{ kind: 'tstr', value: 'a' }] },
      { path: PATH('lock'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')], clearable: true,
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  assertEq(el.querySelector('.tf-multiselect-clear').disabled, true);
  assertEq(el.querySelector('.tf-multiselect-chip-remove').disabled, true);
  // Flip OFF — buttons are enabled again.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: false } }],
  });
  assertEq(el.querySelector('.tf-multiselect-clear').disabled, false);
  assertEq(el.querySelector('.tf-multiselect-chip-remove').disabled, false);
});

test('MultiSelect disabled flip mid-open blocks toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: [] }, { path: PATH('lock'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-multiselect-trigger').click();  // open OK (lock=false)
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: true } }],
  });
  mousedownOn(el.querySelector('.tf-multiselect-option'));
  assertEq(got, null);
});

// ============================================================================
// Keyboard
// ============================================================================

test('MultiSelect Enter toggles the active option without closing the popover', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const trigger = el.querySelector('.tf-multiselect-trigger');
  keydown(trigger, 'ArrowDown');  // opens, active = first
  keydown(trigger, 'ArrowDown');  // active = second
  keydown(trigger, 'Enter');
  assertEq(got, { value: [{ kind: 'tstr', value: 'b' }], kind: 'array' });
  assertEq(el.querySelector('.tf-multiselect-popover').hidden, false);
});

test('MultiSelect Escape closes the popover', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const trigger = el.querySelector('.tf-multiselect-trigger');
  trigger.click();
  assertEq(el.querySelector('.tf-multiselect-popover').hidden, false);
  keydown(trigger, 'Escape');
  assertEq(el.querySelector('.tf-multiselect-popover').hidden, true);
  assertEq(trigger.getAttribute('aria-expanded'), 'false');
});

// ============================================================================
// Labels / a11y / validation
// ============================================================================

test('MultiSelect label bind renders the label element + aria-labelledby', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'Detektory' },
  }))));
  const label = el.querySelector('.tf-multiselect-label');
  assertEq(label.textContent, 'Detektory');
  assertEq(el.querySelector('.tf-multiselect-trigger').getAttribute('aria-labelledby'), label.id);
});

test('MultiSelect without label requires a11y.label + mirror on the trigger', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'Wybierz' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
  }), { a11y: { label: { kind: 'bound', path: PATH('lbl') } } })));
  assertEq(el.getAttribute('aria-label'), 'Wybierz');
  assertEq(el.querySelector('.tf-multiselect-trigger').getAttribute('aria-label'), 'Wybierz');
});

test('MultiSelect without label and without a11y.label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
  }))));
});

test('MultiSelect unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, [
    ...msFields({ options: [], 3: { kind: 'literal', value: 'L' } }),
    [99, 'oops'],
  ])));
});

test('MultiSelect option with unknown group_id throws', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  assertThrows(() => engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A', { groupId: 'unknown_id' })],
    9: [grp],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('MultiSelect group renders role=group + header with resolved label', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple', { groupId: 'fr' })],
    9: [grp],
    3: { kind: 'literal', value: 'L' },
  }))));
  const group = el.querySelector('.tf-multiselect-group');
  assertEq(group.getAttribute('role'), 'group');
  const header = el.querySelector('.tf-multiselect-group-header');
  assertEq(header.textContent, 'Owoce');
  assertEq(group.getAttribute('aria-labelledby'), header.id);
  assert(group.querySelector('.tf-multiselect-option') != null);
});

test('MultiSelect raw component change never reaches listeners (only SDK shape)', () => {
  setup();
  const { engine } = seededEngine([]);
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'Apple')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const shapes = [];
  el.addEventListener('change', (e) => { shapes.push(e.detail); });
  el.querySelector('.tf-multiselect-trigger').click();
  mousedownOn(el.querySelector('.tf-multiselect-option'));
  // Exactly ONE event in the SDK shape — never the raw {value: [idx]}.
  assertEq(shapes, [{ value: [{ kind: 'tstr', value: 'a' }], kind: 'array' }]);
});

test('MultiSelect destroy + outside click does not throw', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MULTISELECT_TAG, msFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  }))));
  el.querySelector('.tf-multiselect-trigger').click();
  engine.destroy(el);
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
