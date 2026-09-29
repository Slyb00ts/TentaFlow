// =============================================================================
// File: protocol/wave14-wire.test.js
// Description: The wire of TentaNas wave 14 (the allowlist flag and
//       "Otwórz dla wszystkich") through the REAL glue
//       (www/js/protocol/wasm_glue*):
//       - `NasTarget.allowlist_mode` and `TargetGetResponse.open_blocked`
//         decode, and default to false from an older node;
//       - "Otwórz dla wszystkich": `codec.encode.tentaNasTargetOpenRequest`
//         → the wasm encoder → `TentaNasBody(TargetOpenRequest)`, with the
//         retyped name and the window's `updated_at`, nothing else.
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

// ----- a minimal CBOR writer: what serde + ciborium write -------------------

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
  if (Array.isArray(value)) return value.reduce((acc, v) => acc.concat(cbor(v)), head(4, value.length));
  if (typeof value === 'number') return head(0, value);
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value);
    return [...head(3, bytes.length), ...bytes];
  }
  const entries = Object.entries(value);
  return entries.reduce((acc, [k, v]) => acc.concat(cbor(k), cbor(v)), head(5, entries.length));
}

const decodeBody = (body) => wasm.decodeMessageBody(new Uint8Array(cbor(body)));

function request(kind, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[kind](91, payload, 3));
  try {
    return wasm.decodeMessageBody(envelope.body);
  } finally {
    envelope.free();
  }
}

// ----- the target detail answer ------------------------------------------------

const TARGET = {
  target_id: 't1', name: 'vm-store', protocol: 'iscsi', wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store', enabled: true,
  luns: [], portals: [{ interface: 'storage0', address: '10.10.0.5', port: 3260, transport: 'tcp' }],
  auth: { method: 'none', username: '', secret: null, mutual_username: '', mutual_secret: null, secret_set: false, mutual_secret_set: false, dhchap_hash: '', dhchap_dhgroup: '' },
  initiators: [], port_groups: [], sessions: 0, sessions_known: true, state: 'active', state_detail: '', created_at: '', updated_at: '', state_reasons: [],
};

test('a closed target and a blocked one decode their wave-14 flags', { skip }, () => {
  const body = decodeBody({
    TentaNasBody: { TargetGetResponse: { target: { ...TARGET, allowlist_mode: true }, sessions: [], config_preview: '', open_blocked: true } },
  });
  assert.equal(body.variant, 'TentaNasTargetGetResponse');
  assert.equal(body.target.allowlistMode, true);
  assert.equal(body.openBlocked, true);
});

test('an older node\'s answer reads as not allowlisted by flag and not blocked', { skip }, () => {
  const body = decodeBody({ TentaNasBody: { TargetGetResponse: { target: TARGET, sessions: [], config_preview: '' } } });
  assert.equal(body.target.allowlistMode, false);
  assert.equal(body.openBlocked, false);
});

// ----- "Otwórz dla wszystkich" --------------------------------------------------

test('TargetOpenRequest crosses the real glue with the retyped name and the window reading, nothing else', { skip }, () => {
  const sent = request('tentaNasTargetOpenRequest', { targetId: 't1', confirmName: 'vm-store', expectedUpdatedAt: '2026-09-29T10:00:00Z', sudoPassword: 'sekret' });
  assert.equal(sent.variant, 'TentaNasTargetOpenRequest');
  assert.equal(sent.targetId, 't1');
  assert.equal(sent.confirmName, 'vm-store');
  assert.equal(sent.expectedUpdatedAt, '2026-09-29T10:00:00Z');
  const fields = Object.keys(sent).filter((k) => k !== 'variant' && !/[A-Z]/.test(k)).sort();
  assert.deepEqual(fields, ['confirm_name', 'expected_updated_at', 'sudo_password', 'target_id']);
  const plain = request('tentaNasTargetOpenRequest', { targetId: 't1', confirmName: 'vm-store' });
  assert.equal(plain.sudoPassword, null);
  assert.equal(plain.expectedUpdatedAt, '', 'an older window sends no reading: no check');
});
