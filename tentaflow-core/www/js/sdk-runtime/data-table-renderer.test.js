// =============================================================================
// Plik: sdk-runtime/data-table-renderer.test.js
// Opis: Testy Table (0x0211) — chunk 3.3d-6.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-table.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { TABLE_TAG } from './data-table-renderer.js';

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

const BUTTON_TAG = 0x0401;

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
// tf-table adopts shadow styles at connect time (shared-styles.js): it touches
// the bare `Document` global and fetches /css/controls.css. Bridge both so the
// async adoption resolves instead of crashing Node with an unhandled rejection.
globalThis.Document = window.Document;
globalThis.fetch = async () => ({ ok: true, text: async () => '' });

function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

/// Mounts a rendered Table wrapper and returns its tf-table shadow root —
/// tf-table builds thead/tbody only after connectedCallback.
function mount(el) {
  document.body.appendChild(el);
  return el.querySelector('tf-table').shadowRoot;
}

/// Helper: TableColumn FieldMap.
function col({ id, header, field, width = { kind: 'auto' }, render = 'text', sortable = false, hidden = false, sticky = false, align, format } = {}) {
  const f = [
    [0, id], [1, { kind: 'literal', value: header || id }],
    [2, PATH(field || id)], [3, width], [4, render],
    [7, sortable], [8, hidden], [9, sticky],
  ];
  if (align) f.push([6, align]);
  if (format) f.push([5, format]);
  return f;
}

function tableFields({
  columns = [], rowsPath = PATH('rows'), rowKeyField = 'id',
  variant = 'default', density = 'default', sortable = false,
  sortByBind = null, selectMode = 'none', selectedIdsBind = null,
  stickyHeader = false, stickyColumns = 0, pagination = null,
  emptyState = null, rowActions = [], bulkActions = [],
  virtualize = false, rowExpandable = false, expandedRowTemplateId = null,
} = {}) {
  const f = [
    [0, columns], [1, rowsPath], [2, rowKeyField],
    [3, variant], [4, density], [5, sortable],
    [7, selectMode], [9, stickyHeader], [10, stickyColumns],
    [13, rowActions], [14, bulkActions],
    [15, virtualize], [16, rowExpandable],
  ];
  if (sortByBind) f.push([6, sortByBind]);
  if (selectedIdsBind) f.push([8, selectedIdsBind]);
  if (pagination) f.push([11, pagination]);
  if (emptyState) f.push([12, emptyState]);
  if (expandedRowTemplateId) f.push([17, expandedRowTemplateId]);
  return f;
}

// ============================================================================
// Render basics
// ============================================================================

test('Table renders tf-table + tf-column with shadow thead/tbody', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', name: 'A' }, { id: 'r2', name: 'B' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name', header: 'Name', field: 'name' })],
  })));
  const tfCol = el.querySelector('tf-column');
  assertEq(tfCol.getAttribute('key'), 'name');
  assertEq(tfCol.getAttribute('label'), 'Name');
  const sr = mount(el);
  assertEq(sr.querySelectorAll('thead th').length, 1);
  assertEq(sr.querySelector('thead th').textContent, 'Name');
  assertEq(sr.querySelectorAll('tbody tr').length, 2);
  assert(el.classList.contains('tf-table--variant-default'));
  assert(el.classList.contains('tf-table--density-default'));
});

test('Table cell content resolved from nested field_path', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', user: { name: 'Ala' } }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const c = col({ id: 'name', header: 'Name' });
  c[2] = [2, PATH('user', 'name')];  // nested
  const el = engine.render(comp(TABLE_TAG, tableFields({ columns: [c] })));
  const sr = mount(el);
  assertEq(sr.querySelector('tbody td').textContent, 'Ala');
});

test('Table XSS-safe: HTML in cell goes through textContent', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', name: '<script>alert(1)</script>' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name', field: 'name' })],
  })));
  const sr = mount(el);
  assertEq(sr.querySelector('script'), null);
  assert(sr.querySelector('tbody td').textContent.includes('<script>'));
});

test('Table row click re-emits row_click with row_id', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', name: 'A' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name' })],
  })));
  const sr = mount(el);
  let got = null;
  el.addEventListener('row_click', (e) => { got = e.detail; });
  sr.querySelector('tbody tr').click();
  assertEq(got, { row_id: 'r1' });
});

