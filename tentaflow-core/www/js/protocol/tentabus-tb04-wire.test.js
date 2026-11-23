// =============================================================================
// File: protocol/tentabus-tb04-wire.test.js
// Description: The TentaBus TB-04 K8 wire through the REAL glue
//       (www/js/protocol/wasm_glue*): "Rozłóż prowadzenie" (plan and apply
//       requests, their answers), the fields TB-04 appended to the node card,
//       the partition row and the from_replica marks, each absent from an older
//       server.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const artifact = new URL('./wasm_glue_bg.wasm', import.meta.url);
// The glue is built by `tentaflow-core`. Without it these tests prove nothing, so CI (which builds the core before `npm test`) fails
// instead of showing a green run of skipped tests; a checkout that never built it skips them, loudly.
if (!existsSync(artifact) && process.env.CI) {
  throw new Error('wasm_glue_bg.wasm is absent in CI: build tentaflow-core before `npm test`, the wire tests would otherwise be skipped silently');
}
const skip = existsSync(artifact) ? false : 'wasm_glue_bg.wasm is absent — build tentaflow-core once to generate the glue (CI fails here instead of skipping)';
if (skip) console.warn(`SKIPPED ${new URL(import.meta.url).pathname.split('/').pop()}: ${skip}`);
const wasm = skip ? null : await import('./wasm_glue.js');
let codec;
if (!skip) {
  await wasm.default({ module_or_path: readFileSync(artifact) });
  codec = await import('./codec.js');
  await codec.codecReady;
}

// ----- a minimal CBOR writer and reader: what serde + ciborium exchange -------

function head(major, value) {
  const n = BigInt(value);
  const base = major << 5;
  if (n < 24n) return [base | Number(n)];
  if (n < 0x100n) return [base | 24, Number(n)];
  if (n < 0x10000n) return [base | 25, Number(n >> 8n), Number(n & 0xffn)];
  const width = n < 0x100000000n ? 4 : 8;
  const out = [base | (width === 4 ? 26 : 27)];
  for (let i = width - 1; i >= 0; i -= 1) out.push(Number((n >> BigInt(8 * i)) & 0xffn));
  return out;
}

function cbor(value) {
  if (value === null || value === undefined) return [0xf6];
  if (value === true) return [0xf5];
  if (value === false) return [0xf4];
  if (value instanceof Uint8Array) return [...head(2, value.length), ...value];
  if (Array.isArray(value)) return value.reduce((acc, v) => acc.concat(cbor(v)), head(4, value.length));
  if (typeof value === 'number') return head(0, value);
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value);
    return [...head(3, bytes.length), ...bytes];
  }
  const entries = Object.entries(value);
  return entries.reduce((acc, [k, v]) => acc.concat(cbor(k), cbor(v)), head(5, entries.length));
}

/** Reads the definite-length subset serde + ciborium write. */
function readCbor(bytes) {
  let pos = 0;
  const arg = (info) => {
    if (info < 24) return info;
    const width = { 24: 1, 25: 2, 26: 4, 27: 8 }[info];
    let n = 0n;
    for (let i = 0; i < width; i += 1) n = (n << 8n) | BigInt(bytes[pos + i]);
    pos += width;
    return n <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(n) : n;
  };
  const item = () => {
    const first = bytes[pos];
    pos += 1;
    const major = first >> 5;
    const info = first & 0x1f;
    if (major === 7) return { 20: false, 21: true, 22: null }[info];
    const n = arg(info);
    if (major === 0) return n;
    if (major === 1) return -1 - n;
    if (major === 2 || major === 3) {
      const slice = bytes.slice(pos, pos + n);
      pos += n;
      return major === 2 ? slice : new TextDecoder().decode(slice);
    }
    if (major === 4) return Array.from({ length: n }, item);
    const out = {};
    for (let i = 0; i < n; i += 1) {
      const key = item();
      out[key] = item();
    }
    return out;
  };
  return item();
}

const decodeBody = (body) => wasm.decodeMessageBody(new Uint8Array(cbor(body)));

/** The `BusEnvelope` a request builder puts on the wire, as plain CBOR data. */
function sentEnvelope(kind, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[kind](7, payload, 1));
  try {
    return readCbor(envelope.body).BusBody;
  } finally {
    envelope.free();
  }
}

const INSTANCE = 'tentabus-0000abcd';

test('the plan request names the instance and, when given, one topic', { skip }, () => {
  const all = sentEnvelope('busLeaderBalancePlanRequest', { instanceId: INSTANCE });
  assert.equal(all.instance_id, INSTANCE);
  assert.deepEqual(Object.keys(all.payload), ['LeaderBalancePlanRequest']);
  assert.equal(all.payload.LeaderBalancePlanRequest.topic ?? null, null);
  const one = sentEnvelope('busLeaderBalancePlanRequest', { instanceId: INSTANCE, topic: 'wizyty' });
  assert.deepEqual(one.payload, { LeaderBalancePlanRequest: { topic: 'wizyty' } });
  assert.throws(() => codec.encode.busLeaderBalancePlanRequest(1, {}), /instanceId/);
});

