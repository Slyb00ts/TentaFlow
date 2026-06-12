// =============================================================================
// Plik: sdk-runtime/layout-nav-breadcrumb-pagination.test.js
// Opis: Breadcrumb (0x0110) + Pagination (0x0111) tests. Breadcrumb renders
// through the <tf-breadcrumb> dashboard web component.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-breadcrumb.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  BREADCRUMB_TAG, PAGINATION_TAG,
} from './layout-nav-breadcrumb-pagination.js';

// tf-breadcrumb references bare MutationObserver; the harness only exposes
// it on the happy-dom window object.
if (window.MutationObserver && !globalThis.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}

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

test('Breadcrumb renders tf-breadcrumb with items, separators and aria-current', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BREADCRUMB_TAG, [
      [
        0,
        [
          { label: { kind: 'literal', value: 'Home' }, action_id: 'go-home', is_current: false },
          { label: { kind: 'literal', value: 'Reports' }, action_id: 'go-reports', is_current: false },
          { label: { kind: 'literal', value: 'Q4' }, is_current: true },
        ],
      ],
      [1, 'chevron'],
    ])
  );
  assertEq(el.tagName, 'TF-BREADCRUMB');
  assertEq(el.getAttribute('data-separator'), 'chevron');
  document.body.appendChild(el);
  const nav = el.querySelector('nav.tf-breadcrumb');
  assertEq(nav.getAttribute('aria-label'), 'Breadcrumb');
  const items = nav.querySelectorAll('.tf-breadcrumb-item');
  assertEq(items.length, 3);
  assertEq(nav.querySelectorAll('.tf-breadcrumb-sep').length, 2);
  assertEq(items[2].getAttribute('aria-current'), 'page');
  assertEq(items[2].textContent, 'Q4');
});

test('Breadcrumb current item renders as span, action items as links', () => {
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
  document.body.appendChild(el);
  const items = el.querySelectorAll('.tf-breadcrumb-item');
  assertEq(items[0].tagName, 'A');
  assertEq(items[1].tagName, 'SPAN');
});

test('Breadcrumb link click dispatches click with action_id and item_index', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BREADCRUMB_TAG, [
      [
        0,
        [
          { label: { kind: 'literal', value: 'A' }, action_id: 'navA', is_current: false },
          { label: { kind: 'literal', value: 'B' }, action_id: 'navB', is_current: false },
          { label: { kind: 'literal', value: 'C' }, is_current: true },
        ],
      ],
      [1, 'dot'],
    ])
  );
  document.body.appendChild(el);
  const received = [];
  el.addEventListener('click', (e) => { received.push(e.detail); });
  const anchors = el.querySelectorAll('a.tf-breadcrumb-item');
  anchors[1].click();
  anchors[0].click();
  // Only our CustomEvent (with detail) reaches the root — the native
  // MouseEvent is stopped in the capture-phase delegate.
  assertEq(received, [
    { action_id: 'navB', item_index: 1 },
    { action_id: 'navA', item_index: 0 },
  ]);
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
  document.body.appendChild(el);
  // 1 first + ellipsis + last 3 (5-2=3) = 5 items shown
  const shown = el.querySelectorAll('.tf-breadcrumb-item');
  assertEq(shown.length, 5);
  assertEq(shown[0].textContent, 'L0');
  assertEq(shown[1].textContent, '…');
  assertEq(shown[4].textContent, 'L7');
});

test('Breadcrumb label BindRef updates rendered text reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('crumb'), value: 'Draft' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(BREADCRUMB_TAG, [
      [0, [{ label: { kind: 'bound', path: PATH('crumb') }, is_current: true }]],
      [1, 'chevron'],
    ])
  );
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-breadcrumb-item').textContent, 'Draft');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('crumb'), op: { kind: 'set', value: 'Published' } }],
  });
  assertEq(el.querySelector('.tf-breadcrumb-item').textContent, 'Published');
});

test('Breadcrumb item icon and local_action are gracefully ignored', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BREADCRUMB_TAG, [
      [
        0,
        [{
          label: { kind: 'literal', value: 'X' },
          is_current: true,
          icon: { kind: 'name', name: 'star' },
          local_action: { kind: 'navigate' },
        }],
      ],
      [1, 'chevron'],
    ])
  );
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-breadcrumb-item').textContent, 'X');
});

test('Breadcrumb rejects invalid separator', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(BREADCRUMB_TAG, [
        [0, [{ label: { kind: 'literal', value: 'X' }, is_current: true }]],
        [1, 'arrow'],
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
