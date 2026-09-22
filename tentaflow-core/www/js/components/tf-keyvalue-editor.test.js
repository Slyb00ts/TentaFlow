// =============================================================================
// File: components/tf-keyvalue-editor.test.js
// Description: Tests for <tf-keyvalue-editor> — the editable key/value row
//       list backing the generic node-config-form's `type: "object"` arm
//       (config.js), first needed for bus_publish's `headers` param. What
//       matters: `.value` round-trips a plain object through the rendered
//       rows, a half-typed row (only a key OR only a value) is dropped from
//       `.value` but stays visible so the user's in-progress edit is not
//       eaten, add/remove keep the other rows' live edits, and every commit
//       fires one bubbling `change` carrying the current object.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}

const { TfKeyvalueEditor } = await import('./tf-keyvalue-editor.js');

function mount(initial) {
  const el = new TfKeyvalueEditor();
  document.body.appendChild(el);
  if (initial !== undefined) el.value = initial;
  return el;
}

function rowEls(el) {
  return [...el.querySelectorAll('.tf-kve-row')];
}

function setRow(rowEl, key, value) {
  const keyEl = rowEl.querySelector('[data-field="key"]');
  const valEl = rowEl.querySelector('[data-field="value"]');
  keyEl.value = key;
  keyEl.dispatchEvent(new window.Event('change', { bubbles: true }));
  valEl.value = value;
  valEl.dispatchEvent(new window.Event('change', { bubbles: true }));
}

// ---------------------------------------------------------------------------
// .value round-trip
// ---------------------------------------------------------------------------

test('value: setting an object renders one row per entry', () => {
  const el = mount({ 'content-type': 'application/json', 'x-source': 'flow' });
  assert.equal(rowEls(el).length, 2);
  assert.deepEqual(el.value, { 'content-type': 'application/json', 'x-source': 'flow' });
});

test('value: an empty/undefined initial value renders zero rows', () => {
  const el = mount();
  assert.equal(rowEls(el).length, 0);
  assert.deepEqual(el.value, {});
});

test('value: reading back after mount matches what was set, including re-set', () => {
  const el = mount({ a: '1' });
  el.value = { b: '2', c: '3' };
  assert.equal(rowEls(el).length, 2);
  assert.deepEqual(el.value, { b: '2', c: '3' });
});

// ---------------------------------------------------------------------------
// Half-typed rows: visible, but excluded from `.value`
// ---------------------------------------------------------------------------

test('value: a row with a key but no value is dropped from .value, not from the DOM', () => {
  const el = mount();
  el._rows = [{ key: 'content-type', value: '' }];
  el._renderRows();
  assert.equal(rowEls(el).length, 1);
  assert.deepEqual(el.value, {});
});

test('value: a row with a value but no key is dropped from .value', () => {
  const el = mount();
  el._rows = [{ key: '', value: 'application/json' }];
  el._renderRows();
  assert.equal(rowEls(el).length, 1);
  assert.deepEqual(el.value, {});
});

// ---------------------------------------------------------------------------
// Add / remove preserve in-flight edits of the OTHER rows
// ---------------------------------------------------------------------------

test('add row: appends an empty row without disturbing an existing one', () => {
  const el = mount({ a: '1' });
  el._addRow();
  assert.equal(rowEls(el).length, 2);
  assert.deepEqual(el.value, { a: '1' });
});

test('add row: a not-yet-blurred edit on an existing row survives the re-render', () => {
  const el = mount({ a: '1' });
  const valEl = rowEls(el)[0].querySelector('[data-field="value"]');
  valEl.value = '2'; // typed, no change/blur fired yet
  el._addRow();
  const rows = rowEls(el);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].querySelector('[data-field="value"]').value, '2');
});

test('remove row: removing one row keeps the others intact', () => {
  const el = mount({ a: '1', b: '2' });
  const btn = rowEls(el)[0].querySelector('[data-action="remove-row"]');
  btn.click();
  assert.equal(rowEls(el).length, 1);
  assert.deepEqual(el.value, { b: '2' });
});

// ---------------------------------------------------------------------------
// change event
// ---------------------------------------------------------------------------

test('change: editing a row fires one bubbling change carrying the current object', () => {
  const el = mount();
  el._addRow();
  const seen = [];
  el.addEventListener('change', (e) => seen.push(e.detail.value));
  setRow(rowEls(el)[0], 'topic', 'orders.created');
  assert.equal(seen.length, 2); // key change, then value change
  assert.deepEqual(seen[seen.length - 1], { topic: 'orders.created' });
});

test('change: removing a row fires change with the row gone', () => {
  const el = mount({ a: '1' });
  const seen = [];
  el.addEventListener('change', (e) => seen.push(e.detail.value));
  rowEls(el)[0].querySelector('[data-action="remove-row"]').click();
  assert.deepEqual(seen[seen.length - 1], {});
});

// ---------------------------------------------------------------------------
// disabled
// ---------------------------------------------------------------------------

test('disabled: rows render without a remove button and inputs carry disabled', () => {
  const el = mount({ a: '1' });
  el.disabled = true;
  assert.equal(el.hasAttribute('disabled'), true);
  const row = rowEls(el)[0];
  assert.equal(row.querySelector('[data-action="remove-row"]'), null);
  assert.equal(row.querySelector('[data-field="key"]').hasAttribute('disabled'), true);
  assert.equal(row.querySelector('[data-field="value"]').hasAttribute('disabled'), true);
});
