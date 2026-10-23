// =============================================================================
// File: components/tf-button.test.js
// Description: Global ARIA given to <tf-button> lives on the inner button and
//   leaves a data mirror on the presentational host (selectors, cloneNode).
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { cleanBody } from '../lib/actions/_test-setup.js';
await import('./tf-button.js');

beforeEach(cleanBody);

function mount(attrs = '') {
  const host = document.createElement('div');
  host.innerHTML = `<tf-button ${attrs}>Go</tf-button>`;
  document.body.appendChild(host);
  return host.querySelector('tf-button');
}

test('aria-label is taken to the inner button and mirrored on the host as data-aria-label', () => {
  const el = mount('aria-label="Row actions"');
  assert.equal(el.hasAttribute('aria-label'), false);
  assert.equal(document.querySelector('tf-button[data-aria-label="Row actions"]'), el);
  assert.equal(el.querySelector('button').getAttribute('aria-label'), 'Row actions');
  assert.equal(el.querySelector('button').closest('tf-button[data-aria-label="Row actions"]'), el);
});

test('later changes reach the inner button and a mirror removal clears them', () => {
  const el = mount();
  el.setAttribute('aria-label', 'Later');
  el.setAttribute('title', 'Hint');
  assert.equal(el.getAttribute('data-aria-label'), 'Later');
  assert.equal(el.querySelector('button').getAttribute('aria-label'), 'Later');
  assert.equal(el.querySelector('button').getAttribute('title'), 'Hint');
  el.removeAttribute('data-aria-label');
  assert.equal(el.querySelector('button').hasAttribute('aria-label'), false);
});

test('a presentational host carries no global ARIA attribute, so browsers do not ignore its role', () => {
  const el = mount('aria-label="L" aria-describedby="d" aria-controls="c" aria-expanded="true" aria-haspopup="menu" title="T"');
  assert.equal(el.getAttribute('role'), 'none');
  const global = [...el.attributes].map((attribute) => attribute.name).filter((name) => name.startsWith('aria-') && name !== 'aria-disabled');
  assert.deepEqual(global, []);
  const inner = el.querySelector('button');
  assert.deepEqual(['aria-label', 'aria-describedby', 'aria-controls', 'aria-expanded', 'aria-haspopup'].map((name) => inner.getAttribute(name)), ['L', 'd', 'c', 'true', 'menu']);
});

test('aria-expanded is taken to the inner button and mirrored on the host as data-expanded', () => {
  const el = mount();
  el.setAttribute('aria-expanded', 'true');
  assert.equal(el.querySelector('button').getAttribute('aria-expanded'), 'true');
  assert.equal(el.hasAttribute('aria-expanded'), false);
  assert.equal(el.getAttribute('data-expanded'), 'true');
  el.removeAttribute('data-expanded');
  assert.equal(el.querySelector('button').hasAttribute('aria-expanded'), false);
});

test('the host is presentational, so a title does not make it a second named node', () => {
  const el = mount('title="Hint"');
  assert.equal(el.getAttribute('role'), 'none');
  assert.equal(el.querySelector('button').getAttribute('title'), 'Hint');
});

test('a deep clone keeps the label mirror on the host and the label on its one button', () => {
  const copy = mount('aria-label="Copy me"').cloneNode(true);
  document.body.appendChild(copy);
  assert.equal(copy.getAttribute('data-aria-label'), 'Copy me');
  assert.equal(copy.querySelector('button').getAttribute('aria-label'), 'Copy me');
  assert.equal(copy.querySelectorAll('button').length, 1, 'a button inside a button is not built');
  copy.querySelector('button').click();
});
