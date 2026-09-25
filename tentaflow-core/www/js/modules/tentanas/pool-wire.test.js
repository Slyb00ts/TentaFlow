// =============================================================================
// File: modules/tentanas/pool-wire.test.js
// Description: What the node now says about a missing pool leaf and a disk
//       alert, as a screen really receives it. A leaf `zpool status` can no
//       longer find carries `NasVdevDisk::last_known_name` (pools.rs
//       `last_known_leaf_name`), and n06 names the leaf by it instead of by
//       its GUID; a decoder built before that field existed drops it
//       silently (`#[serde(default)]`) and every stubbed test stays green.
//       A disk alert's `pool` / `layout` / `advice` parameters and a
//       conflict line's `kept_path` ride the params maps through the same
//       glue.
//
//       The reply frames are hand-built CBOR in the shape tentaflow-protocol
//       writes (`Envelope` around `MessageBody::TentaNasBody`), as in
//       alert-wire.test.js. Its own file for the reason refusal-wire.test.js
//       gives: the wasm has to be initialised BEFORE codec.js is first
//       imported.
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
const { planWarnings } = await import('./pool-wizard.js');

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

// A float field (`compress_ratio`, `read_iops`…) as the CBOR double serde
// writes for an f64.
class F64 { constructor(v) { this.v = v; } }
const f64 = (v) => new F64(v);

