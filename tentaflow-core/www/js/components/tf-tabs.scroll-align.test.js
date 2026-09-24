// =============================================================================
// File: components/tf-tabs.scroll-align.test.js
// Description: tf-tabs scroll-align="center": the strip centres the active tab
// on the first placement without animating, glides only on a real switch, and
// re-centres when a counter changes a tab's width. Geometry is stubbed — the
// DOM harness has no layout.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver || class { observe() {} unobserve() {} disconnect() {} };
}
const { TfTabs, TfTab } = await import('./tf-tabs.js');

function mount(attrs) {
  const host = new TfTabs();
  for (const [k, v] of Object.entries(attrs)) host.setAttribute(k, v);
  for (const id of ['a', 'b', 'c', 'd']) {
    const t = new TfTab();
    t.id = id;
    t.textContent = id.toUpperCase();
    host.appendChild(t);
  }
  document.body.appendChild(host);
  const scroller = host.querySelector('[role="tablist"]');
  // 400px strip holding 4 × 200px tabs; tab i starts at i*200 - scrollLeft.
  let scrollLeft = 0;
  const calls = [];
  Object.defineProperty(scroller, 'scrollWidth', { value: 800 });
  Object.defineProperty(scroller, 'clientWidth', { value: 400 });
  scroller.getBoundingClientRect = () => ({ left: 0, width: 400, right: 400 });
  scroller.scrollBy = ({ left, behavior }) => { calls.push({ left, behavior }); scrollLeft += left; };
  host.querySelectorAll('tf-tab').forEach((t, i) => {
    t.querySelector('.tf-tab').getBoundingClientRect = () => ({ left: i * 200 - scrollLeft, width: 200, right: i * 200 + 200 - scrollLeft });
  });
  return { host, calls };
}

test('centring: jump on a deep link, glide on a switch, re-centre on a width change', () => {
  const { host, calls } = mount({ 'scroll-align': 'center', value: 'c' });
  host._scrollActiveIntoView(host.querySelector('tf-tab#c'), false);
  assert.deepEqual(calls.at(-1), { left: 300, behavior: 'auto' }, 'tab C (400–600) centred in a 400px strip');
  host._scrollActiveIntoView(host.querySelector('tf-tab#b'), true);
  assert.equal(calls.at(-1).behavior, 'smooth');
  const before = calls.length;
  host.querySelector('tf-tab#a').setAttribute('count', '12');
  assert.ok(calls.length > before, 'a counter change re-centres');
  assert.equal(calls.at(-1).behavior, 'auto');
});

test('without scroll-align the strip keeps its edge scrolling', () => {
  const { host, calls } = mount({ value: 'a' });
  host.querySelector('tf-tab#a').setAttribute('count', '3');
  assert.equal(calls.length, 0);
  assert.ok(TfTabs.observedAttributes.includes('scroll-align'));
});
