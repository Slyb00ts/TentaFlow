// =============================================================================
// File: lib/actions/bulk.test.js
// Description: The bulk bar over a selectable tf-table — hidden without a
//   selection, counts ticked rows (with plural forms), runs an action on them,
//   clears the selection.
// =============================================================================

import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { window, I18n, cleanBody, sleep } from './_test-setup.js';
await import('/js/components/tf-table.js');
const { attachBulkBar } = await import('./bulk.js');

beforeEach(cleanBody);

function mount(rows = [{ name: 'a' }, { name: 'b' }, { name: 'c' }]) {
  const table = document.createElement('tf-table');
  table.setAttribute('selectable', 'multi');
  table.innerHTML = '<tf-column key="name" label="Name"></tf-column>';
  document.body.appendChild(table);
  table.rows = rows.map((r) => ({ ...r, _selected: false }));
  return table;
}

function tick(table, name, on = true) {
  const box = [...table.shadowRoot.querySelectorAll('tbody tr')]
    .find((tr) => tr.textContent.includes(name)).querySelector('.tf-table__row-select');
  box.checked = on;
  box.dispatchEvent(new window.Event('change', { bubbles: true, composed: true }));
}

test('the bar is hidden until a row is ticked and follows the selection', () => {
  const table = mount();
  const { bar } = attachBulkBar(table, [{ id: 'assign', label: 'Assign', run() {} }]);
  assert.equal(bar.hidden, true);
  tick(table, 'a');
  assert.equal(bar.hidden, false);
  assert.equal(bar.querySelector('.tf-bulkbar__count').textContent, I18n.t('actions.bulk.selected', { count: 1 }));
  tick(table, 'b');
  assert.equal(bar.querySelector('.tf-bulkbar__count').textContent, '2 items selected');
  tick(table, 'a', false);
  tick(table, 'b', false);
  assert.equal(bar.hidden, true);
});

test('select-all counts every row', () => {
  const table = mount();
  const { bar } = attachBulkBar(table, []);
  const all = table.shadowRoot.querySelector('.tf-table__select-all');
  all.checked = true;
  all.dispatchEvent(new window.Event('change', { bubbles: true, composed: true }));
  assert.equal(bar.querySelector('.tf-bulkbar__count').textContent, '3 items selected');
});

test('an action receives the ticked rows and a subject naming the count', async () => {
  const table = mount();
  let seen;
  const { bar } = attachBulkBar(table, [{ id: 'move', label: 'Move', run: async (rows, ctx) => { seen = { rows, ctx }; } }]);
  tick(table, 'b');
  tick(table, 'c');
  bar.querySelector('[data-action="move"]').click();
  await sleep(0);
  assert.deepEqual(seen.rows.map((r) => r.name), ['b', 'c']);
  assert.equal(seen.ctx.subject, 'Selected: 2 items');
  assert.equal(seen.ctx.anchor, bar.querySelector('[data-action="move"]'));
});

test('a refused action is reported and the bar unlocks', async () => {
  const table = mount();
  const { bar } = attachBulkBar(table, [{ id: 'x', label: 'X', run: () => Promise.reject(new Error('server said no')) }]);
  tick(table, 'a');
  bar.querySelector('[data-action="x"]').click();
  await sleep(0);
  assert.match(document.querySelector('.tf-toast-message').textContent, /server said no/);
  assert.equal(bar.querySelector('[data-action="x"]').hasAttribute('disabled'), false);
});

test('Clear selection unticks every row and hides the bar', () => {
  const table = mount();
  const { bar } = attachBulkBar(table, []);
  tick(table, 'a');
  tick(table, 'b');
  bar.querySelector('[data-clear]').click();
  assert.equal(bar.hidden, true);
  assert.equal(table.rows.some((r) => r._selected), false);
});

test('update() after a reload drops the stale count', () => {
  const table = mount();
  const handle = attachBulkBar(table, []);
  tick(table, 'a');
  table.rows = [{ name: 'a' }, { name: 'b' }];
  handle.update();
  assert.equal(handle.bar.hidden, true);
});

test('destroy() removes the bar and stops listening', () => {
  const table = mount();
  const handle = attachBulkBar(table, []);
  handle.destroy();
  tick(table, 'a');
  assert.equal(handle.bar.isConnected, false);
  assert.equal(handle.bar.hidden, true);
});
