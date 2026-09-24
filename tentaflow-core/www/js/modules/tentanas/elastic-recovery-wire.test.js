// =============================================================================
// File: modules/tentanas/elastic-recovery-wire.test.js
// Description: The recovery wire of an Elastic Array (helper 0.14.0) as a
//       screen really receives and sends it. The node answers
//       `ElasticArrayGetRequest` with a `NasElasticArray` carrying
//       `attention`, `sync_needs_acknowledgement` and `pending_add_disk`;
//       `BinaryWsClient` decodes the frame with the real wasm glue. A decoder
//       built before those fields existed drops them silently (they are
//       `#[serde(default)]`), and the array screen then offers neither the
//       confirm of a Sync over a fault nor "Dokończ dodawanie dysku" while
//       every stubbed test stays green ("a TentaNas request needs four
//       layers"). The other way round, the Sync's acknowledgement and the
//       undo request have to leave the browser in the frame the client sends.
//
//       The reply frame is hand-built CBOR in the shape tentaflow-protocol
//       writes, as in alert-wire.test.js; its own file for the reason
//       refusal-wire.test.js gives: the wasm has to be initialised BEFORE
//       codec.js is first imported.
// =============================================================================

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const wasmUrl = new URL('../../protocol/wasm_glue_bg.wasm', import.meta.url);
const skip = existsSync(wasmUrl) ? false : 'wasm_glue_bg.wasm is absent — build tentaflow-core once to generate the glue';

const wasm = skip ? null : await import('../../protocol/wasm_glue.js');
let codec = null;
let BinaryWsClient = null;
if (!skip) {
  await wasm.default({ module_or_path: readFileSync(wasmUrl) });
  codec = await import('../../protocol/codec.js');
  await codec.codecReady;
  ({ BinaryWsClient } = await import('../../protocol/binary-ws-client.js'));
}
const { fakeScreen, flush } = await import('./_test-setup.js');
const { drawElasticDetail } = await import('./elastic-detail.js');

// See refusal-wire.test.js: the app's own transport starts retrying a socket
// that is not there once the codec is ready; it is closed so the run ends.
after(async () => {
  if (skip) return;
  const shim = await import('../../protocol/api-binary-shim.js');
  (await shim.initTransport())?.close();
});

function cborHead(major, value) {
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
  if (value === null) return [0xf6];
  if (value === true) return [0xf5];
  if (value === false) return [0xf4];
  if (value instanceof Uint8Array) return [...cborHead(2, value.length), ...value];
  if (Array.isArray(value)) return value.reduce((acc, v) => acc.concat(cbor(v)), cborHead(4, value.length));
  if (typeof value === 'number' || typeof value === 'bigint') return cborHead(0, value);
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value);
    return [...cborHead(3, bytes.length), ...bytes];
  }
  const entries = Object.entries(value);
  return entries.reduce((acc, [k, v]) => acc.concat(cbor(k), cbor(v)), cborHead(5, entries.length));
}

// `NasElasticArray` as serde writes it (snake_case keys). Every field the
// struct requires is present; the three recovery fields are the point.
const ARRAY = {
  name: 'media',
  kind: 'elastic-array',
  state: 'needs_attention',
  state_detail: 'krok dodawania dysku nie powiódł się',
  health: 'warning',
  health_reason: '',
  enabled: true,
  union_path: '/mnt/media',
  create_policy: 'mfs',
  filesystem: 'xfs',
  data_disks: [],
  cache_disks: [],
  parity_disks: [],
  folders: [],
  folders_known: true,
  mover: { enabled: false, schedule: null, min_age_secs: 7200, cache_min_free_pct: 20, coupled_sync: true, last_run: null },
  snapraid: {
    installed: true, version: '13.0', config_path: '/etc/tentanas/snapraid-media.conf', last_sync: null, last_scrub: null,
    history: [], sync_schedule: null, scrub_schedule: null, scrub_percent: 8, scrub_older_than_days: 10,
    parity_errors: null, parity_errors_window_days: 30,
  },
  protection: { cache_unprotected_bytes: null, moved_unsynced_bytes: null, status: 'unknown', detail: '', fault_tolerance: null, protected_as_of: null },
  usable_bytes: null,
  used_bytes: null,
  cache_size_bytes: null,
  cache_used_bytes: null,
  unresolved_operation: true,
  mover_settles_unresolved: false,
  parity_run_available: false,
  attention: 'add_disk',
  sync_needs_acknowledgement: true,
  sync_fault_id: '018f2c1e-6b9a-7c3d-8e4f-5a6b7c8d9e0f',
  pending_add_disk: {
    disk_id: 'wwn-0x5000c500a1b2c3d4',
    disk_name: 'sdh',
    disk_last_name: '',
    step: 'mount',
    in_union: false,
    undo_possible: true,
  },
  created_at: '2026-09-24T00:00:00Z',
  updated_at: '2026-09-24T00:00:00Z',
};