test('the apply request sends the moves as they were shown', { skip }, () => {
  const sent = sentEnvelope('busLeaderBalanceApplyRequest', {
    instanceId: INSTANCE,
    moves: [{ topic: 'wyniki-badan', partitions: [0, 1, 2], fromNodeId: 'a', toNodeId: 'b', returnsToPreferred: true }],
  });
  assert.deepEqual(sent.payload.LeaderBalanceApplyRequest.moves, [
    { topic: 'wyniki-badan', partitions: [0, 1, 2], from_node_id: 'a', to_node_id: 'b', returns_to_preferred: true },
  ]);
});

test('the plan answer decodes the moves, the load per node and the blockers', { skip }, () => {
  const body = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { LeaderBalancePlanResponse: {
    moves: [{ topic: 'faktury', partitions: [0, 1, 2], from_node_id: 'a', to_node_id: 'b', returns_to_preferred: true }],
    per_node: [{ node_id: 'a', partitions_before: 35, partitions_after: 22 }],
    blockers: [{ topic: 'wizyty', reason: 'consumer_group_paused' }, { topic: 'x', reason: 'target_behind', node_id: 'b' }],
  } } } });
  const plan = body.payload.LeaderBalancePlanResponse;
  assert.deepEqual(plan.moves[0], { topic: 'faktury', partitions: [0, 1, 2], fromNodeId: 'a', toNodeId: 'b', returnsToPreferred: true });
  assert.deepEqual(plan.perNode[0], { nodeId: 'a', partitionsBefore: 35, partitionsAfter: 22 });
  assert.equal(plan.blockers[0].nodeId, undefined, 'a blocker without a node');
  assert.equal(plan.blockers[1].nodeId, 'b');
});

test('the apply answer decodes how each move ended', { skip }, () => {
  const body = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { LeaderBalanceApplyResponse: { results: [
    { topic: 'a', partitions: [0], from_node_id: 'a', to_node_id: 'b', status: 'moved' },
    { topic: 'b', partitions: [0], from_node_id: 'a', to_node_id: 'b', status: 'failed_rolled_back', error_code: 'bus.transfer_target_behind', detail: 'x' },
  ] } } } });
  const [moved, failed] = body.payload.LeaderBalanceApplyResponse.results;
  assert.equal(moved.status, 'moved');
  assert.equal(moved.errorCode, undefined);
  assert.deepEqual([failed.status, failed.errorCode, failed.detail], ['failed_rolled_back', 'bus.transfer_target_behind', 'x']);
});

test('a node card and a partition row carry what TB-04 appended, and an older server omits it', { skip }, () => {
  const node = { node_id: 'a', label: 'A', environment: 'prod', is_local: true, reachable: true, leader_count: 1, follower_count: 0, isr_count: 1 };
  const partition = {
    partition: 0, leader_node_id: 'a', leader_epoch: 1, replicas: ['b', 'a'], isr: ['a'], lagging: [],
    high_watermark: 0, log_end_offset: 0, unavailable_reason: null,
  };
  const reply = (nodes, partitions) => decodeBody({ BusBody: { instance_id: INSTANCE, payload: { ReplicaListResponse: { nodes, partitions, failovers: [] } } } }).payload.ReplicaListResponse;
  const full = reply([{ ...node, forward_capable: true, forwarding: 'capable' }], [{ ...partition, preferred_leader_node_id: 'b' }]);
  assert.equal(full.nodes[0].forwardCapable, true);
  assert.equal(full.nodes[0].forwarding, 'capable');
  for (const state of ['lacking', 'unknown']) {
    assert.equal(reply([{ ...node, forward_capable: false, forwarding: state }], []).nodes[0].forwarding, state, 'the three states are told apart');
  }
  assert.equal(full.partitions[0].preferredLeaderNodeId, 'b');
  const older = reply([node], [partition]);
  assert.equal(older.nodes[0].forwardCapable, undefined);
  assert.equal(older.nodes[0].forwarding, undefined);
  assert.equal(older.partitions[0].preferredLeaderNodeId, undefined);
});

test('the browse, group, lag and DLQ answers say when they come from a replica', { skip }, () => {
  const lag = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { LagHistoryResponse: { groups: [
    { group: 'g', topic: 't', samples: [], from_replica: true }, { group: 'h', topic: 't', samples: [] },
  ] } } } }).payload.LagHistoryResponse.groups;
  assert.deepEqual(lag.map((g) => g.fromReplica), [true, undefined]);
  const detail = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { GroupDetailResponse: { detail: {
    group: 'g', topic: 't', commit_mode: 'explicit', paused: false, partitions: [], from_replica: true,
  } } } } }).payload.GroupDetailResponse.detail;
  assert.equal(detail.fromReplica, true);
});
