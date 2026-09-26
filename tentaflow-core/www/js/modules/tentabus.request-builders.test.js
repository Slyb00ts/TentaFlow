// =============================================================================
// File: modules/tentabus.request-builders.test.js
// Description: Unit tests for tentabus.js's pure helpers — request builders
//       (replica list, reassign, leader transfer, the per-partition
//       `buildFromOffsetsForNextPage` cursor), formatters
//       (`datetimeLocalToTsMs`), lag math (`sumGroupLag`, `computeLagRatio`,
//       `lagSeverityClass`), the stats join (`findTopicStats`) and the
//       server-error-code mapper (`busErrorCode`/`mapBusErrorMessage`).
//       tentabus.js imports DOM-only custom-element modules at load time
//       (`customElements.define(...)` has no global under plain Node), so —
//       exactly like `services.row-lifecycle.test.js` and `ml-studio.derive-
//       targets.test.js` — the functions under test are cut out of the real
//       source file by brace matching and evaluated in isolation. The code
//       tested here is the code that ships, not a reimplementation of it.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'tentabus.js'), 'utf8');
const pl = JSON.parse(readFileSync(join(here, '../../i18n/pl.json'), 'utf8'));
const en = JSON.parse(readFileSync(join(here, '../../i18n/en.json'), 'utf8'));
const de = JSON.parse(readFileSync(join(here, '../../i18n/de.json'), 'utf8'));
const es = JSON.parse(readFileSync(join(here, '../../i18n/es.json'), 'utf8'));
const fr = JSON.parse(readFileSync(join(here, '../../i18n/fr.json'), 'utf8'));

