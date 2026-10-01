// =============================================================================
// File: lib/actions/menu.test.js
// Description: The "⋯" menu — items, separators, danger, disabled items with
//   their reason, keyboard, and where focus goes.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, key, cleanBody, sleep } from './_test-setup.js';
const { openActionMenu } = await import('./menu.js');

beforeEach(cleanBody);

function anchor() {
  const btn = document.createElement('button');
  btn.textContent = '⋯';
  document.body.appendChild(btn);
  return btn;
}

const labels = (menu) => [...menu.querySelectorAll('tf-menu-item')].map((i) => i.getAttribute('label'));
const rowOf = (menu, label) => [...menu.querySelectorAll('tf-menu-item')].find((i) => i.getAttribute('label') === label).querySelector('.tf-menu-item');

test('the menu lists items in order with separators, icons and the subject as heading', () => {
  const menu = openActionMenu(anchor(), [
    { label: 'Edit', icon: 'edit', run() {} },
    { label: 'Assign', icon: 'user', run() {} },
    { separator: true },
    { label: 'Delete', icon: 'trash', danger: true, run() {} },
  ], 'Task NA-231');
  assert.deepEqual(labels(menu), ['Edit', 'Assign', 'Delete']);
  assert.equal(menu.getAttribute('heading'), 'Task NA-231');
  assert.equal(menu.querySelectorAll('tf-menu-divider').length, 1);
  assert.equal(menu.querySelector('tf-menu-item[danger]').getAttribute('label'), 'Delete');
  assert.equal(menu.querySelector('tf-menu-item[icon="edit"]').getAttribute('label'), 'Edit');
  assert.equal(menu.hasAttribute('open'), true);
});

test('the panel is in the document before it opens, so it is placed by the real size of its items', () => {
  // A panel opened while detached is measured with empty items and then sticks out at the window's right edge.
  const setAttribute = window.Element.prototype.setAttribute;
  const seen = [];
  window.Element.prototype.setAttribute = function spy(name, value) {
    if (this.tagName === 'TF-MENU' && name === 'open') seen.push(this.isConnected);
    return setAttribute.call(this, name, value);
  };
  try {
    openActionMenu(anchor(), [{ label: 'Edit', run() {} }], 'x');
  } finally {
    window.Element.prototype.setAttribute = setAttribute;
  }
  assert.deepEqual(seen, [true]);
});

test('a disabled item shows its reason, does not run and leaves the menu open', () => {
  let ran = 0;
  const menu = openActionMenu(anchor(), [
    { label: 'Delete', disabled: true, reason: 'Only an ended project can be deleted', run: () => { ran++; } },
  ], 'Project');
  const item = menu.querySelector('tf-menu-item');
  assert.equal(item.hasAttribute('disabled'), true);
  assert.equal(item.querySelector('.tf-menu-item-hint').textContent, 'Only an ended project can be deleted');
  assert.equal(item.querySelector('.tf-menu-item').getAttribute('aria-disabled'), 'true');
  item.querySelector('.tf-menu-item').click();
  assert.equal(ran, 0);
  assert.equal(menu.isConnected, true);
});

test('choosing an item closes the menu, returns focus to the anchor and then runs it', () => {
  const btn = anchor();
  const order = [];
  const menu = openActionMenu(btn, [{ label: 'Edit', run: () => order.push(`run focus=${document.activeElement === btn}`) }], 'x');
  rowOf(menu, 'Edit').click();
  assert.deepEqual(order, ['run focus=true']);
  assert.equal(menu.isConnected, false);
});

test('arrows move between items, Enter runs the focused one, Escape closes and returns focus', async () => {
  const btn = anchor();
  const ran = [];
  const menu = openActionMenu(btn, [
    { label: 'One', run: () => ran.push('one') },
    { label: 'Two', run: () => ran.push('two') },
  ], 'x');
  assert.equal(document.activeElement, rowOf(menu, 'One'), 'the first enabled item takes focus');
  key(document.activeElement, 'ArrowDown');
  assert.equal(document.activeElement, rowOf(menu, 'Two'));
  key(document.activeElement, 'ArrowDown');
  assert.equal(document.activeElement, rowOf(menu, 'One'), 'wraps');
  key(document.activeElement, 'End');
  key(document.activeElement, 'Enter');
  assert.deepEqual(ran, ['two']);

  const again = openActionMenu(btn, [{ label: 'One', run() {} }], 'x');
  key(document.activeElement, 'Escape');
  await sleep(0);
  assert.equal(again.isConnected, false);
  assert.equal(document.activeElement, btn);
});

test('the first focused item skips disabled ones', () => {
  const menu = openActionMenu(anchor(), [
    { label: 'Off', disabled: true, reason: 'no', run() {} },
    { label: 'On', run() {} },
  ], 'x');
  assert.equal(document.activeElement, rowOf(menu, 'On'));
});

test('opening the menu again on the same anchor closes it', () => {
  const btn = anchor();
  const first = openActionMenu(btn, [{ label: 'A', run() {} }], 'x');
  assert.equal(openActionMenu(btn, [{ label: 'A', run() {} }], 'x'), null);
  assert.equal(first.isConnected, false);
  assert.equal(document.querySelectorAll('tf-menu').length, 0);
});

test('an item label that looks like markup is text', () => {
  const menu = openActionMenu(anchor(), [{ label: '<img src=x onerror=alert(1)>', run() {} }], '<b>s</b>');
  assert.equal(menu.querySelector('img'), null);
  assert.equal(menu.getAttribute('heading'), '<b>s</b>');
});

test('a modal action menu stays above its window and selection returns focus before running', () => {
  const win = document.createElement('tf-window');
  win.setAttribute('modal', '');
  const btn = document.createElement('button');
  const body = document.createElement('div');
  body.setAttribute('slot', 'body'); body.appendChild(btn); win.appendChild(body);
  document.body.appendChild(win);
  btn.focus();
  const runs = [];
  const menu = openActionMenu(btn, [{ label: 'Edit', run: () => runs.push(document.activeElement === btn) }], 'Task type');
  assert.ok(Number(menu.shadowRoot.querySelector('.tf-menu').style.zIndex) > Number(win.shadowRoot.querySelector('.tf-window').style.zIndex));
  rowOf(menu, 'Edit').click();
  assert.deepEqual(runs, [true]);
  assert.equal(win.isConnected, true);
});

test('Escape dismisses a focused modal menu before the window, and Tab returns to the modal anchor', () => {
  const win = document.createElement('tf-window');
  win.setAttribute('modal', '');
  const btn = document.createElement('button');
  const body = document.createElement('div');
  body.setAttribute('slot', 'body'); body.appendChild(btn); win.appendChild(body);
  document.body.appendChild(win);
  const menu = openActionMenu(btn, [{ label: 'Edit', run() {} }], 'Task type');
  key(document.activeElement, 'Escape');
  assert.equal(menu.isConnected, false);
  assert.equal(win._closing, undefined);
  assert.equal(document.activeElement, btn);
  const again = openActionMenu(btn, [{ label: 'Edit', run() {} }], 'Task type');
  const tab = key(document.activeElement, 'Tab');
  assert.equal(tab.defaultPrevented, false);
  assert.equal(again.isConnected, false);
  assert.equal(document.activeElement, btn);
});
