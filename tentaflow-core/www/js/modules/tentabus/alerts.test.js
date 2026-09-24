// =============================================================================
// File: modules/tentabus/alerts.test.js
// Description: The Przegląd alert rules (PLAN-UI-20260923 P4, decided 23.09)
// at and around every threshold: a consumer falls behind only after 10
// minutes of growth AND more than 1 000 waiting; a topic gains unprocessed
// messages from the first one in the last hour; a paused consumer is an
// alert only with a backlog; lagging replicas fold into one alert per node.
// An unmeasured lag raises nothing.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  computeAlerts, isLagging, lagWording, lagSeriesKey, RISING_RECENT_MS, isPausedWithBacklog, delayedGroups, laggingReplicas,
  LAGGING_MIN_RISE_MS, LAGGING_MIN_WAITING,
} from './alerts.js';

const NOW = 1_800_000_000_000;
const MIN = 60_000;
const group = (over = {}) => ({ group: 'aplikacja-lekarza', topic: 'wyniki-badan', lagTotal: 18420, paused: false, lagRisingSinceMs: NOW - 25 * MIN, ...over });

test('thresholds are the decided ones', () => {
  assert.equal(LAGGING_MIN_RISE_MS, 10 * MIN);
  assert.equal(LAGGING_MIN_WAITING, 1000);
});

test('falling behind: 10 minutes of growth and more than 1 000 waiting, both required', () => {
  assert.equal(isLagging(group(), NOW), true);
  assert.equal(isLagging(group({ lagRisingSinceMs: NOW - 10 * MIN }), NOW), true, 'exactly 10 min counts');
  assert.equal(isLagging(group({ lagRisingSinceMs: NOW - 10 * MIN + 1 }), NOW), false);
  assert.equal(isLagging(group({ lagTotal: 1000 }), NOW), false, '1 000 is not MORE than 1 000');
  assert.equal(isLagging(group({ lagTotal: 1001 }), NOW), true);
  assert.equal(isLagging(group({ lagRisingSinceMs: null }), NOW), false, 'not growing');
  assert.equal(isLagging(group({ lagTotal: null }), NOW), false, 'an unmeasured lag is not a lag');
  assert.equal(isLagging(group({ paused: true }), NOW), false, 'a paused consumer is its own alert');
});

test('paused is an alert only while something waits', () => {
  assert.equal(isPausedWithBacklog(group({ paused: true, lagTotal: 10800 })), true);
  assert.equal(isPausedWithBacklog(group({ paused: true, lagTotal: 0 })), false);
  assert.equal(isPausedWithBacklog(group({ paused: true, lagTotal: null })), false);
  assert.equal(isPausedWithBacklog(group({ paused: false, lagTotal: 5 })), false);
});

test('delayed consumers: anything waiting, measured only', () => {
  const list = [group({ group: 'a', lagTotal: 3 }), group({ group: 'b', lagTotal: 0 }), group({ group: 'c', lagTotal: null })];
  assert.deepEqual(delayedGroups(list).map((g) => g.group), ['a']);
});

test('lagging replicas are attributed to the topic they were asked for, with node labels', () => {
  const lags = laggingReplicas([
    { topic: 'odczyty-urzadzen', partitions: [
      { partition: 0, lagging: [{ nodeId: 'n3', lagBytes: 87, lagMs: 3000 }] },
      { partition: 1, lagging: [] },
      { partition: 2, lagging: [{ nodeId: 'n3', lagBytes: 81, lagMs: 4000 }] },
    ] },
  ], [{ nodeId: 'n3', label: 'mac-studio' }]);
  assert.deepEqual(lags.map((l) => [l.nodeLabel, l.topic, l.partition]), [['mac-studio', 'odczyty-urzadzen', 0], ['mac-studio', 'odczyty-urzadzen', 2]]);
});

