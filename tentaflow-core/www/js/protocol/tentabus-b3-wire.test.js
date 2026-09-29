// =============================================================================
// File: protocol/tentabus-b3-wire.test.js
// Description: The TentaBus B3 wire (PLAN-UI-20260923) through the REAL glue
//       (www/js/protocol/wasm_glue*):
//       - `codec.encode.busSubjectDirectoryRequest` / `busFieldPolicyPreviewRequest`
//         → the wasm encoder → the exact `BusBody` CBOR the server decodes;
//       - `SubjectDirectoryResponse` / `FieldPolicyPreviewResponse` decode to
//         the objects the access and data-hiding windows read, including an
//         answer without the `#[serde(default)]` fields;
//       - an addon row of `AclListResponse` keeps its own subject type;
//       - U6 Dostęp: `AclSetRequest` carries one right of one subject, and
//         `CapabilitiesResponse` hands over the caller's organisation (the
//         middle part of a topic right's id and the REST address's `org_id`),
//         empty on an answer that predates it.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const artifact = new URL('./wasm_glue_bg.wasm', import.meta.url);
const skip = existsSync(artifact) ? false : 'wasm_glue_bg.wasm is absent — build tentaflow-core once to generate the glue';
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

// ----- requests -------------------------------------------------------------

test('the subject directory request carries the kind and the query', { skip }, () => {
  const sent = sentEnvelope('busSubjectDirectoryRequest', { instanceId: INSTANCE, kind: 'group', query: 'Łek' });
  assert.equal(sent.instance_id, INSTANCE);
  assert.deepEqual(sent.payload, { SubjectDirectoryRequest: { kind: 'group', query: 'Łek' } });
  const plain = sentEnvelope('busSubjectDirectoryRequest', { instanceId: INSTANCE, kind: 'addon' });
  assert.deepEqual(plain.payload, { SubjectDirectoryRequest: { kind: 'addon', query: '' } });
});

test('the preview request names the record and the subject', { skip }, () => {
  const sent = sentEnvelope('busFieldPolicyPreviewRequest', {
    instanceId: INSTANCE, topic: 'wyniki', partition: 2, offset: 41, subjectType: 'group', subjectId: 'g-1',
  });
  assert.deepEqual(sent.payload, {
    FieldPolicyPreviewRequest: { topic: 'wyniki', partition: 2, offset: 41, subject_type: 'group', subject_id: 'g-1' },
  });
  const everyone = sentEnvelope('busFieldPolicyPreviewRequest', { instanceId: INSTANCE, topic: 'wyniki', partition: 0, offset: 0 });
  assert.equal(everyone.payload.FieldPolicyPreviewRequest.subject_type, 'any');
  assert.equal(everyone.payload.FieldPolicyPreviewRequest.subject_id, '*');
});

test('a bus request without an instance is refused before it is encoded', { skip }, () => {
  assert.throws(() => codec.encode.busSubjectDirectoryRequest(1, { kind: 'user' }), /instanceId/);
});

// ----- responses ------------------------------------------------------------

test('the directory answer decodes labels, group sizes and the truncation flag', { skip }, () => {
  const body = decodeBody({
    BusBody: {
      instance_id: INSTANCE,
      payload: {
        SubjectDirectoryResponse: {
          entries: [
            { subject_type: 'group', subject_id: 'g-1', label: 'Lekarze', member_count: 12 },
            { subject_type: 'addon', subject_id: 'asystent', label: 'Asystent lekarza', member_count: null },
          ],
          truncated: true,
        },
      },
    },
  });
  assert.equal(body.variant, 'BusSubjectDirectoryResponse');
  assert.equal(body.truncated, true);
  assert.equal(body.entries[0].subjectType, 'group');
  assert.equal(body.entries[0].label, 'Lekarze');
  assert.equal(body.entries[0].memberCount, 12);
  assert.equal(body.entries[1].subjectId, 'asystent');
  assert.equal(body.entries[1].memberCount, null);
});

