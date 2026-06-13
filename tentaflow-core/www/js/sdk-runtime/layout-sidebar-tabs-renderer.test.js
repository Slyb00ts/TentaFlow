// =============================================================================
// Plik: sdk-runtime/layout-sidebar-tabs-renderer.test.js
// Opis: Sidebar (0x010A) + Tabs (0x010B) renderer tests. Covers happy-path
// render (items/tabs/children + slot mounting via data-slot-id), FieldMap
// validation, BigInt tolerance, select/nav event emission + payload shape and
// reactive bind updates.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-tabs.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import {
  registerLayoutSidebarTabsRenderers,
  SIDEBAR_TAG,
  TABS_TAG,
} from './layout-sidebar-tabs-renderer.js';
import { SlotManager } from './slot-manager.js';

// tf-tabs references bare ResizeObserver; the harness only exposes it on the
// happy-dom window object.
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
const LIT = (value) => ({ kind: 'literal', value });
const BOUND = (...segs) => ({ kind: 'bound', path: PATH(...segs) });

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
  registerLayoutSidebarTabsRenderers();
  document.body.innerHTML = '';
}
function mount(el) { document.body.appendChild(el); return el; }

// SidebarItem FieldMap: 0=id, 1=icon, 2=label, 3=badge, 4=active_path,
// 5=action_id, 6=local_action, 7=children.
function sideItem({ id, icon, label, badge, activePath, actionId, localAction, children }) {
  const f = [];
  if (id !== undefined) f.push([0, id]);
  if (icon !== undefined) f.push([1, icon]);
  if (label !== undefined) f.push([2, label]);
  if (badge !== undefined) f.push([3, badge]);
  if (activePath !== undefined) f.push([4, activePath]);
  if (actionId !== undefined) f.push([5, actionId]);
  if (localAction !== undefined) f.push([6, localAction]);
  if (children !== undefined) f.push([7, children]);
  return f;
}
// TabItem FieldMap: 0=id, 1=label, 2=icon, 3=badge, 4=locked, 5=template.
function tabItem({ id, label, icon, badge, locked, template }) {
  const f = [];
  if (id !== undefined) f.push([0, id]);
  if (label !== undefined) f.push([1, label]);
  if (icon !== undefined) f.push([2, icon]);
  if (badge !== undefined) f.push([3, badge]);
  if (locked !== undefined) f.push([4, locked]);
  if (template !== undefined) f.push([5, template]);
  return f;
}

// ============================================================================
// Sidebar
// ============================================================================

test('Sidebar renders nav with items and slot regions', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SIDEBAR_TAG, [
    [0, 'side-header'],
    [1, [
      sideItem({ id: 'home', label: LIT('Home') }),
      sideItem({ id: 'settings', label: LIT('Settings') }),
    ]],
    [2, 'side-footer'],
  ]));
  assertEq(el.tagName, 'NAV');
  assert(el.classList.contains('tf-sidebar'));
  const header = el.querySelector('[data-slot-id="side-header"]');
  const footer = el.querySelector('[data-slot-id="side-footer"]');
  assert(header && header.classList.contains('tf-sidebar__header'));
  assert(footer && footer.classList.contains('tf-sidebar__footer'));
  const links = el.querySelectorAll('.tf-sidebar__link');
  assertEq(links.length, 2);
  assertEq(links[0].dataset.itemId, 'home');
  assertEq(links[0].querySelector('.tf-sidebar__label').textContent, 'Home');
  assertEq(links[1].querySelector('.tf-sidebar__label').textContent, 'Settings');
});

test('Sidebar header slot accepts SlotManager content', () => {
  setup();
  const store = makeStore();
  const engine = makeEngine(store);
  const sm = new SlotManager({ store, componentRenderer: engine });
  const el = mount(engine.render(comp(SIDEBAR_TAG, [
    [0, 'hdr'],
    [1, [sideItem({ id: 'a', label: LIT('A') })]],
  ])));
  const slotEl = el.querySelector('[data-slot-id="hdr"]');
  assert(slotEl, 'header slot container present');
  sm.registerSlot('hdr', slotEl);
  assert(sm.hasSlot('hdr'), 'header slot registered');
  // An empty Sidebar fragment (a registered renderer) mounts into the slot.
  sm.handleSlotContent({ slot_id: 'hdr', fragment: comp(SIDEBAR_TAG, [[1, []]]), state_overlay: null });
  assert(slotEl.firstChild, 'fragment mounted into header slot');
  sm.destroy();
});

