// =============================================================================
// File: components/tf-tabs.bar-variant.test.js
// Description: Tests for the tf-tabs navigation-bar extension — the one shape
// behind the three Code Studio tab strips (scene tabs, dock tabs, phone bottom
// nav) — plus a regression block that pins the behaviour and DOM of every way
// tf-tabs is used today. tf-tabs is consumed by ~30 modules and by the SDK
// renderers, so the regression block is the point of this file as much as the
// new features are.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const WWW_ROOT = join(here, '..', '..');

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}

const { TfTabs, TfTab } = await import('./tf-tabs.js');

// ---- helpers ---------------------------------------------------------------

function makeTab(spec) {
  const tab = new TfTab();
  if (spec.id) tab.id = spec.id;
  if (spec.label != null) tab.textContent = spec.label;
  for (const [k, v] of Object.entries(spec.attrs || {})) tab.setAttribute(k, v);
  return tab;
}

function mount(spec = [], hostAttrs = {}) {
  const host = new TfTabs();
  for (const [k, v] of Object.entries(hostAttrs)) host.setAttribute(k, v);
  for (const s of spec) host.appendChild(makeTab(s));
  document.body.appendChild(host);
  return host;
}

function buttons(host) {
  return [...host.querySelectorAll('button.tf-tab')];
}
function btnOf(host, id) {
  return host.querySelector(`tf-tab#${id} > button.tf-tab`);
}
function scroller(host) {
  return host.querySelector('[role="tablist"]');
}

// ---------------------------------------------------------------------------
// Regression: every existing usage keeps its behaviour and DOM
// ---------------------------------------------------------------------------

test('regression: the three existing variants keep their scroller/indicator classes', () => {
  for (const [variant, scrollerCls, indicatorCls] of [
    ['solid', 'tf-tabs', 'tf-tab-indicator'],
    ['soft', 'tf-tabs-soft', 'tf-tab-indicator'],
    ['underline', 'tf-tabs-underline', 'tf-tab-underline-bar'],
  ]) {
    const host = mount([{ id: 'a', label: 'A' }], { variant });
    assert.equal(scroller(host).className, scrollerCls, `${variant} scroller`);
    assert.equal(host.querySelector(`.${indicatorCls}`) !== null, true, `${variant} indicator`);
  }
});

test('regression: an unknown variant still falls back to solid', () => {
  const host = mount([{ id: 'a', label: 'A' }], { variant: 'nonsense' });
  assert.equal(scroller(host).className, 'tf-tabs');
});

test('regression: a plain tab renders exactly label (+count) and nothing else', () => {
  const host = mount([{ id: 'a', label: 'Alpha' }, { id: 'b', label: 'Beta', attrs: { count: '7' } }]);
  const btns = buttons(host);
  assert.equal(btns.length, 2, 'exactly one button.tf-tab per tab');
  assert.equal(btns[0].querySelector('.tf-tab-label').textContent, 'Alpha');
  assert.deepEqual([...btns[0].children].map((c) => c.className), ['tf-tab-label']);
  assert.deepEqual([...btns[1].children].map((c) => c.className), ['tf-tab-label', 'tf-tab-count']);
  assert.equal(btns[1].querySelector('.tf-tab-count').textContent, '7');
});

test('regression: .tf-tab-label stays a direct child so roles_catalog can poke it', () => {
  // js/modules/roles_catalog.js writes tab.querySelector('.tf-tab-label').textContent
  // directly and relies on _update() not running afterwards.
  const host = mount([{ id: 'a', label: 'pl' }]);
  const label = host.querySelector('tf-tab#a .tf-tab-label');
  label.textContent = '● polski';
  host.value = 'a';
  assert.equal(host.querySelector('tf-tab#a .tf-tab-label').textContent, '● polski',
    'selecting a tab must not rebuild the button and lose the poked label');
});

test('regression: the SDK private label path (_btn._label + _update) still works', () => {
  // js/sdk-runtime/layout-nav-renderers.js and layout-sidebar-tabs-renderer.js.
  const host = mount([{ id: 'a', label: 'First' }]);
  const tab = host.querySelector('tf-tab#a');
  assert.equal(typeof tab._update, 'function');
  tab._btn._label = 'Renamed';
  tab._update();
  assert.equal(host.querySelector('.tf-tab-label').textContent, 'Renamed');
});

