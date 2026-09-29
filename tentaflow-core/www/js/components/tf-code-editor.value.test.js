// =============================================================================
// File: components/tf-code-editor.value.test.js
// Description: Replacing the whole document (`value =`) redraws the rows on
// screen. Per-line versions restart at 0 with every new document, so rows the
// previous document drew must not be taken for the new one — the rendered
// text is checked, not only the `value` property.
// =============================================================================

import { window } from '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

// happy-dom has no canvas backend; the editor measures its character width.
const CONTEXT = new Proxy({}, {
  get(target, property) {
    if (property === 'measureText') return (text) => ({ width: String(text).length * 7 });
    return () => CONTEXT;
  },
});
window.HTMLCanvasElement.prototype.getContext = () => CONTEXT;
if (typeof globalThis.ResizeObserver !== 'function') globalThis.ResizeObserver = class { observe() {} unobserve() {} disconnect() {} };
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
globalThis.fetch = () => Promise.resolve({ ok: true, text: () => Promise.resolve('') });

await import('./tf-code-editor.js');

const frame = () => new Promise((r) => setTimeout(r, 40));
const shownText = (ed) => [...ed.shadowRoot.querySelectorAll('.row')]
  .filter((el) => el._tkey != null)
  .sort((a, b) => a._row - b._row)
  .map((el) => el.textContent)
  .join('\n');

test('a new document with the same number of lines replaces the text on screen', async () => {
  const ed = document.createElement('tf-code-editor');
  ed.setAttribute('readonly', '');
  ed.setAttribute('language', 'json');
  ed.style.height = '300px';
  document.body.appendChild(ed);
  ed.value = '{\n  "version": 2\n}';
  await frame();
  assert.match(shownText(ed), /"version": 2/);
  ed.value = '{\n  "version": 3\n}';
  await frame();
  const text = shownText(ed);
  assert.match(text, /"version": 3/);
  assert.doesNotMatch(text, /"version": 2/);
  ed.remove();
});