test('a directory answer without the optional fields decodes as complete and sizeless', { skip }, () => {
  const body = decodeBody({
    BusBody: {
      instance_id: INSTANCE,
      payload: { SubjectDirectoryResponse: { entries: [{ subject_type: 'user', subject_id: 'u-1', label: 'Anna Kowalska' }] } },
    },
  });
  assert.equal(body.truncated, false);
  assert.equal(body.entries[0].memberCount, null);
});

test('the preview answer decodes the record, the per-field actions and the caller limit', { skip }, () => {
  const payload = new TextEncoder().encode('{"id":"b-1"}');
  const body = decodeBody({
    BusBody: {
      instance_id: INSTANCE,
      payload: {
        FieldPolicyPreviewResponse: {
          record: {
            partition: 1, offset: 41, timestamp_ms: 1000, key: new Uint8Array(), headers: [],
            payload_preview: payload, is_blob_ref: false, truncated: false,
          },
          applied: [{ field: 'id', action: 'show' }, { field: 'pesel', action: 'hide' }],
          limited_by_caller: true,
        },
      },
    },
  });
  assert.equal(body.variant, 'BusFieldPolicyPreviewResponse');
  assert.equal(body.record.offset, 41);
  assert.equal(new TextDecoder().decode(body.record.payloadPreview), '{"id":"b-1"}');
  assert.deepEqual(body.applied.map((a) => [a.field, a.action]), [['id', 'show'], ['pesel', 'hide']]);
  assert.equal(body.limitedByCaller, true);

  const older = decodeBody({
    BusBody: {
      instance_id: INSTANCE,
      payload: {
        FieldPolicyPreviewResponse: {
          record: {
            partition: 0, offset: 0, timestamp_ms: 1, headers: [],
            payload_preview: payload, is_blob_ref: false, truncated: false,
          },
          applied: [],
        },
      },
    },
  });
  assert.equal(older.limitedByCaller, false);
});

test('an addon row of the access list keeps its own subject type and label', { skip }, () => {
  const body = decodeBody({
    BusBody: {
      instance_id: INSTANCE,
      payload: {
        AclListResponse: {
          entries: [{
            subject_type: 'addon', subject_id: 'asystent', access_level: 'deny', action: 'read',
            subject_label: 'Asystent lekarza', member_count: null,
          }],
        },
      },
    },
  });
  assert.equal(body.entries[0].subjectType, 'addon');
  assert.equal(body.entries[0].subjectLabel, 'Asystent lekarza');
  assert.equal(body.entries[0].memberCount, null);
});

test('an access entry request names one right of one subject', { skip }, () => {
  const sent = sentEnvelope('busAclSetRequest', { instanceId: INSTANCE, topic: 'wyniki', subjectType: 'addon', subjectId: 'asystent', accessLevel: 'deny', action: 'write' });
  assert.deepEqual(sent.payload, { AclSetRequest: { topic: 'wyniki', subject_type: 'addon', subject_id: 'asystent', access_level: 'deny', action: 'write' } });
});

test('the capabilities answer carries the caller\'s organisation, empty when an older server sends none', { skip }, () => {
  const base = { can_read: true, can_write: true, can_admin: true, is_site_admin: true, default_replication_factor: 1, node_count: 1, content_types: [], schema_types: [], field_actions: [] };
  const body = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { CapabilitiesResponse: { capabilities: { ...base, org_id: 'org-default', org_name: 'Przychodnia Zdrowie' } } } } });
  assert.equal(body.capabilities.orgId, 'org-default');
  assert.equal(body.capabilities.orgName, 'Przychodnia Zdrowie');
  const older = decodeBody({ BusBody: { instance_id: INSTANCE, payload: { CapabilitiesResponse: { capabilities: base } } } });
  assert.equal(older.capabilities.orgId, '');
  assert.equal(older.capabilities.orgName, null);
});
