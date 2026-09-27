// =============================================================================
// File: modules/tentabus/routes.test.js
// Description: The TentaBus address round-trips through the router params:
// the default tab stays out of the URL, a topic always lives under Topiki and
// a consumer under Odbiorcy, and an unknown tab falls back to Przegląd.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseRoute, routeParams, MAIN_TABS, DEFAULT_TAB, TOPIC_SECTIONS, DEFAULT_SECTION, CONSUMER_SECTIONS } from './routes.js';

test('the six main tabs, overview first and default', () => {
  assert.deepEqual(MAIN_TABS, ['overview', 'topics', 'groups', 'dlq', 'schemas', 'replication']);
  assert.equal(DEFAULT_TAB, 'overview');
});

test('parse: defaults, known tabs, unknown tabs', () => {
  assert.deepEqual(parseRoute({ instance: 'tentabus-1a2b3c4d' }), { instance: 'tentabus-1a2b3c4d', tab: 'overview', topic: null, section: null, group: null, groupTopic: null, dlqTopic: null });
  assert.equal(parseRoute({ tab: 'schemas' }).tab, 'schemas');
  assert.equal(parseRoute({ tab: 'nonsense' }).tab, 'overview');
  assert.equal(parseRoute({}).instance, null);
});

test('parse: a topic forces Topiki, a consumer forces Odbiorcy', () => {
  assert.deepEqual(parseRoute({ tab: 'dlq', topic: 'wizyty' }), { instance: null, tab: 'topics', topic: 'wizyty', section: 'state', group: null, groupTopic: null, dlqTopic: null });
  assert.deepEqual(parseRoute({ group: 'app', gtopic: 'wizyty' }), { instance: null, tab: 'groups', topic: null, section: 'state', group: 'app', groupTopic: 'wizyty', dlqTopic: null });
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
    { instance: 'i', tab: 'dlq', topic: null, section: null, group: null, groupTopic: null, dlqTopic: null },
    { instance: 'i', tab: 'dlq', topic: null, section: null, group: null, groupTopic: null, dlqTopic: 'wyniki-badan' },
    { instance: 'i', tab: 'topics', topic: 'wyniki-badan', section: 'state', group: null, groupTopic: null, dlqTopic: null },
    { instance: 'i', tab: 'topics', topic: 'wyniki-badan', section: 'settings', group: null, groupTopic: null, dlqTopic: null },
    { instance: 'i', tab: 'topics', topic: 'wyniki-badan', section: 'partitions', group: null, groupTopic: null, dlqTopic: null },
    { instance: 'i', tab: 'groups', topic: null, section: 'state', group: 'system-rozliczen', groupTopic: 'faktury', dlqTopic: null },
    { instance: 'i', tab: 'groups', topic: null, section: 'position', group: 'system-rozliczen', groupTopic: 'faktury', dlqTopic: null },
    { instance: 'i', tab: 'groups', topic: null, section: 'settings', group: 'system-rozliczen', groupTopic: 'faktury', dlqTopic: null },
    { instance: 'i', tab: 'overview', topic: null, section: null, group: null, groupTopic: null, dlqTopic: null },
  ]) {
    assert.deepEqual(parseRoute(routeParams(view)), view);
  }
});

test('a topic opens on its first section; the section stays out of the address until it is another', () => {
  assert.deepEqual(TOPIC_SECTIONS, ['state', 'settings', 'partitions']);
  assert.equal(DEFAULT_SECTION, 'state');
  assert.equal(parseRoute({ topic: 'faktury', section: 'nonsense' }).section, 'state');
  assert.equal(parseRoute({ tab: 'overview', section: 'settings' }).section, null, 'a section without a topic means nothing');
  assert.deepEqual(routeParams({ instance: 'i', topic: 'faktury', section: 'state' }), { instance: 'i', tab: 'topics', topic: 'faktury' });
  assert.deepEqual(routeParams({ instance: 'i', topic: 'faktury', section: 'partitions' }), { instance: 'i', tab: 'topics', topic: 'faktury', section: 'partitions' });
});

test('a consumer opens on Stan; its other sections travel in the address', () => {
  assert.deepEqual(CONSUMER_SECTIONS, ['state', 'position', 'settings']);
  assert.equal(parseRoute({ group: 'app', gtopic: 'wizyty', section: 'partitions' }).section, 'state', 'a topic section is not a consumer section');
  assert.deepEqual(routeParams({ instance: 'i', group: 'app', groupTopic: 'wizyty', section: 'state' }), { instance: 'i', tab: 'groups', group: 'app', gtopic: 'wizyty' });
  assert.deepEqual(routeParams({ instance: 'i', group: 'app', groupTopic: 'wizyty', section: 'position' }), { instance: 'i', tab: 'groups', group: 'app', gtopic: 'wizyty', section: 'position' });
});
