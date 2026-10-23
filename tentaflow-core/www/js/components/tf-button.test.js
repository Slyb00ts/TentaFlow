// =============================================================================
// File: components/tf-button.test.js
// Description: Host attributes stay on <tf-button> (selectors, closest,
//   toggleAttribute, cloneNode) and are mirrored onto the inner button.
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

test('the host keeps aria-label so attribute selectors and closest() match it', () => {
  const el = mount('aria-label="Row actions"');
  assert.equal(document.querySelector('tf-button[aria-label="Row actions"]'), el);
  assert.equal(el.querySelector('button').getAttribute('aria-label'), 'Row actions');
  assert.equal(el.querySelector('button').closest('tf-button[aria-label="Row actions"]'), el);
});

test('later changes reach both the host and the inner button', () => {
  const el = mount();
  el.setAttribute('aria-label', 'Later');
  el.setAttribute('title', 'Hint');
  assert.equal(el.getAttribute('aria-label'), 'Later');
  assert.equal(el.querySelector('button').getAttribute('aria-label'), 'Later');
  assert.equal(el.querySelector('button').getAttribute('title'), 'Hint');
  el.removeAttribute('aria-label');
  assert.equal(el.querySelector('button').hasAttribute('aria-label'), false);
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

test('a deep clone keeps the label on the host and on its one button', () => {
  const copy = mount('aria-label="Copy me"').cloneNode(true);
  document.body.appendChild(copy);
  assert.equal(copy.getAttribute('aria-label'), 'Copy me');
  assert.equal(copy.querySelector('button').getAttribute('aria-label'), 'Copy me');
  assert.equal(copy.querySelectorAll('button').length, 1, 'a button inside a button is not built');
  copy.querySelector('button').click();
});