test('Table badge column renders tf-chip cell', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', status: 'ok' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'status', field: 'status', render: 'badge' })],
  })));
  assertEq(el.querySelector('tf-column').getAttribute('renderer'), 'chip');
  const sr = mount(el);
  const chip = sr.querySelector('tbody td .tf-chip');
  assertEq(chip.textContent, 'ok');
});

// ============================================================================
// Selection
// ============================================================================

test('Table selectable=single row click emits selection_change', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }, { id: 'r2' }] },
      { path: PATH('sel'), value: null },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    selectMode: 'single',
    selectedIdsBind: { kind: 'bound', path: PATH('sel') },
  })));
  const sr = mount(el);
  let got = null;
  el.addEventListener('selection_change', (e) => { got = e.detail; });
  const tr = sr.querySelectorAll('tbody tr')[0];
  tr.click();
  assertEq(got, { selected_ids: 'r1', mode: 'single', changed_row_id: 'r1' });
  assert(tr.classList.contains('selected'));
});

test('Table selectable=multi merges clicked row into bound selection', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }, { id: 'r2' }] },
      { path: PATH('sel'), value: ['r1'] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    selectMode: 'multi',
    selectedIdsBind: { kind: 'bound', path: PATH('sel') },
  })));
  const sr = mount(el);
  let got = null;
  el.addEventListener('selection_change', (e) => { got = e.detail; });
  sr.querySelectorAll('tbody tr')[1].click();
  assertEq(got, { selected_ids: ['r1', 'r2'], mode: 'multi', changed_row_id: 'r2' });
});

test('Table selectable != none bez selected_ids throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    selectMode: 'single',
  }))));
});

// ============================================================================
// Sort
// ============================================================================

test('Table sortable header click re-emits sort_change asc', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }] },
      { path: PATH('sort'), value: null },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name', sortable: true })],
    sortable: true,
    sortByBind: { kind: 'bound', path: PATH('sort') },
  })));
  const sr = mount(el);
  let got = null;
  el.addEventListener('sort_change', (e) => { got = e.detail; });
  sr.querySelector('th.sortable').click();
  assertEq(got, { sort: { column_id: 'name', direction: 'asc' } });
});

test('Table second sort click toggles direction to desc', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }] },
      { path: PATH('sort'), value: null },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name', sortable: true })],
    sortable: true,
    sortByBind: { kind: 'bound', path: PATH('sort') },
  })));
  const sr = mount(el);
  let got = null;
  el.addEventListener('sort_change', (e) => { got = e.detail; });
  const th = sr.querySelector('th.sortable');
  th.click();
  th.click();
  assertEq(got, { sort: { column_id: 'name', direction: 'desc' } });
  assert(th.classList.contains('sorted-desc'));
});

test('Table sort click reorders shadow rows', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1', name: 'B' }, { id: 'r2', name: 'A' }] },
      { path: PATH('sort'), value: null },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name', field: 'name', sortable: true })],
    sortable: true,
    sortByBind: { kind: 'bound', path: PATH('sort') },
  })));
  const sr = mount(el);
  assertEq(sr.querySelector('tbody td').textContent, 'B');
  sr.querySelector('th.sortable').click();
  assertEq(sr.querySelector('tbody td').textContent, 'A');
});

test('Table sort_change emits TableColumn id when id != field_path[0]', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [
        { id: 'r1', user: { name: 'B', role: 'admin' } },
        { id: 'r2', user: { name: 'A', role: 'guest' } },
      ] },
      { path: PATH('sort'), value: null },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  // Both columns share field_path[0] === 'user' — keys must stay unique.
  const cName = col({ id: 'user_name', header: 'Name', sortable: true });
  cName[2] = [2, PATH('user', 'name')];
  const cRole = col({ id: 'user_role', header: 'Role' });
  cRole[2] = [2, PATH('user', 'role')];
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [cName, cRole],
    sortable: true,
    sortByBind: { kind: 'bound', path: PATH('sort') },
  })));
  assertEq(el.querySelectorAll('tf-column')[0].getAttribute('key'), 'user_name');
  assertEq(el.querySelectorAll('tf-column')[1].getAttribute('key'), 'user_role');
  const sr = mount(el);
  // Cell rendering still resolves the nested field_path per column.
  const tds = sr.querySelectorAll('tbody tr')[0].querySelectorAll('td');
  assertEq(tds[0].textContent, 'B');
  assertEq(tds[1].textContent, 'admin');
  let got = null;
  el.addEventListener('sort_change', (e) => { got = e.detail; });
  sr.querySelector('th.sortable').click();
  assertEq(got, { sort: { column_id: 'user_name', direction: 'asc' } });
  // tf-table internal sort works on the column-id keyed values.
  assertEq(sr.querySelector('tbody td').textContent, 'A');
});

