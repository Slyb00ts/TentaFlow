// =============================================================================
// File: protocol/wave9b-wire.test.js
// Description: The wire fields and requests of TentaNas wave 9b through the
//       REAL glue (www/js/protocol/wasm_glue*), both directions:
//       - requests: `codec.encode.*` → the wasm encoder → an envelope the
//         wasm decodes back into the fields the node reads;
//       - answers: CBOR written in the shape tentaflow-protocol serialises
//         (serde's externally tagged `MessageBody`) → `decodeMessageBody`,
//         the decoder every screen's answer goes through.
//       A decoder built before a field existed drops it silently (every new
//       field is `#[serde(default)]`), and every stubbed screen test stays
//       green while the real screen shows nothing — which is what this file
//       is here to catch:
//       - the SMART batch: `DiskSmartTestBatchRequest` and `NasJob.disks`;
//       - per-organisation forwarding: `AlertForwardSetRequest.node_wide`
//         and `AccessLogResponse.forward_node`;
//       - n18d: `AddonDisablePreviewRequest` / `…Response`;
//       - MAJOR 22: `AddonTeardownStatusRequest` / `…Response` and the
//         teardown plan's `nodes`, `blocks` and `count_vars`.
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

// ----- the SMART batch ---------------------------------------------------------

test('DiskSmartTestBatchRequest carries every disk and the one password to the node', { skip }, () => {
  const sent = request('tentaNasDiskSmartTestBatchRequest', {
    diskIds: ['wwn-0x5000c500a1b2c3d4', 'sn-S3Z9NB0K'], kind: 'long', sudoPassword: 'test-secret-not-real',
  });
  assert.equal(sent.variant, 'TentaNasDiskSmartTestBatchRequest');
  assert.deepEqual(sent.disk_ids, ['wwn-0x5000c500a1b2c3d4', 'sn-S3Z9NB0K']);
  assert.equal(sent.kind, 'long');
  assert.equal(sent.sudo_password, 'test-secret-not-real');
});

test('a JobResponse keeps the disk lines of a multi-disk job, named, with coded reasons', { skip }, () => {
  const job = {
    job_id: 'j-1', kind: 'smart_test_batch', subject: 'sda, sdq', status: 'running', progress_pct: 40,
    started_by: 'admin', started_at: '2026-09-26T10:00:00Z', finished_at: null, error: null, log: [],
    subject_last_known: false,
    disks: [
      { name: 'sda', last_known: false, state: 'running', progress_pct: 60, reasons: [] },
      { name: 'sdq', last_known: true, state: 'refused', progress_pct: null, reasons: [{ code: 'self_test_running', params: {} }] },
    ],
  };
  const body = decodeBody({ TentaNasBody: { JobResponse: { job } } });
  assert.equal(body.variant, 'TentaNasJobResponse');
  const disks = body.job.disks;
  assert.equal(disks.length, 2, 'the lines survive the decoder');
  assert.equal(disks[0].name, 'sda');
  assert.equal(disks[0].progressPct, 60, 'camelCase for the screen');
  assert.equal(disks[1].lastKnown, true);
  assert.equal(disks[1].reasons[0].code, 'self_test_running');
});

// ----- per-organisation forwarding ------------------------------------------------

test('AlertForwardSetRequest says which target it sets', { skip }, () => {
  const own = request('tentaNasAlertForwardSetRequest', { enabled: true, syslogTarget: 'siem.local:514', includeAccess: true });
  assert.equal(own.node_wide, false, 'the organisation\'s own target unless asked otherwise');
  assert.equal(own.include_access, true);
  const node = request('tentaNasAlertForwardSetRequest', { enabled: false, syslogTarget: 'legacy.local:514', nodeWide: true });
  assert.equal(node.node_wide, true);
});

