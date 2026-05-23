// =============================================================================
// Plik: sdk-runtime/data-diff-dl-json-renderer.test.js
// Opis: Testy Diff (0x021F), DataDefinitionList (0x0221), JsonViewer
// (0x0222) — chunk 3.3d-14.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { DIFF_TAG, DATA_DEFINITION_LIST_TAG, JSON_VIEWER_TAG } from './data-diff-dl-json-renderer.js';

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
const LIT = (value) => ({ kind: 'literal', value });

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
// Diff
// ============================================================================

function diffFields({
  beforePath = PATH('before'), afterPath = PATH('after'),
  variant = 'unified', language = null, wordWrap = false, showLineNumbers = true,
} = {}) {
  const f = [[0, beforePath], [1, afterPath], [2, variant]];
  if (language != null) f.push([3, language]);
  f.push([4, wordWrap], [5, showLineNumbers]);
  return f;
}

test('Diff unified pokazuje add/del/equal rows', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('before'), value: 'a\nb\nc' },
      { path: PATH('after'),  value: 'a\nB\nc' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(DIFF_TAG, diffFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('.tf-diff__row--add').length, 1);
  assertEq(el.querySelectorAll('.tf-diff__row--del').length, 1);
  assertEq(el.querySelectorAll('.tf-diff__row--equal').length, 2);
});

test('Diff split renderuje dwie kolumny', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('before'), value: 'foo' },
      { path: PATH('after'),  value: 'bar' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(DIFF_TAG, diffFields({ variant: 'split' })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-diff__col--before') != null);
  assert(el.querySelector('.tf-diff__col--after')  != null);
  assertEq(el.querySelectorAll('.tf-diff__row--gap').length, 2);  // 1 add no-before + 1 del no-after
});

test('Diff reaguje na patch before/after', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('before'), value: 'x' },
      { path: PATH('after'),  value: 'x' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(DIFF_TAG, diffFields()));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('.tf-diff__row--add').length, 0);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('after'), op: { kind: 'set', value: 'x\ny' } }],
  });
  assertEq(el.querySelectorAll('.tf-diff__row--add').length, 1);
});

test('Diff show_line_numbers=false ukrywa numery', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('before'), value: 'a' },
      { path: PATH('after'),  value: 'a' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(DIFF_TAG, diffFields({ showLineNumbers: false })));
  document.body.appendChild(el);
  assert(!el.classList.contains('tf-diff--line-numbers'));
});

test('Diff >5000 lines renderuje overflow message', () => {
  setup();
  const store = makeStore();
  const big = Array.from({ length: 5001 }, (_, i) => `line ${i}`).join('\n');
  store.applySnapshot({
    entries: [
      { path: PATH('before'), value: '' },
      { path: PATH('after'),  value: big },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(DIFF_TAG, diffFields()));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-diff__overflow') != null);
  assertEq(el.querySelectorAll('.tf-diff__row').length, 0);
});

test('Diff odrzuca unknown variant', () => {
  setup();
  const store = makeStore();
  const engine = makeEngine(store);
  assertThrows(() => engine.render(comp(DIFF_TAG, diffFields({ variant: 'side-by-side' }))));
});

test('Diff content używa textContent (no XSS via patched text)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('before'), value: '' },
      { path: PATH('after'),  value: '<script>alert(1)</script>' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(DIFF_TAG, diffFields()));
  document.body.appendChild(el);
  assert(el.querySelector('script') == null, 'XSS: script tag should not be parsed');
  assertEq(el.querySelector('.tf-diff__content').textContent, '<script>alert(1)</script>');
});

// ============================================================================
// DataDefinitionList
// ============================================================================

function dlFields({
  items = [
    [[0, LIT('Name')], [1, LIT('Alice')]],
    [[0, LIT('Email')], [1, LIT('a@b.c')]],
  ],
  layout = 'stacked',
} = {}) {
  return [[0, items], [1, layout]];
}

test('DataDefinitionList renderuje <dl> z dt/dd parami', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DATA_DEFINITION_LIST_TAG, dlFields()));
  document.body.appendChild(el);
  assertEq(el.tagName, 'DL');
  assertEq(el.querySelectorAll('dt.tf-dl__term').length, 2);
  assertEq(el.querySelectorAll('dd.tf-dl__definition').length, 2);
  assertEq(el.querySelector('dt').textContent, 'Name');
});

test('DataDefinitionList layout=two_column ma odpowiednią klasę', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DATA_DEFINITION_LIST_TAG, dlFields({ layout: 'two_column' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-dl--layout-two_column'));
});

test('DataDefinitionList reaguje na patch BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('name'), value: 'Alice' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const fields = dlFields({ items: [[[0, LIT('Name')], [1, { kind: 'bound', path: PATH('name') }]]] });
  const el = engine.render(comp(DATA_DEFINITION_LIST_TAG, fields));
  document.body.appendChild(el);
  assertEq(el.querySelector('dd').textContent, 'Alice');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('name'), op: { kind: 'set', value: 'Bob' } }] });
  assertEq(el.querySelector('dd').textContent, 'Bob');
});