test('Sidebar renders one level of nested children', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SIDEBAR_TAG, [
    [1, [
      sideItem({
        id: 'parent', label: LIT('Parent'),
        children: [
          sideItem({ id: 'child1', label: LIT('Child 1') }),
          sideItem({ id: 'child2', label: LIT('Child 2') }),
        ],
      }),
    ]],
  ]));
  const sub = el.querySelector('.tf-sidebar__sub');
  assert(sub, 'nested list present');
  const subLinks = sub.querySelectorAll('.tf-sidebar__link');
  assertEq(subLinks.length, 2);
  assertEq(subLinks[0].dataset.itemId, 'child1');
});

test('Sidebar rejects 2-level nesting', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SIDEBAR_TAG, [
    [1, [
      sideItem({
        id: 'p', label: LIT('P'),
        children: [
          sideItem({
            id: 'c', label: LIT('C'),
            children: [sideItem({ id: 'g', label: LIT('G') })],
          }),
        ],
      }),
    ]],
  ])));
});

test('Sidebar active_path drives active state reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('homeActive'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'home', label: LIT('Home'), activePath: PATH('homeActive') })]],
  ])));
  const link = el.querySelector('.tf-sidebar__link');
  assert(link.classList.contains('is-active'));
  assertEq(link.getAttribute('aria-current'), 'page');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('homeActive'), op: { kind: 'set', value: false } }],
  });
  assert(!link.classList.contains('is-active'));
  assert(!link.hasAttribute('aria-current'));
});

test('Sidebar label BindRef updates reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'First' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'x', label: BOUND('lbl') })]],
  ])));
  assertEq(el.querySelector('.tf-sidebar__label').textContent, 'First');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'Renamed' } }],
  });
  assertEq(el.querySelector('.tf-sidebar__label').textContent, 'Renamed');
});

test('Sidebar item click emits select on ROOT with item_id + action_id', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'home', label: LIT('Home'), actionId: 'go_home' })]],
  ])));
  // ComponentRenderer installs the addon's `select` handler on the returned root
  // element, so the integration path requires the event to fire ON THE ROOT.
  let received = null;
  el.addEventListener('select', (e) => {
    if (e.__tfReemit) received = e.detail;
  });
  // The child must NOT receive a non-bubbling event dispatched on root.
  let childReceived = false;
  el.querySelector('.tf-sidebar__link').addEventListener('select', () => { childReceived = true; });
  el.querySelector('.tf-sidebar__link').click();
  assertEq(received, { item_id: 'home', action_id: 'go_home' });
  assert(!childReceived, 'select must fire on root, not the child button');
});

test('Sidebar named IconRef renders sprite reference', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'x', label: LIT('X'), icon: { kind: 'named', name: 'home' } })]],
  ]));
  const use = el.querySelector('.tf-sidebar__icon use');
  assert(use, 'icon use element present');
  assertEq(use.getAttribute('href'), '#i-home');
});

test('Sidebar badge from InlineBadge count BindRef', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'x', label: LIT('X'), badge: { count: LIT('9') } })]],
  ]));
  assertEq(el.querySelector('.tf-sidebar__badge').textContent, '9');
});

test('Sidebar badge count BindRef updates reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('cnt'), value: 3 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'x', label: LIT('X'), badge: { count: BOUND('cnt') } })]],
  ])));
  assertEq(el.querySelector('.tf-sidebar__badge').textContent, '3');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('cnt'), op: { kind: 'set', value: 12 } }],
  });
  assertEq(el.querySelector('.tf-sidebar__badge').textContent, '12');
});

test('Sidebar collapsed BindRef toggles modifier reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('coll'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'x', label: LIT('X') })]],
    [3, BOUND('coll')],
  ])));
  assert(!el.classList.contains('tf-sidebar--collapsed'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('coll'), op: { kind: 'set', value: true } }],
  });
  assert(el.classList.contains('tf-sidebar--collapsed'));
});

test('Sidebar rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SIDEBAR_TAG, [
    [1, []],
    [9, 'nope'],
  ])));
});

