// =============================================================================
// File: modules/tentabus/replication.test.js
// Description: The Kopie i nody tab (T10): node cards counted over the
// reader's topics (leads, holds, copies in sync, last signal), the
// partitions that need attention with "Przenieś prowadzenie" only for an
// administrator and only where a node can take over, partitions per topic
// leading to the topic's own section, and leadership changes in words.
// There is no button to change the copies.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { drawReplication, lastSignal, topicLeadRows, readerFailovers, failoverReason } = await import('./replication.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 20));
const MIB = 1024 ** 2;

const nodes = [
  { nodeId: 'n-rig', label: 'rig26', reachable: true, isLocal: true, lastHeartbeatMsAgo: 0 },
  { nodeId: 'n-main', label: 'mainpc', reachable: true, lastHeartbeatMsAgo: 400 },
  { nodeId: 'n-mac', label: 'mac-studio', reachable: true, lastHeartbeatMsAgo: 1200 },
];
const all3 = ['n-rig', 'n-main', 'n-mac'];
const replicaTopics = [
  { topic: 'odczyty-urzadzen', partitions: [
    { partition: 0, leaderNodeId: 'n-main', replicas: all3, isr: ['n-rig', 'n-main'], lagging: [{ nodeId: 'n-mac', lagBytes: 87 * MIB, lagMs: 3500 }] },
    { partition: 1, leaderNodeId: 'n-rig', replicas: all3, isr: all3, lagging: [] },
  ] },
  { topic: 'wizyty', partitions: [{ partition: 0, leaderNodeId: 'n-mac', replicas: all3, isr: all3, lagging: [] }] },
];
const topics = [
  { name: 'odczyty-urzadzen', partitions: 2 },
  { name: 'wizyty', partitions: 1 },
  { name: '__dlq.wizyty', partitions: 1, isDlq: true },
];
const failovers = [
  { atMs: 1000, topic: 'wizyty', partition: 0, fromNode: 'n-rig', toNode: 'n-mac', durationMs: 1800, reason: 'lease_expired', toEpoch: 3 },
  { atMs: 2000, topic: 'odczyty-urzadzen', partition: 1, fromNode: 'n-mac', toNode: 'n-rig', durationMs: 900, reason: 'manual_transfer', actorLabel: 'Anna Kowalska', toEpoch: 5 },
  { atMs: 3000, topic: '__dlq.wizyty', partition: 0, fromNode: 'n-rig', toNode: 'n-mac', durationMs: 1, reason: 'lease_expired', toEpoch: 2 },
];

function mount({ canAdmin = true, justMoved = new Set(), notice = null, nodesList = nodes } = {}) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const ctx = {
    view: () => ({ nodes: nodesList, replicaTopics, failovers, topics, canAdmin, notice, justMoved, nowMs: Date.now() }),
    go: (a) => moves.push(a),
  };
  drawReplication(body, ctx);
  return { body, moves };
}

test('the last signal: this node answers now, others by their heartbeat', () => {
  assert.equal(lastSignal(nodes[0]), 'teraz (ten node)');
  assert.equal(norm(lastSignal(nodes[1])), '0,4 s temu');
  assert.match(lastSignal({ ...nodes[1], reachable: false }), /nie odpowiada/);
});

test('partitions per topic and who leads them; leadership changes of the reader\'s topics only, newest first', () => {
  const rows = topicLeadRows({ topics, perTopic: replicaTopics, nodes, attention: [{ topic: 'odczyty-urzadzen' }] });
  assert.deepEqual(rows.map((r) => [r.name, r.partitions, r.attention]), [['odczyty-urzadzen', 2, 1], ['wizyty', 1, 0]]);
  assert.deepEqual(rows[0].leads.map((l) => `${l.label} ${l.n}`), ['rig26 1', 'mainpc 1', 'mac-studio 0']);
  const changes = readerFailovers(failovers, topics);
  assert.deepEqual(changes.map((f) => f.topic), ['odczyty-urzadzen', 'wizyty']);
  assert.equal(failoverReason(changes[0]), 'przeniesione ręcznie przez: Anna Kowalska');
  assert.equal(failoverReason({ ...changes[1], fromNode: 'rig26' }), 'rig26 przestał odpowiadać');
});

test('node cards count leads, copies and copies in sync over the reader\'s topics', () => {
  const { body } = mount();
  const cards = [...body.querySelectorAll('.tb-node-card')];
  assert.deepEqual(cards.map((c) => c.dataset.node), ['n-rig', 'n-main', 'n-mac']);
  const text = (c, role) => norm(c.querySelector(`[data-role="${role}"]`).textContent);
  assert.deepEqual([text(cards[2], 'leads'), text(cards[2], 'holds'), text(cards[2], 'sync')], ['1', '2', '2 z 3']);
  assert.ok(cards[2].querySelector('[data-role="sync"]').classList.contains('is-warn'));
  assert.equal(text(cards[0], 'signal'), 'teraz (ten node)');
});

test('the partition that needs attention, with how far the copy trails and the move for an administrator', async () => {
  const { body, moves } = mount();
  await tick();
  const table = body.querySelector('[data-role="attention"]');
  assert.equal(table.rows.length, 1);
  assert.match(table.rows[0].where, /odczyty-urzadzen/);
  assert.match(norm(table.rows[0].behind), /mac-studio: 87 MB · 4 s/);
  assert.equal(table.rows[0]._blocker, null, 'rig26 holds an in-sync copy');
  assert.equal(typeof table.rowActions, 'function');
  const button = table.rowActions(table.rows[0], 0, () => table.rows[0]);
  button.click();
  assert.deepEqual(moves, [{ kind: 'transfer', topic: 'odczyty-urzadzen', partition: 0 }]);
  assert.equal(body.textContent.includes('Zmień repliki'), false, 'no button changes the copies');
});

test('a reader sees the same state without the move', async () => {
  const { body } = mount({ canAdmin: false });
  await tick();
  assert.equal(body.querySelector('[data-role="attention"]').rowActions, null);
});

test('a moved partition is marked and not offered again until refreshed', async () => {
  const { body } = mount({ justMoved: new Set(['odczyty-urzadzen:0']), notice: { title: 'Przeniesiono prowadzenie.', text: 'Partycję 0 topiku odczyty-urzadzen prowadzi teraz node rig26.' } });
  await tick();
  const table = body.querySelector('[data-role="attention"]');
  assert.match(table.rows[0].where, /zmieniono przed chwilą/);
  assert.match(table.rows[0]._blocker, /przed chwilą/);
  assert.equal(body.querySelector('[data-role="attn-foot"]').hidden, false);
  assert.equal(body.querySelector('tf-alert').getAttribute('title'), 'Przeniesiono prowadzenie.');
});

test('a topic row leads to that topic\'s partitions; a single node says every partition has one copy', () => {
  const { body, moves } = mount({ nodesList: [nodes[0]] });
  body.querySelector('[data-go="topic"][data-topic="wizyty"]').click();
  assert.deepEqual(moves, [{ kind: 'topic', topic: 'wizyty' }]);
  assert.match(body.querySelector('[data-role="nodes-sub"]').textContent, /jeden node/);
  const changes = [...body.querySelectorAll('[data-role="changes"] .job-row')].map((r) => norm(r.querySelector('[data-role="name"]').textContent));
  assert.deepEqual(changes, ['odczyty-urzadzen · partycja 1: prowadzenie przeszło z n-mac na rig26', 'wizyty · partycja 0: prowadzenie przeszło z rig26 na n-mac']);
});