// ============================================================================
// Pagination
// ============================================================================

test('Table pagination slice rows wg page_size + current_page', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }, { id: 'r2' }, { id: 'r3' }, { id: 'r4' }, { id: 'r5' }] },
      { path: PATH('page'), value: 2 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const pagination = [[0, 2], [1, PATH('page')], [2, false]];
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    pagination,
  })));
  // page=2, size=2 → rows 3,4
  const sr = mount(el);
  const cells = sr.querySelectorAll('tbody td');
  assertEq(cells.length, 2);
  assertEq(cells[0].textContent, 'r3');
});

test('Table pagination prev disabled na page 1', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }] },
      { path: PATH('page'), value: 1 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    pagination: [[0, 10], [1, PATH('page')], [2, false]],
  })));
  const btns = el.querySelectorAll('.tf-table__page-btn');
  assertEq(btns[0].disabled, true);
});

test('Table pagination next click emituje page_change', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }, { id: 'r2' }, { id: 'r3' }] },
      { path: PATH('page'), value: 1 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    pagination: [[0, 1], [1, PATH('page')], [2, false]],
  })));
  let got = null;
  el.addEventListener('page_change', (e) => { got = e.detail; });
  const btns = el.querySelectorAll('.tf-table__page-btn');
  btns[1].click();  // next
  assertEq(got, { page: 2, page_size: 1 });
});

test('Table show_size_picker renderuje select', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [] }, { path: PATH('page'), value: 1 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    pagination: [[0, 25], [1, PATH('page')], [2, true]],
  })));
  const sel = el.querySelector('.tf-table__page-size');
  assertEq(sel.tagName, 'SELECT');
  assertEq(sel.value, '25');
});

test('Table pagination page_size=0 throws, BigInt page_size accepted', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    pagination: [[0, 0], [1, PATH('page')], [2, false]],
  }))));
  // u32 page_size also accepted as BigInt
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    pagination: [[0, 10n], [1, PATH('page')], [2, false]],
  }), { id: 'c2' }));
  assert(el.querySelector('.tf-table__pagination') != null);
});

// ============================================================================
// Validation
// ============================================================================

test('Table duplicate column ids throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a' }), col({ id: 'a' })],
  }))));
});

test('Table row actions builder throws when row misses row_key_field', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', name: 'A' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name' })],
    rowActions: [rowActionButton({ id: 'act-edit', label: 'Edit', actionId: 'edit_row' })],
  })));
  const tfTable = el.querySelector('tf-table');
  assertThrows(() => tfTable.rowActions({ name: 'no id here' }, 0));
});

test('Table column id shadowing row_key_field keeps original key in events', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'real-1', display_id: 'PRETTY-1' }] },
      { path: PATH('sel'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  // Column id equals row_key_field but reads a DIFFERENT field — the
  // flattened cell value must not clobber the row identifier.
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id', field: 'display_id' })],
    selectMode: 'multi',
    selectedIdsBind: { kind: 'bound', path: PATH('sel') },
  })));
  const sr = mount(el);
  assertEq(sr.querySelector('tbody td').textContent, 'PRETTY-1');
  let clicked = null;
  let selected = null;
  el.addEventListener('row_click', (e) => { clicked = e.detail; });
  el.addEventListener('selection_change', (e) => { selected = e.detail; });
  sr.querySelector('tbody tr').click();
  // Events carry the ORIGINAL row key, not the formatted display value.
  assertEq(clicked, { row_id: 'real-1' });
  assertEq(selected, { selected_ids: ['real-1'], mode: 'multi', changed_row_id: 'real-1' });
});

test('Table reserved __tfRowKey column id throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: '__tfRowKey' })],
  }))));
});

test('Table sticky_columns > columns.length throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a' })],
    stickyColumns: 5,
  }))));
});

test('Table row_expandable=true bez expanded_row_template_id throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a' })],
    rowExpandable: true,
  }))));
});

test('Table column invalid grammar id throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'Bad ID!' })],
  }))));
});

test('Table column invalid width.kind throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a', width: { kind: 'huge' } })],
  }))));
});

