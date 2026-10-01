// =============================================================================
// File: components/tf-searchbox.escape.test.js
// Description: Escape belongs to the search box only while it holds text; on an
//   empty box it reaches the window or dialog around it.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
await import('./tf-searchbox.js');

function mount(value) {
  document.body.innerHTML = '';
  const box = document.createElement('tf-searchbox');
  document.body.appendChild(box);
  const input = box.querySelector('input');
  input.value = value;
  const reached = [];
  document.addEventListener('keydown', (e) => reached.push(e.key), { once: true });
  return { input, reached };
}

const escape = (input) => input.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));

test('Escape clears a box that has text and stops there', () => {
  const { input, reached } = mount('abc');
  escape(input);
  assert.equal(input.value, '');
  assert.deepEqual(reached, []);
});

test('Escape on an empty box passes through', () => {
  const { input, reached } = mount('');
  escape(input);
  assert.deepEqual(reached, ['Escape']);
});
