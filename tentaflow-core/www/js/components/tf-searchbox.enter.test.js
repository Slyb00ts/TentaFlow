// =============================================================================
// File: components/tf-searchbox.enter.test.js
// Description: Enter answers a search at once, without waiting out the typing
//   pause, and the pause's own timer then does not announce it a second time.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
await import('./tf-searchbox.js');

test('Enter emits the search immediately and only once', async () => {
  document.body.innerHTML = '';
  const box = document.createElement('tf-searchbox');
  document.body.appendChild(box);
  const seen = [];
  box.addEventListener('search', (e) => seen.push(e.detail.value));
  const input = box.querySelector('input');
  input.value = 'Ewa';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  input.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
  assert.deepEqual(seen, ['Ewa']);
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual(seen, ['Ewa']);
});

test('the browser\'s own search event does not leave the box', () => {
  document.body.innerHTML = '';
  const box = document.createElement('tf-searchbox');
  document.body.appendChild(box);
  const seen = [];
  box.addEventListener('search', (e) => seen.push(e.detail));
  box.querySelector('input').dispatchEvent(new window.Event('search', { bubbles: true }));
  assert.deepEqual(seen, []);
});
