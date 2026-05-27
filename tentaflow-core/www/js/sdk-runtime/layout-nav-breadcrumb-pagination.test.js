// =============================================================================
// Plik: sdk-runtime/layout-nav-breadcrumb-pagination.test.js
// Opis: Testy Breadcrumb + Pagination (Krok 3.3a-5).
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  BREADCRUMB_TAG, PAGINATION_TAG,
} from './layout-nav-breadcrumb-pagination.js';

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
  typeof s === 'number'
    ? { kind: 'index', value: s }
    : { kind: 'key', value: s });

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

// ============================================================================
// Breadcrumb
// ============================================================================

test('Breadcrumb renders nav role + ol list with items + separators', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BREADCRUMB_TAG, [
      [
        0,
        [
          { label: { kind: 'literal', value: 'Home' }, is_current: false },
          { label: { kind: 'literal', value: 'Reports' }, is_current: false },
          { label: { kind: 'literal', value: 'Q4' }, is_current: true },
        ],
      ],
      [1, 'chevron'],
    ])
  );
  assertEq(el.tagName, 'NAV');
  assertEq(el.getAttribute('aria-label'), 'Breadcrumb');
  const items = el.querySelectorAll('.tf-breadcrumb__item');
  assertEq(items.length, 3);
  const seps = el.querySelectorAll('.tf-breadcrumb__separator');
  assertEq(seps.length, 2);
  assertEq(items[2].querySelector('.tf-breadcrumb__link').getAttribute('aria-current'), 'page');
});

test('Breadcrumb current item renders as span, others as links', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BREADCRUMB_TAG, [
      [
        0,
        [
          { label: { kind: 'literal', value: 'A' }, action_id: 'a-click', is_current: false },
          { label: { kind: 'literal', value: 'B' }, is_current: true },
        ],
      ],
      [1, 'slash'],
    ])
  );
  const items = el.querySelectorAll('.tf-breadcrumb__item');
  assertEq(items[0].querySelector('.tf-breadcrumb__link').tagName, 'A');
  assertEq(items[1].querySelector('.tf-breadcrumb__link').tagName, 'SPAN');
});

test('Breadcrumb link click dispatches click with action_id', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BREADCRUMB_TAG, [
      [
        0,
        [
          { label: { kind: 'literal', value: 'A' }, action_id: 'navA', is_current: false },
        ],
      ],
      [1, 'dot'],
    ])
  );
  let received = null;
  el.addEventListener('click', (e) => { received = e.detail; });
  el.querySelector('a').click();
  assertEq(received, { action_id: 'navA', item_index: 0 });
});

test('Breadcrumb default max_items=5 collapses long trails with ellipsis', () => {
  setup();
  const engine = makeEngine();
  const items = [];
  for (let i = 0; i < 8; i++) {
    items.push({ label: { kind: 'literal', value: `L${i}` }, is_current: i === 7 });
  }
  const el = engine.render(
    comp(BREADCRUMB_TAG, [[0, items], [1, 'chevron']])
  );
  // 1 first + ellipsis + last 3 (5-2=3) = 5 list items shown
  const lis = el.querySelectorAll('.tf-breadcrumb__list > .tf-breadcrumb__item');
  assertEq(lis.length, 5);
  assert(el.querySelector('.tf-breadcrumb__item--ellipsis') != null);
});

test('Breadcrumb item icon present is rejected (defer 3.3d)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BREADCRUMB_TAG, [
        [
          0,
          [{ label: { kind: 'literal', value: 'X' }, is_current: false, icon: { kind: 'name', name: 'star' } }],
        ],
        [1, 'chevron'],
      ])
    )
  );
});

test('Breadcrumb item local_action present is rejected (defer 3.6)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BREADCRUMB_TAG, [
        [
          0,
          [{ label: { kind: 'literal', value: 'X' }, is_current: false, local_action: { kind: 'navigate' } }],
        ],
        [1, 'chevron'],
      ])
    )
  );
});

test('Breadcrumb rejects unknown item key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BREADCRUMB_TAG, [
        [
          0,
          [{ label: { kind: 'literal', value: 'X' }, is_current: false, evil: true }],
        ],
        [1, 'chevron'],
      ])
    )
  );
});

