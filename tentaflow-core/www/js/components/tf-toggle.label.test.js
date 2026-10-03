// ============ File: tf-toggle.label.test.js — visible and accessible toggle captions ============

import { window } from '../sdk-runtime/_dom-test-harness.js';
import test, { afterEach } from 'node:test';
import assert from 'node:assert/strict';
import './tf-toggle.js';

afterEach(() => document.body.replaceChildren());

function mount(attributes = {}) {
  const toggle = document.createElement('tf-toggle');
  for (const [name, value] of Object.entries(attributes)) toggle.setAttribute(name, value);
  document.body.append(toggle);
  return toggle;
}

test('label is visible escaped text and names the focused switch unless aria-label is explicit', () => {
  const text = 'Interrupt <customer & order>';
  const toggle = mount({ label: text });
  const switchControl = toggle.querySelector('[role="switch"]');
  assert.equal(toggle.querySelector('.tf-toggle__label').textContent, text);
  assert.equal(toggle.querySelector('script'), null);
  assert.equal(switchControl.getAttribute('aria-label'), text);
  assert.equal(switchControl.getAttribute('tabindex'), '0');
  toggle.setAttribute('aria-label', 'Explicit switch name');
  assert.equal(switchControl.getAttribute('aria-label'), 'Explicit switch name');
  toggle.label = 'Updated caption';
  assert.equal(toggle.querySelector('.tf-toggle__label').textContent, 'Updated caption');
  assert.equal(switchControl.getAttribute('aria-label'), 'Explicit switch name');
  toggle.removeAttribute('aria-label');
  assert.equal(switchControl.getAttribute('aria-label'), 'Updated caption');
  toggle.label = null;
  assert.equal(toggle.querySelector('.tf-toggle__label'), null);
  assert.equal(switchControl.hasAttribute('aria-label'), false);
});

test('an unlabeled switch does not shadow an external form caption', () => {
  const field = document.createElement('label');
  field.className = 'tf-toggle-field';
  const toggle = document.createElement('tf-toggle');
  toggle.setAttribute('aria-label', 'Notifications');
  const caption = document.createElement('span');
  caption.className = 'tf-toggle__label';
  caption.textContent = 'Notifications';
  field.append(toggle, caption);
  document.body.append(field);
  assert.equal(field.querySelector('.tf-toggle__label'), caption);
  assert.equal(toggle.querySelector('.tf-toggle__label'), null);
  assert.equal(toggle.querySelector('[role="switch"]').getAttribute('aria-label'), 'Notifications');
  toggle.label = 'Local caption';
  assert.equal(toggle.querySelector('.tf-toggle__label').textContent, 'Local caption');
  toggle.label = '';
  assert.equal(field.querySelector('.tf-toggle__label'), caption);
  assert.equal(toggle.querySelector('.tf-toggle__label'), null);
  assert.equal(toggle.querySelector('[role="switch"]').getAttribute('aria-label'), 'Notifications');
});

test('inner switch click and Space each change once; disabled blocks both', () => {
  const toggle = mount({ label: 'Interrupt activity' });
  const switchControl = toggle.querySelector('[role="switch"]');
  const changes = [];
  toggle.addEventListener('change', (event) => changes.push(event.detail.checked));
  switchControl.click();
  assert.equal(toggle.checked, true);
  assert.deepEqual(changes, [true]);
  switchControl.dispatchEvent(new window.KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true }));
  assert.equal(toggle.checked, false);
  assert.deepEqual(changes, [true, false]);
  toggle.querySelector('.tf-toggle__label').click();
  assert.equal(toggle.checked, true);
  assert.deepEqual(changes, [true, false, true]);
  toggle.setAttribute('disabled', '');
  assert.equal(switchControl.getAttribute('tabindex'), '-1');
  switchControl.click();
  switchControl.dispatchEvent(new window.KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true }));
  toggle.querySelector('.tf-toggle__label').click();
  assert.equal(toggle.checked, true);
  assert.deepEqual(changes, [true, false, true]);
});
