// =============================================================================
// File: sdk-runtime/molecule-page-renderer.test.js
// Description: Tests for Header (0x0001 → <tf-detail-header>),
// PageHeader (0x0002), SectionHeader (0x0004), Toolbar (0x0005),
// StatGroup (0x000A), Inspector (0x000C) — chunk 3.3f.
// =============================================================================

import './_dom-test-harness.js';
import { window as harnessWindow } from './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  HEADER_TAG, PAGE_HEADER_TAG, SECTION_HEADER_TAG,
  TOOLBAR_TAG, STAT_GROUP_TAG, INSPECTOR_TAG,
} from './molecule-page-renderer.js';
import { BUTTON_TAG } from './action-button-renderer.js';
import { STAT_CARD_TAG } from './data-stat-labels-renderer.js';

// The harness exports bound globals; a bound class has no .prototype, which
// breaks `class X extends HTMLElement`. Restore the raw constructor before
// loading web components (dynamic import runs after the harness).
globalThis.HTMLElement = harnessWindow.HTMLElement;
// happy-dom exposes observers on the window object only — bridge for tf-tabs.
globalThis.ResizeObserver = harnessWindow.ResizeObserver;
// Keep rAF inert so tf-tabs skips its layout passes (no real layout in Node).
globalThis.requestAnimationFrame = () => 0;
await import('../components/tf-detail-header.js');
await import('../components/tf-tabs.js');
await import('../components/tf-chip.js');
await import('../components/tf-badge.js');

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
const ICON_NAMED = (name) => ({ kind: 'named', name, size: null, tone: null });

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
function btnComp(label) {
  return comp(BUTTON_TAG, [
    [0, 'primary'],    // variant
    [1, 'neutral'],    // tone
    [2, LIT(label)],   // label BindRef
    [5, 'md'],         // size
    [6, false],        // full_width
    [9, 'default'],    // density
  ], { id: `btn-${label}` });
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}
// Attach to the document so custom elements upgrade (connectedCallback)
function mount(el) {
  document.body.appendChild(el);
  return el;
}

// ============================================================================
// Header (0x0001) — <tf-detail-header>
// ============================================================================

test('Header renders <tf-detail-header> with icon slot, title, density', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(HEADER_TAG, [
    [0, ICON_NAMED('user')],
    [1, LIT('Test Header')],
    [4, []],
    [5, []],
    [6, 'compact'],
  ])));
  assertEq(el.tagName, 'TF-DETAIL-HEADER', 'web component tag');
  assertEq(el.getAttribute('title'), 'Test Header', 'title attribute');
  assertEq(el.getAttribute('data-density'), 'compact', 'density marker');
  assertEq(el.querySelector('.tf-detail-title').textContent, 'Test Header', 'title rendered');
  assert(el.querySelector('.tf-detail-icon .tf-icon') != null, 'slotted icon in icon circle');
});

test('Header renders actions as tf-button children in actions area', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(HEADER_TAG, [
    [0, ICON_NAMED('settings')],
    [1, LIT('Actions')],
    [4, []],
    [5, [btnComp('Save'), btnComp('Cancel')]],
    [6, 'default'],
  ])));
  const actions = el.querySelector('.tf-detail-actions');
  assert(actions != null, 'actions area exists');
  assertEq(actions.querySelectorAll('tf-button').length, 2, 'two tf-button actions');
});

test('Header renders subtitle, status badge and meta chips', () => {
  setup();
  const engine = makeEngine();
  const badge = { 0: 'status', 1: 'success', 2: LIT('Active') };
  const chip = { 0: 'solid', 1: 'info', 2: LIT('tag1') };
  const el = mount(engine.render(comp(HEADER_TAG, [
    [0, ICON_NAMED('check')],
    [1, LIT('Main')],
    [2, badge],
    [3, LIT('Sub title')],
    [4, [chip]],
    [5, []],
    [6, 'default'],
  ])));
  assertEq(el.getAttribute('subtitle'), 'Sub title', 'subtitle attribute');
  assertEq(el.querySelector('.tf-detail-subtitle').textContent, 'Sub title', 'subtitle rendered');
  const badges = el.querySelector('.tf-detail-badges');
  assert(badges != null, 'badges area');
  const badgeEl = badges.querySelector('tf-badge');
  assert(badgeEl != null, 'status badge is tf-badge');
  assertEq(badgeEl.getAttribute('tone'), 'success', 'badge tone mapped');
  assertEq(badgeEl.getAttribute('value'), 'Active', 'badge value');
  const chipEl = badges.querySelector('tf-chip');
  assert(chipEl != null, 'meta chip is tf-chip');
  assertEq(chipEl.getAttribute('label'), 'tag1', 'chip label');
  assertEq(chipEl.getAttribute('status'), 'info', 'chip status mapped');
});

test('Header rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(HEADER_TAG, [
    [0, ICON_NAMED('x')],
    [1, LIT('T')],
    [4, []],
    [5, []],
    [6, 'default'],
    [7, 'extra'],
  ])));
});