// ============================================================================
// Pagination
// ============================================================================

test('Pagination compact variant shows N/total', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('cp'), value: 3 },
      { path: PATH('tp'), value: 10 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'bound', path: PATH('cp') }],
      [1, { kind: 'bound', path: PATH('tp') }],
      [2, 'compact'],
      [3, true],
    ])
  );
  const middle = el.querySelector('.tf-pagination__middle');
  assertEq(middle.textContent, '3 / 10');
  assertEq(el.querySelector('.tf-pagination__summary').textContent, 'Strona 3 z 10');
});

test('Pagination prev/next click dispatches change with target page', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('cp'), value: 5 },
      { path: PATH('tp'), value: 10 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'bound', path: PATH('cp') }],
      [1, { kind: 'bound', path: PATH('tp') }],
      [2, 'compact'],
      [3, false],
    ])
  );
  const changes = [];
  el.addEventListener('change', (e) => changes.push(e.detail));
  el.querySelector('.tf-pagination__prev').click();
  el.querySelector('.tf-pagination__next').click();
  assertEq(changes, [{ page: 4 }, { page: 6 }]);
});

test('Pagination prev disabled on page=1, next disabled on page=total', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('cp'), value: 1 },
      { path: PATH('tp'), value: 5 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'bound', path: PATH('cp') }],
      [1, { kind: 'bound', path: PATH('tp') }],
      [2, 'compact'],
      [3, false],
    ])
  );
  assert(el.querySelector('.tf-pagination__prev').hasAttribute('disabled'));
  assert(!el.querySelector('.tf-pagination__next').hasAttribute('disabled'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('cp'), op: { kind: 'set', value: 5 } }],
  });
  assert(!el.querySelector('.tf-pagination__prev').hasAttribute('disabled'));
  assert(el.querySelector('.tf-pagination__next').hasAttribute('disabled'));
});

test('Pagination full variant renders numeric page buttons window', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('cp'), value: 1 },
      { path: PATH('tp'), value: 5 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'bound', path: PATH('cp') }],
      [1, { kind: 'bound', path: PATH('tp') }],
      [2, 'full'],
      [3, false],
    ])
  );
  const buttons = el.querySelectorAll('.tf-pagination__page');
  assertEq(buttons.length, 5);
  assertEq(buttons[0].textContent, '1');
  assertEq(buttons[0].getAttribute('aria-current'), 'page');
});

test('Pagination full variant collapses with ellipsis when total > 7', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('cp'), value: 10 },
      { path: PATH('tp'), value: 20 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'bound', path: PATH('cp') }],
      [1, { kind: 'bound', path: PATH('tp') }],
      [2, 'full'],
      [3, false],
    ])
  );
  // current=10: pokazuje 1, …, 9, 10, 11, …, 20
  const middle = el.querySelector('.tf-pagination__middle');
  const ellipses = middle.querySelectorAll('.tf-pagination__ellipsis');
  assertEq(ellipses.length, 2);
});

test('Pagination input variant renders number input and emits change on commit', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('cp'), value: 3 },
      { path: PATH('tp'), value: 10 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'bound', path: PATH('cp') }],
      [1, { kind: 'bound', path: PATH('tp') }],
      [2, 'input'],
      [3, false],
    ])
  );
  const input = el.querySelector('.tf-pagination__input');
  assertEq(input.value, '3');
  assertEq(input.getAttribute('max'), '10');
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  input.value = '7';
  input.dispatchEvent(new window.Event('change'));
  assertEq(received, { page: 7 });
});

test('Pagination show_summary=false hides summary', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('cp'), value: 1 },
      { path: PATH('tp'), value: 5 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'bound', path: PATH('cp') }],
      [1, { kind: 'bound', path: PATH('tp') }],
      [2, 'compact'],
      [3, false],
    ])
  );
  assertEq(el.querySelector('.tf-pagination__summary'), null);
});

test('Pagination rejects missing fields', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(PAGINATION_TAG, [[2, 'compact'], [3, true]])));
  assertThrows(() => engine.render(
    comp(PAGINATION_TAG, [
      [0, { kind: 'literal', value: 1 }],
      [1, { kind: 'literal', value: 5 }],
      [3, true],
    ])
  ));
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
