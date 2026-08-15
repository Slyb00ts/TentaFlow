// =============================================================================
// File: components/tf-option-row.test.js
// Description: Tests for <tf-option-row> — the row of a list you pick from that
// replaced the bare <button>s of the Code Studio workspace strip, sheet, ask
// composer and member search. What matters is that it stays a real button, that
// a refused option cannot be activated, and that the leading marker keeps both
// of its shapes (an element the module owns, or a shortcut key).
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}

const { TfOptionRow } = await import('./tf-option-row.js');

function mount(props = {}) {
  const row = new TfOptionRow();
  document.body.appendChild(row);
  for (const [k, v] of Object.entries(props)) row[k] = v;
  return row;
}

const button = (row) => row.querySelector('button');

test('row: renders a real button, not a clickable box', () => {
  const row = mount({ label: 'platforma-core' });
  const btn = button(row);
  assert.equal(btn.tagName.toLowerCase(), 'button');
  assert.equal(btn.getAttribute('type'), 'button');
  assert.ok(btn.classList.contains('tf-option-row'));
});

test('row: label and sub-line land in their own elements', () => {
  const row = mount({ label: 'platforma-core', sub: 'mainpc · 1 sesja' });
  assert.equal(row.querySelector('.tf-option-row__label').textContent, 'platforma-core');
  assert.equal(row.querySelector('.tf-option-row__sub').textContent, 'mainpc · 1 sesja');
  assert.equal(row.querySelector('.tf-option-row__sub').hidden, false);
});

test('row: an empty sub-line is hidden instead of holding blank space', () => {
  const row = mount({ label: 'platforma-core' });
  assert.equal(row.querySelector('.tf-option-row__sub').hidden, true);
});

test('row: a module-owned element is the leading marker verbatim', () => {
  const dot = document.createElement('span');
  dot.className = 'cs-dot run';
  const row = mount({ label: 'x', lead: dot });
  const lead = row.querySelector('.tf-option-row__lead');
  assert.equal(lead.hidden, false);
  assert.equal(lead.firstElementChild, dot);
  assert.equal(lead.firstElementChild.className, 'cs-dot run');
});

test('row: a string marker renders as the shortcut badge', () => {
  const row = mount({ label: 'Zezwól raz', marker: '1' });
  assert.equal(row.querySelector('.tf-option-row__marker').textContent, '1');
});

test('row: no marker leaves the lead slot out of the layout', () => {
  const row = mount({ label: 'x' });
  assert.equal(row.querySelector('.tf-option-row__lead').hidden, true);
});

test('row: selection is announced, and it is aria-current, not aria-selected', () => {
  const row = mount({ label: 'x' });
  assert.equal(button(row).hasAttribute('aria-current'), false);
  row.selected = true;
  assert.equal(button(row).getAttribute('aria-current'), 'true');
  row.selected = false;
  assert.equal(button(row).hasAttribute('aria-current'), false);
});

test('row: picking one emits option-select carrying the value', () => {
  const row = mount({ label: 'x', value: 'ws-1' });
  const seen = [];
  row.addEventListener('option-select', (e) => seen.push(e.detail.value));
  button(row).click();
  assert.deepEqual(seen, ['ws-1']);
});

test('row: a refused option is a disabled button and emits nothing', () => {
  const row = mount({ label: 'Zezwól zawsze', value: 'allow_always', disabled: true });
  const btn = button(row);
  assert.equal(btn.disabled, true);
  assert.equal(btn.getAttribute('aria-disabled'), 'true');
  let fired = 0;
  row.addEventListener('option-select', () => { fired += 1; });
  // jsdom dispatches a click even on a disabled button, so the guard inside the
  // row is what has to hold — that is exactly the case being pinned here.
  btn.click();
  assert.equal(fired, 0);
});

test('row: values assigned before upgrade survive the upgrade', () => {
  const row = document.createElement('tf-option-row');
  row.label = 'assigned early';
  row.sub = 'before connect';
  document.body.appendChild(row);
  assert.equal(row.querySelector('.tf-option-row__label').textContent, 'assigned early');
  assert.equal(row.querySelector('.tf-option-row__sub').textContent, 'before connect');
});

test('row: the row does not select itself — the list owns the state', () => {
  const row = mount({ label: 'x', value: 'a' });
  button(row).click();
  assert.equal(row.selected, false);
});