test('AccessLogResponse brings the organisation\'s target and the node-wide one apart', { skip }, () => {
  const target = (over) => ({
    enabled: true, syslog_target: '', webhook_url: '', include_access: false, pending: 0, last_sent_at: null, last_error: '', ...over,
  });
  const body = decodeBody({
    TentaNasBody: {
      AccessLogResponse: {
        events: [], total: 0,
        audit: {
          audited_shares: [], audited_exports: [], unaudited_smb_direct: [], retention_days: 30,
          collector_state: 'ok', detail: '', collected_at: null, event_count: 0,
        },
        shares: [], users: [], operations: [],
        forward: target({ syslog_target: 'org.local:514', include_access: true, pending: 3 }),
        forward_node: target({ syslog_target: 'legacy.local:514', pending: 1 }),
      },
    },
  });
  assert.equal(body.forward.syslogTarget, 'org.local:514');
  assert.equal(body.forward.includeAccess, true);
  assert.equal(body.forwardNode.syslogTarget, 'legacy.local:514', 'the node-wide target reaches the screen');
  assert.equal(body.forwardNode.pending, 1);
});

// ----- n18d: the disable preview -------------------------------------------------

test('AddonDisablePreviewRequest and its answer cross the real glue', { skip }, () => {
  const sent = request('addonDisablePreviewRequest', { addonId: 'tentanas-1a2b3c4d' });
  assert.equal(sent.variant, 'AddonDisablePreviewRequest');
  assert.equal(sent.addonId, 'tentanas-1a2b3c4d');

  const body = decodeBody({
    AddonDisablePreviewResponseBody: {
      addon_id: 'tentanas-1a2b3c4d', display_name: 'TentaNas', node_name: 'helios', background_on_disable: true,
      consequences: [
        { kind: 'tentanas_smb_shares_continue', effect: 'continues', count_vars: { n: 2 }, names: [] },
        { kind: 'tentanas_pools_imported', effect: 'kept', count_vars: { n: 2 }, names: ['fast', 'tank'] },
      ],
    },
  });
  assert.equal(body.variant, 'AddonDisablePreviewResponse');
  assert.equal(body.nodeName, 'helios');
  assert.equal(body.backgroundOnDisable, true);
  assert.deepEqual(body.consequences[0].countVars, { n: 2 });
  assert.deepEqual(body.consequences[1].names, ['fast', 'tank']);
  assert.equal(body.consequences[1].effect, 'kept');
});

// ----- MAJOR 22: the fleet uninstall ------------------------------------------------

test('AddonTeardownStatusRequest and its answer cross the real glue', { skip }, () => {
  const sent = request('addonTeardownStatusRequest', { addonId: 'tentanas-1a2b3c4d' });
  assert.equal(sent.variant, 'AddonTeardownStatusRequest');
  assert.equal(sent.addonId, 'tentanas-1a2b3c4d');

  const body = decodeBody({
    AddonTeardownStatusResponseBody: {
      addon_id: 'tentanas-1a2b3c4d', state: 'failed', phase: 'tentanas_elastic_check', warnings: ['tentanas_backup_failed'],
    },
  });
  assert.equal(body.variant, 'AddonTeardownStatusResponse');
  assert.equal(body.state, 'failed');
  assert.equal(body.phase, 'tentanas_elastic_check');
  assert.deepEqual(body.warnings, ['tentanas_backup_failed']);
});

test('the teardown plan keeps its nodes, its blocking entry and its counts', { skip }, () => {
  const body = decodeBody({
    AddonTeardownPlanResponseBody: {
      addon_id: 'tentanas-1a2b3c4d', display_name: 'TentaNas',
      entries: [
        { path: '/etc/samba/tentanas.conf', kind: 'tentanas_smb_config', description: 'smb', removed: true, size_bytes: 10, count_vars: { n: 2 }, blocks: false },
        { path: '/mnt/tentanas', kind: 'tentanas_elastic_arrays', description: 'arrays', removed: false, size_bytes: 0, count_vars: { n: 1 }, blocks: true },
      ],
      dependents: [],
      nodes: [
        { node_id: 'a'.repeat(64), name: 'helios', local: true, online: true, status: 'ready', last_known: true, last_blocks: [] },
        {
          node_id: 'b'.repeat(64), name: '', local: false, online: false, status: 'unknown', last_known: true,
          last_blocks: [{ path: '', kind: 'tentanas_elastic_arrays', description: '', removed: false, size_bytes: 0, count_vars: { n: 3 }, blocks: true }],
        },
      ],
      privilege: 'password',
      backup_file: 'app-backups/tentanas-helios-….json',
    },
  });
  assert.equal(body.variant, 'AddonTeardownPlanResponse');
  assert.deepEqual(body.entries[0].countVars, { n: 2 }, 'the counts reach the dialog (they never did before)');
  assert.equal(body.entries[1].blocks, true);
  assert.equal(body.nodes.length, 2);
  assert.equal(body.nodes[0].name, 'helios');
  assert.equal(body.nodes[1].online, false);
  assert.equal(body.nodes[1].nodeId, 'b'.repeat(64), 'the id routes the forwarded requests; the dialog never prints it');
  // Round 2: what an offline node last published, its mode and its backup.
  assert.equal(body.nodes[1].lastKnown, true);
  assert.equal(body.nodes[1].lastBlocks[0].kind, 'tentanas_elastic_arrays');
  assert.deepEqual(body.nodes[1].lastBlocks[0].countVars, { n: 3 });
  assert.equal(body.privilege, 'password');
  assert.equal(body.backupFile, 'app-backups/tentanas-helios-….json');
});

