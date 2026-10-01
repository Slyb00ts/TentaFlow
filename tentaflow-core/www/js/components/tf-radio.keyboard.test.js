// =============================================================================
// File: components/tf-radio.keyboard.test.js
// Description: A radio group is one tab stop even with nothing selected, and the
//   arrow keys move the choice between the radios that can take focus.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
await import('./tf-radio.js');

function mount(value = '') {
  document.body.innerHTML = '';
  const group = document.createElement('tf-radio-group');
  if (value) group.setAttribute('value', value);
  group.innerHTML = '<tf-radio value="a" label="A" disabled></tf-radio><tf-radio value="b" label="B"></tf-radio>'
    + '<tf-radio value="c" label="C"></tf-radio><tf-radio value="d" label="D"></tf-radio>';
  document.body.appendChild(group);
  return group;
}

const stops = (group) => [...group.querySelectorAll('tf-radio')].filter((r) => r._input.getAttribute('tabindex') === '0').map((r) => r.value);
const press = (radio, name) => radio._input.dispatchEvent(new window.KeyboardEvent('keydown', { key: name, bubbles: true, cancelable: true }));

test('with nothing selected the first enabled radio is the tab stop', () => {
  assert.deepEqual(stops(mount()), ['b']);
});

test('with a selection that radio is the only tab stop', () => {
  assert.deepEqual(stops(mount('c')), ['c']);
});

test('arrows move the selection over enabled radios and wrap', () => {
  const group = mount('b');
  const radio = (v) => group.querySelector(`tf-radio[value="${v}"]`);
  press(radio('b'), 'ArrowDown');
  assert.equal(group.value, 'c');
  press(radio('c'), 'ArrowRight');
  press(radio('d'), 'ArrowDown');
  assert.equal(group.value, 'b', 'wraps past the end and skips the disabled first radio');
  press(radio('b'), 'ArrowUp');
  assert.equal(group.value, 'd');
});

test('a hidden radio is skipped and the tab stop follows the filter', () => {
  const group = mount();
  group.querySelector('tf-radio[value="b"]').setAttribute('hidden', '');
  group.refresh();
  assert.deepEqual(stops(group), ['c']);
});
