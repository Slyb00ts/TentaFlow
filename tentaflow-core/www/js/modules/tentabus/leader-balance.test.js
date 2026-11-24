// ===== File: modules/tentabus/leader-balance.test.js — the numbers behind "Rozkład prowadzenia" =====

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { markSilent, isOlderNode, leadershipUnits, balanceModel, leastLoadedNode, leaderReturnEnabled, rebalanceAvailability } = await import('./leader-balance.js');

const part = (partition, leader, preferred) => ({ partition, leaderNodeId: leader, replicas: [preferred, leader === preferred ? 'b' : preferred], preferredLeaderNodeId: preferred });
const A = { nodeId: 'a', label: 'Serwer A', reachable: true, isLocal: true, forwarding: 'capable', forwardCapable: true };
const B = { nodeId: 'b', label: 'Serwer B', reachable: true, forwarding: 'capable', forwardCapable: true };

test('only a node that answered and lacks forwarding is an older build', () => {
  assert.equal(isOlderNode({ forwarding: 'lacking', forwardCapable: false, reachable: true }), true);
  assert.equal(isOlderNode({ forwarding: 'unknown', forwardCapable: false, reachable: false }), false, 'down');
  assert.equal(isOlderNode({ forwarding: 'unknown', forwardCapable: false, reachable: true }), false, 'not asked yet');
  assert.equal(isOlderNode({ forwarding: 'capable', forwardCapable: true }), false);
  // An older server sends no `forwarding`: a node that answers and is not capable is the best reading of it.
  assert.equal(isOlderNode({ forwardCapable: false, reachable: true }), true);
  assert.equal(isOlderNode({ forwardCapable: false, reachable: false }), false);
  assert.equal(isOlderNode({ reachable: true }), false, 'a server older than the field sends nothing at all');
});

test('a topic counts with its unprocessed-message partitions, as the server\'s plan weighs it', () => {
  const perTopic = [{ topic: 'wyniki', partitions: [part(0, 'a', 'b'), part(1, 'a', 'b')], dlqPartitions: [part(0, 'a', 'b')] }];
  const units = leadershipUnits({ topics: [{ name: 'wyniki' }], perTopic });
  assert.equal(units[0].partitions, 3);
  const model = balanceModel({ units, nodes: [A, B] });
  assert.deepEqual(model.rows.map((r) => r.partitions), [3, 0]);
  assert.equal(model.mean, 1.5);
});

test('the node a new topic goes to is the one the fewest partitions prefer, whatever leads them now', () => {
  // All three partitions are led by A after a failover, but two prefer B and one prefers A.
  const perTopic = [
    { topic: 'wyniki', partitions: [part(0, 'a', 'b'), part(1, 'a', 'b')], dlqPartitions: [] },
    { topic: 'faktury', partitions: [part(0, 'a', 'a')], dlqPartitions: [part(0, 'a', 'a'), part(1, 'a', 'a')] },
  ];
  // Preferred: B 2, A 3 → B. Counting current leaders (A 5, B 0) would pick B as well, so also check the reverse.
  assert.deepEqual(leastLoadedNode({ perTopic, nodes: [A, B] }), { nodeId: 'b', label: 'Serwer B', partitions: 2 });
  const reverse = [{ topic: 'wyniki', partitions: [part(0, 'b', 'a'), part(1, 'b', 'a'), part(2, 'b', 'a')], dlqPartitions: [] }];
  assert.equal(leastLoadedNode({ perTopic: reverse, nodes: [A, B] }).nodeId, 'b', 'B leads all three now, but A is the one they prefer: B is the less loaded');
  assert.equal(leastLoadedNode({ perTopic, nodes: [A, { ...B, reachable: false }] }).nodeId, 'a', 'a node that does not answer is not chosen');
  assert.equal(leastLoadedNode({ perTopic, nodes: [] }), null);
  const tie = leastLoadedNode({ perTopic: [], nodes: [B, A] });
  assert.equal(tie.nodeId, 'a', 'equal load: by node id');
  assert.deepEqual(tie.tied, ['Serwer A', 'Serwer B'], 'a tie names every tied node, since the server picks by the topic name');
  assert.equal(leastLoadedNode({ perTopic, nodes: [A, B] }).tied, undefined, 'no tie, no list');
});

test('the switch reads on, off, absent (on) and unreadable (off)', () => {
  const entry = (value) => [{ key: 'bus.leader_return:tb-1', value }];
  assert.equal(leaderReturnEnabled(undefined, 'tb-1'), null);
  assert.equal(leaderReturnEnabled([], 'tb-1'), true);
  for (const v of ['on', 'true', '1', ' ON ']) assert.equal(leaderReturnEnabled(entry(v), 'tb-1'), true, v);
  for (const v of ['off', 'false', '0', 'maybe', '']) assert.equal(leaderReturnEnabled(entry(v), 'tb-1'), false, v);
  assert.equal(leaderReturnEnabled([{ key: 'bus.leader_return:other', value: 'off' }], 'tb-1'), true, 'another instance\'s switch is not this one\'s');
});

