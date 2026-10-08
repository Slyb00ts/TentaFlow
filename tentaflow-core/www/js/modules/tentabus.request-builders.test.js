// =============================================================================
// File: modules/tentabus.request-builders.test.js
// Description: Unit tests for tentabus.js's pure helpers — request builders
//       (replica list, leader transfer, the per-partition
//       `buildFromOffsetsForNextPage` cursor of the unprocessed-message
//       pages), the stats join
//       (`findTopicStats`) and the server-error-code mapper
//       (`busErrorCode`/`mapBusErrorMessage`).
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

const CONSTS = ['NO_CAPABILITIES'];

const NAMES = [
  'requireInstanceId',
  'busErrorCode', 'mapBusErrorMessage', 'findTopicStats', 'buildFromOffsetsForNextPage',
  'unwrapCapabilities',
  'isInternalGroupId',
  // Replication request builders and the `not_leader` hint extractor.
  'buildReplicaListRequest', 'buildLeaderTransferRequest',
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
// The broker's internal `tf-*` consumer groups, left out again client-side
// on top of the server hiding them.
// ---------------------------------------------------------------------------

test('isInternalGroupId recognizes the tf-* prefix used by internal probes', () => {
  assert.equal(helpers.isInternalGroupId('tf-system-probe'), true);
  assert.equal(helpers.isInternalGroupId('billing'), false);
  assert.equal(helpers.isInternalGroupId('notifier'), false);
  assert.equal(helpers.isInternalGroupId(''), false);
  assert.equal(helpers.isInternalGroupId(null), false);
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
// buildFromOffsetsForNextPage — the per-partition cursor of the next page of
// a topic's unprocessed messages.
// ---------------------------------------------------------------------------

test('buildFromOffsetsForNextPage carries every partition forward, an exhausted one at its own bound', () => {
  // Partition 1 has nothing left below 30: without its cursor the server
  // would start it at its high watermark and list its newest messages again.
  const partitions = [
    { partition: 0, earliestOffset: 0, highWatermark: 500, nextOffset: 150, hasMore: true },
    { partition: 1, earliestOffset: 0, highWatermark: 60, nextOffset: 30, hasMore: false },
  ];
  assert.deepEqual(helpers.buildFromOffsetsForNextPage(partitions), [
    { partition: 0, offset: 150 },
    { partition: 1, offset: 30 },
  ]);
});

test('buildFromOffsetsForNextPage returns an empty array without partitions', () => {
  assert.deepEqual(helpers.buildFromOffsetsForNextPage(null), []);
  assert.deepEqual(helpers.buildFromOffsetsForNextPage([]), []);
});

// ---------------------------------------------------------------------------
// unwrapCapabilities (P1-1) — `busCapabilitiesRequest` decodes to the
// ENVELOPE `tentaflow-protocol-wasm/src/lib.rs`'s `decode_bus_payload`
// builds for `BP::CapabilitiesResponse`: `{ variant: 'BusCapabilitiesResponse',
// capabilities: { canRead, canWrite, canAdmin, isSiteAdmin } }`. Reading
// that object flat (the P1-1 bug) always yields `undefined` for every
// field, so `canAdmin()`/`isSiteAdmin()` fail closed for EVERY session
// including a site admin, hiding "Nowy topik"/edit/delete/moving leadership
// everywhere at once.
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
  'dlq_of_dlq_not_allowed', 'dlq_record_handled', 'dlq_retry_deduplicated',
  'dlq_retry_source_not_leader', 'dlq_retry_requarantine_failed', 'partition_poisoned', 'partial_publish',
  'blocking_task_failed', 'max_groups_exceeded',
  'key_needs_topic_wide_rule', 'subject_is_addon', 'subject_not_found', 'org_mismatch',
  // What a data-hiding rule or its preview can be refused with.
  'record_not_found', 'partition_out_of_range', 'field_not_allowed', 'required_field_missing', 'field_policy_payload_malformed',
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
// M2 (PLAN-M2.md §1f) — M06 replication/failover request builders
// ---------------------------------------------------------------------------

test('buildReplicaListRequest omits an empty/falsy topic (org-wide scope)', () => {
  assert.deepEqual(helpers.buildReplicaListRequest(IID, ''), { instanceId: IID, topic: undefined });
  assert.deepEqual(helpers.buildReplicaListRequest(IID, undefined), { instanceId: IID, topic: undefined });
  assert.deepEqual(helpers.buildReplicaListRequest(IID, 'pacs.badania.nowe'), { instanceId: IID, topic: 'pacs.badania.nowe' });
});

test('buildLeaderTransferRequest shapes {instanceId, topic, partition, targetNodeId}', () => {
  assert.deepEqual(
    helpers.buildLeaderTransferRequest(IID, 'pacs.badania.nowe', '5', 'gcm-core-01'),
    { instanceId: IID, topic: 'pacs.badania.nowe', partition: 5, targetNodeId: 'gcm-core-01' },
  );
});

test('buildReplicaListRequest/buildLeaderTransferRequest throw without an instance id (W9)', () => {
  assert.throws(() => helpers.buildReplicaListRequest('', 't'));
  assert.throws(() => helpers.buildLeaderTransferRequest(null, 't', 0, 'node-1'));
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

test('mapBusErrorMessage names the leader of the source partition a retried message goes back to', () => {
  const msg = helpers.mapBusErrorMessage(
    "protocol error Conflict: bus.dlq_retry_source_not_leader: 'wizyty'/1 leader_node_id=gcm-core-02",
    (path, params) => {
      if (path === 'errors.dlq_retry_source_not_leader') return pl.tentabus.errors.dlq_retry_source_not_leader;
      if (path === 'errors.not_leader_hint') return `Prowadzi ją teraz: ${params.node}.`;
      return `tentabus.${path}`;
    },
  );
  assert.equal(msg, `${pl.tentabus.errors.dlq_retry_source_not_leader} Prowadzi ją teraz: gcm-core-02.`);
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
