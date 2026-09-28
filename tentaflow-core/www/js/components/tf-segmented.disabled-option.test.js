// =============================================================================
// File: components/tf-segmented.disabled-option.test.js
// Description: A `disabled` <option> of <tf-segmented> stays disabled after
// the control builds its buttons. The build used to drop the attribute, so a
// wizard transport the node cannot serve (iSER on an interface without an RDMA
// device) rendered as a live button that could be clicked.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

await import('./tf-segmented.js');

function mount(html, value) {
  const el = document.createElement('tf-segmented');
  el.setAttribute('value', value);
  el.innerHTML = html;
  document.body.appendChild(el);
  return el;
}

const buttons = (el) => [...el.querySelectorAll('.tf-seg-opt')];
const state = (el) => buttons(el).map((b) => [b.dataset.value, b.disabled]);

test('a disabled option is shown, disabled, and neither a click nor the keyboard selects it', () => {
  const el = mount('<option value="tcp">TCP</option><option value="iser" disabled>iSER</option><option value="x">X</option>', 'tcp');
  const changes = [];
  el.addEventListener('change', (e) => changes.push(e.detail.value));
  assert.deepEqual(state(el), [['tcp', false], ['iser', true], ['x', false]]);
  buttons(el)[1].click();
  assert.equal(el.value, 'tcp');
  // ArrowRight skips the disabled option.
  el.querySelector('.tf-segmented').dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  assert.equal(el.value, 'x');
  assert.deepEqual(changes, ['x']);
  el.remove();
});

test('the current value may be a disabled option, and moving off it works', () => {
  const el = mount('<option value="tcp">TCP</option><option value="iser" disabled>iSER</option>', 'iser');
  assert.ok(buttons(el)[1].classList.contains('active'), 'the kept choice is still shown as the current one');
  buttons(el)[0].click();
  assert.equal(el.value, 'tcp');
  el.remove();
});

test('setOptions carries `disabled`, and a whole-control disabled still disables every option', () => {
  const el = mount('<option value="a">A</option>', 'a');
  el.setOptions([{ value: 'a', label: 'A' }, { value: 'b', label: 'B', disabled: true }], 'a');
  assert.deepEqual(state(el), [['a', false], ['b', true]]);
  el.setAttribute('disabled', '');
  assert.deepEqual(state(el), [['a', true], ['b', true]]);
  el.removeAttribute('disabled');
  assert.deepEqual(state(el), [['a', false], ['b', true]], 'the per-option state survives the control being re-enabled');
  el.remove();
});
