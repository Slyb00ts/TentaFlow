// =============================================================================
// File: modules/tentabus/partitions.test.js
// Description: Partitions and their copies (U2 "Partycje i kopie", T10): the
// state of every copy, which node may take a partition's leadership and why
// the others may not, the rows of a topic's table and of the partitions that
// need attention, and the "Przenieś prowadzenie" window (only an in-sync copy
// can be chosen; a refusal stays in the window; success reports the node).
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  copyStates, transferChoices, transferBlocker, partitionRows, attentionRows, behindText, unavailableText, rangeText, openLeaderTransfer,
} = await import('./partitions.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 0));
const MIB = 1024 ** 2;

const nodes = [
  { nodeId: 'n-rig', label: 'rig26', reachable: true, isLocal: true },
  { nodeId: 'n-main', label: 'mainpc', reachable: true },
  { nodeId: 'n-mac', label: 'mac-studio', reachable: true },
];
const healthy = { partition: 1, leaderNodeId: 'n-main', replicas: ['n-rig', 'n-main', 'n-mac'], isr: ['n-rig', 'n-main', 'n-mac'], lagging: [] };
const behind = {
  partition: 0,
  leaderNodeId: 'n-main',
  replicas: ['n-rig', 'n-main', 'n-mac'],
  isr: ['n-rig', 'n-main'],
  lagging: [{ nodeId: 'n-mac', lagBytes: 87 * MIB, lagMs: 3400, reason: 'lag_bytes=1 exceeds max_bytes=0' }],
};

test('each copy has a state: leader, in sync, behind (with how far), out of sync', () => {
  assert.deepEqual(copyStates(behind, nodes).map((c) => [c.label, c.state]), [['rig26', 'ok'], ['mainpc', 'leader'], ['mac-studio', 'lag']]);
  assert.equal(copyStates(behind, nodes)[2].lagBytes, 87 * MIB);
  const out = { ...healthy, isr: ['n-main'] };
  assert.deepEqual(copyStates(out, nodes).map((c) => c.state), ['out', 'leader', 'out']);
});

test('only an in-sync copy on a node that answers may take over, and the reason is said otherwise', () => {
  assert.deepEqual(transferChoices(behind, nodes).map((c) => c.state), ['ok', 'leader', 'lag']);
  const down = [nodes[0], nodes[1], { ...nodes[2], reachable: false }];
  assert.deepEqual(transferChoices(healthy, down).map((c) => c.state), ['ok', 'leader', 'down']);
  assert.equal(transferBlocker(behind, nodes), null);
  assert.match(transferBlocker(behind, nodes, true), /przed chwilą/);
  assert.match(transferBlocker({ partition: 0, leaderNodeId: 'n-rig', replicas: ['n-rig'], isr: ['n-rig'], lagging: [] }, nodes), /jedną kopię/);
  assert.match(transferBlocker({ ...behind, isr: ['n-main'], lagging: [] }, nodes), /Żaden inny node/);
  assert.match(transferBlocker({ ...healthy, leaderNodeId: null }, nodes), /nie ma teraz nodu prowadzącego/);
});

test('a topic\'s rows join its partitions (numbers, size) with their copies', () => {
  const rows = partitionRows({
    detailPartitions: [
      { partition: 0, earliestOffset: 100, highWatermark: 2000, sizeBytes: 6 * MIB },
      { partition: 1, earliestOffset: 0, highWatermark: 0, sizeBytes: 0 },
    ],
    replicaPartitions: [healthy, behind],
    nodes,
  });
  assert.equal(rows[0].leader, 'mainpc');
  assert.deepEqual(rows[0].range, { from: 100, to: 1999 });
  assert.equal(norm(rangeText(rows[0].range)), 'od 100 do 1 999');
  assert.equal(rangeText(rows[1].range), 'brak wiadomości');
  assert.equal(rows[0].copies.length, 3);
  assert.equal(rows[1].replica, healthy);
});

test('partitions needing attention across topics, and how far each copy trails', () => {
  const rows = attentionRows([
    { topic: 'odczyty-urzadzen', partitions: [healthy, behind] },
    { topic: 'faktury', partitions: [{ ...healthy, partition: 3, unavailableReason: 'NoIsr' }] },
  ], nodes);
  assert.deepEqual(rows.map((r) => r.key), ['faktury:3', 'odczyty-urzadzen:0']);
  assert.equal(norm(behindText(rows[1].behind)), 'mac-studio: 87 MB · 4 s');
  assert.equal(unavailableText(rows[0].unavailable), 'brak zgodnej kopii');
  assert.equal(unavailableText('no_assignment'), 'bez nodu prowadzącego');
  assert.equal(unavailableText('Mystery'), 'partycja niedostępna');
});

function open(overrides = {}) {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const calls = { sent: [], done: [] };
  const win = openLeaderTransfer({
    topic: 'odczyty-urzadzen',
    partition: 0,
    choices: transferChoices(behind, nodes),
    transfer: async (id) => { calls.sent.push(id); },
    describeError: () => 'Większość nodów nie potwierdziła zmiany.',
    onDone: (r) => calls.done.push(r),
    ...overrides,
  });
  return { win, calls };
}

test('the window offers the in-sync copy, disables the others with the reason, and says what happens', async () => {
  const { win, calls } = open();
  assert.equal(win.getAttribute('modal'), '');
  assert.match(win._titleEl.textContent, /odczyty-urzadzen, partycja 0/);
  const cards = [...win.querySelectorAll('tf-choice-card')];
  assert.deepEqual(cards.map((c) => [c.getAttribute('heading'), c.hasAttribute('disabled')]), [['rig26', false], ['mainpc', true], ['mac-studio', true]]);
  assert.match(cards[2].getAttribute('description'), /w tyle o 87 MB — nie może przejąć/);
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Partycję 0 będzie prowadził node rig26\..*najwyżej 5 s.*musi go wysłać ponownie/);
  assert.match(win.textContent, /dzienniku audytu/);
  win.querySelector('[data-act="move"]').click();
  await tick();
  assert.deepEqual(calls.sent, ['n-rig']);
  assert.deepEqual(calls.done, [{ nodeId: 'n-rig', label: 'rig26' }]);
});

test('a refused move stays in the window with the reason', async () => {
  const { win, calls } = open({ transfer: async () => { throw new Error('bus.replication'); } });
  win.querySelector('[data-act="move"]').click();
  await tick();
  await tick();
  assert.equal(calls.done.length, 0);
  assert.equal(win.isConnected, true);
  assert.match(win.querySelector('[data-role="error"]').textContent, /nie potwierdziła/);
});

test('with no node to choose the move cannot be sent', () => {
  const { win } = open({ choices: transferChoices({ ...behind, isr: ['n-main'], lagging: [] }, nodes) });
  assert.ok(win.querySelector('[data-act="move"]').hasAttribute('disabled'));
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Wybierz node ze zgodną kopią/);
});
