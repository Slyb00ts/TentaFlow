// =============================================================================
// File: protocol/wave12-wire.test.js
// Description: The wire fields and requests of TentaNas wave 12 (MAJOR 27,
//       n19) through the REAL glue (www/js/protocol/wasm_glue*):
//       - `TargetGetResponse` brings each session's address and state, the
//         allowlist's "Opis", the sampler's "Ostatnie połączenie" dates and
//         "Nasłuch" per portal — every field `#[serde(default)]`, so a
//         decoder built before them would drop them without a sound;
//       - "Rozłącz": `codec.encode.tentaNasTargetSessionResetRequest` → the
//         wasm encoder → `TentaNasBody(TargetSessionResetRequest)`, the
//         initiator named by its IQN and no session id anywhere;
//       - the allowlist save carries `initiator_descriptions`, and leaves it
//         null (keep) when the screen does not send it.
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
  initiators: ['iqn.1994-05.com.redhat:vmhost-01', 'iqn.1994-05.com.redhat:vmhost-02'],
  initiator_descriptions: { 'iqn.1994-05.com.redhat:vmhost-01': 'Proxmox vmhost-01' },
  port_groups: [], sessions: 1, sessions_known: true, state: 'active', state_detail: '', created_at: '', updated_at: '', state_reasons: [],
};

test('the target detail brings the session address and state, Opis, the sampler dates and Nasłuch', { skip }, () => {
  const body = decodeBody({
    TentaNasBody: {
      TargetGetResponse: {
        target: TARGET,
        sessions: [{ client: 'iqn.1994-05.com.redhat:vmhost-01', user: 'iqn.1994-05.com.redhat:vmhost-01', connected_at: '2026-09-20T07:00:00Z', address: '10.10.0.21', state: 'LOGGED_IN' }],
        config_preview: '',
        initiators_seen: [{ initiator: 'iqn.1994-05.com.redhat:vmhost-02', last_seen_at: '2026-09-25T18:00:00Z', session_since: '2026-09-25T09:00:00Z' }],
        seen_since: '2026-09-26T16:00:00Z',
        listen: [{ address: '10.10.0.5', port: 3260, transport: 'tcp', state: 'target_disabled' }],
      },
    },
  });
  assert.equal(body.variant, 'TentaNasTargetGetResponse');
  assert.equal(body.sessions[0].address, '10.10.0.21');
  assert.equal(body.sessions[0].state, 'LOGGED_IN');
  assert.equal(body.sessions[0].connectedAt, '2026-09-20T07:00:00Z');
  assert.equal(body.target.initiatorDescriptions['iqn.1994-05.com.redhat:vmhost-01'], 'Proxmox vmhost-01');
  assert.equal(body.initiatorsSeen[0].lastSeenAt, '2026-09-25T18:00:00Z');
  assert.equal(body.seenSince, '2026-09-26T16:00:00Z');
  assert.equal(body.listen[0].state, 'target_disabled');
  assert.equal(body.listen[0].port, 3260);
});

test('an older node\'s answer decodes with empty session fields and no dates', { skip }, () => {
  const { initiator_descriptions: _dropped, ...older } = TARGET;
  const body = decodeBody({
    TentaNasBody: {
      TargetGetResponse: {
        target: older,
        sessions: [{ client: 'iqn.a:x', user: 'iqn.a:x', connected_at: null }],
        config_preview: '',
      },
    },
  });
  assert.equal(body.sessions[0].address, '');
  assert.equal(body.sessions[0].state, '');
  assert.deepEqual(body.target.initiatorDescriptions, {});
  assert.deepEqual(body.initiatorsSeen, []);
  assert.equal(body.seenSince, '');
  assert.deepEqual(body.listen, []);
});

// ----- "Rozłącz" and the allowlist save ------------------------------------------

test('TargetSessionResetRequest crosses the real glue named by the IQN, revoke only when asked', { skip }, () => {
  const sent = request('tentaNasTargetSessionResetRequest', { targetId: 't1', initiator: 'iqn.1994-05.com.redhat:vmhost-01', revoke: true, sudoPassword: 'sekret' });
  assert.equal(sent.variant, 'TentaNasTargetSessionResetRequest');
  assert.equal(sent.targetId, 't1');
  assert.equal(sent.initiator, 'iqn.1994-05.com.redhat:vmhost-01');
  assert.equal(sent.revoke, true);
  // Exactly these fields travel: no session id, ISID, TSIH or cntlid.
  const fields = Object.keys(sent).filter((k) => k !== 'variant' && !/[A-Z]/.test(k)).sort();
  assert.deepEqual(fields, ['initiator', 'revoke', 'sudo_password', 'target_id']);
  const plain = request('tentaNasTargetSessionResetRequest', { targetId: 't1', initiator: 'iqn.1994-05.com.redhat:vmhost-01' });
  assert.equal(plain.revoke, false);
  assert.equal(plain.sudoPassword, null);
});

test('the allowlist save carries Opis, and leaves it null when the screen does not send it', { skip }, () => {
  const withOpis = request('tentaNasTargetUpdateRequest', {
    targetId: 't1', initiators: ['iqn.1994-05.com.redhat:vmhost-01'],
    initiatorDescriptions: { 'iqn.1994-05.com.redhat:vmhost-01': 'Proxmox vmhost-01' }, enabled: true,
  });
  assert.equal(withOpis.variant, 'TentaNasTargetUpdateRequest');
  assert.equal(withOpis.initiatorDescriptions['iqn.1994-05.com.redhat:vmhost-01'], 'Proxmox vmhost-01');
  const without = request('tentaNasTargetUpdateRequest', { targetId: 't1', initiators: ['iqn.1994-05.com.redhat:vmhost-01'], enabled: true });
  assert.equal(without.initiatorDescriptions, null, 'absent means keep, never an empty map that wipes');
  const create = request('tentaNasTargetCreateRequest', {
    name: 'scratch', protocol: 'nvmet', source: 'tank/scratch', initiators: ['nqn.2014-08.org.nvmexpress:uuid:9f2c0000-0000-0000-0000-00000000a17b'],
    initiatorDescriptions: { 'nqn.2014-08.org.nvmexpress:uuid:9f2c0000-0000-0000-0000-00000000a17b': 'orion (compute)' },
  });
  assert.equal(create.initiatorDescriptions['nqn.2014-08.org.nvmexpress:uuid:9f2c0000-0000-0000-0000-00000000a17b'], 'orion (compute)');
});
