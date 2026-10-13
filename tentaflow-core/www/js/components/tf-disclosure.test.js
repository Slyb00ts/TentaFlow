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
