// =============================================================================
// File: modules/tentabus.request-builders.test.js
// Description: Unit tests for tentabus.js's pure helpers — request builders
//       (`buildTopicOptionsWire`, `buildMessagesBrowseRequest` incl. tor U's
//       per-partition `fromOffsets`/`buildFromOffsetsForNextPage`),
//       formatters (`retentionPresetFromMs`, `bytesToPreviewText`,
//       `datetimeLocalToTsMs`), lag math (`sumGroupLag`, `computeLagRatio`,
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

const CONSTS = [
  'DLQ_RETRY_ALL_MAX', 'TOPIC_NAME_RE', 'RETENTION_PRESETS_MS', 'NO_CAPABILITIES',
  'DURABILITY_CLASSES',
];

const NAMES = [
  'requireInstanceId',
  'clampInt', 'clampReplicationFactor', 'clampDlqRetryAllMax', 'isValidTopicName',
  'defaultAcksForRf', 'retentionPresetFromMs', 'deriveDurabilityClass',
  'buildTopicOptionsWire', 'sumGroupLag',
  'computeLagRatio', 'lagSeverityClass', 'dlqSourceTopicOptions', 'bytesToPreviewText',
  'findHeader', 'headerText', 'parseBlobRefJson', 'busErrorCode', 'mapBusErrorMessage',
  'buildMessagesBrowseRequest', 'findTopicStats', 'buildFromOffsetsForNextPage',
  'datetimeLocalToTsMs', 'unwrapCapabilities', 'isValidExplicitOffset',
  // task 2 (M08 partition filter, KRYTYK-M1-R2.md's N-3):
  'filterRecordsByPartition', 'fromOffsetsForPartitionSelection',
  'hasMoreForPartitionSelection', 'partitionFilterOptions',
  // task 3 (Groups KPI = list, N-2/N-7):
  'isInternalGroupId', 'filterVisibleGroups',
  // task 4 (P3-14, DLQ header date formatting):
  'formatHeaderValue', 'msToDate',
  // R3-1 (KRYTYK-M1-R3.md, P1: DLQ tab empty on entry) — the single, pure
  // state-transition helper `ensureDlqTabReady` acts on:
  'resolveDlqEntrySource',
  // Fala post-R5 (KRYTYK-M1-R5.md b.2/b.3/b.7) — the wizard's `fsync_interval`
  // option formatter/clamp and the "(polityka jawna)" secondary-label
  // predicate the M01/M03 chip helper now calls.
  'clampFsyncIntervalMs', 'formatFsyncIntervalDurability', 'shouldShowDurabilityExplicitLabel',
  // Incremental-repaint fala (owner requirement: charts/tiles/tables only
  // swap values on a poll, never a full re-render) — `patchText`/`patchAttr`
  // (no-op-on-equal DOM writes), `pushWindowSample` (the live chart's ring
  // buffer), `diffRowsByKey` (M01/M04 table poll-skip gate) and
  // `prefersReducedMotion` (the live chart's entrance-animation gate).
  'patchText', 'patchAttr', 'pushWindowSample', 'diffRowsByKey', 'prefersReducedMotion',
  // M2 (PLAN-M2.md §1f) — M06 replication/failover, M03's partitions tab
  // and M02's node picker. Request builders, the SPEC D4 env-filter, the
  // role-matrix builder, lag/ISR-degraded math and the `not_leader` hint
  // extractor.
  'buildReplicaListRequest', 'buildReassignRequest', 'buildLeaderTransferRequest',
  'isSameEnvironment', 'filterSameEnvNodes', 'autoReplicationFactor',
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

test('clampReplicationFactor stays within PLAN §7.1 range 1-7, defaults to 3', () => {
  assert.equal(helpers.clampReplicationFactor(0), 1);
  assert.equal(helpers.clampReplicationFactor(1), 1);
  assert.equal(helpers.clampReplicationFactor(7), 7);
  assert.equal(helpers.clampReplicationFactor(42), 7);
  assert.equal(helpers.clampReplicationFactor(undefined), 3);
});

test('clampDlqRetryAllMax stays within the server-enforced [1,500] bound', () => {
  assert.equal(helpers.clampDlqRetryAllMax(0), 1);
  assert.equal(helpers.clampDlqRetryAllMax(500), 500);
  assert.equal(helpers.clampDlqRetryAllMax(10000), 500);
  assert.equal(helpers.clampDlqRetryAllMax(undefined), 100);
});

// ---------------------------------------------------------------------------
// isValidTopicName — mirrors bus::topics::validate_user_topic_name's shape
// ---------------------------------------------------------------------------

test('isValidTopicName accepts the PLAN §7.1 shape', () => {
  assert.equal(helpers.isValidTopicName('pacs.badania.nowe'), true);
  assert.equal(helpers.isValidTopicName('orders-created'), true);
  assert.equal(helpers.isValidTopicName('a1'), true);
});

test('isValidTopicName rejects the reserved "__" prefix and empty/short names', () => {
  assert.equal(helpers.isValidTopicName('__dlq.orders'), false);
  assert.equal(helpers.isValidTopicName(''), false);
  assert.equal(helpers.isValidTopicName('a'), false, 'needs at least 2 chars per the regex');
  assert.equal(helpers.isValidTopicName('Orders'), false, 'uppercase not allowed');
  assert.equal(helpers.isValidTopicName('orders_created'), false, 'underscore not in the char class');
});

// ---------------------------------------------------------------------------
// defaultAcksForRf — mirrors bus::topics::Acks::default_for_rf
// ---------------------------------------------------------------------------

test('defaultAcksForRf matches the server default (quorum at RF>=3, else leader)', () => {
  assert.equal(helpers.defaultAcksForRf(1), 'leader');
  assert.equal(helpers.defaultAcksForRf(2), 'leader');
  assert.equal(helpers.defaultAcksForRf(3), 'quorum');
  assert.equal(helpers.defaultAcksForRf(7), 'quorum');
});

// ---------------------------------------------------------------------------
// retentionPresetFromMs
// ---------------------------------------------------------------------------

test('retentionPresetFromMs recognizes every PLAN §7.1 preset', () => {
  assert.equal(helpers.retentionPresetFromMs(86_400_000), '24h');
  assert.equal(helpers.retentionPresetFromMs(604_800_000), '7d');
  assert.equal(helpers.retentionPresetFromMs(2_592_000_000), '30d');
  assert.equal(helpers.retentionPresetFromMs(7_776_000_000), '90d');
  assert.equal(helpers.retentionPresetFromMs(31_536_000_000), '365d');
});

test('retentionPresetFromMs falls back to "custom" for any other value', () => {
  assert.equal(helpers.retentionPresetFromMs(123456), 'custom');
});

// ---------------------------------------------------------------------------
// buildTopicOptionsWire — the exact snake_case boundary the wasm encoder
// passes straight to `serde_json::from_str::<BusTopicOptionsWire>` (see
// tentabus.js's own doc comment on this function for the "why").
// ---------------------------------------------------------------------------

test('buildTopicOptionsWire emits snake_case keys matching BusTopicOptionsWire', () => {
  const wire = helpers.buildTopicOptionsWire({
    partitions: 8,
    retentionMs: 604_800_000,
    cleanupPolicy: 'delete',
    delivery: 'at_least_once',
    dedupWindowMs: 86_400_000,
    maxDeliveryAttempts: 5,
    retryBackoffMs: 1000,
    schemaId: 'cmc-wynik-v2',
    validation: 'off',
    contentType: 'application/json',
    replicationFactor: 3,
    acks: 'quorum',
    durability: 'fsync_batch_full',
    durabilityClass: 'critical',
    maxInlineBytes: 1_048_576,
    compression: 'lz4',
  });
  assert.deepEqual(wire, {
    partitions: 8,
    retention_ms: 604_800_000,
    cleanup_policy: 'delete',
    delivery: 'at_least_once',
    dedup_window_ms: 86_400_000,
    max_delivery_attempts: 5,
    retry_backoff_ms: 1000,
    schema_id: 'cmc-wynik-v2',
    validation: 'off',
    content_type: 'application/json',
    replication_factor: 3,
    acks: 'quorum',
    durability: 'fsync_batch_full',
    durabilityClass: 'critical',
    max_inline_bytes: 1_048_576,
    compression: 'lz4',
  });
});

test('buildTopicOptionsWire omits unset fields so the server default / "leave unchanged" applies', () => {
  assert.deepEqual(helpers.buildTopicOptionsWire({}), {});
  assert.deepEqual(helpers.buildTopicOptionsWire({ partitions: '' }), {});
});

test('buildTopicOptionsWire omits acks/durability when the wizard leaves them on "auto"', () => {
  const wire = helpers.buildTopicOptionsWire({ acks: 'auto', durability: 'auto', replicationFactor: 3 });
  assert.deepEqual(wire, { replication_factor: 3 });
});

// R5-2 fix (KRYTYK-M1-R5.md b.2, P1: "critical → standard is a silent
// no-op"). CONTRACT: "sending `durabilityClass` WITHOUT `durability`
// switches the topic to the class-derived policy" — a class-only edit (the
// radio alone, advanced select left untouched) must put ONLY
// `durabilityClass` on the wire, never a `durability` key, explicit or not.
test('buildTopicOptionsWire sends only durabilityClass for a class-only downgrade (R5-2)', () => {
  const wire = helpers.buildTopicOptionsWire({ durability: 'auto', durabilityClass: 'standard' });
  assert.deepEqual(wire, { durabilityClass: 'standard' });
  assert.equal('durability' in wire, false);
});

// CONTRACT's second half: "sending `durability: 'auto'` clears an explicit
// policy and resolves from the class" — the wizard only sets this flag when
// the topic being edited already had `durabilityExplicit: true` AND the
// operator deliberately re-selected "Automatycznie (wg klasy)".
test('buildTopicOptionsWire sends the literal durability:"auto" clearing signal only when durabilityAutoClear is set', () => {
  assert.deepEqual(
    helpers.buildTopicOptionsWire({ durability: 'auto', durabilityAutoClear: true }),
    { durability: 'auto' },
  );
  assert.deepEqual(
    helpers.buildTopicOptionsWire({ durability: 'auto', durabilityAutoClear: false }),
    {},
    'without the flag, "auto" still means "left alone" — omitted, not sent',
  );
});

test('buildTopicOptionsWire sets an explicit durability policy string as-is regardless of durabilityAutoClear', () => {
  const wire = helpers.buildTopicOptionsWire({ durability: 'os', durabilityAutoClear: true });
  assert.deepEqual(wire, { durability: 'os' });
});

test('buildTopicOptionsWire never emits idempotency_key — the wizard does not offer it (fail-closed, PLAN M3a)', () => {
  const wire = helpers.buildTopicOptionsWire({ idempotencyKey: 'msg.run_id', partitions: 8 });
  assert.equal('idempotency_key' in wire, false);
  assert.deepEqual(wire, { partitions: 8 });
});

// Owner decision B (durability class UI): unlike every neighboring option,
// `durabilityClass` is sent camelCase, as-is — this is the one deliberate
// exception to this function's snake_case boundary (see its doc comment).
test('buildTopicOptionsWire emits durabilityClass camelCase for "standard"/"critical"', () => {
  assert.deepEqual(helpers.buildTopicOptionsWire({ durabilityClass: 'standard' }), { durabilityClass: 'standard' });
  assert.deepEqual(helpers.buildTopicOptionsWire({ durabilityClass: 'critical' }), { durabilityClass: 'critical' });
});

test('buildTopicOptionsWire omits durabilityClass when unset or not a recognized class', () => {
  assert.deepEqual(helpers.buildTopicOptionsWire({}), {});
  assert.deepEqual(helpers.buildTopicOptionsWire({ durabilityClass: undefined }), {});
  assert.deepEqual(helpers.buildTopicOptionsWire({ durabilityClass: '' }), {});
  assert.deepEqual(helpers.buildTopicOptionsWire({ durabilityClass: 'auto' }), {});
  assert.deepEqual(helpers.buildTopicOptionsWire({ durabilityClass: 'CRITICAL' }), {}, 'case-sensitive, not normalized');
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

test('clampFsyncIntervalMs stays within the server-enforced [1,1000] bound, defaults to 50', () => {
  assert.equal(helpers.clampFsyncIntervalMs(50), 50);
  assert.equal(helpers.clampFsyncIntervalMs(0), 1);
  assert.equal(helpers.clampFsyncIntervalMs(-5), 1);
  assert.equal(helpers.clampFsyncIntervalMs(5000), 1000);
  assert.equal(helpers.clampFsyncIntervalMs(1000), 1000);
  assert.equal(helpers.clampFsyncIntervalMs('abc'), 50);
  assert.equal(helpers.clampFsyncIntervalMs(undefined), 50);
  assert.equal(helpers.clampFsyncIntervalMs(12.9), 12, 'truncates toward zero, does not round');
});

test('formatFsyncIntervalDurability builds the fsync_interval:<ms> wire string the server parses', () => {
  assert.equal(helpers.formatFsyncIntervalDurability(50), 'fsync_interval:50');
  assert.equal(helpers.formatFsyncIntervalDurability('120'), 'fsync_interval:120');
  // `Number('')` is 0 — a finite number, same as `clampInt`'s own convention
  // elsewhere in this file — so an empty field clamps into range like any
  // other too-small value; it does not fall back to the 50 ms default (only
  // genuinely non-numeric input, e.g. 'abc', does that).
  assert.equal(helpers.formatFsyncIntervalDurability(''), 'fsync_interval:1');
  assert.equal(helpers.formatFsyncIntervalDurability('abc'), 'fsync_interval:50', 'non-numeric input falls back to the default, not fsync_interval:NaN');
});

// ---------------------------------------------------------------------------
// buildMessagesBrowseRequest — first page uses the legacy scalar
// `fromOffset`; subsequent pages use per-partition `fromOffsets` (tor U
// task 1) once a previous response's `partitions[]` is known. `partition`
// (task 2, M08's partition filter) is additive — see the function's own doc
// comment on why sending it is safe before the backend honors it.
// ---------------------------------------------------------------------------

test('buildMessagesBrowseRequest builds the first-page (global fromOffset) request shape', () => {
  assert.deepEqual(helpers.buildMessagesBrowseRequest(IID, 'pacs.badania.nowe', null, 50), {
    instanceId: IID,
    topic: 'pacs.badania.nowe',
    fromOffset: undefined,
    limit: 50,
    fromOffsets: undefined,
    partition: undefined,
  });
  assert.deepEqual(helpers.buildMessagesBrowseRequest(IID, 'pacs.badania.nowe', 10, 50), {
    instanceId: IID,
    topic: 'pacs.badania.nowe',
    fromOffset: 10,
    limit: 50,
    fromOffsets: undefined,
    partition: undefined,
  });
});

test('buildMessagesBrowseRequest builds a per-partition fromOffsets request for a follow-up page', () => {
  const fromOffsets = [{ partition: 0, offset: 120 }, { partition: 2, offset: 45 }];
  assert.deepEqual(helpers.buildMessagesBrowseRequest(IID, 'pacs.badania.nowe', null, 50, fromOffsets), {
    instanceId: IID,
    topic: 'pacs.badania.nowe',
    fromOffset: undefined,
    limit: 50,
    fromOffsets,
    partition: undefined,
  });
});

test('buildMessagesBrowseRequest treats an empty fromOffsets array as absent', () => {
  const req = helpers.buildMessagesBrowseRequest(IID, 'pacs.badania.nowe', null, 50, []);
  assert.equal(req.fromOffsets, undefined);
});

test('buildMessagesBrowseRequest carries an explicit partition filter (task 2, M08)', () => {
  const req = helpers.buildMessagesBrowseRequest(IID, 'pacs.badania.nowe', null, 50, undefined, 3);
  assert.equal(req.partition, 3);
});

test('buildMessagesBrowseRequest treats partition 0 as a real value, not "unset"', () => {
  // `0 ?? undefined` must stay `0` — a `||`-based implementation would have
  // coerced the very first partition to "all partitions".
  const req = helpers.buildMessagesBrowseRequest(IID, 'pacs.badania.nowe', null, 50, undefined, 0);
  assert.equal(req.partition, 0);
});

test('buildMessagesBrowseRequest emits instanceId and throws when called without one (W9)', () => {
  assert.equal(helpers.buildMessagesBrowseRequest(IID, 't', null, 50).instanceId, IID);
  assert.throws(() => helpers.buildMessagesBrowseRequest('', 't', null, 50));
  assert.throws(() => helpers.buildMessagesBrowseRequest(undefined, 't', null, 50));
});

// ---------------------------------------------------------------------------
// M08 partition filter (task 2, KRYTYK-M1-R2.md's N-3) — client-side
// filtering/paging helpers layered on top of the existing `partitions[]` +
// `fromOffsets` plumbing (tor U task 1).
// ---------------------------------------------------------------------------

test('filterRecordsByPartition returns every record when no partition is selected ("all")', () => {
  const records = [{ partition: 0, offset: 1 }, { partition: 3, offset: 2 }];
  assert.deepEqual(helpers.filterRecordsByPartition(records, null), records);
});

test('filterRecordsByPartition keeps only the selected partition\'s records', () => {
  const records = [{ partition: 0, offset: 1 }, { partition: 3, offset: 2 }, { partition: 3, offset: 3 }];
  assert.deepEqual(helpers.filterRecordsByPartition(records, 3), [
    { partition: 3, offset: 2 },
    { partition: 3, offset: 3 },
  ]);
});

test('filterRecordsByPartition tolerates a non-array input', () => {
  assert.deepEqual(helpers.filterRecordsByPartition(null, 0), []);
  assert.deepEqual(helpers.filterRecordsByPartition(undefined, null), []);
});

test('fromOffsetsForPartitionSelection delegates to buildFromOffsetsForNextPage for "all partitions"', () => {
  const partitions = [
    { partition: 0, nextOffset: 50, hasMore: true },
    { partition: 1, nextOffset: 10, hasMore: false },
  ];
  assert.deepEqual(
    helpers.fromOffsetsForPartitionSelection(partitions, null),
    helpers.buildFromOffsetsForNextPage(partitions),
  );
});

test('fromOffsetsForPartitionSelection returns only the selected partition\'s own cursor', () => {
  const partitions = [
    { partition: 0, nextOffset: 50, hasMore: true },
    { partition: 3, nextOffset: 120, hasMore: true },
  ];
  assert.deepEqual(helpers.fromOffsetsForPartitionSelection(partitions, 3), [{ partition: 3, offset: 120 }]);
});

test('fromOffsetsForPartitionSelection returns [] once the selected partition is exhausted or unknown', () => {
  const partitions = [{ partition: 3, nextOffset: 120, hasMore: false }];
  assert.deepEqual(helpers.fromOffsetsForPartitionSelection(partitions, 3), []);
  assert.deepEqual(helpers.fromOffsetsForPartitionSelection(partitions, 7), []);
  assert.deepEqual(helpers.fromOffsetsForPartitionSelection(null, 3), []);
});

test('hasMoreForPartitionSelection is the aggregate ("any partition") for "all partitions"', () => {
  const partitions = [{ partition: 0, hasMore: false }, { partition: 1, hasMore: true }];
  assert.equal(helpers.hasMoreForPartitionSelection(partitions, null), true);
  assert.equal(helpers.hasMoreForPartitionSelection([{ partition: 0, hasMore: false }], null), false);
});

test('hasMoreForPartitionSelection reads only the selected partition\'s own flag', () => {
  const partitions = [{ partition: 0, hasMore: true }, { partition: 1, hasMore: false }];
  assert.equal(helpers.hasMoreForPartitionSelection(partitions, 1), false);
  assert.equal(helpers.hasMoreForPartitionSelection(partitions, 0), true);
  assert.equal(helpers.hasMoreForPartitionSelection(partitions, 9), false, 'a partition never seen yet has no known "more"');
});

test('partitionFilterOptions builds "all" plus one option per partition, 0-indexed', () => {
  assert.deepEqual(helpers.partitionFilterOptions(3, 'All partitions'), [
    { value: '', label: 'All partitions' },
    { value: '0', label: 'P0' },
    { value: '1', label: 'P1' },
    { value: '2', label: 'P2' },
  ]);
});

test('partitionFilterOptions degrades to just "all" for an unknown/zero partition count', () => {
  assert.deepEqual(helpers.partitionFilterOptions(0, 'All'), [{ value: '', label: 'All' }]);
  assert.deepEqual(helpers.partitionFilterOptions(undefined, 'All'), [{ value: '', label: 'All' }]);
});

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

test('bytesToPreviewText decodes valid UTF-8 as text', () => {
  const bytes = new TextEncoder().encode('{"ok":true}');
  assert.equal(helpers.bytesToPreviewText(bytes), '{"ok":true}');
});

test('bytesToPreviewText falls back to a hex dump for invalid UTF-8', () => {
  const bytes = new Uint8Array([0xff, 0xfe, 0x00, 0x01]);
  assert.equal(helpers.bytesToPreviewText(bytes), 'ff fe 00 01');
});

test('findHeader/headerText locate a header by key and decode its bytes', () => {
  const headers = [{ key: 'dlq.reason', value: new TextEncoder().encode('consumer_error') }];
  assert.equal(helpers.findHeader(headers, 'dlq.reason').key, 'dlq.reason');
  assert.equal(helpers.findHeader(headers, 'missing'), null);
  assert.equal(helpers.headerText(headers, 'dlq.reason'), 'consumer_error');
  assert.equal(helpers.headerText(headers, 'missing'), null);
});

test('parseBlobRefJson recognizes the flow_engine::blob_store::BlobRef shape', () => {
  const blobRef = { id: 'blob-1', size_bytes: 2048, mime: 'application/dicom', sha256: 'abc123' };
  const bytes = new TextEncoder().encode(JSON.stringify(blobRef));
  assert.deepEqual(helpers.parseBlobRefJson(bytes), blobRef);
});

test('parseBlobRefJson returns null for a plain (non-BlobRef) payload', () => {
  const bytes = new TextEncoder().encode(JSON.stringify({ hello: 'world' }));
  assert.equal(helpers.parseBlobRefJson(bytes), null);
  assert.equal(helpers.parseBlobRefJson(new Uint8Array([0xff, 0x00])), null);
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
  'quota_request_too_large', 'max_topics_exceeded', 'max_partitions_exceeded', 'throttled',
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

// Owner decision B (durability class UI, M02/M03/M01) — 5-locale parity for
// the new "Klasa trwałości" wizard section, the M01/M03 chip and its
// resolved-policy tooltip, and the advanced-`durability`-wins-server-side
// hint. Same guard shape as the block above.
const DURABILITY_CLASS_KEYS = [
  'col_durability_class', 'durability_class_chip_standard', 'durability_class_chip_critical',
  'wizard_section_durability_class', 'wizard_durability_class_standard',
  'wizard_durability_class_standard_hint', 'wizard_durability_class_critical',
  'wizard_durability_class_critical_hint', 'wizard_durability_class_latency_note',
  'wizard_durability_override_hint', 'config_row_durability_class',
  // Fala post-R5 (KRYTYK-M1-R5.md b.2/b.3/b.4/b.6/b.7): the "(polityka
  // jawna)" secondary label, the wizard's "Automatycznie (wg klasy)"/
  // `fsync_interval` advanced options, and the live "class inactive" /
  // DLQ notes added by this fala.
  'durability_class_explicit_suffix', 'wizard_durability_auto',
  'wizard_durability_fsync_interval', 'wizard_field_fsync_interval_ms',
  'wizard_field_fsync_interval_ms_hint', 'wizard_durability_class_inactive_warning',
  'wizard_durability_class_dlq_note',
];
for (const [locName, dict] of [['pl', pl], ['en', en], ['de', de], ['es', es], ['fr', fr]]) {
  test(`tentabus.${locName}.json has every durability-class key non-empty`, () => {
    for (const key of DURABILITY_CLASS_KEYS) {
      assert.ok(dict.tentabus[key]?.length > 0, `${locName} is missing tentabus.${key}`);
    }
  });

  test(`tentabus.${locName}.json's durability_class_policy_title keeps the {durability} placeholder`, () => {
    assert.ok(
      dict.tentabus.durability_class_policy_title.includes('{durability}'),
      `${locName} durability_class_policy_title lost {durability}`,
    );
  });
}

// R5-6 fix (KRYTYK-M1-R5.md b.6, P2: "help texts stay silent about the loss
// window"). PLAN §11's decision is literally "an up-to-50-ms window of
// already-acknowledged messages lost on a simultaneous power loss with no
// replica" — the pre-fala hint described only the fsync MECHANISM ("fsync at
// most every 50 ms"), never that CONSEQUENCE. Only PL is asserted verbatim
// (the exact wording the fala brief specifies); every locale's own
// translation is checked for the same substance: a "50" ms figure in the
// standard hint, "no loss window" wording in the critical hint.
test('pl.json\'s standard/critical durability hints state the loss-window consequence, not just the fsync mechanism', () => {
  assert.equal(
    pl.tentabus.wizard_durability_class_standard_hint,
    'fsync co ≤50 ms, ACK po zapisie — przy utracie zasilania bez repliki możliwa utrata do 50 ms potwierdzonych komunikatów.',
  );
  assert.equal(
    pl.tentabus.wizard_durability_class_critical_hint,
    'fsync przed każdym ACK — brak okna utraty, p99 ACK ok. 10–30 ms na jednym dysku, niższa przepustowość.',
  );
});

const LOSS_WINDOW_CRITICAL_PHRASE = {
  pl: 'brak okna utraty', en: 'no loss window', de: 'kein Verlustfenster',
  es: 'sin ventana de pérdida', fr: 'aucune fenêtre de perte',
};
for (const [locName, dict] of [['pl', pl], ['en', en], ['de', de], ['es', es], ['fr', fr]]) {
  test(`tentabus.${locName}.json's standard durability hint names the 50 ms loss window`, () => {
    assert.ok(dict.tentabus.wizard_durability_class_standard_hint.includes('50'));
  });
  test(`tentabus.${locName}.json's critical durability hint states there is no loss window`, () => {
    assert.ok(dict.tentabus.wizard_durability_class_critical_hint.includes(LOSS_WINDOW_CRITICAL_PHRASE[locName]));
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
// (M01/M04 table poll-skip gate), `patchText`/`patchAttr` (no-op-on-equal
// DOM writes) and `prefersReducedMotion` (the live chart's animation gate).
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

test('patchAttr writes the attribute only when the value actually changed', () => {
  let writes = 0;
  const attrs = { value: '5' };
  const el = {
    getAttribute: (name) => attrs[name] ?? null,
    setAttribute: (name, v) => { writes += 1; attrs[name] = v; },
  };
  helpers.patchAttr(el, 'value', '5');
  assert.equal(writes, 0, 'no write for an equal value');
  helpers.patchAttr(el, 'value', '6');
  assert.equal(writes, 1);
  assert.equal(attrs.value, '6');
});

test('patchAttr tolerates a null element (paint call racing an unmounted panel)', () => {
  assert.doesNotThrow(() => helpers.patchAttr(null, 'value', '1'));
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

test('filterSameEnvNodes keeps only nodes matching localEnv (SPEC D4: mesh-test-01 excluded for a prod session)', () => {
  const same = helpers.filterSameEnvNodes(NODES_MIXED_ENV, 'prod');
  assert.deepEqual(same.map((n) => n.nodeId), ['gcm-core-01', 'gczd-edge-02', 'scchs-edge-03']);
});

test('filterSameEnvNodes returns an empty array for a null/empty node list', () => {
  assert.deepEqual(helpers.filterSameEnvNodes(null, 'prod'), []);
  assert.deepEqual(helpers.filterSameEnvNodes([], 'prod'), []);
});

test('autoReplicationFactor is min(3, healthy same-env nodes), clamped to at least 1', () => {
  // 3 prod nodes, one unreachable -> 2 healthy -> RF 2.
  assert.equal(helpers.autoReplicationFactor(NODES_MIXED_ENV, 'prod'), 2);
  // No prod nodes at all healthy/matching for a dev session -> clamps to 1, never 0.
  assert.equal(helpers.autoReplicationFactor(NODES_MIXED_ENV, 'dev'), 1);
});

test('autoReplicationFactor caps at 3 even with many healthy same-env nodes', () => {
  const many = Array.from({ length: 6 }, (_, i) => ({ nodeId: `n${i}`, environment: 'prod', reachable: true }));
  assert.equal(helpers.autoReplicationFactor(many, 'prod'), 3);
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
    if (path === 'errors.not_leader') return 'Ten węzeł nie jest liderem tej partycji.';
    if (path === 'errors.not_leader_hint' && withHint) return `Aktualny lider: ${params.node}.`;
    return `tentabus.${path}`;
  };
}

test('mapBusErrorMessage appends the not_leader hint when the translator resolves errors.not_leader_hint', () => {
  const msg = helpers.mapBusErrorMessage(
    'protocol error BadRequest: bus.not_leader: current leader is gcm-core-01',
    makeNotLeaderTranslate({ withHint: true }),
  );
  assert.equal(msg, 'Ten węzeł nie jest liderem tej partycji. Aktualny lider: gcm-core-01.');
});

test('mapBusErrorMessage falls back to the plain translated message when no hint node is extractable', () => {
  const msg = helpers.mapBusErrorMessage(
    'protocol error BadRequest: bus.not_leader',
    makeNotLeaderTranslate({ withHint: true }),
  );
  assert.equal(msg, 'Ten węzeł nie jest liderem tej partycji.');
});

test('mapBusErrorMessage falls back to the plain translated message when errors.not_leader_hint itself is missing (coordinator has not pasted the M2 i18n block yet)', () => {
  const msg = helpers.mapBusErrorMessage(
    'protocol error BadRequest: bus.not_leader: current leader is gcm-core-01',
    makeNotLeaderTranslate({ withHint: false }),
  );
  assert.equal(msg, 'Ten węzeł nie jest liderem tej partycji.');
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