test('Table empty_state z innym tagiem throws', () => {
  setup();
  const engine = makeEngine();
  const bad = { tag: 0x0201, id: 'x', fields: [], handlers: null, bind: null, a11y: null, visibility: null, test_id: null };
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a' })],
    emptyState: bad,
  }))));
});

test('Table row_actions z innym tagiem niż Button throws', () => {
  setup();
  const engine = makeEngine();
  const bad = { tag: 0x0201, id: 'x', fields: [], handlers: null, bind: null, a11y: null, visibility: null, test_id: null };
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a' })],
    rowActions: [bad],
  }))));
});

test('Table unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, [
    ...tableFields({ columns: [col({ id: 'a' })] }), [99, 'x'],
  ])));
});

// ============================================================================
// Empty state + reactive rebuild
// ============================================================================

test('Table empty rows pokazuje empty_state', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const es = comp(0x0003, [
    [0, { kind: 'named', name: 'search' }],
    [1, { kind: 'literal', value: 'Brak danych' }],
    [5, 'default'],
  ], { id: 'es' });
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a' })],
    emptyState: es,
  })));
  assertEq(el.querySelector('.tf-table__empty-state').hidden, false);
});

test('Table deselect click emits empty selection list', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }] },
      { path: PATH('sel'), value: ['r1'] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    selectMode: 'multi',
    selectedIdsBind: { kind: 'bound', path: PATH('sel') },
  })));
  const sr = mount(el);
  let got = null;
  el.addEventListener('selection_change', (e) => { got = e.detail; });
  const tr = sr.querySelector('tbody tr');
  tr.click();  // selects in tf-table → ['r1']
  assertEq(got.selected_ids, ['r1']);
  tr.click();  // deselects → []
  assertEq(got, { selected_ids: [], mode: 'multi', changed_row_id: 'r1' });
});

test('Table bulk actions toolbar follows selection bind', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('rows'), value: [{ id: 'r1' }, { id: 'r2' }] },
      { path: PATH('sel'), value: ['r1', 'r2'] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
    selectMode: 'multi',
    selectedIdsBind: { kind: 'bound', path: PATH('sel') },
    bulkActions: [rowActionButton({ id: 'bulk-del', label: 'Delete', actionId: 'delete_rows' })],
  })));
  const toolbar = el.querySelector('.tf-table__bulk-actions');
  assertEq(toolbar.hidden, false);
  assert(toolbar.querySelector('tf-button') != null, 'bulk Button must be rendered');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: [] } }],
  });
  assertEq(toolbar.hidden, true);
});

test('Table empty rows render zero shadow rows', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a' })],
  })));
  const sr = mount(el);
  assertEq(sr.querySelectorAll('tbody tr').length, 0);
});

test('Table ValueFormat z obcym polem dla wariantu throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a', format: { kind: 'plain', code: 'USD' } })],
  }))));
});

test('Table column field_path z malformed segment throws', () => {
  setup();
  const engine = makeEngine();
  const c = col({ id: 'a' });
  c[2] = [2, [{ kind: 'unknown', value: 'x' }]];
  assertThrows(() => engine.render(comp(TABLE_TAG, tableFields({ columns: [c] }))));
});

test('Table reactive rebuild po patch rows', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'id' })],
  })));
  const sr = mount(el);
  assertEq(sr.querySelectorAll('tbody tr').length, 1);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('rows'), op: { kind: 'set', value: [{ id: 'r1' }, { id: 'r2' }, { id: 'r3' }] } }],
  });
  assertEq(sr.querySelectorAll('tbody tr').length, 3);
});

test('Table sticky_header adds wrapper class, BigInt sticky_columns accepted', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', a: '1', b: '2' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'a', field: 'a' }), col({ id: 'b', field: 'b' })],
    stickyHeader: true,
    stickyColumns: 1n,
  })));
  assert(el.classList.contains('tf-table--sticky-header'));
});

// ============================================================================
// Row actions (kebab menu)
// ============================================================================

/// Build a row-action Button component with a backend handler.
function rowActionButton({ id, label, actionId, params = {}, icon = null, variant = 'secondary' }) {
  const fields = [
    [0, variant], [1, 'neutral'],
    [2, { kind: 'literal', value: label }],
    [5, 'sm'], [6, false], [9, 'default'],
  ];
  if (icon) fields.push([3, { kind: 'named', name: icon }]);
  return {
    tag: BUTTON_TAG, id, fields,
    handlers: [['click', { kind: 'backend', action_id: actionId, params }]],
    bind: null, a11y: null, visibility: null, test_id: null,
  };
}

