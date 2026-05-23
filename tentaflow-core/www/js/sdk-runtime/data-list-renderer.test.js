// =============================================================================
// Plik: sdk-runtime/data-list-renderer.test.js
// Opis: Testy List (0x0212) + EmptyState (0x0003 tymczasowo) — chunk 3.3d-5.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  LIST_TAG, EMPTY_STATE_COMPONENT_TAG,
} from './data-list-renderer.js';

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

function emptyStateComp(opts = {}, id = 'es') {
  const f = [
    [0, opts.icon || { kind: 'named', name: 'search' }],
    [1, opts.heading || { kind: 'literal', value: 'Pusto' }],
    [5, opts.variant || 'default'],
  ];
  if (opts.message) f.push([2, opts.message]);
  if (opts.primary) f.push([3, opts.primary]);
  if (opts.secondary) f.push([4, opts.secondary]);
  return comp(EMPTY_STATE_COMPONENT_TAG, f, { id });
}

function listFields({
  itemsPath = PATH('items'), templateId = 'list_item',
  divider = false, density = 'default', virtualize = false,
  emptyState = null, maxVisible = null,
} = {}) {
  const f = [
    [0, itemsPath], [1, templateId],
    [2, divider], [3, density], [4, virtualize],
  ];
  if (emptyState != null) f.push([5, emptyState]);
  if (maxVisible != null) f.push([6, maxVisible]);
  return f;
}

// ============================================================================
// List
// ============================================================================

test('List renderuje <ul> z N items', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIST_TAG, listFields()));
  assertEq(el.querySelectorAll('.tf-list__item').length, 2);
  assertEq(el.getAttribute('data-template-id'), 'list_item');
});

test('List item fallback text z label/title/id', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [
      { id: 'a', label: 'L' },
      { id: 'b', title: 'T' },
      { id: 'c' },
    ] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIST_TAG, listFields()));
  const items = el.querySelectorAll('.tf-list__item-content');
  assertEq(items[0].textContent, 'L');
  assertEq(items[1].textContent, 'T');
  assertEq(items[2].textContent, 'c');
});

test('List XSS-safe: HTML w item.label nie tworzy elementu', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [{ id: 'x', label: '<img src=x onerror=alert(1)>' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIST_TAG, listFields()));
  assertEq(el.querySelector('img'), null);
  assertEq(el.querySelector('.tf-list__item-content').textContent, '<img src=x onerror=alert(1)>');
});

test('List item click emituje item_click z item_id+item_index+template_id', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [{ id: 'a' }, { id: 'b' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIST_TAG, listFields({ templateId: 'my_tpl' })));
  let got = null;
  el.addEventListener('item_click', (e) => { got = e.detail; });
  el.querySelectorAll('.tf-list__item')[1].click();
  assertEq(got, { item_id: 'b', item_index: 1, template_id: 'my_tpl' });
});

test('List keyboard Enter na item triggers item_click', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [{ id: 'x' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIST_TAG, listFields()));
  let got = null;
  el.addEventListener('item_click', (e) => { got = e.detail; });
  el.querySelector('.tf-list__item').dispatchEvent(new (globalThis.KeyboardEvent || globalThis.Event)('keydown', { key: 'Enter', bubbles: false, cancelable: true }));
  assertEq(got.item_id, 'x');
});

test('List items=[] z empty_state pokazuje empty_state, ukrywa <ul>', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const es = emptyStateComp({ heading: { kind: 'literal', value: 'Brak danych' } });
  const el = engine.render(comp(LIST_TAG, listFields({ emptyState: es })));
  assertEq(el.querySelector('.tf-list__items').hidden, true);
  const esEl = el.querySelector('.tf-empty-state');
  assertEq(esEl.hidden, false);
  assertEq(esEl.querySelector('.tf-empty-state__heading').textContent, 'Brak danych');
});

test('List items dodawane po empty toggle visibility', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const es = emptyStateComp();
  const el = engine.render(comp(LIST_TAG, listFields({ emptyState: es })));
  assertEq(el.querySelector('.tf-list__items').hidden, true);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('items'), op: { kind: 'set', value: [{ id: 'a' }] } }],
  });
  assertEq(el.querySelector('.tf-list__items').hidden, false);
  assertEq(el.querySelector('.tf-empty-state').hidden, true);
});

