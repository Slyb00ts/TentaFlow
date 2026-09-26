// =============================================================================
// File: protocol/wave10-wire.test.js
// Description: The wire fields and requests of TentaNas wave 10 through the
//       REAL glue (www/js/protocol/wasm_glue*), both directions:
//       - the per-node disable table: `AddonDisablePreviewResponse.nodes`
//         and `.privilege` reach the screen through `decodeMessageBody`
//         (a decoder built before them drops both silently — every field is
//         `#[serde(default)]` — and the dialog shows one node);
//       - n18d "Wyłącz i zatrzymaj udostępnianie…": `codec.encode.
//         tentaNasSharingStopRequest` → the wasm encoder → the envelope the
//         node decodes as `TentaNasBody(SharingStopRequest {})`, and the
//         parked answer comes back with its coded detail.
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

// ----- the per-node disable table ------------------------------------------------

test('the disable preview brings every node by name and the answering node\'s mode', { skip }, () => {
  const body = decodeBody({
    AddonDisablePreviewResponseBody: {
      addon_id: 'tentanas-1a2b3c4d', display_name: 'TentaNas', node_name: 'helios', background_on_disable: true,
      consequences: [{ kind: 'tentanas_api_closed', effect: 'stops', count_vars: {}, names: [] }],
      nodes: [
        { node_id: 'a'.repeat(64), name: 'helios', local: true, online: true, status: 'ready', last_known: true, last_blocks: [], unpaired: false },
        { node_id: 'b'.repeat(64), name: 'atlas', local: false, online: false, status: 'unknown', last_known: false, last_blocks: [], unpaired: true },
      ],
      privilege: 'password',
    },
  });
  assert.equal(body.variant, 'AddonDisablePreviewResponse');
  assert.equal(body.privilege, 'password', 'the mode chip survives the decoder');
  assert.equal(body.nodes.length, 2, 'the node list survives the decoder');
  assert.equal(body.nodes[0].name, 'helios');
  assert.equal(body.nodes[0].local, true);
  assert.equal(body.nodes[1].nodeId, 'b'.repeat(64), 'the id routes the forwarded request');
  assert.equal(body.nodes[1].online, false);
  assert.equal(body.nodes[1].unpaired, true);
  assert.equal(body.consequences[0].kind, 'tentanas_api_closed');
});

test('an older node\'s preview without nodes decodes to an empty list and no mode', { skip }, () => {
  const body = decodeBody({
    AddonDisablePreviewResponseBody: {
      addon_id: 'tentanas-1a2b3c4d', display_name: 'TentaNas', node_name: 'helios', background_on_disable: false, consequences: [],
    },
  });
  assert.deepEqual(body.nodes, []);
  assert.equal(body.privilege, '');
});

// ----- n18d: stop sharing on this node ----------------------------------------------

test('SharingStopRequest crosses the real glue as the one field-less request', { skip }, () => {
  const sent = request('tentaNasSharingStopRequest', {});
  assert.equal(sent.variant, 'TentaNasSharingStopRequest');
  // Nothing rides along: the node reads its own shares and targets.
  const extra = Object.keys(sent).filter((k) => k !== 'variant');
  assert.deepEqual(extra, []);
});

test('the parked stop comes back with its coded detail for the approver', { skip }, () => {
  const body = decodeBody({
    TentaNasBody: {
      ApprovalPendingResponse: {
        approval: {
          request_id: 'r-1', operation: 'sharing_stop', subject: 'helios',
          detail: 'stops sharing on helios: shares media (1 SMB, 0 NFS), targets — (0 iSCSI, 0 NVMe-oF), then disables TentaNas on every node',
          detail_reasons: [{ code: 'sharing_stop', params: { node: 'helios', shares: 'media', targets: '', other_shares: '0', other_targets: '0' } }],
          status: 'pending', requested_by: 'u-1', requested_at: '2026-09-26T10:00:00Z', expires_at: '2026-09-27T10:00:00Z',
          decided_by: null, decided_at: null, decision_note: '', decision_job_id: null, is_own_request: true,
        },
      },
    },
  });
  assert.equal(body.variant, 'TentaNasApprovalPendingResponse');
  assert.equal(body.approval.operation, 'sharing_stop');
  assert.equal(body.approval.subject, 'helios');
  assert.equal(body.approval.detailReasons[0].code, 'sharing_stop');
  assert.equal(body.approval.detailReasons[0].params.shares, 'media');
});

test('a sharing job\'s step lines reach the screen named by their step', { skip }, () => {
  const body = decodeBody({
    TentaNasBody: {
      JobResponse: {
        job: {
          job_id: 'j-1', kind: 'sharing_stop', subject: 'helios', status: 'running', progress_pct: null,
          started_by: 'u-2', started_at: '2026-09-26T10:00:00Z', finished_at: null, error: null, log: [], subject_last_known: false,
          disks: [
            { name: 'shares', last_known: false, state: 'done', progress_pct: null, reasons: [{ code: 'shares_stopped', params: { smb: '2', nfs: '1' } }] },
            { name: 'targets', last_known: false, state: 'running', progress_pct: null, reasons: [] },
            { name: 'disable', last_known: false, state: 'pending', progress_pct: null, reasons: [] },
          ],
        },
      },
    },
  });
  assert.equal(body.job.kind, 'sharing_stop');
  assert.deepEqual(body.job.disks.map((d) => d.name), ['shares', 'targets', 'disable']);
  assert.equal(body.job.disks[0].reasons[0].params.smb, '2');
});
