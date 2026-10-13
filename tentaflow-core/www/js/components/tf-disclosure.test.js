// =============================================================================
// File: components/tf-disclosure.test.js
// Description: The collapsible section — label, content kept, open state
//   reflected both ways, the toggle event.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, cleanBody } from '../lib/actions/_test-setup.js';
await import('./tf-disclosure.js');

beforeEach(cleanBody);

function mount(html = '<p data-role="inner">Details</p>', attrs = 'label="More"') {
  const host = document.createElement('div');
  host.innerHTML = `<tf-disclosure ${attrs}>${html}</tf-disclosure>`;
  document.body.appendChild(host);
  return host.querySelector('tf-disclosure');
}

test('the label is the summary and the content stays inside, closed by default', () => {
  const el = mount();
  assert.equal(el.querySelector('summary').textContent, 'More');
  assert.equal(el.querySelector('[data-role="inner"]').textContent, 'Details');
  assert.equal(el.querySelector('details').open, false);
  assert.equal(el.open, false);
});

test('open is reflected into the details and back', () => {
  const el = mount('<p>x</p>', 'label="More" open');
  assert.equal(el.querySelector('details').open, true);
  el.open = false;
  assert.equal(el.querySelector('details').open, false);
  el.setAttribute('label', 'Less');
  assert.equal(el.querySelector('summary').textContent, 'Less');
});

test('toggling the details sets open on the host and tells listeners', () => {
  const el = mount();
  const seen = [];
  el.addEventListener('toggle', (e) => seen.push(e.detail.open));
  const details = el.querySelector('details');
  details.open = true;
  details.dispatchEvent(new window.Event('toggle'));
  assert.equal(el.hasAttribute('open'), true);
  assert.deepEqual(seen, [true]);
});

test('children appended after the first render go inside the fold', async () => {
  const el = mount();
  const late = document.createElement('p');
  late.dataset.role = 'late';
  el.appendChild(late);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(late.parentElement.className, 'tf-disclosure__body');
  assert.equal(el.querySelector('details').contains(late), true);
  assert.equal(el.children.length, 1, 'the host holds only the details');
});

test('code that sets open gets the toggle event like a click, once, and opening scrolls the content into view', () => {
  const el = mount();
  const seen = [];
  el.addEventListener('toggle', (e) => seen.push(e.detail.open));
  const scrolled = [];
  el.querySelector('.tf-disclosure__body').scrollIntoView = (options) => scrolled.push(options);
  el.open = true;
  assert.deepEqual(seen, [true]);
  assert.deepEqual(scrolled, [{ block: 'nearest' }]);
  // The details' own toggle event that follows a programmatic change adds nothing.
  el.querySelector('details').dispatchEvent(new window.Event('toggle'));
  assert.deepEqual(seen, [true]);
  el.open = false;
  assert.deepEqual(seen, [true, false]);
  assert.equal(scrolled.length, 1, 'closing does not scroll');
});

test('the focus ring hugs the label and the marker, not the whole row', async () => {
  const { readFile } = await import('node:fs/promises');
  const css = await readFile(new URL('../../css/controls.css', import.meta.url), 'utf8');
  const summary = /\.tf-disclosure__summary \{([^}]*)\}/.exec(css)?.[1] || '';
  assert.match(summary, /width: fit-content/);
  assert.match(summary, /list-style-position: inside/);
  assert.match(css, /\.tf-disclosure__summary:focus-visible \{[^}]*outline: 2px solid/);
});
