// =============================================================================
// Plik: sdk-runtime/data-tree-empty-renderer.test.js
// Opis: Testy Tree + EmptyCell — chunk 3.3d-4.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-tree.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  TREE_TAG, EMPTY_CELL_TAG,
} from './data-tree-empty-renderer.js';

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

// ============================================================================
// Tree
// ============================================================================

test('Tree renderuje flat root nodes', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  assertEq(el.querySelector('.tf-tree').getAttribute('role'), 'tree');
  assertEq(el.querySelectorAll('.tf-tree__node').length, 2);
});

test('Tree expanded node pokazuje children', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A', children: [{ id: 'a1', label: 'A1' }] }] },
      { path: PATH('exp'), value: ['a'] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  assertEq(el.querySelectorAll('.tf-tree__node').length, 2);
  const parent = el.querySelector('[data-node-id=a]');
  assertEq(parent.getAttribute('aria-expanded'), 'true');
});

test('Tree non-expanded node ukrywa children', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A', children: [{ id: 'a1', label: 'A1' }] }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  assertEq(el.querySelectorAll('.tf-tree__node').length, 1);
  const a = el.querySelector('[data-node-id=a]');
  assertEq(a.getAttribute('aria-expanded'), 'false');
});

test('Tree caret click emituje expand event', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A', children: [{ id: 'a1', label: 'A1' }] }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  let got = null;
  el.addEventListener('expand', (e) => { got = e.detail; });
  el.querySelector('[data-node-id=a] .tf-tree__caret').click();
  assertEq(got, { node_id: 'a', lazy_load: false });
});

test('Tree caret click na expanded emituje collapse event', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A', children: [{ id: 'a1' }] }] },
      { path: PATH('exp'), value: ['a'] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  let got = null;
  el.addEventListener('collapse', (e) => { got = e.detail; });
  el.querySelector('[data-node-id=a] .tf-tree__caret').click();
  assertEq(got, { node_id: 'a' });
});

test('Tree row click (poza caret) emituje select', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A' }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  let got = null;
  el.addEventListener('select', (e) => { got = e.detail; });
  el.querySelector('[data-node-id=a] .tf-tree__label').click();
  assertEq(got, { node_id: 'a' });
});

test('Tree selected_id BindRef → aria-selected highlight', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a' }, { id: 'b' }] },
      { path: PATH('exp'), value: [] },
      { path: PATH('sel'), value: 'b' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [2, { kind: 'bound', path: PATH('sel') }],
    [3, 'default'], [4, false],
  ]));
  const b = el.querySelector('[data-node-id=b]');
  assertEq(b.getAttribute('aria-selected'), 'true');
});

test('Tree lazy_load=true + has_children=true bez children renderuje caret', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A', has_children: true }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, true],
  ]));
  const a = el.querySelector('[data-node-id=a]');
  assertEq(a.getAttribute('aria-expanded'), 'false');
  let got = null;
  el.addEventListener('expand', (e) => { got = e.detail; });
  el.querySelector('[data-node-id=a] .tf-tree__caret').click();
  assertEq(got, { node_id: 'a', lazy_load: true });
});

test('Tree disabled node blokuje select/expand', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', label: 'A', disabled: true, children: [{ id: 'a1' }] }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  let got = null;
  el.addEventListener('select', (e) => { got = e.detail; });
  el.addEventListener('expand', (e) => { got = e.detail; });
  const row = el.querySelector('[data-node-id=a] .tf-tree__row');
  row.click();
  assertEq(got, null);
});

test('Tree reactive: nodes update rebuilds DOM', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a' }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  assertEq(el.querySelectorAll('.tf-tree__node').length, 1);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('nodes'), op: { kind: 'set', value: [{ id: 'a' }, { id: 'b' }, { id: 'c' }] } }],
  });
  assertEq(el.querySelectorAll('.tf-tree__node').length, 3);
});

test('Tree invalid node (brak id) throws', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ label: 'X' }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ])));
});

test('Tree expanded_ids required throws gdy brak', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [3, 'default'], [4, false],
  ])));
});

test('Tree invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'literal', value: [] }],
    [3, 'fancy'], [4, false],
  ])));
});

test('Tree unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'literal', value: [] }],
    [3, 'default'], [4, false], [99, 'x'],
  ])));
});

test('Tree disabled node ArrowRight/Left NIE emituje expand/collapse', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a', disabled: true, children: [{ id: 'a1' }] }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('expand', (e) => { got = e.detail; });
  el.addEventListener('collapse', (e) => { got = e.detail; });
  const rowA = el.querySelector('[data-node-id=a] .tf-tree__row');
  rowA.setAttribute('tabindex', '0');
  rowA.focus();
  rowA.dispatchEvent(new (globalThis.KeyboardEvent || globalThis.Event)('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }));
  assertEq(got, null);
});

test('Tree keyboard ArrowDown przechodzi do następnego node', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('nodes'), value: [{ id: 'a' }, { id: 'b' }] },
      { path: PATH('exp'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TREE_TAG, [
    [0, PATH('nodes')], [1, { kind: 'bound', path: PATH('exp') }],
    [3, 'default'], [4, false],
  ]));
  document.body.appendChild(el);
  const rowA = el.querySelector('[data-node-id=a] .tf-tree__row');
  rowA.setAttribute('tabindex', '0');
  rowA.focus();
  rowA.dispatchEvent(new (globalThis.KeyboardEvent || globalThis.Event)('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }));
  // Focus powinien być na 'b'.
  assertEq(document.activeElement && document.activeElement.parentElement.getAttribute('data-node-id'), 'b');
});

// ============================================================================
// EmptyCell
// ============================================================================

test('EmptyCell variant=dash renderuje "–"', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(EMPTY_CELL_TAG, [[0, 'dash']]));
  assertEq(el.textContent, '–');
});

test('EmptyCell variant=em_dash renderuje "—"', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(EMPTY_CELL_TAG, [[0, 'em_dash']]));
  assertEq(el.textContent, '—');
});

test('EmptyCell variant=n_a renderuje "N/A" z aria-label', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(EMPTY_CELL_TAG, [[0, 'n_a']]));
  assertEq(el.textContent, 'N/A');
  assertEq(el.getAttribute('aria-label'), 'Not available');
});

test('EmptyCell variant=none jest aria-hidden+blank', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(EMPTY_CELL_TAG, [[0, 'none']]));
  assertEq(el.textContent, '');
  assertEq(el.getAttribute('aria-hidden'), 'true');
});

test('EmptyCell variant=loading ma role=status', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(EMPTY_CELL_TAG, [[0, 'loading']]));
  assertEq(el.getAttribute('role'), 'status');
  assertEq(el.getAttribute('aria-label'), 'Loading');
});

test('EmptyCell invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(EMPTY_CELL_TAG, [[0, 'bad']])));
});

test('EmptyCell unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(EMPTY_CELL_TAG, [[0, 'dash'], [99, 'x']])));
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
