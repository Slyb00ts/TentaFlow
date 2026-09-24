// =============================================================================
// File: modules/tentabus/model.test.js
// Description: The numbers the header, the tab strip and Przegląd share:
// `__dlq.*` stores never count as topics, a figure whose source has not
// answered is null (left out, not zero), the busiest-topic bar is relative to
// the busiest topic, and node rows compare in-sync copies with every copy.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { userTopics, shellCounts, overviewKpis, busiestTopics, nodeRows } from './model.js';

const topicList = [
  { name: 'wyniki-badan', partitions: 6, contentType: 'application/hl7-v2', isDlq: false },
  { name: 'wizyty', partitions: 6, contentType: 'application/json', isDlq: false },
  { name: 'faktury', partitions: 3, contentType: '', isDlq: false },
  { name: '__dlq.wyniki-badan', partitions: 6, isDlq: true },
  { name: '__bus.metrics', partitions: 1, isDlq: false },
];
const stats = {
  topicCount: 4,
  dlqTopicCount: 1,
  totalMsgsInPerSec: 600,
  totalBytesOnDisk: 1000,
  totalDlqDepth: 20,
  topics: [
    { topic: 'wyniki-badan', msgsInPerSec: 412, totalLag: 18420, totalBytesOnDisk: 500 },
    { topic: 'wizyty', msgsInPerSec: 188, totalLag: 0, totalBytesOnDisk: 300 },
    { topic: 'faktury', msgsInPerSec: 0, totalLag: 10800, totalBytesOnDisk: 150 },
    { topic: '__dlq.wyniki-badan', msgsInPerSec: 0, totalLag: 0, totalBytesOnDisk: 50 },
    { topic: '__bus.metrics', msgsInPerSec: 3, totalLag: 0, totalBytesOnDisk: 10 },
  ],
  groups: [{ group: 'a' }, { group: 'b' }],
};

test('user topics leave the broker\'s own `__*` topics out', () => {
  assert.deepEqual(userTopics(topicList).map((t) => t.name), ['wyniki-badan', 'wizyty', 'faktury']);
});

test('shell counts: from the snapshot, null until a source answers', () => {
  assert.deepEqual(shellCounts({ stats, topicList, subjects: [{}, {}], nodes: [{}] }), { topics: 3, groups: 2, dlq: 20, schemas: 2, nodes: 1 });
  assert.deepEqual(shellCounts({ stats: null, topicList: null, subjects: null, nodes: null }), { topics: null, groups: null, dlq: null, schemas: null, nodes: null });
  assert.equal(shellCounts({ stats: null, topicList, subjects: null, nodes: null }).topics, 3, 'the topic list answers before the first snapshot');
});

test('KPI: partitions, rate and bytes of the reader\'s topics only', () => {
  const k = overviewKpis({ stats, topicList });
  assert.equal(k.bytesOnDisk, 950, 'the __ topics\' bytes are left out');
  assert.equal(k.topics, 3);
  assert.equal(k.partitions, 15);
  assert.equal(k.writingTopics, 2);
  assert.equal(k.rate, 600);
  assert.equal(k.dlq, 20);
  assert.equal(k.groups, 2);
});

test('busiest topics: by rate, then size; bar relative to the busiest; content type carried', () => {
  const rows = busiestTopics({ stats, topicList });
  assert.deepEqual(rows.map((r) => [r.name, r.share]), [['wyniki-badan', 100], ['wizyty', 46], ['faktury', 0]]);
  assert.equal(rows[0].waiting, 18420);
  assert.equal(rows[0].contentType, 'application/hl7-v2');
  assert.equal(rows[0].partitions, 6);
  assert.equal(busiestTopics({ stats, topicList, limit: 1 }).length, 1);
});

test('an idle instance still lists its topics in a stable order, with empty bars', () => {
  const idle = { ...stats, topics: stats.topics.map((t) => ({ ...t, msgsInPerSec: 0 })) };
  assert.deepEqual(busiestTopics({ stats: idle, topicList }).map((r) => [r.name, r.share]), [['wyniki-badan', 0], ['wizyty', 0], ['faktury', 0]]);
});

test('node rows count the partitions of the reader\'s topics, not the broker\'s', () => {
  const nodes = [{ nodeId: 'a', label: 'rig26' }, { nodeId: 'b', label: 'mac-studio', reachable: false }];
  const perTopic = [
    { topic: 'wyniki-badan', partitions: [
      { partition: 0, leaderNodeId: 'a', replicas: ['a', 'b'], isr: ['a', 'b'] },
      { partition: 1, leaderNodeId: 'b', replicas: ['a', 'b'], isr: ['b'] },
    ] },
    { topic: '__bus.metrics', partitions: [{ partition: 0, leaderNodeId: 'a', replicas: ['a'], isr: ['a'] }] },
  ];
  const [a, b] = nodeRows(nodes, perTopic);
  assert.deepEqual([a.label, a.leads, a.holds, a.total, a.inSync, a.reachable], ['rig26', 1, 1, 2, 1, true]);
  assert.deepEqual([b.label, b.leads, b.holds, b.total, b.inSync, b.reachable], ['mac-studio', 1, 1, 2, 2, false]);
  assert.equal(nodeRows(nodes, null), null, 'unknown until the per-topic answers arrive');
});
