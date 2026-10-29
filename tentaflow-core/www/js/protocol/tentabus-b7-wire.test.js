// =============================================================================
// File: protocol/tentabus-b7-wire.test.js
// Description: The TentaBus F4 B7 wire (masking and hashing of read-rule fields)
//       through the REAL glue (www/js/protocol/wasm_glue*):
//       - `FieldPolicySetRequest` carries `masked_fields` / `hashed_fields`
//         (left out when the caller names none, so the server keeps the stored
//         ones; `[]` removes them);
//       - `FieldHashKeyInfoRequest` / `FieldHashKeyRotateRequest` build the exact
//         `BusBody` CBOR the server decodes, the rotation naming the key version
//         (0 — which rotates nothing — when it is missing);
//       - the rule list, the key answers, the preview's per-field actions and the
//         capabilities decode to the objects the dashboard reads, including an
//         answer from a server that predates the new fields.
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

test('a rule write names the fields it masks and hashes; unnamed ones are left to the server', { skip }, () => {
  const sent = sentEnvelope('busFieldPolicySetRequest', {
    instanceId: INSTANCE, topic: 'wyniki', subjectType: 'group', subjectId: 'g-1', direction: 'read',
    fields: ['id', 'pesel', 'nazwisko'], maskedFields: ['pesel'], hashedFields: ['nazwisko'], expectAbsent: true,
  });
  assert.deepEqual(sent.payload.FieldPolicySetRequest.fields, ['id', 'pesel', 'nazwisko']);
  assert.deepEqual(sent.payload.FieldPolicySetRequest.masked_fields, ['pesel']);
  assert.deepEqual(sent.payload.FieldPolicySetRequest.hashed_fields, ['nazwisko']);
  assert.equal(sent.payload.FieldPolicySetRequest.expect_absent, true);

  const plain = sentEnvelope('busFieldPolicySetRequest', {
    instanceId: INSTANCE, topic: 'wyniki', subjectType: 'any', subjectId: '*', direction: 'read', fields: ['id'],
  });
  // Not named = not sent: the server keeps the stored masks and hashes
  // (a client that never heard of them must not unmask a field).
  assert.equal(plain.payload.FieldPolicySetRequest.masked_fields ?? null, null);
  assert.equal(plain.payload.FieldPolicySetRequest.hashed_fields ?? null, null);

  // `[]` is an explicit "none".
  const cleared = sentEnvelope('busFieldPolicySetRequest', {
    instanceId: INSTANCE, topic: 'wyniki', subjectType: 'any', subjectId: '*', direction: 'read', fields: ['id'],
    maskedFields: [], hashedFields: [],
  });
  assert.deepEqual(cleared.payload.FieldPolicySetRequest.masked_fields, []);
  assert.deepEqual(cleared.payload.FieldPolicySetRequest.hashed_fields, []);
});

test('the hash key requests carry the instance, and the rotation the version it was shown', { skip }, () => {
  const info = sentEnvelope('busFieldHashKeyInfoRequest', { instanceId: INSTANCE });
  assert.equal(info.instance_id, INSTANCE);
  assert.deepEqual(info.payload, { FieldHashKeyInfoRequest: {} });

  const rotate = sentEnvelope('busFieldHashKeyRotateRequest', { instanceId: INSTANCE, expectedKeyVersion: 3 });
  assert.deepEqual(rotate.payload, { FieldHashKeyRotateRequest: { expected_key_version: 3 } });
  const missing = sentEnvelope('busFieldHashKeyRotateRequest', { instanceId: INSTANCE });
  assert.deepEqual(missing.payload, { FieldHashKeyRotateRequest: { expected_key_version: 0 } });
  assert.throws(() => codec.encode.busFieldHashKeyRotateRequest(1, { expectedKeyVersion: 1 }), /instanceId/);
});

