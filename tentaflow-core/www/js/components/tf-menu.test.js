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
