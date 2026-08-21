// =============================================================================
// File: protocol/events-codec.test.js
// Description: The Zdarzenia screen's half of the wire, pinned at the codec.
//       `MessageBody::EventsBody` is carried by ciborium's EXTERNALLY TAGGED
//       encoding, which tags an enum by variant NAME — so a rename on either
//       side silently stops matching while both halves still compile. The two
//       legs are proved here against the real wasm glue rather than a mock:
//       a BrowseRequest is encoded and the literal names are looked for in the
//       bytes, and a BrowseResponse is built as the SERVER would emit it and
//       decoded into the object shape modules/events.js reads.
//
//       The response bytes are hand-built CBOR on purpose: taking them from the
//       encoder would prove only that the codec agrees with itself, and the
//       question is whether it agrees with what tentaflow-protocol writes.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));

// wasm_glue_bg.wasm is gitignored — tentaflow-core/build.rs produces it. Rather
// than fail every `npm test` on a tree that has not been built yet, the suite
// names the missing artifact and skips; the reporter shows the reason, so a
// skipped run is never mistaken for a passing one.
const wasmPath = join(here, 'wasm_glue_bg.wasm');
const skip = existsSync(wasmPath)
  ? false
  : `${wasmPath} is absent — build tentaflow-core once to generate the glue`;

const wasm = skip ? null : await import('./wasm_glue.js');
if (!skip) await wasm.default({ module_or_path: readFileSync(wasmPath) });

// -----------------------------------------------------------------------------
// Minimal CBOR writer — enough for the shapes serde emits for these structs.
// -----------------------------------------------------------------------------

function head(major, value) {
  const n = BigInt(value);
  if (n < 24n) return [Number((BigInt(major) << 5n) | n)];
  const base = major << 5;
  if (n < 0x100n) return [base | 24, Number(n)];
  if (n < 0x10000n) return [base | 25, Number(n >> 8n) & 0xff, Number(n) & 0xff];
  if (n < 0x100000000n) {
    return [base | 26, Number((n >> 24n) & 0xffn), Number((n >> 16n) & 0xffn),
      Number((n >> 8n) & 0xffn), Number(n & 0xffn)];
  }
  const out = [base | 27];
  for (let shift = 56n; shift >= 0n; shift -= 8n) out.push(Number((n >> shift) & 0xffn));
  return out;
}

function enc(value) {
  if (value === null) return [0xf6];
  if (value === true) return [0xf5];
  if (value === false) return [0xf4];
  if (typeof value === 'number' || typeof value === 'bigint') {
    const n = BigInt(value);
    return n < 0n ? head(1, -n - 1n) : head(0, n);
  }
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value);
    return head(3, bytes.length).concat([...bytes]);
  }
  if (Array.isArray(value)) {
    return value.reduce((acc, item) => acc.concat(enc(item)), head(4, value.length));
  }
  const entries = Object.entries(value);
  return entries.reduce((acc, [k, v]) => acc.concat(enc(k), enc(v)), head(5, entries.length));
}

const ROW = {
  run_id: 'run-1',
  seq: 3,
  at_ms: 1_760_000_000_000n,
  kind: 'tool_call',
  origin: 'code_studio',
  actor_kind: 'api_key',
  actor_id: 'key-42',
  actor_user_id: null,
  org_id: 'org-a',
  correlation_id: 'corr-9',
  session_id: 'sess-7',
  node_id: 'llm-1',
  call_id: 'c-a70',
  payload_json: '{"kind":"tool_call","name":"core.fs_read"}',
};

test('BrowseRequest carries the variant names and every filter field', { skip }, () => {
  const bytes = wasm.encodeEventsBrowseRequest(JSON.stringify({
    origins: ['code_studio', 'api'],
    actor_id: 'key-42',
    org_id: null,
    session_id: null,
    correlation_id: 'corr-9',
    from_ms: 1_760_000_000_000,
    to_ms: null,
    search: 'fs_read',
    cursor: { at_ms: 5, run_id: 'run-1', seq: 3 },
    limit: 200,
  }));
  const text = Buffer.from(bytes).toString('latin1');
  assert.ok(text.includes('EventsBody'), 'outer variant name must be on the wire');
  assert.ok(text.includes('BrowseRequest'), 'inner variant name must be on the wire');

  // The decoder's *Request arm names the variant back, which is the only place
  // the encode and decode legs of this family meet inside the codec.
  const echoed = wasm.decodeMessageBody(bytes);
  assert.equal(echoed.variant, 'EventsBrowseRequest');
});

test('an empty origin list is not the same wire value as no origin filter', { skip }, () => {
  const none = Buffer.from(wasm.encodeEventsBrowseRequest(
    JSON.stringify({ origins: null, limit: 1 }),
  )).toString('latin1');
  const empty = Buffer.from(wasm.encodeEventsBrowseRequest(
    JSON.stringify({ origins: [], limit: 1 }),
  )).toString('latin1');
  assert.notEqual(none, empty, 'every chip off must not encode as no constraint');
});

