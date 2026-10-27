// =============================================================================
// File: utils.toast.test.js
// Description: The shared toast container is a polite live region, so a message
// that only appears on screen (a value cut back, a refusal) reaches a screen reader.
// =============================================================================

import { window } from './sdk-runtime/_dom-test-harness.js';
import { test, after } from 'node:test';
import assert from 'node:assert/strict';

const { toast } = await import('./utils.js');

after(async () => { await window.happyDOM.close(); });

test('the toast container is a polite status region and holds the message', () => {
  toast('Limit reduced to 3', 'info');
  const container = document.querySelector('.toast-container');
  assert.equal(container.getAttribute('role'), 'status');
  assert.equal(container.getAttribute('aria-live'), 'polite');
  assert.equal(container.querySelector('.toast').textContent, 'Limit reduced to 3');
});
