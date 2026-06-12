// =============================================================================
// Plik: sdk-runtime/layout-nav-renderers.test.js
// Opis: NavTabs (0x010C) tests — renderer output is the <tf-tabs> + <tf-tab>
// dashboard web components.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-tabs.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { NAV_TABS_TAG } from './layout-nav-renderers.js';

// tf-tabs references bare ResizeObserver; the harness only exposes it on
// the happy-dom window object.
if (window.ResizeObserver && !globalThis.ResizeObserver) {
  globalThis.ResizeObserver = window.ResizeObserver;
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
    : { kind: 'key', value: s }
);

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
// NavTab item as FieldMap: 0=id, 1=label, 2=icon, 3=badge, 4=panel_id, 5=locked.
function navItem({ id, label, icon, badge, panelId, locked }) {
  const fields = [];
  if (id !== undefined) fields.push([0, id]);
  if (label !== undefined) fields.push([1, { kind: 'literal', value: label }]);
  if (icon !== undefined) fields.push([2, icon]);
  if (badge !== undefined) fields.push([3, badge]);
  if (panelId !== undefined) fields.push([4, panelId]);
  if (locked !== undefined) fields.push([5, locked]);
  return fields;
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

// ============================================================================
// NavTabs
// ============================================================================

test('NavTabs renders tf-tabs with mapped variant attribute', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [0, [
        navItem({ id: 't1', label: 'One', locked: false }),
        navItem({ id: 't2', label: 'Two', locked: false }),
      ]],
      [1, { kind: 'literal', value: 't1' }],
      [2, 'underlined'],
      [3, false],
    ])
  );
  assertEq(el.tagName, 'TF-TABS');
  // SDK 'underlined' maps onto the tf-tabs 'underline' variant.
  assertEq(el.getAttribute('variant'), 'underline');
  const tabs = el.querySelectorAll('tf-tab');
  assertEq(tabs.length, 2);
  assertEq(tabs[0].id, 't1');
  assertEq(tabs[1].id, 't2');
});

test('NavTabs variant pills maps to soft, default maps to solid', () => {
  setup();
  const engine = makeEngine();
  const pills = engine.render(
    comp(NAV_TABS_TAG, [
      [0, []], [1, { kind: 'literal', value: '' }], [2, 'pills'], [3, false],
    ])
  );
  assertEq(pills.getAttribute('variant'), 'soft');
  const solid = engine.render(
    comp(NAV_TABS_TAG, [
      [0, []], [1, { kind: 'literal', value: '' }], [2, 'default'], [3, false],
    ], { id: 'c2' })
  );
  assertEq(solid.getAttribute('variant'), 'solid');
});

test('NavTabs builds tab buttons with labels when connected', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(
    comp(NAV_TABS_TAG, [
      [0, [
        navItem({ id: 'a', label: 'Alpha', locked: false }),
        navItem({ id: 'b', label: 'Beta', locked: false }),
      ]],
      [1, { kind: 'literal', value: 'a' }],
      [2, 'default'],
      [3, false],
    ])
  ));
  const btns = el.querySelectorAll('button.tf-tab');
  assertEq(btns.length, 2);
  assertEq(btns[0].querySelector('.tf-tab-label').textContent, 'Alpha');
  assertEq(btns[1].querySelector('.tf-tab-label').textContent, 'Beta');
});

test('NavTabs active tab follows active_id BindRef reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('active'), value: 't1' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(NAV_TABS_TAG, [
      [0, [
        navItem({ id: 't1', label: 'A', locked: false }),
        navItem({ id: 't2', label: 'B', locked: false }),
      ]],
      [1, { kind: 'bound', path: PATH('active') }],
      [2, 'default'],
      [3, false],
    ])
  ));
  assertEq(el.getAttribute('value'), 't1');
  const btns = el.querySelectorAll('button.tf-tab');
  assert(btns[0].classList.contains('active'));
  assert(!btns[1].classList.contains('active'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('active'), op: { kind: 'set', value: 't2' } }],
  });
  assertEq(el.getAttribute('value'), 't2');
  assert(!btns[0].classList.contains('active'));
  assert(btns[1].classList.contains('active'));
});

