// ===== File: modules/tentabus/leader-balance.test.js — the numbers behind "Rozkład prowadzenia" =====

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { isOlderNode, leadershipUnits, balanceModel, leastLoadedNode, leaderReturnEnabled, rebalanceAvailability } = await import('./leader-balance.js');

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
