// =============================================================================
// File: modules/tentanas/refusal-wire.test.js
// Description: A refusal code as a screen really catches it (wave-4 critic
//       B1). The node answers a TentaNas request with an IS_ERROR frame whose
//       message is `refusal:<code>`; `BinaryWsClient` decodes it with the real
//       wasm glue and rejects the pending request with its own
//       `protocol error <Code>: <message>` Error. `errMessage` must word the
//       code through that wrapping.
//
//       The reply frame is hand-built CBOR in the shape tentaflow-protocol
//       writes (`Envelope` with the IS_ERROR flag around `MessageBody::Error`),
//       so the test proves the client's wrapping rather than a string copied
//       from its source. Its own file because the wasm has to be initialised
//       BEFORE codec.js is first imported: codec.js starts its init at import
//       time, and under the shared test setup that init fails for good.
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
const { errMessage } = await import('./format.js');

// With the codec ready, the shared setup's `I18n.setLanguage('pl')` makes the
// app's own transport try a socket that is not there and keep retrying it.
// That client is not under test; it is closed so the run can end.
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
  if (typeof value === 'number' || typeof value === 'bigint') return cborHead(0, value);
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value);
    return [...cborHead(3, bytes.length), ...bytes];
  }
  const entries = Object.entries(value);
  return entries.reduce((acc, [k, v]) => acc.concat(cbor(k), cbor(v)), cborHead(5, entries.length));
}

// Sends one real request through a client with no socket and answers it the
// way the node refuses: `_send` is the one seam between the client and the
// transport, and `_handleBytes` is where every inbound frame lands.
async function refusedThroughTheClient(code, message) {
  const client = new BinaryWsClient('ws://node.invalid/ws/api');
  client.nextCorrelationId = codec.makeCorrelationIdGenerator();
  client._send = (frame) => {
    // A u64 well above 2^53: kept a BigInt, or the reply matches nothing.
    const correlationId = BigInt(wasm.decodeEnvelope(frame).correlation_id);
    const body = Uint8Array.from(cbor({ Error: { code, message, trace_id: null } }));
    const reply = Uint8Array.from(cbor({
      schema_version: codec.schemaVersion(),
      correlation_id: correlationId,
      sequence: 1,
      message_kind: 0xF002,
      flags: 1,
      routing: 'Direct',
      forwarded_session_claim: null,
      body,
    }));
    queueMicrotask(() => client._handleBytes(reply));
  };
  try {
    await client.request('tentaNasApprovalDecideRequest', { requestId: 'r-1', approve: true, note: '' });
  } catch (e) {
    return e;
  }
  throw new Error('the request was not refused');
}

test('a refusal wrapped by the websocket client is still worded', { skip }, async () => {
  const error = await refusedThroughTheClient('PolicyDenied', 'refusal:approval_own_request');
  // The premise: this is the wrapping a screen really catches.
  assert.equal(error.message, 'protocol error PolicyDenied: refusal:approval_own_request');
  assert.equal(errMessage(error), 'Autor zgłoszenia nie może go zatwierdzić — potrzebny jest drugi administrator');

  const inUse = await refusedThroughTheClient('Conflict', 'refusal:share_user_in_use_elsewhere');
  assert.doesNotMatch(errMessage(inUse), /refusal:|protocol error/, errMessage(inUse));

  // A code this build has no words for is shown as the client reported it.
  const unknown = await refusedThroughTheClient('PolicyDenied', 'refusal:quota_exceeded');
  assert.equal(errMessage(unknown), 'protocol error PolicyDenied: refusal:quota_exceeded');
});
