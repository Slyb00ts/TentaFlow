// =============================================================================
// File: modules/tentabus/format.test.js
// Description: TentaBus formatters against the real Polish strings: exact
// grouped counts, byte sizes with one decimal from a gigabyte up, elapsed
// and "rośnie od" durations with correct plural-free units, plain-word
// content types (and none for an unknown one), and the T12 error classes
// taken from the protocol code, never from translated text.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { fmtCount, fmtBytes, fmtElapsed, fmtSince, fmtLagSeconds, contentTypeLabel, loadErrorKind } = await import('./format.js');

// Intl groups with a no-break space; the screen prints exactly that.
const sp = (s) => s.replace(/\u00a0|\u202f/g, ' ');

test('counts are exact and grouped: "18 420", "2 716", "0"', () => {
  assert.equal(sp(fmtCount(18420)), '18 420');
  assert.equal(sp(fmtCount(2716)), '2 716');
  assert.equal(fmtCount(0), '0');
  assert.equal(fmtCount(null), '—');
});

test('byte sizes: whole units below a gigabyte, one decimal from there', () => {
  assert.equal(fmtBytes(0), '0 B');
  assert.equal(fmtBytes(331 * 1024 * 1024), '331 MB');
  assert.equal(fmtBytes(125.5 * 1024 ** 3), '125,5 GB');
  assert.equal(fmtBytes(0.8 * 1024 ** 3), '819 MB');
  assert.equal(fmtBytes(-1), '—');
  assert.equal(fmtBytes(undefined), '—');
});

test('elapsed time rounds down: seconds, minutes, hours', () => {
  assert.equal(fmtElapsed(3200), '3 s');
  assert.equal(fmtElapsed(59_999), '59 s');
  assert.equal(fmtElapsed(2 * 60_000 + 30_000), '2 min');
  assert.equal(fmtElapsed(3 * 3_600_000), '3 godz.');
});

test('"rośnie od": at least a minute, hours and minutes past an hour', () => {
  const now = 10_000_000;
  assert.equal(fmtSince(now - 25 * 60_000, now), '25 min');
  assert.equal(fmtSince(now - 10_000, now), '1 min');
  assert.equal(fmtSince(now - 60 * 60_000, now), '1 godz.');
  assert.equal(fmtSince(now - 125 * 60_000, now), '2 godz. 5 min');
});

test('replica lag seconds round UP — "do 4 s" is a bound', () => {
  assert.equal(fmtLagSeconds(3100), '4 s');
  assert.equal(fmtLagSeconds(0), '1 s');
});

test('content types in plain words; an unknown or missing one prints nothing', () => {
  assert.equal(contentTypeLabel('application/hl7-v2'), 'HL7 v2');
  assert.equal(contentTypeLabel('x-application/hl7-v2+er7'), 'HL7 v2');
  assert.equal(contentTypeLabel('application/json'), 'JSON');
  assert.equal(contentTypeLabel('text/xml'), 'XML');
  assert.equal(contentTypeLabel('application/octet-stream'), 'Binarna');
  assert.equal(contentTypeLabel(''), '');
  assert.equal(contentTypeLabel('application/x-unknown'), '');
});

test('load errors are classified on the protocol code first', () => {
  assert.equal(loadErrorKind(Object.assign(new Error('x'), { code: 'PolicyDenied' })), 'denied');
  assert.equal(loadErrorKind(new Error('protocol error PolicyDenied: bus.read denied')), 'denied');
  assert.equal(loadErrorKind(new Error('protocol error Forbidden: bus.permission_denied: read on \'x\'')), 'denied');
  assert.equal(loadErrorKind(Object.assign(new Error('the bus is not running on this node'), { code: 'AppUnavailable' })), 'unavailable');
  assert.equal(loadErrorKind(new Error('request busStatsSnapshotRequest timed out after 15000ms')), 'timeout');
  assert.equal(loadErrorKind(new Error('socket closed')), 'lost');
});