test('the spread waits for an older node, and says nothing of a version when a node is merely down', () => {
  assert.equal(rebalanceAvailability([A, B]).reason, null);
  assert.match(rebalanceAvailability([A, { ...B, forwarding: 'lacking', forwardCapable: false }]).reason, /Serwer B ma starszą wersję/);
  const down = rebalanceAvailability([A, { ...B, reachable: false, forwarding: 'unknown', forwardCapable: false }]);
  assert.match(down.reason, /Żaden inny node nie odpowiada/);
  assert.deepEqual(down.older, []);
});

test('the node card, the table and the preview count the same partitions, with an internal topic present', async () => {
  const { nodeRows } = await import('./model.js');
  const { afterBalance } = await import('./rebalance-window.js');
  const perTopic = [
    { topic: 'wyniki', partitions: [part(0, 'a', 'b'), part(1, 'a', 'b')], dlqPartitions: [part(0, 'a', 'b')] },
    { topic: 'faktury', partitions: [part(0, 'b', 'b')], dlqPartitions: [] },
    { topic: '__bus.metrics', partitions: [part(0, 'a', 'a'), part(1, 'a', 'a'), part(2, 'a', 'a')], dlqPartitions: [] },
  ];
  const topics = [{ name: 'wyniki' }, { name: 'faktury' }, { name: '__bus.metrics' }];
  const units = leadershipUnits({ topics, perTopic });
  assert.deepEqual(units.map((u) => [u.name, u.partitions]), [['faktury', 1], ['wyniki', 3]], 'the internal topic is no unit');
  const table = balanceModel({ units, nodes: [A, B] }).rows.map((r) => r.partitions);
  const card = nodeRows([A, B], perTopic).map((r) => r.leads);
  assert.deepEqual(table, [3, 1]);
  assert.deepEqual(card, table, 'the card says what the table says');
  const moves = [{ topic: 'wyniki', partitions: [0, 1], fromNodeId: 'a', toNodeId: 'b' }];
  assert.deepEqual(afterBalance({ moves, units, nodes: [A, B] }).map((n) => n.partitions), [0, 4], 'the preview counts the topic with its __dlq and nothing else');
});

test('which node leads a topic when it is not this one', async () => {
  const { leaderElsewhere } = await import('./leader-balance.js');
  const topic = (topicName, ...leaders) => ({ topic: topicName, partitions: leaders.map((l, i) => part(i, l, l)) });
  const perTopic = [topic('mine', 'a', 'a'), topic('theirs', 'b', 'b'), topic('split', 'b', 'b', 'a')];
  assert.equal(leaderElsewhere('mine', perTopic, [A, B]), null, 'this node leads it');
  assert.deepEqual(leaderElsewhere('theirs', perTopic, [A, B]), { nodeId: 'b', node: 'Serwer B' });
  assert.deepEqual(leaderElsewhere('split', perTopic, [A, B])?.nodeId, 'b', 'the node that leads most of it');
  assert.equal(leaderElsewhere('unknown', perTopic, [A, B]), null, 'nothing known about the topic');
  assert.equal(leaderElsewhere('theirs', null, [A, B]), null, 'replica lists not read yet');
  assert.equal(leaderElsewhere('theirs', perTopic, [{ ...A, isLocal: false }, B]), null, 'no local node known');
  const unnamed = { nodeId: '365123c5d3d331c7ddff1a60767636af18b66eb8dba890b57cbeebba600f6a54', label: '' };
  assert.equal(leaderElsewhere('far', [topic('far', unnamed.nodeId)], [A, unnamed])?.node, '365123c5');
});

test('a node whose heartbeat is old, or that leads a partition without a usable leader, is taken as not responding', () => {
  const local = { nodeId: 'a', isLocal: true, reachable: true, lastHeartbeatMsAgo: 0 };
  const peer = (over = {}) => ({ nodeId: 'b', reachable: true, lastHeartbeatMsAgo: 400, ...over });
  const parts = (over = {}) => [{ topic: 't', partitions: [{ partition: 0, leaderNodeId: 'b', ...over }], dlqPartitions: [] }];
  const silent = (nodes, perTopic) => markSilent(nodes, perTopic).map((n) => n.reachable !== false);
  assert.deepEqual(silent([local, peer()], parts()), [true, true], 'a fresh heartbeat and a leader that works');
  assert.deepEqual(silent([local, peer({ lastHeartbeatMsAgo: 10_001 })], parts()), [true, false], 'heartbeat older than 10 s');
  assert.deepEqual(silent([local, peer({ lastHeartbeatMsAgo: 10_000 })], parts()), [true, true], '10 s exactly still answers');
  assert.deepEqual(silent([local, peer()], parts({ unavailableReason: 'no_isr' })), [true, false], 'its partition has no usable leader');
  assert.deepEqual(silent([local, peer()], [{ topic: 't', partitions: [], dlqPartitions: [{ partition: 0, leaderNodeId: 'b', unavailableReason: 'epoch_fenced' }] }]), [true, false], 'also by its dead-letter partition');
  assert.deepEqual(silent([{ ...local, lastHeartbeatMsAgo: 60_000 }, peer()], parts({ leaderNodeId: 'a', unavailableReason: 'no_isr' })), [true, true], 'this node is never marked');
  assert.equal(markSilent([local, peer({ lastHeartbeatMsAgo: 20_000 })], []).at(1).silent, true);
  assert.equal(markSilent([peer({ reachable: false })], [])[0].silent, undefined, 'already unreachable: unchanged');
});