function cbor(value) {
  if (value === null) return [0xf6];
  if (value === true) return [0xf5];
  if (value === false) return [0xf4];
  if (value instanceof F64) {
    const view = new DataView(new ArrayBuffer(8));
    view.setFloat64(0, value.v);
    return [0xfb, ...new Uint8Array(view.buffer)];
  }
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

// `NasVdevDisk` field for field, as serde writes it.
const leaf = (name, extra = {}) => ({
  disk_id: null, name, path: '', state: 'online', read_errors: 0, write_errors: 0, cksum_errors: 0, size_bytes: 0, note: '', ...extra,
});

const POOL = {
  name: 'tank', guid: '42', kind: 'zfs', state: 'degraded', health: 'critical', health_reason: '',
  size_bytes: 0, alloc_bytes: 0, free_bytes: 0, usable_bytes: 0, used_bytes: 0, available_bytes: 0,
  capacity_pct: 0, fragmentation_pct: 0, compress_ratio: f64(1), dedup_ratio: f64(1), ashift: 12,
  autotrim: false, read_only: false, layout: 'raidz2', data_disks: 3, fault_tolerance: 2,
  vdevs: [{
    id: 'raidz2-0', role: 'data', kind: 'raidz2', state: 'degraded', fault_tolerance: 2,
    disks: [
      leaf('sda', { disk_id: 'wwn-a' }),
      leaf('12156453278383891134', { state: 'unavail', last_known_name: 'sdk' }),
      leaf('3847561029384756102', { state: 'unavail', last_known_name: null }),
    ],
  }],
  scan: { kind: 'none', status: 'none', progress_pct: null, started_at: null, finished_at: null, duration_secs: null, eta_secs: null, errors: 0, scanned_bytes: 0 },
  read_errors: 0, write_errors: 0, cksum_errors: 0, dataset_count: 0, snapshot_count: 0,
  io: { read_bps: 0, write_bps: 0, read_iops: f64(0), write_iops: f64(0), read_latency_ms: f64(0), write_latency_ms: f64(0) },
  compression: 'lz4', encryption: false, scrub_schedule: null, last_scrub_at: null, next_scrub_at: null,
};

const DISK_ALERT = {
  alert_id: 'a-disk', severity: 'warning', subject_kind: 'disk', subject_id: 'wwn-0x5000c500a1b2c3d4',
  title: 'Disk sdd: warning', detail: '', raised_at: '2026-09-24T00:00:00Z', acked_at: null, resolved_at: null,
  code: 'disk_health',
  params: { advice: 'replace', health: 'warning', layout: 'raidz2', name: 'sdd', name_source: 'live', pool: 'tank' },
  reasons: [{ code: 'reallocated_growing', params: { from: '5', to: '8' } }],
};

const KEPT = '/mnt/tentanas-branches/media/cache/nvme2n1/.tentanas-quarantine-01a0cf8c-5a61-7283-8410-924a0fceb01f-3';
const CONFLICT_ALERT = {
  alert_id: 'a-conflict', severity: 'warning', subject_kind: 'elastic-array', subject_id: 'media',
  title: 'Pliki zachowane w dwóch wersjach: 1', detail: '', raised_at: '2026-09-24T00:00:00Z', acked_at: null, resolved_at: null,
  code: 'elastic_conflict',
  params: { array: 'media', count: '1' },
  reasons: [{ code: 'conflict_file', params: { kept_disk: 'nvme2n1', kept_kind: 'quarantine', kept_path: KEPT, path: 'docs/a.odt', visible: '/mnt/media/docs/a.odt' } }],
};

// Sends a real request through a client with no socket and answers it with
// `reply` (a `TentaNasPayload` variant), the way alert-wire.test.js does.
async function throughTheClient(kind, payload, reply) {
  const client = new BinaryWsClient('ws://node.invalid/ws/api');
  client.nextCorrelationId = codec.makeCorrelationIdGenerator();
  client._send = (frame) => {
    const correlationId = BigInt(wasm.decodeEnvelope(frame).correlation_id);
    const body = Uint8Array.from(cbor({ TentaNasBody: reply }));
    const frameBytes = Uint8Array.from(cbor({
      schema_version: codec.schemaVersion(),
      correlation_id: correlationId,
      sequence: 1,
      message_kind: codec.messageKind().META_HEARTBEAT,
      flags: 0,
      routing: 'Direct',
      forwarded_session_claim: null,
      body,
    }));
    queueMicrotask(() => client._handleBytes(frameBytes));
  };
  const { body } = await client.request(kind, payload);
  return body;
}

test('a missing pool leaf keeps the name the node remembers through the real decoder', { skip }, async () => {
  const body = await throughTheClient('tentaNasPoolGetRequest', { name: 'tank' }, {
    PoolGetResponse: { pool: POOL, properties: [], datasets: [], alerts: [], history: [] },
  });
  const disks = body.pool.vdevs[0].disks;
  assert.equal(disks[1].name, '12156453278383891134');
  assert.equal(disks[1].lastKnownName, 'sdk', 'the remembered name arrives, under the name the screen reads');
  assert.equal(disks[2].lastKnownName ?? null, null);
});

test('a disk alert\'s pool and advice, and a conflict line\'s kept path, arrive through the real decoder', { skip }, async () => {
  const body = await throughTheClient('tentaNasAlertsListRequest', { includeAcked: false }, {
    AlertsListResponse: { alerts: [DISK_ALERT, CONFLICT_ALERT] },
  });
  const [disk, conflict] = body.alerts;
  const diskText = alertText(disk);
  assert.equal(diskText.place, 'tank · RAIDZ2');
  assert.equal(diskText.advice, 'zaplanuj wymianę dysku');
  const conflictText = alertText(conflict);
  assert.deepEqual(conflictText.copies, [{ path: 'docs/a.odt', kept: KEPT }]);
  assert.doesNotMatch(`${conflictText.title} ${conflictText.detail} ${conflictText.tooltip}`, /quarantine-/, 'the kept path is only for the clipboard');
});

test('the layout plan\'s warning codes arrive through the real decoder and are worded', { skip }, async () => {
  const option = { layout: 'mirror', available: true, reason: '', usable_bytes: 4, raw_bytes: 8, fault_tolerance: 1, recommended: true };
  const body = await throughTheClient('tentaNasPoolPlanRequest', { diskIds: ['a', 'b'] }, {
    PoolPlanResponse: {
      options: [option],
      warnings: ['mixed SSD and HDD: the vdev runs at the speed of its slowest member'],
      smallest_disk_bytes: 4,
      warning_codes: [{ code: 'mixed_media', params: {} }],
    },
  });
  assert.deepEqual(body.warningCodes.map((c) => c.code), ['mixed_media'], 'the codes arrive');
  assert.deepEqual(planWarnings(body), ['SSD i HDD razem: vdev działa z prędkością najwolniejszego dysku.']);
});