test('regression: label/count/icon/disabled attributes behave as before', () => {
  const host = mount([{ id: 'a', label: 'x' }]);
  const tab = host.querySelector('tf-tab#a');
  tab.setAttribute('label', 'Override');
  assert.equal(btnOf(host, 'a').querySelector('.tf-tab-label').textContent, 'Override');
  tab.setAttribute('icon', 'star');
  assert.match(btnOf(host, 'a').innerHTML, /#i-star/);
  tab.setAttribute('count', '3');
  assert.equal(btnOf(host, 'a').querySelector('.tf-tab-count').textContent, '3');
  tab.removeAttribute('count');
  assert.equal(btnOf(host, 'a').querySelector('.tf-tab-count'), null);
  tab.setAttribute('disabled', '');
  assert.ok(btnOf(host, 'a').hasAttribute('disabled'));
});

test('regression: selection, change event and disabled tabs are unchanged', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' },
    { id: 'c', label: 'C', attrs: { disabled: '' } }]);
  assert.equal(host.getAttribute('value'), 'a', 'first tab becomes active by default');
  assert.ok(btnOf(host, 'a').classList.contains('active'));

  const seen = [];
  host.addEventListener('change', (e) => seen.push(e.detail.value));
  btnOf(host, 'b').click();
  assert.deepEqual(seen, ['b']);
  assert.equal(host.getAttribute('value'), 'b');
  assert.ok(btnOf(host, 'b').classList.contains('active'));
  assert.ok(!btnOf(host, 'a').classList.contains('active'));

  btnOf(host, 'c').click();
  assert.deepEqual(seen, ['b'], 'a disabled tab emits nothing');
});

test('regression: dirty still renders the dot between label and count', () => {
  const host = mount([{ id: 'a', label: 'f.rs', attrs: { dirty: '', count: '2' } }]);
  const kids = [...btnOf(host, 'a').children].map((c) => c.className);
  assert.ok(kids.indexOf('tf-tab-label') < kids.indexOf('tf-tab-dirty'));
  assert.ok(kids.indexOf('tf-tab-dirty') < kids.indexOf('tf-tab-count'));
});

test('regression: setting value programmatically still syncs (profile-timeline path)', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'flame', label: 'Flame' }]);
  host.setAttribute('value', 'flame');
  assert.ok(btnOf(host, 'flame').classList.contains('active'));
});

test('regression: an empty tf-tabs does not throw', () => {
  const host = mount([]);
  assert.ok(scroller(host));
  assert.equal(buttons(host).length, 0);
});

// ---------------------------------------------------------------------------
// ARIA + keyboard
// ---------------------------------------------------------------------------

test('aria: the strip is a tablist and each tab reports its selection', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }]);
  assert.equal(scroller(host).getAttribute('role'), 'tablist');
  assert.equal(btnOf(host, 'a').getAttribute('role'), 'tab');
  assert.equal(btnOf(host, 'a').getAttribute('aria-selected'), 'true');
  assert.equal(btnOf(host, 'b').getAttribute('aria-selected'), 'false');
  btnOf(host, 'b').click();
  assert.equal(btnOf(host, 'a').getAttribute('aria-selected'), 'false');
  assert.equal(btnOf(host, 'b').getAttribute('aria-selected'), 'true');
});

test('aria: panel maps to aria-controls, absent by default', () => {
  const host = mount([{ id: 'a', label: 'A' }]);
  assert.equal(btnOf(host, 'a').hasAttribute('aria-controls'), false);
  host.querySelector('tf-tab#a').setAttribute('panel', 'pane-a');
  assert.equal(btnOf(host, 'a').getAttribute('aria-controls'), 'pane-a');
});

test('aria: tabs keep their natural tab order (no roving tabindex)', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }]);
  for (const b of buttons(host)) {
    assert.equal(b.hasAttribute('tabindex'), false,
      'adding a roving tabindex would change Tab-key behaviour for existing modules');
  }
});