// A client with no socket: `_send` is the one seam to the transport and
// `_handleBytes` is where every inbound frame lands. `sent` keeps the frames
// the client really encoded.
function clientAnswering(bodyFor) {
  const client = new BinaryWsClient('ws://node.invalid/ws/api');
  client.nextCorrelationId = codec.makeCorrelationIdGenerator();
  const sent = [];
  client._send = (frame) => {
    sent.push(frame);
    const correlationId = BigInt(wasm.decodeEnvelope(frame).correlation_id);
    const body = Uint8Array.from(cbor(bodyFor()));
    const reply = Uint8Array.from(cbor({
      schema_version: codec.schemaVersion(),
      correlation_id: correlationId,
      sequence: 1,
      message_kind: codec.messageKind().META_HEARTBEAT,
      flags: 0,
      routing: 'Direct',
      forwarded_session_claim: null,
      body,
    }));
    queueMicrotask(() => client._handleBytes(reply));
  };
  return { client, sent };
}

// The payload of a frame the client sent, as the node's decoder reads it.
function sentPayload(frame) {
  const envelope = wasm.decodeEnvelope(frame);
  try {
    return wasm.decodeMessageBody(envelope.body);
  } finally {
    envelope.free();
  }
}

test('the pending add, the cause and the confirm flag survive the real decoder and reach the screen', { skip }, async () => {
  const { client } = clientAnswering(() => ({ TentaNasBody: { ElasticArrayGetResponse: { array: ARRAY } } }));
  const { body } = await client.request('tentaNasElasticArrayGetRequest', { name: 'media' });
  const array = body.array;
  assert.equal(array.attention, 'add_disk');
  assert.equal(array.syncNeedsAcknowledgement, true);
  assert.equal(array.syncFaultId, '018f2c1e-6b9a-7c3d-8e4f-5a6b7c8d9e0f');
  assert.equal(array.pendingAddDisk.diskId, 'wwn-0x5000c500a1b2c3d4');
  assert.equal(array.pendingAddDisk.diskName, 'sdh');
  assert.equal(array.pendingAddDisk.step, 'mount');
  assert.equal(array.pendingAddDisk.inUnion, false);
  assert.equal(array.pendingAddDisk.undoPossible, true);

  // And the screen offers what arrived: the finish and the undo, the cause as
  // a sentence, no id.
  const screen = fakeScreen({ tentaNasElasticArrayGetRequest: { array } });
  screen.array = 'media';
  screen.openArray = () => {};
  const host = document.createElement('div');
  document.body.appendChild(host);
  try {
    await drawElasticDetail(screen, host);
    await flush();
    assert.equal(host.querySelector('[data-act="add-disk-resume"]').getAttribute('label'), 'Dokończ dodawanie dysku sdh');
    assert.ok(host.querySelector('[data-act="add-disk-undo"]'));
    assert.match(host.querySelector('[data-f="state-detail"]').textContent, /Dodawanie dysku do macierzy nie zostało dokończone/);
    assert.doesNotMatch(host.innerHTML, /wwn-0x5000c500a1b2c3d4|018f2c1e/);
  } finally {
    screen.dispose();
  }
});

test('the acknowledgement and the undo leave the browser in the frame the client sends', { skip }, async () => {
  const { client, sent } = clientAnswering(() => ({
    TentaNasBody: { JobResponse: { job: { job_id: 'j-1', kind: 'elastic_sync', subject: 'media', status: 'running', progress_pct: null, started_by: 'u', started_at: '2026-09-24T00:00:00Z', finished_at: null, log: [], error: null } } },
  }));
  await client.request('tentaNasElasticArraySyncRequest', { name: 'media', acknowledgeParityFault: '018f2c1e-6b9a-7c3d-8e4f-5a6b7c8d9e0f', sudoPassword: 'test-secret-not-real' });
  await client.request('tentaNasElasticArraySyncRequest', { name: 'media' });
  await client.request('tentaNasElasticArrayAddDiskAbortRequest', { name: 'media', diskId: 'wwn-0x5000c500a1b2c3d4', confirmName: 'media' });
  const [acknowledged, plain, undo] = sent.map(sentPayload);
  assert.equal(acknowledged.variant, 'TentaNasElasticArraySyncRequest');
  assert.equal(acknowledged.acknowledge_parity_fault, '018f2c1e-6b9a-7c3d-8e4f-5a6b7c8d9e0f', 'the confirm names its fault');
  assert.equal(plain.acknowledge_parity_fault ?? null, null, 'a Sync that says nothing is not acknowledged');
  assert.equal(undo.variant, 'TentaNasElasticArrayAddDiskAbortRequest');
  assert.equal(undo.disk_id, 'wwn-0x5000c500a1b2c3d4');
  assert.equal(undo.confirm_name, 'media');
});