test('AddonTeardownArmRequest carries the password to the node and its answer comes back', { skip }, () => {
  const envelope = wasm.decodeEnvelope(codec.encode.addonTeardownArmRequest(92, { addonId: 'tentanas-1a2b3c4d', sudoPassword: 'test-secret-not-real' }, 4));
  try {
    const bytes = envelope.body;
    const echo = wasm.decodeMessageBody(bytes);
    assert.equal(echo.variant, 'AddonTeardownArmRequest');
    assert.equal(echo.addonId, 'tentanas-1a2b3c4d');
    assert.equal(echo.sudoPassword, undefined, 'the echo never carries the password into a JS object');
    assert.ok(new TextDecoder().decode(bytes).includes('test-secret-not-real'), 'but the wire does carry it to the node');
  } finally {
    envelope.free();
  }
  const body = decodeBody({ AddonTeardownArmResponseBody: { addon_id: 'tentanas-1a2b3c4d', armed_until: '2026-09-26T12:30:00Z' } });
  assert.equal(body.variant, 'AddonTeardownArmResponse');
  assert.equal(body.armedUntil, '2026-09-26T12:30:00Z');
});

// ----- round 3 -----------------------------------------------------------------

test('AddonUninstallRequest carries the acknowledged nodes; AddonTeardownDisarmRequest reaches the node', { skip }, () => {
  const envelope = wasm.decodeEnvelope(codec.encode.addonUninstallRequest(93, {
    addonId: 'tentanas-1a2b3c4d', acknowledgedNodes: [{ nodeId: 'c'.repeat(64), confirmName: 'LOST' }],
  }, 5));
  try {
    const echo = wasm.decodeMessageBody(envelope.body);
    assert.equal(echo.variant, 'AddonUninstallRequest');
    assert.equal(echo.acknowledgedCount, 1);
    assert.ok(new TextDecoder().decode(envelope.body).includes('LOST'), 'the retyped word travels to the node');
  } finally {
    envelope.free();
  }
  assert.equal(request('addonTeardownDisarmRequest', { addonId: 'tentanas-1a2b3c4d' }).variant, 'AddonTeardownDisarmRequest');
});

test('a plan node says it is unpaired; a forwarding target says its webhook needs a change', { skip }, () => {
  const plan = decodeBody({
    AddonTeardownPlanResponseBody: {
      addon_id: 'x', display_name: 'X', entries: [], dependents: [],
      nodes: [{ node_id: 'd'.repeat(64), name: 'vega', local: false, online: false, status: 'ready', last_known: false, last_blocks: [], unpaired: true }],
      privilege: '', backup_file: '',
    },
  });
  assert.equal(plan.nodes[0].unpaired, true);
  const target = { enabled: true, syslog_target: '', webhook_url: 'http://old.example.com/…', include_access: false, pending: 0, last_sent_at: null, last_error: '', webhook_needs_migration: true };
  const body = decodeBody({
    TentaNasBody: {
      AccessLogResponse: {
        events: [], total: 0,
        audit: { audited_shares: [], audited_exports: [], unaudited_smb_direct: [], retention_days: 30, collector_state: 'ok', detail: '', collected_at: null, event_count: 0 },
        shares: [], users: [], operations: [], forward: target, forward_node: { ...target, webhook_needs_migration: false },
      },
    },
  });
  assert.equal(body.forward.webhookNeedsMigration, true);
});
