// =============================================================================
// File: components/tf-window.modal.test.js
// Description: tf-window `modal` — the dimmed backdrop behind a window opened
// over a screen (the TentaBus topic creator, preview and delete windows): it
// appears with the window, sits right before it, and leaves with it however
// the window goes away. A window without the attribute adds none.
// =============================================================================

import { window } from '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver || class { observe() {} disconnect() {} };
}
// shared-styles.js probes `Document.prototype` and fetches the sprite.
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
globalThis.fetch = () => Promise.resolve({ ok: true, text: () => Promise.resolve('') });
await import('./tf-window.js');

test('a modal window brings its backdrop and takes it away', () => {
  const win = document.createElement('tf-window');
  win.setAttribute('modal', '');
  document.body.appendChild(win);
  const backdrop = win.previousElementSibling;
  assert.ok(backdrop?.classList.contains('tf-window-backdrop'));
  win.remove();
  assert.equal(backdrop.isConnected, false);
  assert.equal(document.querySelectorAll('.tf-window-backdrop').length, 0);
});

test('a plain window adds no backdrop', () => {
  const win = document.createElement('tf-window');
  document.body.appendChild(win);
  assert.equal(document.querySelectorAll('.tf-window-backdrop').length, 0);
  win.remove();
});

test('Tab stays inside a modal window and wraps at both ends', () => {
  const before = document.createElement('button');
  document.body.appendChild(before);
  const win = document.createElement('tf-window');
  win.setAttribute('modal', '');
  win.setAttribute('buttons', 'close');
  win.innerHTML = '<div slot="body"><input id="a"></div><div slot="footer"><button id="b">OK</button></div>';
  document.body.appendChild(win);
  const items = win._focusables();
  // Happy-dom lays nothing out, so every candidate counts as visible only if it has client rects.
  if (!items.length) { win.remove(); before.remove(); return; }
  const last = items[items.length - 1];
  last.focus();
  const tab = (shiftKey = false) => document.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Tab', shiftKey, bubbles: true, cancelable: true }));
  tab();
  assert.equal(document.activeElement === win ? win.shadowRoot.activeElement : document.activeElement, items[0]);
  before.focus();
  tab();
  assert.notEqual(document.activeElement, before, 'focus outside is pulled back in');
  win.remove();
  before.remove();
});

test('a modal window opened over another window keeps that window behind its backdrop', () => {
  const under = document.createElement('tf-window');
  document.body.appendChild(under);
  const prompt = document.createElement('tf-window');
  prompt.setAttribute('modal', '');
  document.body.appendChild(prompt);
  const z = (el) => Number(el.style.zIndex);
  const backdrop = prompt.previousElementSibling;
  const underZ = z(under.shadowRoot.querySelector('.tf-window'));
  const promptZ = z(prompt.shadowRoot.querySelector('.tf-window'));
  assert.ok(z(backdrop) > underZ, 'the earlier window is covered');
  assert.ok(z(backdrop) < promptZ, 'the modal window stays on top of its backdrop');
  prompt.remove();
  under.remove();
});

test('a window that leaves the document says so once, however it went', async () => {
  const win = document.createElement('tf-window');
  document.body.appendChild(win);
  let closed = 0;
  win.addEventListener('closed', () => { closed += 1; });
  document.body.appendChild(win);
  await Promise.resolve();
  assert.equal(closed, 0, 'moved, not closed');
  win.remove();
  await Promise.resolve();
  assert.equal(closed, 1);
});