test('Sidebar rejects unknown SidebarItem key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SIDEBAR_TAG, [
    [1, [[[0, 'x'], [2, LIT('X')], [11, 'bad']]]],
  ])));
});

test('Sidebar rejects duplicate item id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SIDEBAR_TAG, [
    [1, [
      sideItem({ id: 'dup', label: LIT('A') }),
      sideItem({ id: 'dup', label: LIT('B') }),
    ]],
  ])));
});

test('Sidebar rejects empty item id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: '', label: LIT('A') })]],
  ])));
});

test('Sidebar rejects action_id + local_action together', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({
      id: 'x', label: LIT('X'),
      actionId: 'a', localAction: { kind: 'navigate', panel_id: 'p' },
    })]],
  ])));
});

// ============================================================================
// Tabs
// ============================================================================

test('Tabs renders tf-tabs strip + content slot region', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TABS_TAG, [
    [0, 'underlined'],
    [1, [
      tabItem({ id: 't1', label: LIT('One'), locked: false }),
      tabItem({ id: 't2', label: LIT('Two'), locked: false }),
    ]],
    [2, LIT('t1')],
    [3, 'tab-content'],
    [4, 'default'],
  ]));
  assert(el.classList.contains('tf-tabs-container'));
  const tfTabs = el.querySelector('tf-tabs');
  assert(tfTabs, 'tf-tabs present');
  assertEq(tfTabs.getAttribute('variant'), 'underline');
  assertEq(tfTabs.querySelectorAll('tf-tab').length, 2);
  const content = el.querySelector('[data-slot-id="tab-content"]');
  assert(content && content.classList.contains('tf-tabs-content'));
});

test('Tabs variant mapping pills->soft, default/boxed->solid', () => {
  setup();
  const engine = makeEngine();
  const mk = (variant, id) => engine.render(comp(TABS_TAG, [
    [0, variant], [1, []], [2, LIT('')], [3, 'cs'], [4, 'default'],
  ], { id }));
  assertEq(mk('pills', 'a').querySelector('tf-tabs').getAttribute('variant'), 'soft');
  assertEq(mk('default', 'b').querySelector('tf-tabs').getAttribute('variant'), 'solid');
  assertEq(mk('boxed', 'c').querySelector('tf-tabs').getAttribute('variant'), 'solid');
});

test('Tabs content slot accepts SlotManager content', () => {
  setup();
  const store = makeStore();
  const engine = makeEngine(store);
  const sm = new SlotManager({ store, componentRenderer: engine });
  const el = mount(engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [tabItem({ id: 't1', label: LIT('One'), locked: false })]],
    [2, LIT('t1')],
    [3, 'tab-content'],
    [4, 'default'],
  ])));
  const slotEl = el.querySelector('[data-slot-id="tab-content"]');
  assert(slotEl, 'content slot container present');
  sm.registerSlot('tab-content', slotEl);
  assert(sm.hasSlot('tab-content'), 'content slot registered');
  sm.handleSlotContent({ slot_id: 'tab-content', fragment: comp(SIDEBAR_TAG, [[1, []]]), state_overlay: null });
  assert(slotEl.firstChild, 'fragment mounted into content slot');
  sm.destroy();
});

test('Tabs active_id BindRef drives active tab reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('active'), value: 't1' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [
      tabItem({ id: 't1', label: LIT('A'), locked: false }),
      tabItem({ id: 't2', label: LIT('B'), locked: false }),
    ]],
    [2, BOUND('active')],
    [3, 'cs'],
    [4, 'default'],
  ])));
  const tfTabs = el.querySelector('tf-tabs');
  assertEq(tfTabs.getAttribute('value'), 't1');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('active'), op: { kind: 'set', value: 't2' } }],
  });
  assertEq(tfTabs.getAttribute('value'), 't2');
});

test('Tabs label BindRef updates rendered tab text', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'First' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [tabItem({ id: 't1', label: BOUND('lbl'), locked: false })]],
    [2, LIT('t1')],
    [3, 'cs'],
    [4, 'default'],
  ])));
  assertEq(el.querySelector('.tf-tab-label').textContent, 'First');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'Renamed' } }],
  });
  assertEq(el.querySelector('.tf-tab-label').textContent, 'Renamed');
});

