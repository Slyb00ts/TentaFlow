// ===== File: lib/correlation.test.js =====

import test from 'node:test';
import assert from 'node:assert/strict';
import {
  shortCorrelation, normalizeCorrelation, eventsCorrelationHref, busRecordHref, busSourceOf,
} from './correlation.js';

test('a long id is shortened to its ends, a short one stays whole', () => {
  assert.equal(shortCorrelation('0193c7e2-5b1a-7c3e-9a40-2f6d8e1b7a55'), '0193c7e2…7a55');
  assert.equal(shortCorrelation('req-42'), 'req-42');
  assert.equal(shortCorrelation(null), '');
});

test('the filter text is trimmed and an empty one means no filter', () => {
  assert.equal(normalizeCorrelation('  req-42 '), 'req-42');
  assert.equal(normalizeCorrelation('   '), null);
  assert.equal(normalizeCorrelation(undefined), null);
});

test('addresses carry the id encoded', () => {
  assert.equal(eventsCorrelationHref('a b&c'), '#/events?correlation=a%20b%26c');
  assert.equal(
    busRecordHref({ topic: 'zgloszenia.nowe', partition: 0, offset: 18402 }, 'req-1'),
    '#/tentabus?topic=zgloszenia.nowe&corr=req-1&partition=0&offset=18402',
  );
  assert.equal(
    busRecordHref({ instanceId: 'tentabus-1', topic: 'a', partition: 1, offset: 5 }),
    '#/tentabus?instance=tentabus-1&topic=a&partition=1&offset=5',
  );
  assert.equal(
    busRecordHref({ topic: 'a/b', partition: 1, offset: 5 }),
    '#/tentabus?topic=a%2Fb&partition=1&offset=5',
  );
});

test('coordinates come from the opening row only when they are well formed', () => {
  const row = (bus_source) => ({ payload: { kind: 'request_started', bus_source } });
  assert.deepEqual(busSourceOf(row({ topic: 't', partition: 2, offset: 9 })), { instanceId: null, topic: 't', partition: 2, offset: 9 });
  assert.equal(busSourceOf(row({ instance_id: 'tentabus-1', topic: 't', partition: 2, offset: 9 })).instanceId, 'tentabus-1');
  assert.equal(busSourceOf(row(undefined)), null);
  assert.equal(busSourceOf(row({ topic: '', partition: 0, offset: 0 })), null);
  assert.equal(busSourceOf(row({ topic: 't', partition: -1, offset: 0 })), null);
  assert.equal(busSourceOf(row({ topic: 't', partition: 0, offset: 1.5 })), null);
  assert.equal(busSourceOf(null), null);
  assert.equal(busSourceOf({ payload: null }), null);
});
