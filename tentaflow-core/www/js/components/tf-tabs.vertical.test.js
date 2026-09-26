// =============================================================================
// File: components/tf-tabs.vertical.test.js
// Description: tf-tabs orientation="vertical" — the section menu of a detail
// page: the tablist says it is vertical, Up/Down/Home/End move the focus along
// it while Left/Right do nothing, a click selects and emits "change", the
// strip never scrolls sideways and the moving indicator stays out of the way.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver || class { observe() {} unobserve() {} disconnect() {} };
}
const { TfTabs, TfTab } = await import('./tf-tabs.js');

function mount(attrs, ids = ['state', 'settings', 'partitions']) {
  const host = new TfTabs();
  for (const [k, v] of Object.entries(attrs)) host.setAttribute(k, v);
  for (const id of ids) {
    const t = new TfTab();
    t.id = id;
    t.textContent = id;
    host.appendChild(t);
  }
  document.body.appendChild(host);
  return host;
}

const button = (host, id) => host.querySelector(`tf-tab#${id} > .tf-tab`);
const key = (el, k) => el.dispatchEvent(new window.KeyboardEvent('keydown', { key: k, bubbles: true }));

test('the tablist is announced as vertical, and only while the attribute says so', () => {
  const host = mount({ orientation: 'vertical', value: 'state' });
  const list = host.querySelector('[role="tablist"]');
  assert.equal(list.getAttribute('aria-orientation'), 'vertical');
  assert.ok(list.classList.contains('tf-tabs--vertical'));
  assert.ok(TfTabs.observedAttributes.includes('orientation'));
  host.removeAttribute('orientation');
  host.attributeChangedCallback('orientation');
  assert.equal(list.getAttribute('aria-orientation'), null);
  assert.ok(!list.classList.contains('tf-tabs--vertical'));
});

test('Up/Down/Home/End move the focus along the menu, Left/Right do not', () => {
  const host = mount({ orientation: 'vertical', value: 'state' });
  button(host, 'state').focus();
  key(button(host, 'state'), 'ArrowDown');
  assert.equal(document.activeElement, button(host, 'settings'));
  key(button(host, 'settings'), 'ArrowRight');
  assert.equal(document.activeElement, button(host, 'settings'), 'Right is not the axis of a vertical menu');
  key(button(host, 'settings'), 'End');
  assert.equal(document.activeElement, button(host, 'partitions'));
  key(button(host, 'partitions'), 'ArrowDown');
  assert.equal(document.activeElement, button(host, 'state'), 'the menu wraps');
  key(button(host, 'state'), 'ArrowUp');
  assert.equal(document.activeElement, button(host, 'partitions'));
});

test('a click selects the section and says so; a disabled one stays out of reach', () => {
  const host = mount({ orientation: 'vertical', value: 'state' });
  host.querySelector('tf-tab#partitions').setAttribute('disabled', '');
  const seen = [];
  host.addEventListener('change', (e) => seen.push(e.detail.value));
  button(host, 'settings').click();
  button(host, 'partitions').click();
  assert.deepEqual(seen, ['settings']);
  assert.equal(host.value, 'settings');
  assert.equal(button(host, 'settings').getAttribute('aria-selected'), 'true');
  button(host, 'settings').focus();
  key(button(host, 'settings'), 'ArrowDown');
  assert.equal(document.activeElement, button(host, 'state'), 'focus skips the disabled section');
});

test('the menu never scrolls sideways and keeps the indicator hidden', () => {
  const host = mount({ orientation: 'vertical', value: 'settings' });
  const scroller = host.querySelector('[role="tablist"]');
  let scrolled = 0;
  scroller.scrollBy = () => { scrolled += 1; };
  host._scrollActiveIntoView(host.querySelector('tf-tab#partitions'), true);
  host._syncIndicator();
  const wheel = new window.WheelEvent('wheel', { deltaY: 120, bubbles: true, cancelable: true });
  scroller.dispatchEvent(wheel);
  assert.equal(scrolled, 0);
  assert.equal(wheel.defaultPrevented, false, 'the page keeps the wheel');
  assert.equal(host.querySelector('.tf-tab-indicator, .tf-tab-underline-bar').hasAttribute('data-ready'), false);
});