test('the key answers decode the version, its age and what a rotation affects', { skip }, () => {
  const key = {
    exists: true, key_version: 3, created_at_ms: 1000, rotated_at_ms: 5000,
    hashing_rules: 4, hashing_topics: 3, hashing_instances: 2, can_rotate_here: false,
  };
  for (const variant of ['FieldHashKeyInfoResponse', 'FieldHashKeyRotateResponse']) {
    const body = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { [variant]: { key } } } });
    assert.equal(body._variant, 'BusBody');
    assert.deepEqual(body.payload[variant].key, {
      exists: true, keyVersion: 3, createdAtMs: 1000, rotatedAtMs: 5000,
      hashingRules: 4, hashingTopics: 3, hashingInstances: 2, canRotateHere: false,
    });
  }
  const none = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { FieldHashKeyInfoResponse: { key: {
    exists: false, key_version: 0, created_at_ms: 0, rotated_at_ms: 0, hashing_rules: 0, hashing_topics: 0, hashing_instances: 0, can_rotate_here: true,
  } } } } });
  assert.equal(none.payload.FieldHashKeyInfoResponse.key.exists, false);
  const older = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { FieldHashKeyInfoResponse: { key: {
    exists: true, key_version: 1, created_at_ms: 1, rotated_at_ms: 1, hashing_rules: 1, hashing_topics: 1, hashing_instances: 1,
  } } } } });
  assert.equal(older.payload.FieldHashKeyInfoResponse.key.canRotateHere, undefined, 'an older server does not say: the dashboard then assumes it may');
});

test('the rule list decodes the masked and hashed fields, absent from an older server', { skip }, () => {
  const rule = {
    subject_type: 'group', subject_id: 'g-1', direction: 'read', fields: ['id', 'pesel', 'nazwisko'],
    required_fields: [], created_at_ms: 1, updated_at_ms: 2,
  };
  const body = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { FieldPolicyListResponse: { policies: [
    { ...rule, masked_fields: ['pesel'], hashed_fields: ['nazwisko'] },
    { ...rule, subject_id: 'g-2' },
  ] } } } });
  const [full, older] = body.payload.FieldPolicyListResponse.policies;
  assert.deepEqual(full.fields, ['id', 'pesel', 'nazwisko']);
  assert.deepEqual(full.maskedFields, ['pesel']);
  assert.deepEqual(full.hashedFields, ['nazwisko']);
  assert.equal(older.maskedFields, undefined);
  assert.equal(older.hashedFields, undefined);
});

test('the preview names the mask and the hash a rule applies, and the server lists both actions', { skip }, () => {
  const preview = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { FieldPolicyPreviewResponse: {
    record: {
      partition: 0, offset: 0, timestamp_ms: 1, headers: [], payload_preview: new TextEncoder().encode('{}'),
      is_blob_ref: false, truncated: false,
    },
    applied: [
      { field: 'id', action: 'show' }, { field: 'nazwisko', action: 'hash' },
      { field: 'pesel', action: 'mask' }, { field: 'uwagi', action: 'hide' },
    ],
    limited_by_caller: false,
    hash_key_missing: true,
  } } } });
  assert.equal(preview.payload.FieldPolicyPreviewResponse.hashKeyMissing, true);
  assert.deepEqual(
    preview.payload.FieldPolicyPreviewResponse.applied.map((a) => [a.field, a.action]),
    [['id', 'show'], ['nazwisko', 'hash'], ['pesel', 'mask'], ['uwagi', 'hide']],
  );
  const capabilities = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { CapabilitiesResponse: { capabilities: {
    can_read: true, can_write: true, can_admin: true, is_site_admin: false, default_replication_factor: 1, node_count: 1,
    content_types: [], schema_types: [], field_actions: ['hide', 'mask', 'hash'],
  } } } } });
  assert.deepEqual(capabilities.payload.CapabilitiesResponse.capabilities.fieldActions, ['hide', 'mask', 'hash']);
});
