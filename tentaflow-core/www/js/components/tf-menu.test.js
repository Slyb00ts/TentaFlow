// =============================================================================
// File: components/tf-menu.test.js
// Description: A closed menu leaves the tab order and the accessibility tree;
//   the arrows still walk in when focus sits on an item that is hidden or
//   disabled.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { cleanBody } from '../lib/actions/_test-setup.js';
await import('./tf-menu.js');

beforeEach(cleanBody);

const css = readFileSync(new URL('../../css/controls.css', import.meta.url), 'utf8');

function mount(items) {
  const host = document.createElement('div');
  host.innerHTML = `<tf-menu open>${items}</tf-menu>`;
  document.body.appendChild(host);
  return host.querySelector('tf-menu');
}

const rowOf = (menu, name) => menu.querySelector(`tf-menu-item[data-name="${name}"] > .tf-menu-item`);
const press = (key) => document.dispatchEvent(new window.KeyboardEvent('keydown', { key, bubbles: true }));

test('the closed panel is visibility:hidden and the open one visible, so Tab cannot land in it', () => {
  assert.match(css, /\.tf-menu \{[^}]*visibility: hidden/);
  assert.match(css, /\.tf-menu\.open \{[^}]*visibility: visible/);
  assert.match(css, /:host-context\(\.tf-menu--inline\) \.tf-menu \{[^}]*visibility: visible/);
});

test('arrows continue from a disabled item to the next or the previous enabled one', () => {
  const menu = mount(`<tf-menu-item data-name="a">A</tf-menu-item><tf-menu-item data-name="b" disabled>B</tf-menu-item><tf-menu-item data-name="c">C</tf-menu-item><tf-menu-item data-name="d">D</tf-menu-item>`);
  rowOf(menu, 'b').focus();
  press('ArrowDown');
  assert.equal(document.activeElement, rowOf(menu, 'c'));
  rowOf(menu, 'b').focus();
  press('ArrowUp');
  assert.equal(document.activeElement, rowOf(menu, 'a'));
});

test('arrows wrap from a disabled item at either end of the list', () => {
  const menu = mount(`<tf-menu-item data-name="a">A</tf-menu-item><tf-menu-item data-name="b">B</tf-menu-item><tf-menu-item data-name="z" disabled>Z</tf-menu-item>`);
  rowOf(menu, 'z').focus();
  press('ArrowDown');
  assert.equal(document.activeElement, rowOf(menu, 'a'));
  rowOf(menu, 'z').focus();
  press('ArrowUp');
  assert.equal(document.activeElement, rowOf(menu, 'b'));
});

test('a long panel scrolls inside itself and keeps the wheel to itself', () => {
  assert.match(css, /\.tf-menu \{[^}]*overscroll-behavior: contain/);
});

function anchored({ scroller = true } = {}) {
  const host = document.createElement('div');
  host.innerHTML = `<div data-role="bar"><button data-role="trigger">More</button></div><div data-role="other"></div><tf-menu><tf-menu-item>A</tf-menu-item></tf-menu>`;
  document.body.appendChild(host);
  const menu = host.querySelector('tf-menu');
  const trigger = host.querySelector('[data-role="trigger"]');
  const rect = { top: 40, bottom: 70, left: 100, right: 160, width: 60, height: 30 };
  trigger.getBoundingClientRect = () => ({ ...rect, x: rect.left, y: rect.top });
  menu.anchor = trigger;
  menu.open();
  return { menu, trigger, bar: host.querySelector('[data-role="bar"]'), other: host.querySelector('[data-role="other"]'), rect };
}

const scrolled = (target) => target.dispatchEvent(new window.Event('scroll', { bubbles: false }));

test('a scroll of the trigger\'s own container keeps the menu open while the trigger is still on screen', () => {
  const { menu, bar } = anchored();
  scrolled(bar);
  assert.equal(menu.hasAttribute('open'), true);
});

test('a scroll that carries the trigger off screen closes the menu', () => {
  const { menu, bar, rect } = anchored();
  rect.top = -80; rect.bottom = -50;
  scrolled(bar);
  assert.equal(menu.hasAttribute('open'), false);
});

test('a scroll of an unrelated container leaves the menu alone', () => {
  const { menu, other } = anchored();
  scrolled(other);
  assert.equal(menu.hasAttribute('open'), true);
});

test('a page scroll closes the menu and so does a resize', () => {
  const first = anchored();
  scrolled(document);
  assert.equal(first.menu.hasAttribute('open'), false);
  const second = anchored();
  window.dispatchEvent(new window.Event('resize'));
  assert.equal(second.menu.hasAttribute('open'), false);
});

test('a scroll inside the panel itself never closes it', () => {
  const { menu } = anchored();
  scrolled(menu.querySelector('tf-menu-item'));
  assert.equal(menu.hasAttribute('open'), true);
});

