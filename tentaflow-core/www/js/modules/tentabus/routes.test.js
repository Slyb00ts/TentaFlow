// =============================================================================
// File: modules/tentabus/routes.test.js
// Description: The TentaBus address round-trips through the router params:
// the default tab stays out of the URL, a topic always lives under Topiki and
// a consumer under Odbiorcy, and an unknown tab falls back to Przegląd.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseRoute, routeParams, MAIN_TABS, DEFAULT_TAB } from './routes.js';

test('the six main tabs, overview first and default', () => {
  assert.deepEqual(MAIN_TABS, ['overview', 'topics', 'groups', 'dlq', 'schemas', 'replication']);
  assert.equal(DEFAULT_TAB, 'overview');
});

test('parse: defaults, known tabs, unknown tabs', () => {
  assert.deepEqual(parseRoute({ instance: 'tentabus-1a2b3c4d' }), { instance: 'tentabus-1a2b3c4d', tab: 'overview', topic: null, group: null, groupTopic: null, dlqTopic: null });
  assert.equal(parseRoute({ tab: 'schemas' }).tab, 'schemas');
  assert.equal(parseRoute({ tab: 'nonsense' }).tab, 'overview');
  assert.equal(parseRoute({}).instance, null);
});

test('parse: a topic forces Topiki, a consumer forces Odbiorcy', () => {
  assert.deepEqual(parseRoute({ tab: 'dlq', topic: 'wizyty' }), { instance: null, tab: 'topics', topic: 'wizyty', group: null, groupTopic: null, dlqTopic: null });
  assert.deepEqual(parseRoute({ group: 'app', gtopic: 'wizyty' }), { instance: null, tab: 'groups', topic: null, group: 'app', groupTopic: 'wizyty', dlqTopic: null });
});

test('the unprocessed-messages source travels only with its tab', () => {
  assert.equal(parseRoute({ tab: 'dlq', source: 'wizyty' }).dlqTopic, 'wizyty');
  assert.equal(parseRoute({ tab: 'groups', source: 'wizyty' }).dlqTopic, null);
  assert.deepEqual(routeParams({ instance: 'i', tab: 'topics', dlqTopic: 'wizyty' }), { instance: 'i', tab: 'topics' });
});

test('build: short addresses, and every view survives a round trip', () => {
  assert.deepEqual(routeParams({ instance: 'i', tab: 'overview' }), { instance: 'i' });
  assert.deepEqual(routeParams({ instance: 'i', tab: 'replication' }), { instance: 'i', tab: 'replication' });
  for (const view of [
    { instance: 'i', tab: 'dlq', topic: null, group: null, groupTopic: null, dlqTopic: null },
    { instance: 'i', tab: 'dlq', topic: null, group: null, groupTopic: null, dlqTopic: 'wyniki-badan' },
    { instance: 'i', tab: 'topics', topic: 'wyniki-badan', group: null, groupTopic: null, dlqTopic: null },
    { instance: 'i', tab: 'groups', topic: null, group: 'system-rozliczen', groupTopic: 'faktury', dlqTopic: null },
    { instance: 'i', tab: 'overview', topic: null, group: null, groupTopic: null, dlqTopic: null },
  ]) {
    assert.deepEqual(parseRoute(routeParams(view)), view);
  }
});