function cut(src, name) {
  const start = src.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  let depth = 0;
  let i = src.indexOf('{', start);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

// A handful of pure helpers close over a module-level `const` (a regex or a
// lookup table) instead of a literal — those constants have to be cut out
// and prepended too, or the extracted function throws a ReferenceError the
// moment it runs.
function cutConst(src, name) {
  const marker = `const ${name} =`;
  const start = src.indexOf(marker);
  if (start < 0) throw new Error(`no const definition: ${name}`);
  let j = start + marker.length;
  while (/\s/.test(src[j])) j += 1;
  if (src[j] === '{') {
    let depth = 0;
    let i = j;
    for (; i < src.length; i += 1) {
      if (src[i] === '{') depth += 1;
      else if (src[i] === '}') {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    const end = src[i + 1] === ';' ? i + 2 : i + 1;
    return src.slice(start, end);
  }
  const semi = src.indexOf(';', j);
  return src.slice(start, semi + 1);
}

const CONSTS = ['DLQ_RETRY_ALL_MAX', 'NO_CAPABILITIES'];

const NAMES = [
  'requireInstanceId',
  'clampInt', 'clampDlqRetryAllMax', 'deriveDurabilityClass', 'sumGroupLag',
  'computeLagRatio', 'lagSeverityClass', 'dlqSourceTopicOptions',
  'busErrorCode', 'mapBusErrorMessage', 'findTopicStats', 'buildFromOffsetsForNextPage',
  'datetimeLocalToTsMs', 'unwrapCapabilities', 'isValidExplicitOffset',
  // task 3 (Groups KPI = list, N-2/N-7):
  'isInternalGroupId', 'filterVisibleGroups',
  // task 4 (P3-14, DLQ header date formatting):
  'formatHeaderValue', 'msToDate',
  // R3-1 (KRYTYK-M1-R3.md, P1: DLQ tab empty on entry) — the single, pure
  // state-transition helper `ensureDlqTabReady` acts on:
  'resolveDlqEntrySource',
  // Fala post-R5 (KRYTYK-M1-R5.md b.7) — the "(polityka jawna)"
  // secondary-label predicate the M03 chip helper calls.
  'shouldShowDurabilityExplicitLabel',
  // Incremental-repaint fala (owner requirement: charts/tiles/tables only
  // swap values on a poll, never a full re-render) — `patchText`
  // (no-op-on-equal DOM writes), `pushWindowSample` (the live chart's ring
  // buffer), `diffRowsByKey` (M04 table poll-skip gate) and
  // `prefersReducedMotion` (the live chart's entrance-animation gate).
  'patchText', 'pushWindowSample', 'diffRowsByKey', 'prefersReducedMotion',
  // M2 (PLAN-M2.md §1f) — M06 replication/failover and M03's partitions
  // tab. Request builders, the SPEC D4 env check, the role-matrix builder,
  // lag/ISR-degraded math and the `not_leader` hint extractor.
  'buildReplicaListRequest', 'buildReassignRequest', 'buildLeaderTransferRequest',
  'isSameEnvironment',
  'computeReplicationLag', 'isIsrDegraded', 'roleForNode', 'buildRoleMatrix',
  'leaderTransferCandidates', 'nodeDegradedReason', 'unavailableReasonI18nKey',
  'extractNotLeaderHint',
];

const consts = CONSTS.map((n) => cutConst(source, n)).join('\n');
const body = NAMES.map((n) => cut(source, n)).join('\n');
// eslint-disable-next-line no-new-func
const helpers = new Function(`${consts}\n${body}\nreturn { ${NAMES.join(', ')}, NO_CAPABILITIES };`)();

// A stand-in instance id, shaped like the real `BusInstanceId` format
// (`tentabus-<8hex>`) used throughout the request-builder tests below.
const IID = 'tentabus-1a2b3c4d';

// ---------------------------------------------------------------------------
// requireInstanceId (W9, SUM/tentabus/PLAN-APP-PLATFORM.md §3.1/§6.1) — the
// one guard every request builder below routes its instance id through.
// ---------------------------------------------------------------------------

test('requireInstanceId returns a non-empty string instance id unchanged', () => {
  assert.equal(helpers.requireInstanceId(IID), IID);
});

test('requireInstanceId throws for a missing/empty/non-string instance id', () => {
  assert.throws(() => helpers.requireInstanceId(undefined));
  assert.throws(() => helpers.requireInstanceId(null));
  assert.throws(() => helpers.requireInstanceId(''));
  assert.throws(() => helpers.requireInstanceId(0));
});

// ---------------------------------------------------------------------------
// clampInt / clampReplicationFactor / clampDlqRetryAllMax
// ---------------------------------------------------------------------------

test('clampInt clamps within [min,max] and falls back on non-finite input', () => {
  assert.equal(helpers.clampInt(5, 1, 10, 0), 5);
  assert.equal(helpers.clampInt(-3, 1, 10, 0), 1);
  assert.equal(helpers.clampInt(99, 1, 10, 0), 10);
  assert.equal(helpers.clampInt('abc', 1, 10, 7), 7);
  assert.equal(helpers.clampInt(3.9, 1, 10, 0), 3, 'truncates toward zero, does not round');
});

test('clampDlqRetryAllMax stays within the server-enforced [1,500] bound', () => {
  assert.equal(helpers.clampDlqRetryAllMax(0), 1);
  assert.equal(helpers.clampDlqRetryAllMax(500), 500);
  assert.equal(helpers.clampDlqRetryAllMax(10000), 500);
  assert.equal(helpers.clampDlqRetryAllMax(undefined), 100);
});

// ---------------------------------------------------------------------------
// deriveDurabilityClass — owner decision B's defensive fallback for a topic
// response that predates the wire's `durabilityClass` field: derive it from
// the always-present, already-resolved `durability` policy string.
// ---------------------------------------------------------------------------

test('deriveDurabilityClass trusts an already-resolved durabilityClass from the wire', () => {
  assert.equal(helpers.deriveDurabilityClass({ durabilityClass: 'standard', durability: 'fsync_batch_full' }), 'standard');
  assert.equal(helpers.deriveDurabilityClass({ durabilityClass: 'critical', durability: 'os' }), 'critical');
});

test('deriveDurabilityClass classifies fsync_batch/fsync_batch_full as critical when durabilityClass is missing', () => {
  assert.equal(helpers.deriveDurabilityClass({ durability: 'fsync_batch' }), 'critical');
  assert.equal(helpers.deriveDurabilityClass({ durability: 'fsync_batch_full' }), 'critical');
});

test('deriveDurabilityClass classifies os / fsync_interval:<ms> as standard when durabilityClass is missing', () => {
  assert.equal(helpers.deriveDurabilityClass({ durability: 'os' }), 'standard');
  assert.equal(helpers.deriveDurabilityClass({ durability: 'fsync_interval:50' }), 'standard');
});

test('deriveDurabilityClass degrades to standard for null/undefined/garbage input', () => {
  assert.equal(helpers.deriveDurabilityClass(null), 'standard');
  assert.equal(helpers.deriveDurabilityClass(undefined), 'standard');
  assert.equal(helpers.deriveDurabilityClass({}), 'standard');
  assert.equal(helpers.deriveDurabilityClass({ durabilityClass: 'bogus', durability: 42 }), 'standard');
});

// ---------------------------------------------------------------------------
// shouldShowDurabilityExplicitLabel — the "(polityka jawna)" secondary-label
// predicate (KRYTYK-M1-R5.md b.7: the report calls this label impossible
// without a stored class-vs-override distinction; `durabilityExplicit` on
// the wire is exactly that distinction).
// ---------------------------------------------------------------------------

test('shouldShowDurabilityExplicitLabel is true only when durabilityExplicit is strictly true', () => {
  assert.equal(helpers.shouldShowDurabilityExplicitLabel({ durabilityExplicit: true }), true);
  assert.equal(helpers.shouldShowDurabilityExplicitLabel({ durabilityExplicit: false }), false);
  assert.equal(helpers.shouldShowDurabilityExplicitLabel({}), false);
  assert.equal(helpers.shouldShowDurabilityExplicitLabel(null), false);
  assert.equal(helpers.shouldShowDurabilityExplicitLabel({ durabilityExplicit: 'true' }), false, 'not coerced from a truthy non-boolean');
});

// ---------------------------------------------------------------------------
// clampFsyncIntervalMs / formatFsyncIntervalDurability — the wizard's new
// `fsync_interval` advanced-durability option (KRYTYK-M1-R5.md b.3, P2: the
// select had no way to express Prod/Test's own default policy family).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// buildMessagesBrowseRequest — first page uses the legacy scalar
// `fromOffset`; subsequent pages use per-partition `fromOffsets` (tor U
// task 1) once a previous response's `partitions[]` is known. `partition`
// (task 2, M08's partition filter) is additive — see the function's own doc
// comment on why sending it is safe before the backend honors it.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// M08 partition filter (task 2, KRYTYK-M1-R2.md's N-3) — client-side
// filtering/paging helpers layered on top of the existing `partitions[]` +
// `fromOffsets` plumbing (tor U task 1).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Groups KPI = list (task 3, KRYTYK-M1-R2.md's N-2/N-7) — the client-side
// `tf-*` filter applied as defense in depth on top of the backend hiding
// them (POSTEP.md's "Decyzje koordynatora po krytyku R2" #3), and the
// exact list both the M04 table and the KPI strip now share.
// ---------------------------------------------------------------------------

test('isInternalGroupId recognizes the tf-* prefix used by internal probes', () => {
  assert.equal(helpers.isInternalGroupId('tf-system-probe'), true);
  assert.equal(helpers.isInternalGroupId('billing'), false);
  assert.equal(helpers.isInternalGroupId('notifier'), false);
  assert.equal(helpers.isInternalGroupId(''), false);
  assert.equal(helpers.isInternalGroupId(null), false);
});

test('filterVisibleGroups drops every tf-* group and keeps business groups, in order', () => {
  const groups = [
    { group: 'billing', topic: 'lab.results' },
    { group: 'tf-system-probe', topic: 'lab.results' },
    { group: 'notifier', topic: 'orders.created' },
    { group: 'tf-system-probe', topic: 'orders.created' },
  ];
  assert.deepEqual(helpers.filterVisibleGroups(groups), [
    { group: 'billing', topic: 'lab.results' },
    { group: 'notifier', topic: 'orders.created' },
  ]);
});

test('filterVisibleGroups tolerates a non-array input', () => {
  assert.deepEqual(helpers.filterVisibleGroups(null), []);
  assert.deepEqual(helpers.filterVisibleGroups(undefined), []);
});

// ---------------------------------------------------------------------------
// formatHeaderValue (P3-14) — DLQ record detail's `dlq.*_at_ms` headers
// render as a formatted date instead of a raw epoch, exactly like every
// other millisecond timestamp `msToDate` already formats elsewhere.
// ---------------------------------------------------------------------------

test('formatHeaderValue formats a numeric "_at_ms"-suffixed header as a date', () => {
  const formatted = helpers.formatHeaderValue('dlq.first_failed_at_ms', '1787862468957');
  assert.equal(formatted, helpers.msToDate(1787862468957));
  assert.notEqual(formatted, '1787862468957');
});

test('formatHeaderValue leaves non-"_at_ms" and non-numeric values untouched', () => {
  assert.equal(helpers.formatHeaderValue('dlq.reason', 'schema_violation'), 'schema_violation');
  assert.equal(helpers.formatHeaderValue('dlq.first_failed_at_ms', 'not-a-number'), 'not-a-number');
  assert.equal(helpers.formatHeaderValue(null, '123'), '123');
});

// ---------------------------------------------------------------------------
// findTopicStats — M01/M03's join between a topic row and
// `BusStatsSnapshotWire.topics` (tor U task 3).
// ---------------------------------------------------------------------------

test('findTopicStats finds a topic\'s stats row by name', () => {
  const topics = [{ topic: 'a', msgsInPerSec: 1 }, { topic: 'b', msgsInPerSec: 2 }];
  assert.deepEqual(helpers.findTopicStats(topics, 'b'), { topic: 'b', msgsInPerSec: 2 });
});

test('findTopicStats returns null when the topic is not (yet) in the snapshot', () => {
  assert.equal(helpers.findTopicStats([{ topic: 'a' }], 'missing'), null);
  assert.equal(helpers.findTopicStats(null, 'a'), null);
  assert.equal(helpers.findTopicStats(undefined, 'a'), null);
});

// ---------------------------------------------------------------------------
// buildFromOffsetsForNextPage — M08/M05 per-partition paging cursor.
// ---------------------------------------------------------------------------

test('buildFromOffsetsForNextPage carries forward only partitions that reported hasMore', () => {
  const partitions = [
    { partition: 0, earliestOffset: 0, highWatermark: 500, nextOffset: 150, hasMore: true },
    { partition: 1, earliestOffset: 0, highWatermark: 30, nextOffset: 30, hasMore: false },
  ];
  assert.deepEqual(helpers.buildFromOffsetsForNextPage(partitions), [{ partition: 0, offset: 150 }]);
});

test('buildFromOffsetsForNextPage returns an empty array once every partition is exhausted', () => {
  assert.deepEqual(helpers.buildFromOffsetsForNextPage([{ partition: 0, hasMore: false, nextOffset: 10 }]), []);
  assert.deepEqual(helpers.buildFromOffsetsForNextPage(null), []);
});

// ---------------------------------------------------------------------------
// datetimeLocalToTsMs — M04's 4th offset-reset mode (`timestamp`).
// ---------------------------------------------------------------------------

test('datetimeLocalToTsMs converts a datetime-local value to an epoch-ms number', () => {
  const ms = helpers.datetimeLocalToTsMs('2026-08-27T14:30');
  assert.equal(ms, new Date('2026-08-27T14:30').getTime());
});

test('datetimeLocalToTsMs returns null for empty/invalid input', () => {
  assert.equal(helpers.datetimeLocalToTsMs(''), null);
  assert.equal(helpers.datetimeLocalToTsMs(null), null);
  assert.equal(helpers.datetimeLocalToTsMs('not-a-date'), null);
});

// ---------------------------------------------------------------------------
// isValidExplicitOffset (P3-6) — the reset modal's `explicit` mode used to
// coerce an empty field to offset 0 via `Number('' || 0)` with no error.
// ---------------------------------------------------------------------------

test('isValidExplicitOffset accepts a non-negative integer (as a string or a number)', () => {
  assert.equal(helpers.isValidExplicitOffset('0'), true);
  assert.equal(helpers.isValidExplicitOffset('150'), true);
  assert.equal(helpers.isValidExplicitOffset(150), true);
});

test('isValidExplicitOffset rejects empty/whitespace-only/negative/non-numeric input', () => {
  assert.equal(helpers.isValidExplicitOffset(''), false);
  assert.equal(helpers.isValidExplicitOffset('   '), false);
  assert.equal(helpers.isValidExplicitOffset(undefined), false);
  assert.equal(helpers.isValidExplicitOffset(null), false);
  assert.equal(helpers.isValidExplicitOffset('-1'), false);
  assert.equal(helpers.isValidExplicitOffset('abc'), false);
});

// ---------------------------------------------------------------------------
// unwrapCapabilities (P1-1) — `busCapabilitiesRequest` decodes to the
// ENVELOPE `tentaflow-protocol-wasm/src/lib.rs`'s `decode_bus_payload`
// builds for `BP::CapabilitiesResponse`: `{ variant: 'BusCapabilitiesResponse',
// capabilities: { canRead, canWrite, canAdmin, isSiteAdmin } }`. Reading
// that object flat (the P1-1 bug) always yields `undefined` for every
// field, so `canAdmin()`/`isSiteAdmin()` fail closed for EVERY session
// including a site admin, hiding "Nowy topik"/edit/delete/pause-resume/DLQ
// retry-discard/offset-reset everywhere at once.
// ---------------------------------------------------------------------------

test('unwrapCapabilities unwraps the real BusCapabilitiesResponse envelope shape', () => {
  // Exact shape from the wasm decoder / KRYTYK-M1.md's captured console
  // dump of `await ApiBinary.one('busCapabilitiesRequest')`.
  const envelope = {
    variant: 'BusCapabilitiesResponse',
    capabilities: { canRead: true, canWrite: true, canAdmin: true, isSiteAdmin: true },
  };
  assert.deepEqual(helpers.unwrapCapabilities(envelope), {
    canRead: true, canWrite: true, canAdmin: true, isSiteAdmin: true,
  });
});

test('unwrapCapabilities accepts an already-flat shape defensively', () => {
  const flat = { canRead: true, canWrite: false, canAdmin: false, isSiteAdmin: false };
  assert.deepEqual(helpers.unwrapCapabilities(flat), flat);
});

test('unwrapCapabilities fails closed to NO_CAPABILITIES for null/undefined/garbage', () => {
  assert.deepEqual(helpers.unwrapCapabilities(null), helpers.NO_CAPABILITIES);
  assert.deepEqual(helpers.unwrapCapabilities(undefined), helpers.NO_CAPABILITIES);
  assert.deepEqual(helpers.unwrapCapabilities({}), helpers.NO_CAPABILITIES);
  assert.deepEqual(helpers.unwrapCapabilities({ variant: 'BusCapabilitiesResponse' }), helpers.NO_CAPABILITIES);
});

// ---------------------------------------------------------------------------
// Lag math
// ---------------------------------------------------------------------------

test('sumGroupLag adds lagTotal (camelCase) across every group', () => {
  assert.equal(helpers.sumGroupLag([{ lagTotal: 10 }, { lagTotal: 5 }]), 15);
  assert.equal(helpers.sumGroupLag([]), 0);
  assert.equal(helpers.sumGroupLag(null), 0);
});

test('computeLagRatio is lag/highWatermark clamped to [0,1], 0 when hw<=0', () => {
  assert.equal(helpers.computeLagRatio(50, 100), 0.5);
  assert.equal(helpers.computeLagRatio(150, 100), 1);
  assert.equal(helpers.computeLagRatio(5, 0), 0);
});

test('lagSeverityClass buckets the ratio into ok/warn/danger', () => {
  assert.equal(helpers.lagSeverityClass(0.1), '');
  assert.equal(helpers.lagSeverityClass(0.4), 'tb-lagbar--warn');
  assert.equal(helpers.lagSeverityClass(0.8), 'tb-lagbar--danger');
});

// ---------------------------------------------------------------------------
// DLQ source options / byte preview / headers / BlobRef detection
// ---------------------------------------------------------------------------

test('dlqSourceTopicOptions excludes __dlq.* topics (isDlq=true)', () => {
  const opts = helpers.dlqSourceTopicOptions([
    { name: 'lab.wyniki.scchs', isDlq: false },
    { name: '__dlq.lab.wyniki.scchs', isDlq: true },
  ]);
  assert.deepEqual(opts, [{ value: 'lab.wyniki.scchs', label: 'lab.wyniki.scchs' }]);
});

// ---------------------------------------------------------------------------
// resolveDlqEntrySource (R3-1, KRYTYK-M1-R3.md's P1 blocker) — the state
// transition `ensureDlqTabReady` is built around. This is a pure function on
// purpose: the bug it fixes was a PAINT-time side effect
// (`paintDlqSourceOptions` used to also assign `state.dlqSource`) racing a
// guard that only checked whether `state.dlqSource` was already truthy —
// the side effect always won, so the guard's own `selectDlqSource` call
// (the only place that triggered `loadDlqRecords`) never ran and the DLQ
// tab stayed on `dlqRecords === null` forever. A helper with no side effects
// cannot have that race: callers decide what to DO with its answer.
// ---------------------------------------------------------------------------

test('resolveDlqEntrySource picks the first non-DLQ topic when nothing is selected yet', () => {
  const topics = [
    { name: '__dlq.lab.results', isDlq: true },
    { name: 'lab.results', isDlq: false },
    { name: 'orders.created', isDlq: false },
  ];
  assert.equal(helpers.resolveDlqEntrySource('', topics), 'lab.results');
  assert.equal(helpers.resolveDlqEntrySource(null, topics), 'lab.results');
});

test('resolveDlqEntrySource keeps an already-selected source untouched', () => {
  const topics = [{ name: 'lab.results', isDlq: false }, { name: 'orders.created', isDlq: false }];
  assert.equal(helpers.resolveDlqEntrySource('orders.created', topics), 'orders.created');
});

test('resolveDlqEntrySource degrades to "" when no source topic exists yet (topics still loading, or an org with only DLQ topics)', () => {
  assert.equal(helpers.resolveDlqEntrySource('', []), '');
  assert.equal(helpers.resolveDlqEntrySource('', [{ name: '__dlq.x', isDlq: true }]), '');
});

// ---------------------------------------------------------------------------
// Server error code mapping (dispatch/bus.rs::map_bus_error's "bus.<code>"
// convention) — every code this test exercises must have a translation in
// ALL FIVE locales, guarding the same "raw i18n key leaked to the user"
// regression `services.row-lifecycle.test.js` guards for its own module.
// ---------------------------------------------------------------------------

test('busErrorCode extracts the stable bus.<code> token', () => {
  assert.equal(helpers.busErrorCode("bus.topic_already_exists: 'orders.created'"), 'topic_already_exists');
  assert.equal(helpers.busErrorCode('bus.permission_denied: bus.read required'), 'permission_denied');
  assert.equal(helpers.busErrorCode('a transport-level error with no bus. prefix'), null);
  assert.equal(helpers.busErrorCode(undefined), null);
});

// P1-2: the string `busErrorCode` actually receives at runtime is NEVER the
// bare `bus.<code>: ...` shape above — `binary-ws-client.js`'s pending-
// request rejection wraps `ProtocolError` as `protocol error ${code}:
// ${message}` before `busErrorCode` ever sees it, and `ProtocolError`'s own
// `Display` (`message_body.rs`) is `"{Kind:?}: {message}"`, so `bus.<code>`
// never sits at index 0. A `^`-anchored regex (the bug) never matched this
// shape and always returned `null`. These are the exact two strings quoted
// in KRYTYK-M1.md's console capture.
test('busErrorCode finds bus.<code> after the "protocol error <Kind>: " wrapper (real wire shape)', () => {
  assert.equal(
    helpers.busErrorCode("protocol error NotFound: bus.topic_not_found: '__dlq.lab.wyniki.scchs' (DLQ never used yet)"),
    'topic_not_found',
  );
  assert.equal(
    helpers.busErrorCode('protocol error BadRequest: bus.invalid_topic_config: partitions must be 1-256, got 999'),
    'invalid_topic_config',
  );
});

test('mapBusErrorMessage translates the real "protocol error BadRequest: bus.invalid_topic_config: ..." shape to Polish', () => {
  const translate = makeTranslate(pl);
  const mapped = helpers.mapBusErrorMessage(
    'protocol error BadRequest: bus.invalid_topic_config: partitions must be 1-256, got 999',
    translate,
  );
  assert.equal(mapped, pl.tentabus.errors.invalid_topic_config);
});

function makeTranslate(dict) {
  return (path) => {
    const [, key] = path.split('.'); // 'errors.<code>'
    const value = dict?.tentabus?.errors?.[key];
    return value === undefined ? `tentabus.${path}` : value;
  };
}

const ERROR_CODES = [
  'not_initialized', 'db_error', 'fjall_error', 'codec_error', 'io_error', 'engine_error',
  'invalid_topic_name', 'invalid_topic_config', 'corrupt_topic_row', 'topic_already_exists',
  'topic_not_found', 'group_not_found', 'permission_denied', 'quota_exceeded',
  'quota_request_too_large', 'max_topics_exceeded', 'max_partitions_exceeded',
  'max_bytes_total_exceeded', 'throttled',
  'payload_too_large', 'dedup_key_required', 'producer_fenced', 'environment_mismatch',
  'invalid_argument', 'invalid_field', 'not_subscribed', 'offset_regression',
  'offset_out_of_range', 'offset_reset_mode_unsupported', 'group_paused',
  'dlq_of_dlq_not_allowed', 'partition_poisoned', 'partial_publish', 'blocking_task_failed',
  'max_groups_exceeded',
];

for (const [locName, dict] of [['pl', pl], ['en', en], ['de', de], ['es', es], ['fr', fr]]) {
  test(`mapBusErrorMessage translates every known bus.* code in ${locName}.json (no raw key leaks)`, () => {
    const translate = makeTranslate(dict);
    for (const code of ERROR_CODES) {
      const mapped = helpers.mapBusErrorMessage(`bus.${code}: some server detail`, translate);
      assert.notEqual(mapped, `tentabus.errors.${code}`, `${locName} is missing errors.${code}`);
      assert.ok(mapped.length > 0);
    }
  });
}

// R3-1's DLQ-load error state (`dlq_load_error_retry`) and R3-3's row
// activation hint (`row_activate_hint`) are new keys added in this fala —
// guard 5-locale parity for both the same way the error codes above are
// guarded, plus R3-5's confirm body still carrying both placeholders after
// its wording was extended to mention that discarded records are skipped.
for (const [locName, dict] of [['pl', pl], ['en', en], ['de', de], ['es', es], ['fr', fr]]) {
  test(`tentabus.${locName}.json has non-empty row_activate_hint and dlq_load_error_retry`, () => {
    assert.ok(dict.tentabus.row_activate_hint?.length > 0, `${locName} is missing row_activate_hint`);
    assert.ok(dict.tentabus.dlq_load_error_retry?.length > 0, `${locName} is missing dlq_load_error_retry`);
  });

  test(`tentabus.${locName}.json's dlq_retry_all_confirm_body keeps both {max}/{topic} placeholders`, () => {
    const body = dict.tentabus.dlq_retry_all_confirm_body;
    assert.ok(body.includes('{max}'), `${locName} dlq_retry_all_confirm_body lost {max}`);
    assert.ok(body.includes('{topic}'), `${locName} dlq_retry_all_confirm_body lost {topic}`);
  });
}

test('mapBusErrorMessage falls back to the raw server message for an unknown code', () => {
  const translate = makeTranslate(pl);
  assert.equal(
    helpers.mapBusErrorMessage('bus.some_future_code: detail', translate),
    'bus.some_future_code: detail',
  );
});

test('mapBusErrorMessage falls back to errors.generic for a non-"bus." message', () => {
  const translate = makeTranslate(pl);
  assert.equal(helpers.mapBusErrorMessage('', translate), pl.tentabus.errors.generic);
});

// ---------------------------------------------------------------------------
// Incremental-repaint helpers (owner requirement: "the chart must not draw
// from zero every time … all other data must only swap values, not
// re-render the page") — `pushWindowSample` (ring buffer), `diffRowsByKey`
// (M01/M04 table poll-skip gate), `patchText` (no-op-on-equal DOM
// writes) and `prefersReducedMotion` (the live chart's animation gate).
// ---------------------------------------------------------------------------

test('pushWindowSample keeps only the last maxLen samples, oldest evicted first (ring buffer)', () => {
  const arr = [];
  for (let i = 0; i < 5; i += 1) helpers.pushWindowSample(arr, { x: i, y: i * 10 }, 3);
  assert.deepEqual(arr, [{ x: 2, y: 20 }, { x: 3, y: 30 }, { x: 4, y: 40 }]);
});

test('pushWindowSample mutates and returns the SAME array reference — an in-place scroll, not a fresh series the chart would redraw from zero', () => {
  const arr = [];
  const returned = helpers.pushWindowSample(arr, { x: 1, y: 1 }, 40);
  assert.equal(returned, arr);
});

test('pushWindowSample is a plain append while under the window size', () => {
  const arr = [{ x: 0, y: 0 }];
  helpers.pushWindowSample(arr, { x: 1, y: 1 }, 40);
  assert.deepEqual(arr, [{ x: 0, y: 0 }, { x: 1, y: 1 }]);
});

test('diffRowsByKey reports changed:false when every row is byte-for-byte identical to the last paint', () => {
  const prev = [{ id: 'a', v: 1 }, { id: 'b', v: 2 }];
  const next = prev.map((r) => ({ ...r }));
  assert.deepEqual(helpers.diffRowsByKey(prev, next, (r) => r.id), {
    added: [], updated: [], removed: [], changed: false,
  });
});

test('diffRowsByKey reports an added key for a new row', () => {
  const prev = [{ id: 'a', v: 1 }];
  const next = [{ id: 'a', v: 1 }, { id: 'b', v: 2 }];
  const diff = helpers.diffRowsByKey(prev, next, (r) => r.id);
  assert.deepEqual(diff.added, ['b']);
  assert.deepEqual(diff.updated, []);
  assert.deepEqual(diff.removed, []);
  assert.equal(diff.changed, true);
});

test('diffRowsByKey reports a removed key for a dropped row', () => {
  const prev = [{ id: 'a', v: 1 }, { id: 'b', v: 2 }];
  const next = [{ id: 'a', v: 1 }];
  const diff = helpers.diffRowsByKey(prev, next, (r) => r.id);
  assert.deepEqual(diff.removed, ['b']);
  assert.equal(diff.changed, true);
});

test('diffRowsByKey reports an updated key when a value changes for the same key', () => {
  const prev = [{ id: 'a', v: 1 }];
  const next = [{ id: 'a', v: 2 }];
  const diff = helpers.diffRowsByKey(prev, next, (r) => r.id);
  assert.deepEqual(diff.updated, ['a']);
  assert.equal(diff.changed, true);
});

test('diffRowsByKey treats a missing/null prevRows as "everything added"', () => {
  const next = [{ id: 'a', v: 1 }];
  assert.deepEqual(helpers.diffRowsByKey(null, next, (r) => r.id).added, ['a']);
  assert.deepEqual(helpers.diffRowsByKey(undefined, next, (r) => r.id).added, ['a']);
});

test('patchText writes textContent only when the value actually changed', () => {
  let writes = 0;
  const el = {
    _text: 'old',
    get textContent() { return this._text; },
    set textContent(v) { writes += 1; this._text = v; },
  };
  helpers.patchText(el, 'old');
  assert.equal(writes, 0, 'no write for an equal value — avoids layout churn on a flat poll');
  helpers.patchText(el, 'new');
  assert.equal(writes, 1);
  assert.equal(el.textContent, 'new');
});

test('patchText coerces null/undefined values to an empty string and tolerates a null element', () => {
  const el = { textContent: 'x' };
  helpers.patchText(el, null);
  assert.equal(el.textContent, '');
  assert.doesNotThrow(() => helpers.patchText(null, 'x'));
});

test('prefersReducedMotion defaults to false when matchMedia is unavailable (this non-browser test env)', () => {
  assert.equal(helpers.prefersReducedMotion(), false);
});

// ---------------------------------------------------------------------------
// M2 (PLAN-M2.md §1f) — M06 replication/failover request builders
// ---------------------------------------------------------------------------

test('buildReplicaListRequest omits an empty/falsy topic (org-wide scope)', () => {
  assert.deepEqual(helpers.buildReplicaListRequest(IID, ''), { instanceId: IID, topic: undefined });
  assert.deepEqual(helpers.buildReplicaListRequest(IID, undefined), { instanceId: IID, topic: undefined });
  assert.deepEqual(helpers.buildReplicaListRequest(IID, 'pacs.badania.nowe'), { instanceId: IID, topic: 'pacs.badania.nowe' });
});

test('buildReassignRequest carries a copy of the replicas array and a numeric partition', () => {
  const replicas = ['gcm-core-01', 'gczd-edge-02'];
  const req = helpers.buildReassignRequest(IID, 'pacs.badania.nowe', '5', replicas);
  assert.deepEqual(req, { instanceId: IID, topic: 'pacs.badania.nowe', partition: 5, replicas: ['gcm-core-01', 'gczd-edge-02'] });
  replicas.push('scchs-edge-03');
  assert.equal(req.replicas.length, 2, 'the request holds its OWN copy, not a live reference');
});

test('buildReassignRequest omits partition when null/undefined (whole-topic reassign)', () => {
  assert.equal(helpers.buildReassignRequest(IID, 't', null, []).partition, undefined);
  assert.equal(helpers.buildReassignRequest(IID, 't', undefined, []).partition, undefined);
});

test('buildLeaderTransferRequest shapes {instanceId, topic, partition, targetNodeId}', () => {
  assert.deepEqual(
    helpers.buildLeaderTransferRequest(IID, 'pacs.badania.nowe', '5', 'gcm-core-01'),
    { instanceId: IID, topic: 'pacs.badania.nowe', partition: 5, targetNodeId: 'gcm-core-01' },
  );
});

test('buildReplicaListRequest/buildReassignRequest/buildLeaderTransferRequest throw without an instance id (W9)', () => {
  assert.throws(() => helpers.buildReplicaListRequest('', 't'));
  assert.throws(() => helpers.buildReassignRequest(undefined, 't', null, []));
  assert.throws(() => helpers.buildLeaderTransferRequest(null, 't', 0, 'node-1'));
});

// ---------------------------------------------------------------------------
// SPEC D4 — env-filter for the M02/M06 node multiselects
// ---------------------------------------------------------------------------

const NODES_MIXED_ENV = [
  { nodeId: 'gcm-core-01', environment: 'prod', reachable: true },
  { nodeId: 'gczd-edge-02', environment: 'prod', reachable: true },
  { nodeId: 'scchs-edge-03', environment: 'prod', reachable: false },
  { nodeId: 'mesh-test-01', environment: 'test', reachable: true },
];

test('isSameEnvironment is false when localEnv is falsy (fail-closed — no node selectable until known)', () => {
  assert.equal(helpers.isSameEnvironment(NODES_MIXED_ENV[0], null), false);
  assert.equal(helpers.isSameEnvironment(NODES_MIXED_ENV[0], ''), false);
});

test('isSameEnvironment matches on the node\'s own environment field', () => {
  assert.equal(helpers.isSameEnvironment(NODES_MIXED_ENV[0], 'prod'), true);
  assert.equal(helpers.isSameEnvironment(NODES_MIXED_ENV[3], 'prod'), false);
});

// ---------------------------------------------------------------------------
// M03 lag/ISR-degraded math
// ---------------------------------------------------------------------------

test('computeReplicationLag is leo - hw, clamped to 0', () => {
  assert.equal(helpers.computeReplicationLag(100, 110), 10);
  assert.equal(helpers.computeReplicationLag(110, 110), 0);
  assert.equal(helpers.computeReplicationLag(110, 100), 0, 'hw can never legitimately exceed leo — clamp, do not go negative');
});

test('computeReplicationLag treats non-numeric input as 0', () => {
  assert.equal(helpers.computeReplicationLag(undefined, undefined), 0);
  assert.equal(helpers.computeReplicationLag(null, 50), 50);
});

test('isIsrDegraded is true iff isrCount < replicaCount', () => {
  assert.equal(helpers.isIsrDegraded(2, 3), true);
  assert.equal(helpers.isIsrDegraded(3, 3), false);
  assert.equal(helpers.isIsrDegraded(1, 1), false);
});

// ---------------------------------------------------------------------------
// M06 role-matrix builder
// ---------------------------------------------------------------------------

const PARTITION_P5 = {
  partition: 5,
  leaderNodeId: 'gcm-core-01',
  leaderEpoch: 4,
  replicas: ['gcm-core-01', 'gczd-edge-02', 'scchs-edge-03'],
  isr: ['gcm-core-01', 'gczd-edge-02'],
  lagging: [{ nodeId: 'scchs-edge-03', lagBytes: 91226112, lagMs: 4200, reason: 'lag 87 MiB > 64 MiB' }],
  highWatermark: 1000,
  logEndOffset: 1005,
  unavailableReason: null,
};

test('roleForNode: leader wins over isr/lagging for the leader\'s own id', () => {
  assert.equal(helpers.roleForNode(PARTITION_P5, 'gcm-core-01'), 'leader');
});

test('roleForNode: lagging wins over isr membership (mockup m06 p5: scchs-edge-03)', () => {
  assert.equal(helpers.roleForNode(PARTITION_P5, 'scchs-edge-03'), 'lagging');
});

test('roleForNode: isr for a non-leader, non-lagging replica in isr[]', () => {
  assert.equal(helpers.roleForNode(PARTITION_P5, 'gczd-edge-02'), 'isr');
});

test('roleForNode: none for an unrelated node id, and for null partition/nodeId', () => {
  assert.equal(helpers.roleForNode(PARTITION_P5, 'mesh-test-01'), 'none');
  assert.equal(helpers.roleForNode(null, 'gcm-core-01'), 'none');
  assert.equal(helpers.roleForNode(PARTITION_P5, null), 'none');
});

test('buildRoleMatrix builds one row per partition with a cell per requested node id', () => {
  const rows = helpers.buildRoleMatrix([PARTITION_P5], ['gcm-core-01', 'gczd-edge-02', 'scchs-edge-03']);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].partition, 5);
  assert.equal(rows[0].leaderEpoch, 4);
  assert.deepEqual(rows[0].cells, { 'gcm-core-01': 'leader', 'gczd-edge-02': 'isr', 'scchs-edge-03': 'lagging' });
});

test('buildRoleMatrix tolerates a missing/non-array partitions or nodeIds input', () => {
  assert.deepEqual(helpers.buildRoleMatrix(null, ['a']), []);
  assert.deepEqual(helpers.buildRoleMatrix([PARTITION_P5], null)[0].cells, {});
});

test('leaderTransferCandidates is ISR minus the current leader (PLAN-M2 K-M2-3)', () => {
  assert.deepEqual(helpers.leaderTransferCandidates(PARTITION_P5), ['gczd-edge-02']);
});

test('leaderTransferCandidates is empty when the only ISR member is the leader itself', () => {
  const soleLeader = { ...PARTITION_P5, isr: ['gcm-core-01'] };
  assert.deepEqual(helpers.leaderTransferCandidates(soleLeader), []);
});

// ---------------------------------------------------------------------------
// M06 node-card degraded state
// ---------------------------------------------------------------------------

test('nodeDegradedReason: unreachable wins regardless of lagging data', () => {
  const node = { nodeId: 'scchs-edge-03', reachable: false };
  assert.deepEqual(helpers.nodeDegradedReason(node, [PARTITION_P5]), { kind: 'unreachable' });
});

test('nodeDegradedReason: lagging when the node appears in some partition\'s lagging[]', () => {
  const node = { nodeId: 'scchs-edge-03', reachable: true };
  const reason = helpers.nodeDegradedReason(node, [PARTITION_P5]);
  assert.equal(reason.kind, 'lagging');
  assert.equal(reason.partition, 5);
  assert.equal(reason.lag.nodeId, 'scchs-edge-03');
});

test('nodeDegradedReason is null for a healthy, non-lagging node', () => {
  const node = { nodeId: 'gcm-core-01', reachable: true };
  assert.equal(helpers.nodeDegradedReason(node, [PARTITION_P5]), null);
});

// ---------------------------------------------------------------------------
// unavailableReasonI18nKey — PascalCase/snake_case tolerant
// ---------------------------------------------------------------------------

test('unavailableReasonI18nKey converts PascalCase Rust variant names to a snake_case i18n key', () => {
  assert.equal(helpers.unavailableReasonI18nKey('NoIsr'), 'replication.unavailable_no_isr');
  assert.equal(helpers.unavailableReasonI18nKey('EpochFenced'), 'replication.unavailable_epoch_fenced');
  assert.equal(helpers.unavailableReasonI18nKey('NoAssignment'), 'replication.unavailable_no_assignment');
});

test('unavailableReasonI18nKey passes an already-snake_case reason through unchanged', () => {
  assert.equal(helpers.unavailableReasonI18nKey('no_isr'), 'replication.unavailable_no_isr');
});

test('unavailableReasonI18nKey returns null for a falsy reason (the common, available case)', () => {
  assert.equal(helpers.unavailableReasonI18nKey(null), null);
  assert.equal(helpers.unavailableReasonI18nKey(''), null);
});

// ---------------------------------------------------------------------------
// extractNotLeaderHint — best-effort leader-node extraction from a
// `bus.not_leader` server message
// ---------------------------------------------------------------------------

test('extractNotLeaderHint pulls a node id out of a few plausible message shapes', () => {
  assert.equal(helpers.extractNotLeaderHint('bus.not_leader: current leader is gcm-core-01'), 'gcm-core-01');
  assert.equal(helpers.extractNotLeaderHint('bus.not_leader: leader_node_id=gcm-core-01'), 'gcm-core-01');
  assert.equal(helpers.extractNotLeaderHint('bus.not_leader: leader: "gcm-core-01"'), 'gcm-core-01');
});

test('extractNotLeaderHint returns null when the message does not carry a recognizable node id', () => {
  assert.equal(helpers.extractNotLeaderHint('bus.not_leader'), null);
  assert.equal(helpers.extractNotLeaderHint(''), null);
  assert.equal(helpers.extractNotLeaderHint(undefined), null);
});

// ---------------------------------------------------------------------------
// mapBusErrorMessage — M2's not_leader hint appended when both the code
// resolves AND a hint node id is extractable
// ---------------------------------------------------------------------------

// `mapBusErrorMessage` calls `translate(path, params)` with the RAW
// `errors.<code>` path (no `tentabus.` prefix — that prefix only appears in
// a MISS's own return value, `T`'s real convention: see `makeTranslate`
// above), and only THIS module's real `T` does `{param}` interpolation — a
// minimal stand-in for that here, params-aware, so these three tests do not
// need `pl.json` to already carry the not-yet-pasted M2 keys.
function makeNotLeaderTranslate({ withHint } = {}) {
  return (path, params) => {
    if (path === 'errors.not_leader') return 'Ten node nie jest liderem tej partycji.';
    if (path === 'errors.not_leader_hint' && withHint) return `Aktualny lider: ${params.node}.`;
    return `tentabus.${path}`;
  };
}

test('mapBusErrorMessage appends the not_leader hint when the translator resolves errors.not_leader_hint', () => {
  const msg = helpers.mapBusErrorMessage(
    'protocol error BadRequest: bus.not_leader: current leader is gcm-core-01',
    makeNotLeaderTranslate({ withHint: true }),
  );
  assert.equal(msg, 'Ten node nie jest liderem tej partycji. Aktualny lider: gcm-core-01.');
});

test('mapBusErrorMessage falls back to the plain translated message when no hint node is extractable', () => {
  const msg = helpers.mapBusErrorMessage(
    'protocol error BadRequest: bus.not_leader',
    makeNotLeaderTranslate({ withHint: true }),
  );
  assert.equal(msg, 'Ten node nie jest liderem tej partycji.');
});

test('mapBusErrorMessage falls back to the plain translated message when errors.not_leader_hint itself is missing (coordinator has not pasted the M2 i18n block yet)', () => {
  const msg = helpers.mapBusErrorMessage(
    'protocol error BadRequest: bus.not_leader: current leader is gcm-core-01',
    makeNotLeaderTranslate({ withHint: false }),
  );
  assert.equal(msg, 'Ten node nie jest liderem tej partycji.');
});

// ---------------------------------------------------------------------------
// resolveInstanceGate — the Playwright critic pass (05.09.2026) opened
// `?instance=<id of an uninstalled instance>` from a stale sidebar entry and
// was shown ANOTHER instance's data under that URL. The gate is async and
// closes over `fetchTentaBusInstances`, so it is cut out and evaluated with
// that one dependency injected, rather than through the shared `helpers`
// bundle above.
// ---------------------------------------------------------------------------

function makeGate(instances) {
  // eslint-disable-next-line no-new-func
  return new Function(
    'fetchTentaBusInstances',
    // `cut` matches on `function <name>(`, so the `async` keyword in front of
    // the real declaration is left behind — put it back, or the extracted
    // body's `await` is a SyntaxError.
    `async ${cut(source, 'resolveInstanceGate')}\nreturn resolveInstanceGate;`,
  )(async () => instances);
}

const INST_TEST = { addonId: 'tentabus-1111aaaa', title: 'test', enabled: true };
const INST_PROD = { addonId: 'tentabus-2222bbbb', title: 'prod', enabled: true };

test('resolveInstanceGate refuses an unknown ?instance= even when exactly one instance is enabled', async () => {
  const gate = await makeGate([INST_PROD])('tentabus-1111aaaa');
  assert.equal(gate.target, null, 'must not fall through to the single-enabled shortcut');
  assert.equal(gate.unknownRequestedId, 'tentabus-1111aaaa');
});

test('resolveInstanceGate refuses an unknown ?instance= with several enabled instances', async () => {
  const gate = await makeGate([INST_TEST, INST_PROD])('tentabus-9999ffff');
  assert.equal(gate.target, null);
  assert.equal(gate.unknownRequestedId, 'tentabus-9999ffff');
});

test('resolveInstanceGate honours a named instance, including a disabled one', async () => {
  const disabled = { ...INST_TEST, enabled: false };
  const gate = await makeGate([disabled, INST_PROD])(disabled.addonId);
  assert.equal(gate.target, disabled);
  assert.equal(gate.unknownRequestedId, undefined);
});

test('resolveInstanceGate auto-enters the single enabled instance when no id is requested', async () => {
  const gate = await makeGate([INST_PROD, { ...INST_TEST, enabled: false }])(null);
  assert.equal(gate.target, INST_PROD);
  assert.equal(gate.unknownRequestedId, undefined);
});

test('resolveInstanceGate renders the chooser (no target) for several enabled instances and no requested id', async () => {
  const gate = await makeGate([INST_TEST, INST_PROD])(null);
  assert.equal(gate.target, null);
  assert.equal(gate.unknownRequestedId, undefined);
  assert.equal(gate.instances.length, 2);
});

test('instance_picker_unknown exists in every locale and interpolates {id}', () => {
  for (const [name, loc] of [['pl', pl], ['en', en], ['de', de], ['es', es], ['fr', fr]]) {
    const value = loc.tentabus?.instance_picker_unknown;
    assert.equal(typeof value, 'string', `${name}: key missing`);
    assert.ok(value.includes('{id}'), `${name}: no {id} placeholder`);
  }
});

// A source scan, not a behaviour test, because the gate is spread across six
// render sites and a wrong one is invisible until someone with exactly the
// wrong session opens exactly that panel.
//
// `dispatch/bus.rs` no longer has a separate site-admin tier: all eleven former
// admin variants sit on the plain `UserSession` dispatch and each opens with
// `gate_admin` (`bus.admin` in the instance matrix AND the `org.admin` role),
// which is precisely what `capabilities_v1` folds into `can_admin`. Gating any
// control on `isSiteAdmin` instead hides working controls from the delegated
// org operator the double lock exists for, and shows them to a site admin
// acting in an org where `gate_admin` refuses.
test('no control in tentabus.js is gated on isSiteAdmin', () => {
  const code = source
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('//'))
    .join('\n');
  const calls = code.match(/isSiteAdmin\s*\(/g) || [];
  assert.deepEqual(
    calls,
    [],
    'every admin control must gate on canAdmin() — the site-admin dispatch tier is gone',
  );
});

// The wording drifted behind the gate once already: the strings still named a
// role the backend had stopped asking for, so a user who was refused knew the
// wrong reason to go fix.
// The double lock (the instance's admin permission AND the org admin role),
// said in plain words rather than as a raw permission id.
test('the admin-required notes name both roles the double lock needs, in every locale', () => {
  const keys = ['acl_admin_required', 'group_detail_admin_required'];
  const words = {
    pl: ['administrator instancji', 'administratorem organizacji'],
    en: ['instance administrator', 'organisation administrator'],
    de: ['Administrator der Instanz', 'Administrator der Organisation'],
    es: ['administrador de la instancia', 'administrador de la organización'],
    fr: ['administrateur de l’instance', 'administrateur de l’organisation'],
  };
  for (const [name, loc] of [['pl', pl], ['en', en], ['de', de], ['es', es], ['fr', fr]]) {
    for (const key of keys) {
      const value = loc.tentabus?.[key];
      assert.equal(typeof value, 'string', `${name}.${key}: key missing`);
      for (const w of words[name]) assert.ok(value.includes(w), `${name}.${key} must name "${w}": ${value}`);
      assert.doesNotMatch(value, /\bbus\.[a-z_]+/, `${name}.${key}: no raw permission id`);
    }
  }
});
