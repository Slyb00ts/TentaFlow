// =============================================================================
// File: modules/tentanas/fleet-wire.test.js
// Description: The wave-7 fleet row as a screen really receives it. A fleet
//       row gained `armed_until` (MAJOR 17), and `shares_total` now counts
//       block targets too (M7) — the first is `#[serde(default)]`, so a
//       decoder built before it drops it silently and every mode-B node then
//       reads "not armed" while every stubbed test stays green. So the reply
//       goes through the real glue: a hand-built CBOR frame in the shape
//       tentaflow-protocol writes, decoded by `BinaryWsClient` with the wasm
//       in www/js/protocol, and handed to the function the screens judge the
//       channel with. Its own file for the reason alert-wire.test.js gives:
//       the wasm is initialised BEFORE codec.js is first imported.
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
const { nodeChannelMode } = await import('./format.js');

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

// Sends `request` through a client with no socket and answers it with
// `TentaNasBody::<variant>`, the way alert-wire.test.js does.
async function throughTheClient(request, payload, variant, fields) {
  const client = new BinaryWsClient('ws://node.invalid/ws/api');
  client.nextCorrelationId = codec.makeCorrelationIdGenerator();
  client._send = (frame) => {
    const correlationId = BigInt(wasm.decodeEnvelope(frame).correlation_id);
    const body = Uint8Array.from(cbor({ TentaNasBody: { [variant]: fields } }));
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
  const { body } = await client.request(request, payload);
  return body;
}


const ARMED_UNTIL = '2099-01-01T00:00:00Z';
const nodeRow = (extra) => ({
  node_id: 'n-1', node_name: 'vega', is_local: false, online: true, instance_status: 'ready', health: 'ok',
  os_name: 'Debian 13', zfs_version: '2.3.1', elevation_mode: 'interactive', disks_total: 8, disks_warning: 0,
  pools_total: 1, shares_total: 0, alerts_active: 0, capacity_bytes: 32_000_000_000_000, used_bytes: 14_000_000_000_000,
  updated_at: '2026-09-25T10:00:00Z', ...extra,
});

test('a fleet row keeps armed_until through the real decoder, and the channel is judged by it', { skip }, async () => {
  const body = await throughTheClient('tentaNasNodesListRequest', {}, 'NodesListResponse', {
    local_node_id: 'n-0',
    nodes: [
      nodeRow({ armed_until: ARMED_UNTIL }),
      nodeRow({ node_id: 'n-2', node_name: 'atlas', armed_until: null }),
      // A row from a node too old to send the field.
      nodeRow({ node_id: 'n-3', node_name: 'orion' }),
    ],
  });
  const [armed, expired, old] = body.nodes;
  assert.equal(armed.armedUntil, ARMED_UNTIL, 'the field survives the decoder');
  assert.equal(nodeChannelMode(armed), 'interactive');
  assert.equal(nodeChannelMode(expired), 'interactive_unarmed');
  assert.equal(nodeChannelMode(old), 'interactive_unarmed', 'no field is no promise of a held password');
  assert.equal(armed.capacityBytes, 32_000_000_000_000);
});

// Critic wave 7, MINOR 11: the node counts the seconds left by its own clock,
// and the screen adds them to the moment the row arrived — a browser clock
// hours off can neither expire an armed node nor keep an expired one armed.
test('a fleet row keeps armed_secs_left through the real decoder, and it outranks the browser clock', { skip }, async () => {
  const body = await throughTheClient('tentaNasNodesListRequest', {}, 'NodesListResponse', {
    local_node_id: 'n-0',
    nodes: [
      // By this browser's clock the instant is long past; by the node's own
      // clock ten minutes are left.
      nodeRow({ armed_until: '2000-01-01T00:10:00Z', armed_secs_left: 600 }),
      // The instant is far ahead by this browser's clock; the node says it is over.
      nodeRow({ node_id: 'n-2', node_name: 'atlas', armed_until: ARMED_UNTIL, armed_secs_left: 0 }),
    ],
  });
  const [armed, expired] = body.nodes;
  assert.equal(Number(armed.armedSecsLeft), 600, 'the field survives the decoder');
  const receivedAt = Date.now();
  assert.equal(nodeChannelMode({ ...armed, receivedAt }), 'interactive');
  assert.equal(nodeChannelMode({ ...armed, receivedAt }, receivedAt + 601_000), 'interactive_unarmed', 'and it ends when the node said it would');
  assert.equal(nodeChannelMode({ ...expired, receivedAt }), 'interactive_unarmed');
});

// MAJOR 2 (wave 7): the fleet asks for the LIGHT target list. The flag must
// survive codec.js and the wasm encoder, or every fleet tick silently pays for
// the full Sharing-tab answer again.
test('the fleet\'s summary flag reaches the node through the real encoder', { skip }, async () => {
  const decodeRequest = (payload) => {
    const frame = codec.encode.tentaNasTargetsListRequest(1n, payload);
    return wasm.decodeMessageBody(wasm.decodeEnvelope(frame).body);
  };
  const light = decodeRequest({ summary: true });
  assert.equal(light.variant, 'TentaNasTargetsListRequest');
  assert.equal(light.summary, true);
  assert.equal(decodeRequest({}).summary, false, 'the Sharing tab keeps the full answer');
});

// Owner decision (wave 7): `zpool detach` of the disk a hot spare replaced.
// The request must reach the node with the pool and the leaf's name.
test('the detach request survives the real encoder', { skip }, async () => {
  const frame = codec.encode.tentaNasPoolDetachRequest(1n, { name: 'tank', device: 'sdb', sudoPassword: 'x' });
  const sent = wasm.decodeMessageBody(wasm.decodeEnvelope(frame).body);
  assert.equal(sent.variant, 'TentaNasPoolDetachRequest');
  assert.deepEqual([sent.name, sent.device], ['tank', 'sdb']);
});
