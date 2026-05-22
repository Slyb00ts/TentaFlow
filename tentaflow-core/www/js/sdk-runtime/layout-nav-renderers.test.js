// =============================================================================
// Plik: sdk-runtime/layout-nav-renderers.test.js
// Opis: Testy NavTabs (Krok 3.3a-4).
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { NAV_TABS_TAG } from './layout-nav-renderers.js';

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
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

// ============================================================================
// NavTabs
// ============================================================================

test('NavTabs renders nav with role=tablist and variant class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [
        0,
        [
          { id: 't1', label: { kind: 'literal', value: 'One' }, locked: false },
          { id: 't2', label: { kind: 'literal', value: 'Two' }, locked: false },
        ],
      ],
      [1, { kind: 'literal', value: 't1' }],
      [2, 'underlined'],
      [3, false],
    ])
  );
  assertEq(el.tagName, 'NAV');
  assertEq(el.getAttribute('role'), 'tablist');
  assert(el.classList.contains('tf-nav-tabs'));
  assert(el.classList.contains('tf-nav-tabs--variant-underlined'));
});

test('NavTabs aria-selected follows active_id BindRef reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('active'), value: 't1' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [
        0,
        [
          { id: 't1', label: { kind: 'literal', value: 'A' }, locked: false },
          { id: 't2', label: { kind: 'literal', value: 'B' }, locked: false },
        ],
      ],
      [1, { kind: 'bound', path: PATH('active') }],
      [2, 'default'],
      [3, false],
    ])
  );
  const tabs = el.querySelectorAll('.tf-nav-tabs__tab');
  assertEq(tabs[0].getAttribute('aria-selected'), 'true');
  assertEq(tabs[0].getAttribute('tabindex'), '0');
  assertEq(tabs[1].getAttribute('aria-selected'), 'false');
  assertEq(tabs[1].getAttribute('tabindex'), '-1');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('active'), op: { kind: 'set', value: 't2' } }],
  });
  assertEq(tabs[0].getAttribute('aria-selected'), 'false');
  assertEq(tabs[1].getAttribute('aria-selected'), 'true');
});

test('NavTabs click dispatches select with item_id detail', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [
        0,
        [{ id: 'tab1', label: { kind: 'literal', value: 'X' }, locked: false }],
      ],
      [1, { kind: 'literal', value: 'tab1' }],
      [2, 'default'],
      [3, false],
    ])
  );
  let received = null;
  el.addEventListener('select', (e) => { received = e.detail; });
  el.querySelector('.tf-nav-tabs__tab').click();
  assertEq(received, { item_id: 'tab1' });
});

test('NavTabs locked tab is disabled and does not dispatch select', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [
        0,
        [{ id: 'tab1', label: { kind: 'literal', value: 'X' }, locked: true }],
      ],
      [1, { kind: 'literal', value: 'other' }],
      [2, 'default'],
      [3, false],
    ])
  );
  const tab = el.querySelector('.tf-nav-tabs__tab');
  assert(tab.hasAttribute('disabled'));
  assert(tab.classList.contains('tf-nav-tabs__tab--locked'));
  let received = null;
  el.addEventListener('select', (e) => { received = e.detail; });
  tab.click();
  assertEq(received, null);
});

test('NavTabs scroll_overflow=true adds scroll class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [0, []],
      [1, { kind: 'literal', value: '' }],
      [2, 'pills'],
      [3, true],
    ])
  );
  assert(el.classList.contains('tf-nav-tabs--scroll'));
});

test('NavTabs panel_id attribute exposed for router', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [
        0,
        [{ id: 't1', label: { kind: 'literal', value: 'X' }, locked: false, panel_id: 'p42' }],
      ],
      [1, { kind: 'literal', value: 't1' }],
      [2, 'default'],
      [3, false],
    ])
  );
  const tab = el.querySelector('.tf-nav-tabs__tab');
  assertEq(tab.getAttribute('data-nav-panel-id'), 'p42');
});

test('NavTabs icon present is rejected (deferred to chunk 3.3d)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [
          0,
          [{
            id: 't1',
            label: { kind: 'literal', value: 'X' },
            locked: false,
            icon: { kind: 'name', name: 'star' },
          }],
        ],
        [1, { kind: 'literal', value: 't1' }],
        [2, 'default'],
        [3, false],
      ])
    )
  );
});

test('NavTabs badge present is rejected (deferred to chunk 3.3d)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [
          0,
          [{
            id: 't1',
            label: { kind: 'literal', value: 'X' },
            locked: false,
            badge: { variant: 'solid', tone: 'primary', pulse: false },
          }],
        ],
        [1, { kind: 'literal', value: 't1' }],
        [2, 'default'],
        [3, false],
      ])
    )
  );
});

test('NavTabs rejects duplicate item id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [
          0,
          [
            { id: 'x', label: { kind: 'literal', value: 'A' }, locked: false },
            { id: 'x', label: { kind: 'literal', value: 'B' }, locked: false },
          ],
        ],
        [1, { kind: 'literal', value: 'x' }],
        [2, 'default'],
        [3, false],
      ])
    )
  );
});

test('NavTabs rejects item with unknown key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(NAV_TABS_TAG, [
        [
          0,
          [{
            id: 't1',
            label: { kind: 'literal', value: 'X' },
            locked: false,
            evil: true,
          }],
        ],
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

test('NavTabs keyboard ArrowRight moves focus to next tab', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(NAV_TABS_TAG, [
      [
        0,
        [
          { id: 'a', label: { kind: 'literal', value: 'A' }, locked: false },
          { id: 'b', label: { kind: 'literal', value: 'B' }, locked: false },
        ],
      ],
      [1, { kind: 'literal', value: 'a' }],
      [2, 'default'],
      [3, false],
    ])
  );
  document.body.appendChild(el);
  const tabs = el.querySelectorAll('.tf-nav-tabs__tab');
  tabs[0].focus();
  el.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  // happy-dom obsługuje focus + bubbling — assert focus moved.
  assertEq(document.activeElement, tabs[1]);
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
