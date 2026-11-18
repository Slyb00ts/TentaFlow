// =============================================================================
// File: protocol/tentabus-b6-wire.test.js
// Description: The TentaBus F4 B6 wire (field policies on topics of Avro, Protobuf
//       and Thrift messages) through the REAL glue (www/js/protocol/wasm_glue*):
//       - the record previews (browse, unprocessed messages, "how does X see it")
//         carry `schemaRefId`, the id of the schema the bytes are written under
//         (for a restricted reader: its sub-schema; 0 = none/withheld);
//       - `SchemaForRecordGetRequest` / `SchemaFieldsGetRequest` build the exact
//         `BusBody` CBOR the server decodes, and their answers decode to the
//         objects the dashboard reads;
//       - the capabilities name the actions a rule on a binary topic may use,
//         absent from an older server.
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

test('the schema of a record is asked for by topic and the id the record carried', { skip }, () => {
  const sent = sentEnvelope('busSchemaForRecordGetRequest', { instanceId: INSTANCE, topic: 'wyniki', schemaRefId: 4242 });
  assert.equal(sent.instance_id, INSTANCE);
  assert.deepEqual(sent.payload, { SchemaForRecordGetRequest: { topic: 'wyniki', schema_ref_id: 4242 } });
  assert.throws(() => codec.encode.busSchemaForRecordGetRequest(1, { topic: 'wyniki', schemaRefId: 1 }), /instanceId/);
});

test('the fields of a binary pattern are asked for by subject, the version optional', { skip }, () => {
  const latest = sentEnvelope('busSchemaFieldsGetRequest', { instanceId: INSTANCE, subject: 'wynik.proto' });
  assert.equal(latest.payload.SchemaFieldsGetRequest.subject, 'wynik.proto');
  assert.equal(latest.payload.SchemaFieldsGetRequest.version ?? null, null);
  const pinned = sentEnvelope('busSchemaFieldsGetRequest', { instanceId: INSTANCE, subject: 'wynik.proto', version: 3 });
  assert.equal(pinned.payload.SchemaFieldsGetRequest.version, 3);
});

test('the answers decode to what the dashboard reads', { skip }, () => {
  const schema = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { SchemaForRecordGetResponse: { schema: {
    subject: 'wynik.proto', schema_type: 'protobuf', version: 2, schema_ref_id: 77, derived: true, schema_text: 'syntax = "proto3";',
  } } } } });
  assert.deepEqual(schema.payload.SchemaForRecordGetResponse.schema, {
    subject: 'wynik.proto', schemaType: 'protobuf', version: 2, schemaRefId: 77, derived: true, schemaText: 'syntax = "proto3";',
  });
  const fields = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { SchemaFieldsGetResponse: {
    subject: 'wynik.proto', schema_type: 'protobuf', version: 2, fields: ['id', 'pesel', 'wynik'],
  } } } });
  assert.deepEqual(fields.payload.SchemaFieldsGetResponse, {
    subject: 'wynik.proto', schemaType: 'protobuf', version: 2, fields: ['id', 'pesel', 'wynik'],
  });
});

test('the previews carry the id of the schema the bytes are written under, absent from an older server', { skip }, () => {
  const record = { partition: 0, offset: 1, timestamp_ms: 1, headers: [], payload_preview: new Uint8Array([8, 5]), is_blob_ref: false, truncated: false };
  const browse = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { MessagesBrowseResponse: { result: {
    records: [{ ...record, schema_ref_id: 91 }, record], has_more: false, next_offset: 2, partitions: [],
  } } } } });
  const [withId, older] = browse.payload.MessagesBrowseResponse.result.records;
  assert.equal(withId.schemaRefId, 91);
  assert.equal(older.schemaRefId, undefined);
  const dlq = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { DlqListResponse: { result: {
    records: [{ ...record, key: new Uint8Array(0), schema_ref_id: 5 }], has_more: false, next_offset: 2, partitions: [],
  } } } } });
  assert.equal(dlq.payload.DlqListResponse.result.records[0].schemaRefId, 5);
  const preview = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { FieldPolicyPreviewResponse: {
    record: { ...record, schema_ref_id: 0 }, applied: [{ field: 'id', action: 'show' }, { field: 'pesel', action: 'hide' }],
    limited_by_caller: false, hash_key_missing: false,
  } } } });
  assert.equal(preview.payload.FieldPolicyPreviewResponse.record.schemaRefId, 0);
});

test('the capabilities name what a rule on a binary topic may do', { skip }, () => {
  const base = {
    can_read: true, can_write: true, can_admin: true, is_site_admin: false, default_replication_factor: 1, node_count: 1,
    content_types: [], schema_types: [], field_actions: ['hide', 'mask', 'hash'],
  };
  const answer = (capabilities) => decodeBody({ BusBody: { instance_id: INSTANCE, payload: { CapabilitiesResponse: { capabilities } } } })
    .payload.CapabilitiesResponse.capabilities;
  assert.deepEqual(answer({ ...base, binary_field_actions: ['hide'] }).binaryFieldActions, ['hide']);
  assert.equal(answer(base).binaryFieldActions, undefined);
});

test('a message browse carries the correlation filter only when one is asked for (TB-02)', { skip }, () => {
  const filtered = sentEnvelope('busMessagesBrowseRequest', {
    instanceId: INSTANCE, topic: 'wyniki', limit: 50, correlationId: 'req-42',
  });
  assert.equal(filtered.payload.MessagesBrowseRequest.correlation_id, 'req-42');
  assert.equal(filtered.payload.MessagesBrowseRequest.topic, 'wyniki');
  for (const none of [undefined, null, '']) {
    const plain = sentEnvelope('busMessagesBrowseRequest', {
      instanceId: INSTANCE, topic: 'wyniki', limit: 50, correlationId: none,
    });
    assert.equal(plain.payload.MessagesBrowseRequest.correlation_id ?? null, null);
  }
});

test('a flow execution answer decodes with and without the correlation fields (TB-02)', { skip }, () => {
  const full = decodeBody({ FlowExecutionsListResponse: { executions: [{
    id: '1', flow_id: 'f', status: 'completed', started_at_epoch: 5, completed_at_epoch: 6,
    request_id: 'run-1', correlation_id: 'req-42', origin: 'bus',
  }] } });
  assert.equal(full.executions[0].correlationId, 'req-42');
  assert.equal(full.executions[0].requestId, 'run-1');
  assert.equal(full.executions[0].origin, 'bus');
  const legacy = decodeBody({ FlowExecutionsListResponse: { executions: [{
    id: '1', flow_id: 'f', status: 'completed', started_at_epoch: 5, completed_at_epoch: null,
  }] } });
  assert.equal(legacy.executions[0].correlationId ?? null, null);
});