test('RunRequest names its own variant', { skip }, () => {
  const bytes = wasm.encodeEventsRunRequest('run-1', 2, 100);
  const text = Buffer.from(bytes).toString('latin1');
  assert.ok(text.includes('EventsBody'));
  assert.ok(text.includes('RunRequest'));
  assert.equal(wasm.decodeMessageBody(bytes).variant, 'EventsRunRequest');
});

test('a server BrowseResponse decodes into the shape events.js reads', { skip }, () => {
  const bytes = Uint8Array.from(enc({
    EventsBody: {
      BrowseResponse: {
        rows: [ROW],
        next_cursor: { at_ms: 1_760_000_000_000n, run_id: 'run-1', seq: 3 },
        scoped_to_self: true,
      },
    },
  }));
  const body = wasm.decodeMessageBody(bytes);

  assert.equal(body.variant, 'EventsBrowseResponse');
  assert.equal(body.rows.length, 1);
  assert.equal(body.scopedToSelf, true);
  assert.equal(body.scoped_to_self, true);

  const row = body.rows[0];
  assert.equal(row.runId, 'run-1');
  assert.equal(row.run_id, 'run-1');
  assert.equal(row.seq, 3);
  assert.equal(row.atMs, 1_760_000_000_000);
  assert.equal(row.kind, 'tool_call');
  assert.equal(row.origin, 'code_studio');
  assert.equal(row.actorKind, 'api_key');
  assert.equal(row.actorId, 'key-42');
  // A field the writer had no value for must arrive as an explicit absence,
  // never as an empty string the UI would render as an unbound actor's id.
  assert.equal(row.actorUserId, null);
  assert.equal(row.orgId, 'org-a');
  assert.equal(row.correlationId, 'corr-9');
  assert.equal(row.sessionId, 'sess-7');
  assert.equal(row.nodeId, 'llm-1');
  assert.equal(row.callId, 'c-a70');
  assert.equal(row.payloadJson, ROW.payload_json);

  // The cursor comes back in the spelling the encoder reads, so a second page
  // can be asked for with the object the first page returned.
  assert.equal(body.nextCursor.runId, 'run-1');
  assert.equal(body.nextCursor.at_ms, 1_760_000_000_000);
  const next = wasm.encodeEventsBrowseRequest(JSON.stringify({
    cursor: {
      at_ms: body.nextCursor.at_ms,
      run_id: body.nextCursor.run_id,
      seq: body.nextCursor.seq,
    },
    limit: 200,
  }));
  assert.ok(Buffer.from(next).toString('latin1').includes('run-1'));
});

test('a BrowseResponse without a next cursor ends the paging', { skip }, () => {
  const bytes = Uint8Array.from(enc({
    EventsBody: {
      BrowseResponse: { rows: [], next_cursor: null, scoped_to_self: false },
    },
  }));
  const body = wasm.decodeMessageBody(bytes);
  assert.equal(body.variant, 'EventsBrowseResponse');
  assert.deepEqual([...body.rows], []);
  assert.equal(body.nextCursor, null);
  assert.equal(body.scopedToSelf, false);
});

test('a RunResponse decodes with its timeline and paging seq', { skip }, () => {
  const bytes = Uint8Array.from(enc({
    EventsBody: {
      RunResponse: { run_id: 'run-1', events: [ROW], next_after_seq: 4 },
    },
  }));
  const body = wasm.decodeMessageBody(bytes);
  assert.equal(body.variant, 'EventsRunResponse');
  assert.equal(body.runId, 'run-1');
  assert.equal(body.events.length, 1);
  assert.equal(body.events[0].callId, 'c-a70');
  assert.equal(body.nextAfterSeq, 4);
});

test("the screen's own call resolves through the codec facade", { skip }, async () => {
  // The wasm module above is already initialised, so codec.js's own init()
  // returns it instead of fetching — the facade is exercised, not stubbed.
  const codec = await import('./codec.js');
  await codec.codecReady;

  // The name the Zdarzenia module passes to ApiBinary.one. A missing entry is
  // exactly the failure this test exists to catch: the shim reports it as
  // "unknown request kind", far from the codec that lacks the encoder.
  assert.equal(typeof codec.encode.eventsBrowseRequest, 'function');
  assert.equal(typeof codec.encode.eventsRunRequest, 'function');

  // modules/events.js::buildRequest, camelCase and all.
  const frame = codec.encode.eventsBrowseRequest(7, {
    origins: ['chat', 'code_studio'],
    actorId: null,
    fromMs: 1_760_000_000_000,
    search: null,
    cursor: null,
    limit: 200,
  }, 3);
  const env = wasm.decodeEnvelope(frame);
  assert.equal(env.correlation_id, 7n);
  assert.equal(wasm.decodeMessageBody(env.body).variant, 'EventsBrowseRequest');
});