test('DataDefinitionList odrzuca explicit null items', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(DATA_DEFINITION_LIST_TAG, [[0, null], [1, 'stacked']])));
});

test('DataDefinitionList odrzuca DefItem z extra key', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(DATA_DEFINITION_LIST_TAG, dlFields({
    items: [[[0, LIT('a')], [1, LIT('b')], [2, LIT('extra')]]],
  }))));
});

test('DataDefinitionList odrzuca DefItem.term który nie jest BindRef', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(DATA_DEFINITION_LIST_TAG, dlFields({
    items: [[[0, 'plain'], [1, LIT('b')]]],
  }))));
});

// ============================================================================
// JsonViewer
// ============================================================================

function jvFields({
  valuePath = PATH('json'), collapsedDepth = 2,
  maxHeightPx = 400, searchable = false,
} = {}) {
  const f = [[0, valuePath]];
  if (collapsedDepth != null) f.push([1, collapsedDepth]);
  f.push([2, maxHeightPx], [3, searchable]);
  return f;
}

test('JsonViewer renderuje tree z obiektu', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('json'), value: { a: 1, b: { c: 'hello' } } }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-json-viewer__summary').textContent, 'Object(2)');
  assert(el.querySelector('.tf-json-viewer__value--number') != null);
  assert(el.querySelector('.tf-json-viewer__value--string') != null);
});

test('JsonViewer collapsed_depth=0 zwija od razu', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('json'), value: { a: { b: 1 } } }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields({ collapsedDepth: 0 })));
  document.body.appendChild(el);
  // Top-level toggle aria-expanded=false (zwinięty).
  assertEq(el.querySelector('.tf-json-viewer__toggle').getAttribute('aria-expanded'), 'false');
});

test('JsonViewer toggle expand/collapse', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('json'), value: { a: { b: 1 } } }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields({ collapsedDepth: 0 })));
  document.body.appendChild(el);
  const toggle = el.querySelector('.tf-json-viewer__toggle');
  toggle.click();
  assertEq(el.querySelector('.tf-json-viewer__toggle').getAttribute('aria-expanded'), 'true');
});

test('JsonViewer searchable renderuje input', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('json'), value: { a: 1 } }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields({ searchable: true })));
  document.body.appendChild(el);
  assert(el.querySelector('input[type="search"]') != null);
});

test('JsonViewer search filtruje i auto-expanduje ancestor branch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('json'), value: { a: { hidden: { target: 'needle' } }, b: 'other' } }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields({ searchable: true, collapsedDepth: 0 })));
  document.body.appendChild(el);
  const input = el.querySelector('input[type="search"]');
  input.value = 'needle';
  input.dispatchEvent(new globalThis.Event('input'));
  // "needle" node + ancestors: root obj, "a" obj, "hidden" obj, "target" scalar = 4.
  // "b" subtree should be filtered out.
  const nodes = el.querySelectorAll('.tf-json-viewer__node');
  assertEq(nodes.length, 4);
  // Ancestors auto-expanded despite collapsedDepth=0.
  const toggles = el.querySelectorAll('.tf-json-viewer__toggle');
  for (const t of toggles) assertEq(t.getAttribute('aria-expanded'), 'true');
});

test('JsonViewer odrzuca explicit null collapsed_depth', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(JSON_VIEWER_TAG, [[0, PATH('json')], [1, null], [2, 400], [3, false]])));
});

test('JsonViewer odrzuca max_height_px=0', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(JSON_VIEWER_TAG, jvFields({ maxHeightPx: 0 }))));
});

test('JsonViewer default collapsed_depth=2 gdy nie podano', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('json'), value: { a: 1 } }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const fields = [[0, PATH('json')], [2, 400], [3, false]];
  const el = engine.render(comp(JSON_VIEWER_TAG, fields));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-json-viewer__toggle').getAttribute('aria-expanded'), 'true');
});

test('JsonViewer reaguje na patch value_path', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('json'), value: { x: 1 } }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-json-viewer__summary').textContent, 'Object(1)');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('json'), op: { kind: 'set', value: { x: 1, y: 2, z: 3 } } }],
  });
  assertEq(el.querySelector('.tf-json-viewer__summary').textContent, 'Object(3)');
});

test('JsonViewer string scalar — addon-controlled, no XSS', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('json'), value: { k: '<img src=x onerror=alert(1)>' } }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields()));
  document.body.appendChild(el);
  assert(el.querySelector('img') == null, 'XSS img injection rejected');
});

test('JsonViewer empty value renders (no data)', () => {
  setup();
  const store = makeStore();
  const engine = makeEngine(store);
  const el = engine.render(comp(JSON_VIEWER_TAG, jvFields()));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-json-viewer__empty').textContent, '(no data)');
});

// ============================================================================
const failed = results.filter((r) => !r.ok);
console.log(`diff+dl+json tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
