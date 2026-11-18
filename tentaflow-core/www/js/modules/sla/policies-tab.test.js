import '../tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { renderPolicyTab, policyRowActions, busPolicyIdsOf } = await import('./policies-tab.js');
const { slaCellHtml, paintSlaCells } = await import('./flow-cell.js');
const { slaView } = await import('./model.js');

const policy = (id, name, assignedFlows) => ({ id, assignedFlows, spec: { name, description: '', maxConcurrency: 4, ordering: 'partition' } });
const handlers = { onNew() {}, onEdit() {}, onDelete() {} };

test('without can_manage the list is read-only: no row actions, no create button', () => {
  const host = document.createElement('div');
  const table = renderPolicyTab(host, { policies: [policy('p1', 'Standard', 0)], canManage: false }, handlers);
  assert.ok(!table.rowActions);
  const empty = document.createElement('div');
  renderPolicyTab(empty, { policies: [], canManage: false }, handlers);
  assert.equal(empty.querySelector('#empty-new-policy'), null);
  renderPolicyTab(empty, { policies: [], canManage: true }, handlers);
  assert.ok(empty.querySelector('#empty-new-policy'));
});

test('delete is disabled for a policy in use and says how many flows hold it', () => {
  const host = document.createElement('div');
  const table = renderPolicyTab(host, { policies: [policy('p1', 'Standard', 2)], canManage: true }, handlers);
  assert.equal(typeof table.rowActions, 'function');
  const row = { id: 'p1', assignedFlows: 2 };
  const buttons = policyRowActions(row, () => row, handlers).querySelectorAll('tf-button');
  const del = buttons[buttons.length - 1];
  assert.ok(del.hasAttribute('disabled'));
  assert.match(del.getAttribute('title'), /2 przepływów/);
  const free = { id: 'p2', assignedFlows: 0 };
  const freeButtons = policyRowActions(free, () => free, handlers).querySelectorAll('tf-button');
  assert.ok(!freeButtons[freeButtons.length - 1].hasAttribute('disabled'));
});

test('a governed-elsewhere cell has no policy name of ours and a repaint keeps its nodes', () => {
  const flow = { id: 'f1', slaPolicyId: 'x', slaBrief: { state: 'ok', inFlight: 1, maxConcurrency: 2, governedElsewhere: true } };
  const view = slaView(flow, new Map());
  const root = document.createElement('div');
  root.innerHTML = slaCellHtml('f1', view);
  assert.match(root.textContent, /Inna organizacja/);
  const chip = root.querySelector('[data-sla-chip]');
  paintSlaCells(root, new Map([['f1', view]]));
  assert.equal(chip.getAttribute('label'), 'OK');
  const next = slaView({ ...flow, slaBrief: { ...flow.slaBrief, state: 'warn', inFlight: 2 } }, new Map());
  paintSlaCells(root, new Map([['f1', next]]));
  assert.equal(root.querySelector('[data-sla-chip]'), chip, 'the chip node survives a poll');
  assert.equal(chip.getAttribute('status'), 'warn');
  assert.match(root.querySelector('[data-sla-detail]').textContent, /2\/2/);
});

test('system flows read "niedostępna"', () => {
  assert.match(slaCellHtml('s', slaView({ id: 's', isSystem: true }, new Map())), /niedostępna/);
});

test('bus captions appear only in the rows of policies that a bus-fed flow uses', () => {
  const flows = [
    { id: 'a', slaPolicyId: 'p-bus', busConsumer: { state: 'running' } },
    { id: 'b', slaPolicyId: 'p-api' },
    { id: 'c', slaPolicyId: 'p-sys', busConsumer: { state: 'running' }, isSystem: true },
    { id: 'd', busConsumer: { state: 'running' } },
  ];
  const bus = busPolicyIdsOf(flows);
  assert.deepEqual([...bus], ['p-bus']);
  const withSpec = (id) => ({ id, assignedFlows: 1, spec: { name: id, maxConcurrency: 2, callLimit: 10, callWindowSecs: 60, ordering: 'partition' } });
  const host = document.createElement('div');
  const table = renderPolicyTab(host, { policies: [withSpec('p-bus'), withSpec('p-api')], canManage: false }, handlers, { busPolicyIds: bus });
  const [busRow, apiRow] = table.rows;
  assert.match(busRow.threads, /bus: równolegle z różnych partycji/);
  assert.match(busRow.calls, /bus tylko zwalnia/);
  assert.doesNotMatch(apiRow.threads, /bus/);
  assert.doesNotMatch(apiRow.calls, /bus/);
});