test('Header rejects non-Button in actions', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(HEADER_TAG, [
    [0, ICON_NAMED('x')],
    [1, LIT('T')],
    [4, []],
    [5, [comp(0x9999, [], { id: 'bad' })]],
    [6, 'default'],
  ])));
});

// ============================================================================
// PageHeader (0x0002)
// ============================================================================

test('PageHeader renders title and subtitle', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PAGE_HEADER_TAG, [
    [0, LIT('Dashboard')],
    [1, LIT('Overview')],
    [3, []],
  ]));
  assert(el.classList.contains('tf-page-header'), 'class');
  assert(el.querySelector('.tf-page-header__title').textContent === 'Dashboard', 'title');
  assert(el.querySelector('.tf-page-header__subtitle').textContent === 'Overview', 'subtitle');
});

test('PageHeader renders breadcrumbs', () => {
  setup();
  const engine = makeEngine();
  const crumbs = [
    { 0: LIT('Home'), 4: false },
    { 0: LIT('Settings'), 4: true },
  ];
  const el = engine.render(comp(PAGE_HEADER_TAG, [
    [0, LIT('Page')],
    [2, crumbs],
    [3, []],
  ]));
  const list = el.querySelector('.tf-molecule-breadcrumbs__list');
  assert(list != null, 'breadcrumbs rendered');
  const items = list.querySelectorAll('.tf-molecule-breadcrumbs__item');
  assertEq(items.length, 2, 'two breadcrumb items');
  assert(items[1].classList.contains('tf-molecule-breadcrumbs__item--current'), 'second current');
});

test('PageHeader renders tabs as <tf-tabs> with tf-tab children', () => {
  setup();
  const engine = makeEngine();
  const tabs = [
    { 0: 'tab1', 1: LIT('General') },
    { 0: 'tab2', 1: LIT('Advanced'), 5: true },
  ];
  const el = mount(engine.render(comp(PAGE_HEADER_TAG, [
    [0, LIT('Page')],
    [3, []],
    [4, tabs],
  ])));
  const tabsEl = el.querySelector('tf-tabs');
  assert(tabsEl != null, 'tf-tabs rendered');
  const tabEls = tabsEl.querySelectorAll('tf-tab');
  assertEq(tabEls.length, 2, 'two tabs');
  assertEq(tabEls[0].getAttribute('label'), 'General', 'first label');
  assert(tabEls[1].hasAttribute('disabled'), 'second tab locked');
  assert(tabEls[1].querySelector('button').disabled, 'locked tab button disabled');
});

// ============================================================================
// SectionHeader (0x0004)
// ============================================================================

test('SectionHeader renders with divider', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SECTION_HEADER_TAG, [
    [0, LIT('Section A')],
    [2, []],
    [3, true],
  ]));
  assert(el.classList.contains('tf-section-header'), 'class');
  assert(el.querySelector('.tf-section-header__title').textContent === 'Section A', 'title');
  assert(el.querySelector('.tf-section-header__divider') != null, 'divider present');
});

test('SectionHeader without divider', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SECTION_HEADER_TAG, [
    [0, LIT('No Divider')],
    [2, []],
    [3, false],
  ]));
  assert(el.querySelector('.tf-section-header__divider') == null, 'no divider');
});

test('SectionHeader renders actions', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SECTION_HEADER_TAG, [
    [0, LIT('With Actions')],
    [2, [btnComp('Edit')]],
    [3, false],
  ]));
  const actions = el.querySelector('.tf-section-header__actions');
  assert(actions != null, 'actions');
  assertEq(actions.children.length, 1, 'one action');
  assertEq(actions.children[0].tagName, 'TF-BUTTON', 'action is tf-button');
});

// ============================================================================
// Toolbar (0x0005)
// ============================================================================

test('Toolbar renders with density', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TOOLBAR_TAG, [
    [1, []],
    [4, []],
    [5, 'compact'],
  ]));
  assert(el.classList.contains('tf-toolbar'), 'class');
  assert(el.classList.contains('tf-toolbar--density-compact'), 'density');
  assert(el.getAttribute('role') === 'toolbar', 'role');
});

test('Toolbar renders trailing actions', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TOOLBAR_TAG, [
    [1, []],
    [4, [btnComp('Add'), btnComp('Remove')]],
    [5, 'default'],
  ]));
  const trailing = el.querySelector('.tf-toolbar__trailing');
  assert(trailing != null, 'trailing section');
  assertEq(trailing.children.length, 2, 'two trailing buttons');
});

test('Toolbar rejects wrong search tag', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TOOLBAR_TAG, [
    [0, comp(0x9999, [], { id: 'bad' })],
    [1, []],
    [4, []],
    [5, 'default'],
  ])));
});