test('a menu without an anchor ignores scrolls entirely', () => {
  const menu = mount('<tf-menu-item>A</tf-menu-item>');
  scrolled(document);
  assert.equal(menu.hasAttribute('open'), true);
});

test('the closing panel is inert at once, so the fade cannot catch a click meant for what lies below', () => {
  const { menu } = anchored();
  const panel = menu.shadowRoot.querySelector('.tf-menu');
  assert.equal(panel.inert, false);
  menu.close();
  assert.equal(panel.inert, true);
  menu.open();
  assert.equal(panel.inert, false);
});

test('a panel taller than the room scrolls inside the viewport instead of running past it', () => {
  const { menu, trigger, rect } = anchored();
  const panel = menu.shadowRoot.querySelector('.tf-menu');
  Object.defineProperty(window, 'innerHeight', { value: 300, configurable: true });
  Object.defineProperty(panel, 'offsetHeight', { value: 900, configurable: true });
  Object.defineProperty(panel, 'offsetWidth', { value: 200, configurable: true });
  rect.top = 140; rect.bottom = 170;
  menu.anchor = trigger;
  const top = parseInt(panel.style.top, 10);
  const max = parseInt(panel.style.maxHeight, 10);
  assert.ok(top >= 8, `top ${top} stays inside`);
  assert.ok(top + max <= 300 - 8, `bottom ${top + max} stays inside the 300 px viewport`);
  assert.equal(panel.style.overflowY, 'auto');
});

test('a viewport shorter than the old 120 px floor still holds the panel', () => {
  const { menu, trigger, rect } = anchored();
  const panel = menu.shadowRoot.querySelector('.tf-menu');
  Object.defineProperty(window, 'innerHeight', { value: 100, configurable: true });
  Object.defineProperty(panel, 'offsetHeight', { value: 500, configurable: true });
  Object.defineProperty(panel, 'offsetWidth', { value: 200, configurable: true });
  rect.top = 40; rect.bottom = 60;
  menu.anchor = trigger;
  assert.ok(parseInt(panel.style.top, 10) + parseInt(panel.style.maxHeight, 10) <= 100 - 8);
});

const frames = (n = 3) => new Promise((resolve) => {
  const step = (left) => (left === 0 ? resolve() : requestAnimationFrame(() => step(left - 1)));
  step(n);
});

test('an anchor inside a shadow root that moves without any scroll event drags the panel along', async () => {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const root = host.attachShadow({ mode: 'open' });
  root.innerHTML = '<div data-role="body"><button data-role="trigger">More</button></div>';
  const trigger = root.querySelector('[data-role="trigger"]');
  const rect = { top: 40, bottom: 70, left: 100, right: 160, width: 60, height: 30 };
  trigger.getBoundingClientRect = () => ({ ...rect, x: rect.left, y: rect.top });
  const menu = document.createElement('tf-menu');
  menu.innerHTML = '<tf-menu-item>A</tf-menu-item>';
  document.body.appendChild(menu);
  menu.anchor = trigger;
  menu.open();
  const panel = menu.shadowRoot.querySelector('.tf-menu');
  const before = panel.style.top;
  rect.top = 90; rect.bottom = 120;
  await frames();
  assert.equal(menu.hasAttribute('open'), true);
  assert.notEqual(panel.style.top, before);
  rect.top = -80; rect.bottom = -50;
  await frames();
  assert.equal(menu.hasAttribute('open'), false);
});

test('an anchor scrolled out of its clipping container closes the menu even while inside the viewport', async () => {
  const { menu, bar, rect } = anchored();
  bar.style.overflow = 'auto';
  bar.getBoundingClientRect = () => ({ top: 100, bottom: 300, left: 0, right: 400, width: 400, height: 200 });
  rect.top = 10; rect.bottom = 40;
  await frames();
  assert.equal(menu.hasAttribute('open'), false);
});

test('the frame watch stops with the menu', async () => {
  const { menu } = anchored();
  assert.ok(menu._frame);
  menu.close();
  assert.equal(menu._frame, 0);
  assert.equal(menu._viewportBound, false);
});

test('closing with focus inside the panel returns it to the trigger', () => {
  const { menu, trigger } = anchored();
  const row = menu.querySelector('tf-menu-item > .tf-menu-item');
  row.focus();
  assert.equal(document.activeElement, row);
  menu.close();
  assert.equal(document.activeElement, trigger);
});

test('closing does not take focus back when the reader already moved it elsewhere', () => {
  const { menu, other } = anchored();
  const elsewhere = document.createElement('button');
  other.appendChild(elsewhere);
  elsewhere.focus();
  menu.close();
  assert.equal(document.activeElement, elsewhere);
});