test('keyboard: arrows move focus, Home/End jump, disabled tabs are skipped', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B', attrs: { disabled: '' } },
    { id: 'c', label: 'C' }]);
  const focused = [];
  for (const b of buttons(host)) b.addEventListener('focus', () => focused.push(b.dataset.tabId));

  const press = (from, key) => {
    const ev = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
    btnOf(host, from).dispatchEvent(ev);
    return ev;
  };

  const right = press('a', 'ArrowRight');
  assert.equal(right.defaultPrevented, true);
  assert.deepEqual(focused, ['c'], 'the disabled tab is skipped');

  press('c', 'ArrowRight');
  assert.deepEqual(focused, ['c', 'a'], 'navigation wraps');

  press('a', 'End');
  assert.deepEqual(focused, ['c', 'a', 'c']);
  press('c', 'Home');
  assert.deepEqual(focused, ['c', 'a', 'c', 'a']);
});

test('keyboard: arrows do not select — activation stays manual', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }]);
  const seen = [];
  host.addEventListener('change', (e) => seen.push(e.detail.value));
  btnOf(host, 'a').dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }));
  assert.deepEqual(seen, []);
  assert.equal(host.getAttribute('value'), 'a');
});

test('keyboard: unrelated keys are left alone', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }]);
  const ev = new KeyboardEvent('keydown', { key: 'x', bubbles: true, cancelable: true });
  btnOf(host, 'a').dispatchEvent(ev);
  assert.equal(ev.defaultPrevented, false);
});

// ---------------------------------------------------------------------------
// Late-appended tabs (the Code Studio file strip)
// ---------------------------------------------------------------------------

test('a tab appended after connect is adopted into the tablist', () => {
  const host = mount([], { variant: 'underline' });
  host.appendChild(makeTab({ id: 'f1', label: 'a.rs' }));
  assert.ok(host.querySelector('tf-tab#f1').parentElement === scroller(host),
    'late tabs must land inside the tablist, not beside it');
  assert.ok(btnOf(host, 'f1').classList.contains('active'));
  assert.equal(host.getAttribute('value'), 'f1');
});

test('adopted tabs participate in selection and keyboard navigation', () => {
  const host = mount([], { variant: 'bar' });
  host.appendChild(makeTab({ id: 'f1', label: 'a.rs' }));
  host.appendChild(makeTab({ id: 'f2', label: 'b.rs' }));
  const seen = [];
  host.addEventListener('change', (e) => seen.push(e.detail.value));
  btnOf(host, 'f2').click();
  assert.deepEqual(seen, ['f2']);

  const focused = [];
  for (const b of buttons(host)) b.addEventListener('focus', () => focused.push(b.dataset.tabId));
  btnOf(host, 'f1').dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }));
  assert.deepEqual(focused, ['f2']);
});

test('the indicator stays the last child of the tablist after adoption', () => {
  const host = mount([{ id: 'a', label: 'A' }]);
  host.appendChild(makeTab({ id: 'b', label: 'B' }));
  const kids = [...scroller(host).children].map((c) => c.tagName.toLowerCase());
  assert.equal(kids[kids.length - 1], 'span', 'indicator remains last');
  assert.deepEqual(kids.slice(0, 2), ['tf-tab', 'tf-tab']);
});

test('removing a tab externally is tolerated', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }]);
  host.querySelector('tf-tab#b').remove();
  assert.equal(buttons(host).length, 1);
  assert.ok(btnOf(host, 'a'));
});

// ---------------------------------------------------------------------------
// variant="bar" + layout + indicator + safe-area
// ---------------------------------------------------------------------------

test('bar: the variant gets its own scroller and indicator, no name collision', () => {
  const host = mount([{ id: 'a', label: 'A' }], { variant: 'bar' });
  assert.equal(scroller(host).className, 'tf-tabs-navbar');
  assert.ok(host.querySelector('.tf-tab-bar-line'), 'bar uses the shared FLIP indicator');
  // css/access-keys.css owns an unrelated legacy .tf-tabs-bar — must not be reused.
  assert.equal(scroller(host).classList.contains('tf-tabs-bar'), false);
});

test('bar: switching variants swaps the scroller class cleanly', () => {
  const host = mount([{ id: 'a', label: 'A' }], { variant: 'bar' });
  host.setAttribute('variant', 'soft');
  assert.equal(scroller(host).className, 'tf-tabs-soft');
  host.setAttribute('variant', 'bar');
  assert.equal(scroller(host).className, 'tf-tabs-navbar');
});