test('NavTabs label BindRef updates rendered tab text', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'First' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(NAV_TABS_TAG, [
      [0, [
        [[0, 't1'], [1, { kind: 'bound', path: PATH('lbl') }], [5, false]],
      ]],
      [1, { kind: 'literal', value: 't1' }],
      [2, 'default'],
      [3, false],
    ])
  ));
  assertEq(el.querySelector('.tf-tab-label').textContent, 'First');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'Renamed' } }],
  });
  assertEq(el.querySelector('.tf-tab-label').textContent, 'Renamed');
});

test('NavTabs tab click dispatches select with item_id detail', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(
    comp(NAV_TABS_TAG, [
      [0, [
        navItem({ id: 'tab1', label: 'X', locked: false }),
        navItem({ id: 'tab2', label: 'Y', locked: false }),
      ]],
      [1, { kind: 'literal', value: 'tab1' }],
      [2, 'default'],
      [3, false],
    ])
  ));
  let received = null;
  el.addEventListener('select', (e) => { received = e.detail; });
  el.querySelectorAll('button.tf-tab')[1].click();
  assertEq(received, { item_id: 'tab2' });
  assertEq(el.getAttribute('value'), 'tab2');
});

test('NavTabs locked tab is disabled and does not dispatch select', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(
    comp(NAV_TABS_TAG, [
      [0, [
        navItem({ id: 'tab1', label: 'X', locked: false }),
        navItem({ id: 'tab2', label: 'Y', locked: true }),
      ]],
      [1, { kind: 'literal', value: 'tab1' }],
      [2, 'default'],
      [3, false],
    ])
  ));
  const lockedTab = el.querySelectorAll('tf-tab')[1];
  assert(lockedTab.hasAttribute('disabled'));
  assert(lockedTab.querySelector('button.tf-tab').hasAttribute('disabled'));
  let received = null;
  el.addEventListener('select', (e) => { received = e.detail; });
  lockedTab.querySelector('button.tf-tab').click();
  assertEq(received, null);
});

test('NavTabs badge maps to tf-tab count attribute', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [0, [navItem({ id: 't1', label: 'X', locked: false, badge: '7' })]],
      [1, { kind: 'literal', value: 't1' }],
      [2, 'default'],
      [3, false],
    ])
  );
  assertEq(el.querySelector('tf-tab').getAttribute('count'), '7');
});

test('NavTabs string icon maps to tf-tab icon attribute', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [0, [navItem({ id: 't1', label: 'X', locked: false, icon: 'star' })]],
      [1, { kind: 'literal', value: 't1' }],
      [2, 'default'],
      [3, false],
    ])
  );
  assertEq(el.querySelector('tf-tab').getAttribute('icon'), 'star');
});

test('NavTabs panel_id attribute exposed for router', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [0, [navItem({ id: 't1', label: 'X', locked: false, panelId: 'p42' })]],
      [1, { kind: 'literal', value: 't1' }],
      [2, 'default'],
      [3, false],
    ])
  );
  assertEq(el.querySelector('tf-tab').getAttribute('data-nav-panel-id'), 'p42');
});

test('NavTabs rejects duplicate item id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [0, [
          navItem({ id: 'x', label: 'A', locked: false }),
          navItem({ id: 'x', label: 'B', locked: false }),
        ]],
        [1, { kind: 'literal', value: 'x' }],
        [2, 'default'],
        [3, false],
      ])
    )
  );
});

test('NavTabs rejects empty item id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [0, [navItem({ id: '', label: 'A', locked: false })]],
        [1, { kind: 'literal', value: '' }],
        [2, 'default'],
        [3, false],
      ])
    )
  );
});

test('NavTabs rejects missing item label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [0, [navItem({ id: 't1', locked: false })]],
        [1, { kind: 'literal', value: 't1' }],
        [2, 'default'],
        [3, false],
      ])
    )
  );
});

test('NavTabs rejects missing active_id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [[0, []], [2, 'default'], [3, false]])
    )
  );
});

test('NavTabs rejects missing variant', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [0, []], [1, { kind: 'literal', value: '' }], [3, false],
      ])
    )
  );
});

test('NavTabs rejects missing scroll_overflow', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [0, []], [1, { kind: 'literal', value: '' }], [2, 'default'],
      ])
    )
  );
});

test('NavTabs rejects unknown component field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [0, []], [1, { kind: 'literal', value: '' }], [2, 'default'], [3, false],
        [42, 'rogue'],
      ])
    )
  );
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