/// Mirror of addon-app.js dispatcher merge: params <- {...handler.params,
/// ...dom_event.detail}. Proves the row key reaches the backend params.
function dispatcherWithCapture(captured) {
  return {
    emit({ handler, dom_event, action_id, ...rest }) {
      if (!handler || (handler.kind !== 'backend' && handler.kind !== 'both')) return;
      const params = { ...(handler.params || {}) };
      if (dom_event && dom_event.detail && typeof dom_event.detail === 'object') {
        Object.assign(params, dom_event.detail);
      }
      captured.push({ action_id: handler.action_id, params });
    },
  };
}

test('Table row_actions ustawia builder akcji na tf-table', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ camera_id: 'cam-1', name: 'A' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name' })],
    rowKeyField: 'camera_id',
    rowActions: [rowActionButton({ id: 'act-edit', label: 'Edytuj', actionId: 'edit_camera' })],
  })));
  const tfTable = el.querySelector('tf-table');
  assert(tfTable != null, 'tf-table musi istnieć');
  assertEq(typeof tfTable.rowActions, 'function');
});

test('Table row_actions builder renderuje tf-menu z pozycjami', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ camera_id: 'cam-1', name: 'A' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name' })],
    rowKeyField: 'camera_id',
    rowActions: [
      rowActionButton({ id: 'act-edit', label: 'Edytuj', actionId: 'edit_camera' }),
      rowActionButton({ id: 'act-del', label: 'Usuń', actionId: 'delete_camera', variant: 'destructive' }),
    ],
  })));
  const tfTable = el.querySelector('tf-table');
  // tf-table calls the builder with its own (transformed) rows.
  const menuEl = tfTable.rowActions(tfTable.rows[0], 0);
  const items = menuEl.querySelectorAll('tf-menu-item');
  assertEq(items.length, 2);
  assertEq(items[0].getAttribute('action'), 'act-edit');
  assertEq(items[0].textContent, 'Edytuj');
  assert(items[1].hasAttribute('danger'), 'akcja destructive musi mieć danger');
});

test('Table row_action klik niesie row_id i klucz wiersza do backend params', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ camera_id: 'cam-42', name: 'A' }] }],
    state_revision: 0, truncated: false,
  });
  const captured = [];
  const engine = new ComponentRenderer({
    store, eventDispatcher: dispatcherWithCapture(captured), locale: 'en-US',
  });
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name' })],
    rowKeyField: 'camera_id',
    rowActions: [rowActionButton({
      id: 'act-edit', label: 'Edytuj', actionId: 'edit_camera', params: { mode: 'inline' },
    })],
  })));
  const tfTable = el.querySelector('tf-table');
  const menuEl = tfTable.rowActions(tfTable.rows[0], 0);
  // tf-menu zamyka się we własnym listenerze na zbubblowanym tf-menu-select,
  // więc listener pozycji nie może zatrzymać propagacji.
  let reachedMenu = 0;
  menuEl.addEventListener('tf-menu-select', () => { reachedMenu += 1; });
  // Symulacja wyboru pozycji menu — emituje tf-menu-select jak tf-menu-item.
  const item = menuEl.querySelector('tf-menu-item');
  item.dispatchEvent(new (globalThis.CustomEvent)('tf-menu-select', {
    bubbles: true, detail: { action: 'act-edit' },
  }));
  assertEq(captured.length, 1);
  assertEq(captured[0].action_id, 'edit_camera');
  // Statyczne params zachowane + wzbogacone o klucz wiersza.
  assertEq(captured[0].params, { mode: 'inline', row_id: 'cam-42', camera_id: 'cam-42' });
  // Event musi dotrzeć do <tf-menu>, żeby menu się zamknęło — dokładnie raz.
  assertEq(reachedMenu, 1, 'tf-menu-select musi bubblować do tf-menu (brak stopPropagation)');
});

test('Table bez row_actions nie ustawia buildera akcji', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('rows'), value: [{ id: 'r1', name: 'A' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(TABLE_TAG, tableFields({
    columns: [col({ id: 'name' })],
  })));
  const tfTable = el.querySelector('tf-table');
  assertEq(tfTable.rowActions, null);
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