test('bar: the entry animation plays on a switch, not on every re-measure', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }], { variant: 'bar' });
  const line = host.querySelector('.tf-tab-bar-line');
  assert.equal(line.classList.contains('is-entering'), false, 'no animation on first render');
  btnOf(host, 'b').click();
  assert.ok(line.classList.contains('is-entering'), 'animation on an actual switch');
});

test('bar: the indicator inset follows the layout', () => {
  const inline = mount([{ id: 'a', label: 'A' }], { variant: 'bar' });
  assert.equal(inline._indicatorInset(100), 0);
  inline.setAttribute('layout', 'stacked');
  assert.equal(inline._indicatorInset(100), 24, 'stacked underlines only the middle (24% each side)');

  const underline = mount([{ id: 'a', label: 'A' }], { variant: 'underline' });
  assert.equal(underline._indicatorInset(100), 10, 'underline keeps its historical 10px');

  const solid = mount([{ id: 'a', label: 'A' }]);
  assert.equal(solid._indicatorInset(100), 0);
});

// ---------------------------------------------------------------------------
// The mockup's tab states, one by one
// ---------------------------------------------------------------------------

test('scene strip: status dot | status letter | icon fill ONE leading slot', () => {
  const host = mount([
    { id: 'd', label: 'session', attrs: { dot: '', tone: 'ok' } },
    { id: 'm', label: 'router.rs', attrs: { marker: 'M', tone: 'warn' } },
    { id: 'i', label: 'Plain', attrs: { icon: 'code' } },
  ], { variant: 'bar' });

  const dot = btnOf(host, 'd').firstElementChild;
  assert.equal(dot.className, 'tf-tab-dot tf-tab-dot--ok');
  const marker = btnOf(host, 'm').firstElementChild;
  assert.equal(marker.className, 'tf-tab-marker tf-tab-marker--warn');
  assert.equal(marker.textContent, 'M');
  assert.equal(btnOf(host, 'i').firstElementChild.tagName.toLowerCase(), 'svg');

  // Priority: a tab carrying all three shows only the dot.
  const all = mount([{ id: 'x', label: 'X', attrs: { dot: '', marker: 'A', icon: 'code' } }]);
  assert.equal(btnOf(all, 'x').querySelectorAll('.tf-tab-dot, .tf-tab-marker, svg').length, 1);
  assert.ok(btnOf(all, 'x').querySelector('.tf-tab-dot'));
});

test('scene strip: mono label + subtitle', () => {
  const host = mount([{ id: 'a', label: 'embeddings.rs', attrs: { mono: '', sub: 'src/api' } }],
    { variant: 'bar' });
  const btn = btnOf(host, 'a');
  const text = btn.querySelector('.tf-tab-text');
  assert.ok(text, 'label and sub share a stacked wrapper');
  assert.equal(text.querySelector('.tf-tab-label').textContent, 'embeddings.rs');
  assert.ok(text.querySelector('.tf-tab-label').classList.contains('tf-tab-label--mono'));
  assert.equal(text.querySelector('.tf-tab-sub').textContent, 'src/api');
});

test('scene strip: closable renders a × sibling that never becomes a tab button', () => {
  const host = mount([{ id: 'a', label: 'a.rs', attrs: { closable: '' } }], { variant: 'bar' });
  const tab = host.querySelector('tf-tab#a');
  const close = tab.querySelector('.tf-tab-close');
  assert.ok(close, 'close button exists');
  assert.ok(close.parentElement === tab, 'sibling of the tab button, not a child of it');
  assert.equal(close.classList.contains('tf-tab'), false, 'must not match button.tf-tab');
  assert.equal(buttons(host).length, 1, 'querySelectorAll("button.tf-tab") is unaffected');
  assert.equal(close.getAttribute('aria-label'), 'Close');
});

test('scene strip: the × emits tab-close and never selects the tab', () => {
  const host = mount([{ id: 'a', label: 'A' }, { id: 'b', label: 'B', attrs: { closable: '' } }],
    { variant: 'bar' });
  const closes = [];
  const changes = [];
  host.addEventListener('tab-close', (e) => closes.push(e.detail.id));
  host.addEventListener('change', (e) => changes.push(e.detail.value));
  host.querySelector('tf-tab#b .tf-tab-close').click();
  assert.deepEqual(closes, ['b']);
  assert.deepEqual(changes, [], 'closing is not selecting');
  assert.ok(host.querySelector('tf-tab#b'), 'the component never removes the tab itself');
});

