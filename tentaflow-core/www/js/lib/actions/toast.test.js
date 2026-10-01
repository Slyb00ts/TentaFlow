// =============================================================================
// File: lib/actions/toast.test.js
// Description: The undo toast — its button calls onUndo once, a refusal is
//   reported, the toast leaves after the timeout.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { I18n, cleanBody, sleep } from './_test-setup.js';
const { showUndoToast } = await import('./toast.js');

beforeEach(cleanBody);

const undoButton = (toast) => toast.querySelector('tf-button');

test('the toast carries the message and an Undo button', () => {
  const toast = showUndoToast({ message: 'Assigned: Anna', onUndo() {} });
  assert.equal(toast.querySelector('.tf-toast-message').textContent, 'Assigned: Anna');
  assert.equal(undoButton(toast).getAttribute('label'), I18n.t('actions.undo'));
  assert.equal(toast.querySelector('.tf-toast').getAttribute('role'), 'status');
});

test('clicking Undo calls onUndo once and locks the button meanwhile', async () => {
  let calls = 0;
  let release;
  const toast = showUndoToast({ message: 'x', onUndo: () => { calls++; return new Promise((r) => { release = r; }); } });
  undoButton(toast).click();
  undoButton(toast).click();
  assert.equal(calls, 1);
  assert.equal(undoButton(toast).hasAttribute('disabled'), true);
  release();
  await sleep(0);
});

test('a refused undo is reported in a second toast', async () => {
  const toast = showUndoToast({ message: 'x', onUndo: () => Promise.reject(new Error('already changed')) });
  undoButton(toast).click();
  await sleep(0);
  const messages = [...document.querySelectorAll('.tf-toast-message')].map((m) => m.textContent);
  assert.ok(messages.some((m) => m === I18n.t('actions.undo_failed', { message: 'already changed' })), messages.join('|'));
});

test('the toast starts leaving after the timeout', async () => {
  const toast = showUndoToast({ message: 'x', onUndo() {}, timeoutMs: 20 });
  assert.equal(toast.querySelector('.tf-toast').classList.contains('tf-toast-out'), false);
  await sleep(100);
  assert.equal(toast.querySelector('.tf-toast').classList.contains('tf-toast-out'), true);
});