test('Toolbar renders filter chips as clickable tf-chip', () => {
  setup();
  const engine = makeEngine();
  const filters = [
    { 0: 'f1', 1: LIT('Active') },
    { 0: 'f2', 1: LIT('Archived') },
  ];
  const el = engine.render(comp(TOOLBAR_TAG, [
    [1, filters],
    [4, []],
    [5, 'default'],
  ]));
  const filtersEl = el.querySelector('.tf-molecule-filters');
  assert(filtersEl != null, 'filters container');
  assert(filtersEl.classList.contains('tf-toolbar__filters'), 'toolbar filters class');
  const chips = filtersEl.querySelectorAll('tf-chip');
  assertEq(chips.length, 2, 'two filter chips');
  assert(chips[0].hasAttribute('clickable'), 'chip clickable');
  assertEq(chips[0].getAttribute('label'), 'Active', 'chip label');
  assertEq(chips[0].dataset.filterId, 'f1', 'filter id');
});

// ============================================================================
// StatGroup (0x000A)
// ============================================================================

test('StatGroup renders grid of tf-stat-card with columns', () => {
  setup();
  const engine = makeEngine();
  const stat1 = comp(STAT_CARD_TAG, [
    [0, LIT('Revenue')],
    [2, LIT('$1.2M')],
    [8, false],
  ], { id: 's1' });
  const stat2 = comp(STAT_CARD_TAG, [
    [0, LIT('Users')],
    [2, LIT('4.5K')],
    [8, false],
  ], { id: 's2' });
  const el = engine.render(comp(STAT_GROUP_TAG, [
    [0, [stat1, stat2]],
    [1, 2],
    [2, 'default'],
  ]));
  assert(el.classList.contains('tf-stat-group'), 'class');
  assert(el.style.gridTemplateColumns === 'repeat(2, 1fr)', 'grid cols');
  assertEq(el.querySelectorAll('tf-stat-card').length, 2, 'two tf-stat-card items');
});

test('StatGroup defaults columns to stats count', () => {
  setup();
  const engine = makeEngine();
  const stat = comp(STAT_CARD_TAG, [
    [0, LIT('Metric')],
    [2, LIT('42')],
    [8, false],
  ], { id: 's1' });
  const el = engine.render(comp(STAT_GROUP_TAG, [
    [0, [stat, stat, stat]],
    [2, 'comfortable'],
  ]));
  assert(el.style.gridTemplateColumns === 'repeat(3, 1fr)', 'default 3 cols');
  assert(el.classList.contains('tf-stat-group--density-comfortable'), 'density');
});

test('StatGroup rejects non-StatCard children', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STAT_GROUP_TAG, [
    [0, [comp(0x9999, [], { id: 'bad' })]],
    [2, 'default'],
  ])));
});

// ============================================================================
// Inspector (0x000C)
// ============================================================================

test('Inspector renders with slot and collapsible tf-button toggle', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INSPECTOR_TAG, [
    [0, LIT('Details')],
    [1, 'detail-slot'],
    [2, []],
    [4, true],
  ]));
  assert(el.classList.contains('tf-inspector'), 'class');
  assert(el.classList.contains('tf-inspector--collapsible'), 'collapsible');
  assert(el.querySelector('.tf-inspector__title').textContent === 'Details', 'title');
  const body = el.querySelector('.tf-inspector__body');
  assert(body.getAttribute('data-slot-id') === 'detail-slot', 'slot');
  const toggle = el.querySelector('.tf-inspector__toggle');
  assert(toggle != null, 'toggle present');
  assertEq(toggle.tagName, 'TF-BUTTON', 'toggle is tf-button');
  toggle.dispatchEvent(new Event('click', { bubbles: true }));
  assert(el.classList.contains('tf-inspector--collapsed'), 'collapsed after click');
  assertEq(toggle.getAttribute('aria-expanded'), 'false', 'aria-expanded after click');
});

test('Inspector renders without collapsible', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(INSPECTOR_TAG, [
    [0, LIT('Info')],
    [1, 'info-slot'],
    [2, []],
    [4, false],
  ]));
  assert(!el.classList.contains('tf-inspector--collapsible'), 'not collapsible');
  assert(el.querySelector('.tf-inspector__toggle') == null, 'no toggle');
});

test('Inspector renders tabs and actions', () => {
  setup();
  const engine = makeEngine();
  const tabs = [
    { 0: 'properties', 1: LIT('Properties') },
    { 0: 'history', 1: LIT('History') },
  ];
  const el = mount(engine.render(comp(INSPECTOR_TAG, [
    [0, LIT('Item')],
    [1, 'item-slot'],
    [2, [btnComp('Close')]],
    [3, tabs],
    [4, false],
  ])));
  const tabsEl = el.querySelector('tf-tabs');
  assert(tabsEl != null, 'tf-tabs rendered');
  assertEq(tabsEl.querySelectorAll('tf-tab').length, 2, 'two tabs');
  assert(el.querySelector('.tf-inspector__actions') != null, 'actions');
});

// ============================================================================
// Summary
// ============================================================================

const passed = results.filter((r) => r.ok).length;
const failed = results.filter((r) => !r.ok);
console.log(`\nmolecule-page-renderer: ${passed}/${results.length} passed`);
for (const f of failed) console.error(`  FAIL: ${f.name}`, f.err);
if (failed.length > 0) process.exit(1);
