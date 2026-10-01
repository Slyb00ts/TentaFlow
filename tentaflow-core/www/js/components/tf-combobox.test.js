// ============ File: tf-combobox.test.js — Semantic query and selection events preserve stable IDs across input blur ============

import test, { afterEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { window } from '../sdk-runtime/_dom-test-harness.js';
await import('./tf-combobox.js');

afterEach(() => { document.body.innerHTML = ''; });
after(async () => { await window.happyDOM.close(); });

function box(options = [{ value: 'epic-id', label: 'WF-1 · Epic parent' }]) {
  const element = document.createElement('tf-combobox');
  element.setAttribute('clearable', '');
  element.options = options;
  document.body.appendChild(element);
  return element;
}

test('typing emits one structured query and selecting then blurring keeps the semantic ID', () => {
  const element = box();
  const queries = [];
  const selections = [];
  element.addEventListener('input', (event) => queries.push({ target: event.target, detail: event.detail }));
  element.addEventListener('change', (event) => selections.push({ target: event.target, detail: event.detail }));
  const input = element.querySelector('input');
  input.focus(); input.value = 'WF-1';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  assert.equal(queries.length, 1);
  assert.equal(queries[0].target, element);
  assert.deepEqual(queries[0].detail, { query: 'WF-1' });
  element.querySelector('[role="option"]').dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
  input.blur();
  input.dispatchEvent(new Event('change', { bubbles: true }));
  assert.equal(selections.length, 1, 'native blur/change cannot overwrite a selected ID');
  assert.equal(selections[0].target, element);
  assert.deepEqual(selections[0].detail, { value: 'epic-id', label: 'WF-1 · Epic parent' });
  element.querySelector('.tf-combobox-clear').click();
  assert.deepEqual(selections.at(-1).detail, { value: null, label: null });
  assert.equal(selections.length, 2, 'clear remains an explicit semantic selection');
});

test('keyboard selection with equal labels keeps distinct IDs and free input retains its public event', () => {
  const element = box([{ value: 'one', label: 'Same title' }, { value: 'two', label: 'Same title' }]);
  const selected = [];
  element.addEventListener('change', (event) => selected.push(event.detail));
  const input = element.querySelector('input');
  input.focus();
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  assert.equal(selected.at(-1).value, 'two');
  assert.equal(document.activeElement, input);
  element.setAttribute('free-input', ''); element.options = [];
  input.value = 'Custom value';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  input.dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(selected.at(-1), { value: 'Custom value', label: null, free: true });
  assert.equal(selected.length, 2);
});

test('editable text commits once on change while selecting an option then blurring retains its ID', () => {
  const element = box();
  element.setAttribute('free-input', '');
  const selected = [];
  element.addEventListener('change', (event) => selected.push(event.detail));
  const input = element.querySelector('input');
  element.querySelector('[role="option"]').dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
  input.dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(selected, [{ value: 'epic-id', label: 'WF-1 · Epic parent' }]);
  input.value = 'Edited text'; input.dispatchEvent(new Event('input', { bubbles: true }));
  input.dispatchEvent(new Event('change', { bubbles: true }));
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  assert.deepEqual(selected.at(-1), { value: 'Edited text', label: null, free: true });
  assert.equal(selected.length, 2, 'blur and Enter do not duplicate the same commit');
  input.value = ''; input.dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(selected.at(-1), { value: '', label: null, free: true });
});

test('reactive property or attribute updates establish a new baseline for subsequent text commits', () => {
  for (const update of [(element) => { element.value = 'B'; }, (element) => element.setAttribute('value', 'B')]) {
    const element = box([]); element.setAttribute('free-input', '');
    const selected = [];
    element.addEventListener('change', (event) => selected.push(event.detail.value));
    const input = element.querySelector('input');
    input.value = 'A'; input.dispatchEvent(new Event('change', { bubbles: true }));
    update(element);
    input.value = 'A'; input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
    assert.deepEqual(selected, ['A', 'A'], 'a model change to B does not suppress a later real A commit');
  }
});

test('disabled inner native input and change events do not emit editable commits', () => {
  const element = box([]); element.setAttribute('free-input', ''); element.disabled = true;
  const events = [];
  for (const kind of ['input', 'change']) element.addEventListener(kind, (event) => events.push(event.detail));
  const input = element.querySelector('input');
  input.value = 'Unaccepted value';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  input.dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(events, []);
});

test('a focused query opens asynchronous results and commits the matching stable ID', async () => {
  const element = box([]);
  const selected = [];
  element.addEventListener('change', (event) => selected.push(event.detail));
  const input = element.querySelector('input');
  input.focus(); input.value = 'WF-1'; input.dispatchEvent(new Event('input', { bubbles: true }));
  assert.equal(element.querySelector('[role="listbox"]').hidden, true);
  await Promise.resolve();
  element.options = [{ value: 'epic-id', label: 'WF-1 · Epic parent' }, { value: 'other-id', label: 'WF-2 · Other task' }];
  assert.equal(element.querySelector('[role="listbox"]').hidden, false);
  assert.equal(input.getAttribute('aria-expanded'), 'true');
  assert.equal(element.querySelectorAll('[role="option"]')[1].hidden, true);
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  input.blur(); input.dispatchEvent(new Event('change', { bubbles: true }));
  assert.deepEqual(selected, [{ value: 'epic-id', label: 'WF-1 · Epic parent' }]);
  element.options = [{ value: 'epic-id', label: 'WF-1 · Epic parent' }];
  assert.equal(element.querySelector('[role="listbox"]').hidden, true, 'later results do not reopen a committed selection');
});

test('asynchronous options preserve min-chars, disabled, blur and explicit dismissal gates', async () => {
  for (const dismiss of ['threshold', 'disabled', 'blur', 'Escape', 'Tab', 'outside']) {
    const element = box([]);
    element.setAttribute('min-chars', '3');
    const input = element.querySelector('input');
    input.focus(); input.value = dismiss === 'threshold' ? 'WF' : 'WF-1';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    if (dismiss === 'disabled') element.disabled = true;
    else if (dismiss === 'blur') input.blur();
    else if (dismiss === 'outside') document.body.click();
    else if (dismiss === 'Escape' || dismiss === 'Tab') input.dispatchEvent(new KeyboardEvent('keydown', { key: dismiss, bubbles: true }));
    await Promise.resolve(); element.options = [{ value: 'epic-id', label: 'WF-1 · Epic parent' }];
    assert.equal(element.querySelector('[role="listbox"]').hidden, true, dismiss);
    element.remove();
  }
});
