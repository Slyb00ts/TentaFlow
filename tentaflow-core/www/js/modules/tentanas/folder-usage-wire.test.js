// =============================================================================
// File: modules/tentanas/folder-usage-wire.test.js
// Description: The n11 Folders "Użycie" column as a screen really receives it.
//       `NasElasticFolder` carries the node's last bounded walk:
//       `used_bytes`, `used_measured_at` and, when there is no figure,
//       `used_reasons`. The two new fields are `#[serde(default)]`, so a
//       decoder built before them drops them silently and every folder reads
//       "not measured" while stubbed tests stay green. The reply frame is
//       hand-built CBOR decoded by `BinaryWsClient` with the real wasm glue;
//       its own file because the wasm is initialised BEFORE codec.js is first
//       imported (see refusal-wire.test.js).
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

const TiB = 1024 ** 4;
const MEASURED_AT = new Date(Date.now() - 2 * 3600 * 1000).toISOString();

// `NasElasticArray` as serde writes it (snake_case keys), with three folders:
// one measured, one too large for the walk's bound, one not measured yet.
const ARRAY = {
  name: 'media', kind: 'elastic-array', state: 'active', state_detail: '', health: 'ok', health_reason: '',
  enabled: true, union_path: '/mnt/media', create_policy: 'mfs', filesystem: 'xfs',
  data_disks: [], cache_disks: [], parity_disks: [],
  folders: [
    { name: 'filmy', path: '/mnt/media/filmy', cache_policy: 'yes', used_bytes: Math.round(3.8 * TiB), used_measured_at: MEASURED_AT, used_reasons: [], share_id: '', share_label: '' },
    { name: 'foto', path: '/mnt/media/foto', cache_policy: 'only', used_bytes: null, used_measured_at: null,
      used_reasons: [{ code: 'folder_usage_over_budget', params: { entries: '5000000', minutes: '5' } }], share_id: '', share_label: '' },
    { name: 'muzyka', path: '/mnt/media/muzyka', cache_policy: 'yes', used_bytes: null,
      used_reasons: [{ code: 'folder_usage_pending', params: {} }], share_id: '', share_label: '' },
  ],
  folders_known: true,
  mover: { enabled: false, schedule: null, min_age_secs: 7200, cache_min_free_pct: 20, coupled_sync: true, last_run: null },
  snapraid: {
    installed: true, version: '13.0', config_path: '/etc/tentanas/snapraid-media.conf', last_sync: null, last_scrub: null,
    history: [], sync_schedule: null, scrub_schedule: null, scrub_percent: 8, scrub_older_than_days: 10,
    parity_errors: null, parity_errors_window_days: 30,
  },
  protection: { cache_unprotected_bytes: null, moved_unsynced_bytes: null, status: 'unknown', detail: '', fault_tolerance: null, protected_as_of: null },
  usable_bytes: null, used_bytes: null, cache_size_bytes: null, cache_used_bytes: null,
  created_at: '2026-09-24T00:00:00Z', updated_at: '2026-09-24T00:00:00Z',
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

test('a folder\'s measured size, its age and the reason for a missing one survive the real decoder', { skip }, async () => {
  const { client } = clientAnswering(() => ({ TentaNasBody: { ElasticArrayGetResponse: { array: ARRAY } } }));
  const { body } = await client.request('tentaNasElasticArrayGetRequest', { name: 'media' });
  const [filmy, foto, muzyka] = body.array.folders;
  assert.equal(filmy.usedBytes, Math.round(3.8 * TiB));
  assert.equal(filmy.usedMeasuredAt, MEASURED_AT);
  assert.equal(foto.usedBytes, null);
  assert.equal(foto.usedReasons[0].code, 'folder_usage_over_budget');
  assert.equal(foto.usedReasons[0].params.entries, '5000000');
  assert.equal(muzyka.usedReasons[0].code, 'folder_usage_pending');

  const screen = fakeScreen({ tentaNasElasticArrayGetRequest: { array: body.array } });
  screen.array = 'media';
  screen.openArray = () => {};
  const host = document.createElement('div');
  document.body.appendChild(host);
  try {
    await drawElasticDetail(screen, host);
    await flush();
    const cell = (name) => host.querySelector(`.nas-folders .fr[data-folder="${name}"] [data-f="folder-used"]`);
    assert.equal(cell('filmy').textContent, '3.8 TiB');
    assert.match(cell('filmy').getAttribute('title'), /^Zmierzono 2 h temu/);
    assert.equal(cell('foto').textContent, '—');
    assert.match(cell('foto').getAttribute('title'), /za dużo plików/);
    assert.match(cell('foto').getAttribute('title'), /5 min/);
    assert.equal(cell('muzyka').textContent, '—');
    assert.match(cell('muzyka').getAttribute('title'), /Jeszcze nie zmierzono/);
  } finally {
    screen.dispose();
    host.remove();
  }
});