test('List max_visible truncates + renderuje "+N more"', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [{ id: '1' }, { id: '2' }, { id: '3' }, { id: '4' }, { id: '5' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIST_TAG, listFields({ maxVisible: 3 })));
  assertEq(el.querySelectorAll('.tf-list__item').length, 3);
  const more = el.querySelector('.tf-list__more');
  assertEq(more.textContent, '+2 more');
});

test('List max_visible=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LIST_TAG, listFields({ maxVisible: 0 }))));
});

test('List invalid item_template_id grammar throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LIST_TAG, listFields({ templateId: 'BAD ID!' }))));
});

test('List empty_state z innym tagiem throws', () => {
  setup();
  const engine = makeEngine();
  const bad = { tag: 0x0201, id: 'x', fields: [], handlers: null, bind: null, a11y: null, visibility: null, test_id: null };
  assertThrows(() => engine.render(comp(LIST_TAG, listFields({ emptyState: bad }))));
});

test('List divider=true ustawia klasę', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(LIST_TAG, listFields({ divider: true })));
  assert(el.classList.contains('tf-list--divider'));
});

test('List reactive rebuild — usunięte items zwalniają listenery', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('items'), value: [{ id: 'a' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIST_TAG, listFields()));
  // Patch: zmiana items, stary item DOM wymieniony.
  const oldItem = el.querySelector('.tf-list__item');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('items'), op: { kind: 'set', value: [{ id: 'b' }] } }],
  });
  assert(!el.contains(oldItem));
  let got = null;
  el.addEventListener('item_click', (e) => { got = e.detail; });
  oldItem.click();  // już odpięty od wrapper'a; powinien NIE wyemitować
  assertEq(got, null);
});

test('List unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LIST_TAG, [
    [0, PATH('items')], [1, 'tpl'], [2, false], [3, 'default'], [4, false], [99, 'x'],
  ])));
});

test('List invalid density throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LIST_TAG, listFields({ density: 'huge' }))));
});

// ============================================================================
// EmptyState
// ============================================================================

test('EmptyState renderuje icon+heading+message+actions', () => {
  setup();
  const engine = makeEngine();
  // Button (0x0401): 0=variant, 1=tone, 2=label, 5=size, 6=full_width.
  const primary = comp(0x0401, [
    [0, 'primary'], [1, 'primary'], [2, { kind: 'literal', value: 'Add' }],
    [5, 'md'], [6, false], [9, 'default'],
  ], { id: 'btn1' });
  const el = engine.render(emptyStateComp({
    heading: { kind: 'literal', value: 'No items' },
    message: { kind: 'literal', value: 'Click below to add' },
    primary,
  }));
  assertEq(el.querySelector('.tf-empty-state__heading').textContent, 'No items');
  assertEq(el.querySelector('.tf-empty-state__message').textContent, 'Click below to add');
  assert(el.querySelector('.tf-empty-state__action--primary') != null);
});

test('EmptyState variant=illustrated ustawia klasę', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(emptyStateComp({ variant: 'illustrated' }));
  assert(el.classList.contains('tf-empty-state--variant-illustrated'));
});

test('EmptyState primary_action z innym tagiem throws', () => {
  setup();
  const engine = makeEngine();
  const bad = { tag: 0x0201, id: 'x', fields: [], handlers: null, bind: null, a11y: null, visibility: null, test_id: null };
  assertThrows(() => engine.render(emptyStateComp({ primary: bad })));
});

test('EmptyState icon required throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(EMPTY_STATE_COMPONENT_TAG, [
    [1, { kind: 'literal', value: 'X' }], [5, 'default'],
  ])));
});

test('EmptyState invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(emptyStateComp({ variant: 'fancy' })));
});

test('EmptyState unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(EMPTY_STATE_COMPONENT_TAG, [
    [0, { kind: 'named', name: 'search' }],
    [1, { kind: 'literal', value: 'X' }],
    [5, 'default'], [99, 'x'],
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
