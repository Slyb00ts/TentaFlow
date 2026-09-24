// =============================================================================
// File: modules/tentanas/alert-wire.test.js
// Description: A coded alert as a screen really receives it (wave-4 critic
//       B2 / round-2 M-A). The node answers `AlertsListRequest` with a
//       `TentaNasBody::AlertsListResponse` whose `NasAlert`s carry `code`,
//       `params` and `reasons`; `BinaryWsClient` decodes the frame with the
//       real wasm glue. A decoder built before those fields existed drops
//       them silently (they are `#[serde(default)]`), and every n01/n02 alert
//       then reads "Alert węzła" while every stubbed test stays green.
//
//       The reply frame is hand-built CBOR in the shape tentaflow-protocol
//       writes (`Envelope` around `MessageBody::TentaNasBody`), so the test
//       holds the glue in www/js/protocol to the protocol, not to a copy of
//       its output. Its own file for the reason refusal-wire.test.js gives:
//       the wasm has to be initialised BEFORE codec.js is first imported.
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
await import('./_test-setup.js');
const { alertText } = await import('./format.js');

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

// `NasAlert` field for field, as serde writes it (snake_case keys, every
// parameter a string).
const DISK_ALERT = {
  alert_id: 'a-disk',
  severity: 'warning',
  subject_kind: 'disk',
  subject_id: 'd-1',
  title: 'Disk sdd: warning',
  detail: 'reallocated sectors grew from 5 to 8 in 7 days',
  raised_at: '2026-09-24T00:00:00Z',
  acked_at: null,
  resolved_at: null,
  code: 'disk_health',
  params: { health: 'warning', name: 'sdd', name_source: 'live' },
  reasons: [{ code: 'reallocated_growing', params: { from: '5', to: '8' } }],
};

const APPROVAL_ALERT = {
  alert_id: 'a-approval',
  severity: 'info',
  subject_kind: 'approval',
  subject_id: 'r-1',
  title: "a red-path operation on 'helios' waits for a second admin",
  detail: 'approve or reject it in the task queue',
  raised_at: '2026-09-24T00:00:00Z',
  acked_at: null,
  resolved_at: null,
  code: 'approval_pending',
  params: { operation: 'pool_destroy', subject: 'tank' },
  reasons: [],
};

// Sends a real `AlertsListRequest` through a client with no socket and
// answers it with the list, the way `refusal-wire.test.js` answers a refusal.
async function alertsThroughTheClient(alerts) {
  const client = new BinaryWsClient('ws://node.invalid/ws/api');
  client.nextCorrelationId = codec.makeCorrelationIdGenerator();
  client._send = (frame) => {
    const correlationId = BigInt(wasm.decodeEnvelope(frame).correlation_id);
    const body = Uint8Array.from(cbor({ TentaNasBody: { AlertsListResponse: { alerts } } }));
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
  const { body } = await client.request('tentaNasAlertsListRequest', { includeAcked: false });
  return body.alerts;
}

test('a coded alert keeps its code, params and reasons through the real decoder', { skip }, async () => {
  const [disk, approval] = await alertsThroughTheClient([DISK_ALERT, APPROVAL_ALERT]);

  assert.equal(disk.code, 'disk_health');
  assert.equal(disk.params.name_source, 'live');
  assert.equal(disk.params.name, 'sdd');
  assert.equal(disk.reasons.length, 1);
  assert.equal(disk.reasons[0].code, 'reallocated_growing');
  assert.deepEqual({ ...disk.reasons[0].params }, { from: '5', to: '8' });
  assert.equal(approval.code, 'approval_pending');
  assert.equal(approval.params.subject, 'tank');

  // And the screen words what arrived, not the generic "Alert węzła".
  const diskText = alertText(disk);
  assert.equal(diskText.known, true);
  assert.equal(diskText.title, 'sdd: 3 nowe realokowane sektory w 7 dni');
  const approvalText = alertText(approval);
  assert.equal(approvalText.known, true);
  assert.match(approvalText.title, /„tank” czeka na drugiego administratora$/);
});
