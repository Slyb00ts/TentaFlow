// =============================================================================
// File: components/tf-input.stepper.test.js
// Description: tf-input `stepper` — the − / + buttons around a number field
// (the topic creator's "Partycje"): they move the value by `step` inside
// [min, max], emit the same input/change events as typing, carry their own
// accessible names and disable themselves at a bound.
// =============================================================================

import { window } from '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.MutationObserver !== 'function') globalThis.MutationObserver = window.MutationObserver;
const { TfInput } = await import('./tf-input.js');

function mount(attrs = {}) {
  const el = new TfInput();
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  document.body.appendChild(el);
  return el;
}

test('buttons step the value inside the bounds and report it like typing', () => {
  const el = mount({ type: 'number', stepper: '', min: '1', max: '3', value: '2', 'stepper-dec-label': 'Mniej', 'stepper-inc-label': 'Więcej' });
  const dec = el.querySelector('.tf-input-step--dec');
  const inc = el.querySelector('.tf-input-step--inc');
  assert.equal(dec.getAttribute('aria-label'), 'Mniej');
  assert.equal(inc.getAttribute('aria-label'), 'Więcej');
  const seen = [];
  el.addEventListener('input', (e) => seen.push(['input', e.detail.value]));
  el.addEventListener('change', (e) => seen.push(['change', e.detail.value]));
  inc.click();
  assert.equal(el.value, '3');
  assert.deepEqual(seen, [['input', '3'], ['change', '3']]);
  assert.equal(inc.disabled, true, 'the upper bound disables +');
  inc.click();
  assert.equal(el.value, '3');
  dec.click();
  dec.click();
  assert.equal(el.value, '1');
  assert.equal(dec.disabled, true, 'the lower bound disables −');
  assert.equal(inc.disabled, false);
});

test('an empty field steps from the lower bound; step is honoured', () => {
  const el = mount({ type: 'number', stepper: '', min: '0', max: '100', step: '10' });
  el.querySelector('.tf-input-step--inc').click();
  assert.equal(el.value, '0');
  el.querySelector('.tf-input-step--inc').click();
  assert.equal(el.value, '10');
});

test('without the attribute there are no buttons; removing it takes them away', () => {
  const plain = mount({ type: 'number' });
  assert.equal(plain.querySelector('.tf-input-step'), null);
  const el = mount({ type: 'number', stepper: '' });
  assert.equal(el.querySelectorAll('.tf-input-step').length, 2);
  el.removeAttribute('stepper');
  assert.equal(el.querySelector('.tf-input-step'), null);
});

test('a disabled field disables both buttons', () => {
  const el = mount({ type: 'number', stepper: '', value: '5', disabled: '' });
  assert.equal(el.querySelector('.tf-input-step--dec').disabled, true);
  assert.equal(el.querySelector('.tf-input-step--inc').disabled, true);
});
