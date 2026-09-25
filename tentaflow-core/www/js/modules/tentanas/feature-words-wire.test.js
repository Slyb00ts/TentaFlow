// =============================================================================
// File: modules/tentanas/feature-words-wire.test.js
// Description: The Environment rows' coded details (`featureReasons`) and the
//       Elastic capabilities' coded reasons, as a screen really receives
//       them: a hand-built CBOR reply decoded through the real wasm glue and
//       handed to the functions the Environment tab and the pool wizard word
//       them with. Both fields are `#[serde(default)]`, so a glue built before
//       them drops them silently and every stubbed test stays green — the
//       reason this goes through the glue. Its own file: the wasm is
//       initialised BEFORE codec.js is first imported (alert-wire.test.js).
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
const { featureDetail, elasticCapabilitiesDetail } = await import('./feature-words.js');

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

const reason = (code, params = {}) => ({ code, params });
const feature = (id, status, detail) => ({
  id, status, version: null, required_version: null, binaries: [], kernel_module: null, packages: [], detail, optional: true,
});

test('the Environment rows keep their coded details through the real decoder and are worded', { skip }, async () => {
  const body = await throughTheClient('tentaNasEnvironmentRequest', { refresh: false }, 'EnvironmentResponse', {
    environment: {
      platform: 'linux', full_support: false, os_name: 'CachyOS', os_version: '', kernel: '6.16', hostname: 'helios',
      package_manager: 'pacman', ram_bytes: 0, uptime_secs: 0,
      features: [
        feature('nvme-cli', 'missing', 'missing: nvme'),
        feature('nvmet', 'ok', '/sys/kernel/config/nvmet present · nvmet-rdma available'),
        feature('dhchap', 'missing_module', 'this kernel was built without CONFIG_NVME_TARGET_AUTH (/proc/config.gz)'),
        feature('ksmbd', 'no_device', 'no RDMA interface with an address · EXPERIMENTAL (kernel docs) · ksmbd is not in this kernel\'s module tree'),
      ],
      elevation: {
        mode: 'helper', helper_state: 'ok', helper_path: '', helper_version: null, sudoers_path: '', core_user: '', core_version: '',
        armed_until: null, ttl_secs: 900, audit_entries: 0, provisioned_at: null, provisioned_by: null,
      },
      probed_at: '2026-09-25T10:00:00Z',
      feature_reasons: {
        'nvme-cli': [reason('feature_binaries_missing', { binaries: 'nvme' })],
        nvmet: [reason('configfs_present', { path: '/sys/kernel/config/nvmet' }), reason('module_on_demand', { module: 'nvmet-rdma' })],
        dhchap: [reason('dhchap_not_built', { path: '/proc/config.gz' })],
        ksmbd: [reason('ksmbd_no_interface'), reason('ksmbd_experimental'), reason('module_absent', { module: 'ksmbd' })],
      },
    },
  });
  const env = body.environment;
  assert.ok(env.featureReasons, 'the decoder keeps featureReasons');
  const rows = Object.fromEntries(env.features.map((f) => [f.id, featureDetail(f, env.featureReasons)]));
  assert.equal(rows['nvme-cli'].text, 'brak programu: nvme');
  assert.equal(rows['nvme-cli'].title, 'missing: nvme', 'the node’s sentence is the tooltip');
  assert.equal(rows.nvmet.text, '/sys/kernel/config/nvmet jest obecny · nvmet-rdma dostępny, ładowany przy pierwszym użyciu');
  assert.equal(rows.dhchap.text, 'to jądro zbudowano bez CONFIG_NVME_TARGET_AUTH (/proc/config.gz)');
  assert.equal(rows.ksmbd.text, 'brak interfejsu RDMA z adresem · EKSPERYMENTALNE (wg dokumentacji jądra) · modułu ksmbd nie ma w drzewie modułów jądra');
  for (const row of Object.values(rows)) assert.ok(!/\b(missing|loaded|present|available)\b/.test(row.text), row.text);
});

test('the Elastic capabilities keep their coded reasons through the real decoder and are worded', { skip }, async () => {
  const detail = 'mergerfs: missing: mergerfs; snapraid: not probed';
  const body = await throughTheClient('tentaNasElasticCapabilitiesRequest', {}, 'ElasticCapabilitiesResponse', {
    capabilities: {
      mergerfs: false, mergerfs_version: '', snapraid: false, snapraid_version: '', filesystems: ['xfs'], detail,
      reasons: [reason('elastic_tool_unavailable', { tool: 'mergerfs', status: 'missing' }), reason('elastic_tool_not_probed', { tool: 'snapraid' })],
    },
    free_disks: [],
  });
  const worded = elasticCapabilitiesDetail(body.capabilities);
  assert.equal(worded.text, 'mergerfs: brak · SnapRAID: nie sprawdzono (Środowisko → Sprawdź ponownie)');
  assert.equal(worded.title, detail);
});

test('a row this build cannot word in full, or an older node, shows the node’s sentence', () => {
  // One unknown part: every part or none (`wordReasons`).
  const f = { id: 'rdma', detail: 'mlx5_0 ACTIVE · something new' };
  assert.deepEqual(featureDetail(f, { rdma: [reason('rdma_devices', { devices: 'mlx5_0 ACTIVE' }), reason('brand_new_code')] }), { text: f.detail, title: '' });
  assert.deepEqual(featureDetail(f, undefined), { text: f.detail, title: '' });
  // A status this build has no word for is not worded either.
  const caps = { detail: 'snapraid: odd', reasons: [reason('elastic_tool_unavailable', { tool: 'snapraid', status: 'quantum' })] };
  assert.equal(elasticCapabilitiesDetail(caps).text, 'snapraid: odd');
  // A by-id path in an unworded node sentence never reaches the text.
  const byId = { detail: 'mkfs failed on /dev/disk/by-id/wwn-0x5000c500a1b2c3d4', reasons: [] };
  assert.ok(!elasticCapabilitiesDetail(byId).text.includes('wwn-0x5000c500a1b2c3d4'), elasticCapabilitiesDetail(byId).text);
});