test('scene strip: the close button survives a label/count rebuild', () => {
  // _update() rewrites the button innerHTML on every count tick; a close button
  // rendered inside it would lose its listener.
  const host = mount([{ id: 'a', label: 'A', attrs: { closable: '' } }]);
  const tab = host.querySelector('tf-tab#a');
  const close = tab.querySelector('.tf-tab-close');
  tab.setAttribute('count', '4');
  tab.setAttribute('label', 'Renamed');
  assert.ok(tab.querySelector('.tf-tab-close') === close, 'same node, same listener');
  const closes = [];
  host.addEventListener('tab-close', (e) => closes.push(e.detail.id));
  close.click();
  assert.deepEqual(closes, ['a']);
});

test('scene strip: closable can be toggled off', () => {
  const host = mount([{ id: 'a', label: 'A', attrs: { closable: '' } }]);
  const tab = host.querySelector('tf-tab#a');
  tab.removeAttribute('closable');
  assert.equal(tab.querySelector('.tf-tab-close'), null);
});

test('scene strip: a disabled closable tab cannot be closed', () => {
  const host = mount([{ id: 'a', label: 'A', attrs: { closable: '', disabled: '' } }]);
  const closes = [];
  host.addEventListener('tab-close', (e) => closes.push(e.detail.id));
  host.querySelector('.tf-tab-close').click();
  assert.deepEqual(closes, []);
});

test('dock tabs: count carries the hot tone', () => {
  const host = mount([{ id: 'z', label: 'Zmiany', attrs: { count: '3', 'count-tone': 'hot' } }],
    { variant: 'bar', layout: 'stacked', indicator: 'bottom' });
  const count = btnOf(host, 'z').querySelector('.tf-tab-count');
  assert.ok(count.classList.contains('tf-tab-count--hot'));
  assert.equal(count.textContent, '3');
});

test('pinned + nudge are plain attributes the CSS keys off', () => {
  const host = mount([{ id: 'home', label: 'Konsola', attrs: { pinned: '', nudge: '', icon: 'message' } },
    { id: 'a', label: 'a.rs' }], { variant: 'bar' });
  const home = host.querySelector('tf-tab#home');
  assert.ok(home.hasAttribute('pinned'));
  assert.ok(home.hasAttribute('nudge'));
  assert.ok(home.parentElement === scroller(host), 'the pinned cell is a normal tab');
});

// ---------------------------------------------------------------------------
// The CSS the mockup states depend on
// ---------------------------------------------------------------------------

test('css: every mockup state has a rule in controls.css', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  const required = [
    ['.tf-tabs-navbar', 'bar strip'],
    ['.tf-tabs-navbar .tf-tab.active', 'active cell'],
    ['.tf-tab-bar-line', 'moving 2px rule'],
    ['@keyframes tf-tab-line-in', 'csTabIn equivalent'],
    ['tf-tabs[indicator="bottom"]', 'dock underline'],
    ['tf-tabs[layout="stacked"]', 'icon above label'],
    ['tf-tabs[safe-area]', 'phone bottom nav'],
    ['env(safe-area-inset-bottom', 'home-indicator inset'],
    ['min-height: 46px', 'touch target'],
    ['.tf-tab .tf-tab-dot', 'status dot'],
    ['.tf-tab .tf-tab-marker', 'status letter'],
    ['.tf-tab .tf-tab-sub', 'subtitle'],
    ['.tf-tab .tf-tab-label--mono', 'monospace label'],
    ['.tf-tab .tf-tab-count--hot', 'hot counter'],
    ['.tf-tab-close', 'close affordance'],
    ['tf-tab[pinned]', 'pinned first cell'],
    ['tf-tab[nudge]', 'asking state'],
    ['.tf-tab:focus-visible', 'focus ring'],
  ];
  for (const [needle, what] of required) {
    assert.ok(css.includes(needle), `${what} — missing rule ${needle}`);
  }
  assert.match(css.slice(css.indexOf('.tf-tab-bar-line {')), /var\(--tf-gradient-accent\)/);
});

test('css: the bar variant neutralises the global .tf-tab.active glow', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  const rule = css.slice(css.indexOf('.tf-tabs-navbar .tf-tab.active {'));
  assert.match(rule.slice(0, 220), /box-shadow:\s*none/);
});