test('Tabs tab-switch emits select on ROOT with item_id detail', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [
      tabItem({ id: 'tab1', label: LIT('X'), locked: false }),
      tabItem({ id: 'tab2', label: LIT('Y'), locked: false }),
    ]],
    [2, LIT('tab1')],
    [3, 'cs'],
    [4, 'default'],
  ])));
  // The addon's `select` handler is attached to the returned root container, so
  // the converted event must originate there, not on the nested tf-tabs.
  let received = null;
  el.addEventListener('select', (e) => { if (e.__tfReemit) received = e.detail; });
  const tfTabs = el.querySelector('tf-tabs');
  let childReceived = false;
  tfTabs.addEventListener('select', (e) => { if (e.__tfReemit) childReceived = true; });
  el.querySelectorAll('button.tf-tab')[1].click();
  assertEq(received, { item_id: 'tab2' });
  assert(!childReceived, 'select must fire on root, not the nested tf-tabs');
  assertEq(tfTabs.getAttribute('value'), 'tab2');
});

test('Tabs badge count BindRef updates the tab count reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('cnt'), value: 2 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [tabItem({ id: 't1', label: LIT('X'), locked: false, badge: { count: BOUND('cnt') } })]],
    [2, LIT('t1')],
    [3, 'cs'],
    [4, 'default'],
  ])));
  assertEq(el.querySelector('tf-tab').getAttribute('count'), '2');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('cnt'), op: { kind: 'set', value: 5 } }],
  });
  assertEq(el.querySelector('tf-tab').getAttribute('count'), '5');
});

test('Tabs locked tab is disabled and emits no select', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [
      tabItem({ id: 'tab1', label: LIT('X'), locked: false }),
      tabItem({ id: 'tab2', label: LIT('Y'), locked: true }),
    ]],
    [2, LIT('tab1')],
    [3, 'cs'],
    [4, 'default'],
  ])));
  const locked = el.querySelectorAll('tf-tab')[1];
  assert(locked.hasAttribute('disabled'));
  let received = null;
  el.querySelector('tf-tabs').addEventListener('select', (e) => { received = e.detail; });
  locked.querySelector('button.tf-tab').click();
  assertEq(received, null);
});

test('Tabs accepts BigInt locked via numeric path is bool-typed (BigInt tolerance on badge/density)', () => {
  // density/variant are enums; the BigInt-tolerant surface here is the badge
  // count carried as a literal that may arrive as BigInt from CBOR.
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [tabItem({ id: 't1', label: LIT('X'), locked: false, badge: { count: LIT(7n) } })]],
    [2, LIT('t1')],
    [3, 'cs'],
    [4, 'default'],
  ]));
  assertEq(el.querySelector('tf-tab').getAttribute('count'), '7');
});

test('Sidebar accepts BigInt badge count (CBOR int tolerance)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SIDEBAR_TAG, [
    [1, [sideItem({ id: 'x', label: LIT('X'), badge: { count: LIT(42n) } })]],
  ]));
  assertEq(el.querySelector('.tf-sidebar__badge').textContent, '42');
});

test('Tabs rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABS_TAG, [
    [0, 'default'], [1, []], [2, LIT('')], [3, 'cs'], [4, 'default'], [7, 'x'],
  ])));
});

test('Tabs rejects unknown TabItem key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [[[0, 't1'], [1, LIT('X')], [9, 'bad']]]],
    [2, LIT('t1')], [3, 'cs'], [4, 'default'],
  ])));
});

test('Tabs rejects bad variant enum', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABS_TAG, [
    [0, 'rainbow'], [1, []], [2, LIT('')], [3, 'cs'], [4, 'default'],
  ])));
});

test('Tabs rejects missing content_slot', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABS_TAG, [
    [0, 'default'], [1, []], [2, LIT('')], [4, 'default'],
  ])));
});

test('Tabs rejects missing density', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABS_TAG, [
    [0, 'default'], [1, []], [2, LIT('')], [3, 'cs'],
  ])));
});

test('Tabs rejects duplicate item id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TABS_TAG, [
    [0, 'default'],
    [1, [
      tabItem({ id: 'dup', label: LIT('A'), locked: false }),
      tabItem({ id: 'dup', label: LIT('B'), locked: false }),
    ]],
    [2, LIT('dup')], [3, 'cs'], [4, 'default'],
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