test('computeAlerts: order, content and one alert per lagging node', () => {
  const alerts = computeAlerts({
    nowMs: NOW,
    groups: [
      group(),
      group({ group: 'raporty', lagTotal: 0, lagRisingSinceMs: null }),
      group({ group: 'system-rozliczen', topic: 'faktury', paused: true, lagTotal: 10800, lagRisingSinceMs: NOW - 15 * MIN }),
    ],
    topics: [
      { topic: 'wyniki-badan', dlqLastHour: 3, dlqDepth: 14 },
      { topic: 'wizyty', dlqLastHour: 0, dlqDepth: 6 },
      { topic: '__dlq.wyniki-badan', dlqLastHour: 9, dlqDepth: 0 },
    ],
    replicaLags: [
      { nodeId: 'n3', nodeLabel: 'mac-studio', topic: 'odczyty-urzadzen', partition: 0, lagBytes: 100, lagMs: 3000 },
      { nodeId: 'n3', nodeLabel: 'mac-studio', topic: 'odczyty-urzadzen', partition: 2, lagBytes: 50, lagMs: 3900 },
    ],
  });
  assert.deepEqual(alerts.map((a) => a.kind), ['lagging', 'dlq', 'paused', 'replica']);
  const [lagging, dlq, paused, replica] = alerts;
  assert.equal(lagging.group, 'aplikacja-lekarza');
  assert.equal(lagging.waiting, 18420);
  assert.equal(lagging.risingSinceMs, NOW - 25 * MIN);
  assert.deepEqual([dlq.topic, dlq.lastHour, dlq.total], ['wyniki-badan', 3, 14]);
  assert.deepEqual([paused.group, paused.topic, paused.waiting], ['system-rozliczen', 'faktury', 10800]);
  assert.deepEqual([replica.nodeLabel, replica.partitions, replica.topics, replica.lagBytes, replica.maxLagMs],
    ['mac-studio', 2, ['odczyty-urzadzen'], 150, 3900]);
  assert.equal(replica.tone, 'info');
  assert.ok(alerts.slice(0, 3).every((a) => a.tone === 'warning'));
  assert.equal(new Set(alerts.map((a) => a.key)).size, alerts.length, 'keys are unique');
});

test('a quiet instance has no alerts', () => {
  assert.deepEqual(computeAlerts({ nowMs: NOW, groups: [group({ lagTotal: 0, lagRisingSinceMs: null })], topics: [{ topic: 't', dlqLastHour: 0, dlqDepth: 2 }], replicaLags: [] }), []);
});

test('"rośnie" only while the newest recent sample is above the one before it', () => {
  const at = (minAgo, lagTotal) => ({ atMs: NOW - minAgo * MIN, lagTotal });
  assert.equal(RISING_RECENT_MS, 3 * MIN);
  assert.equal(lagWording([at(2, 100), at(1, 150)], NOW), 'rising');
  assert.equal(lagWording([at(2, 150), at(1, 150)], NOW), 'waiting', 'flat');
  assert.equal(lagWording([at(2, 150), at(1, 120)], NOW), 'waiting', 'falling');
  assert.equal(lagWording([at(6, 100), at(5, 150)], NOW), 'waiting', 'grew, but not recently');
  assert.equal(lagWording([at(1, 150)], NOW), 'waiting', 'one sample says nothing about growth');
  assert.equal(lagWording(undefined, NOW), 'waiting');
});

test('the lagging alert carries the wording its history supports', () => {
  const groups = [group()];
  const rising = new Map([[lagSeriesKey('aplikacja-lekarza', 'wyniki-badan'), [{ atMs: NOW - 2 * MIN, lagTotal: 18000 }, { atMs: NOW - MIN, lagTotal: 18420 }]]]);
  assert.equal(computeAlerts({ groups, nowMs: NOW, lagSeries: rising })[0].wording, 'rising');
  assert.equal(computeAlerts({ groups, nowMs: NOW })[0].wording, 'waiting');
});
