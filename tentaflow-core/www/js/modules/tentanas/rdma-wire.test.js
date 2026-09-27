// =============================================================================
// File: modules/tentanas/rdma-wire.test.js
// Description: RDMA listeners (measured on rig11 2026-09-27) as a screen
//       really receives them: hand-built CBOR replies decoded through the
//       real wasm glue — "Nasłuch RDMA" with the devices it is bound on, a
//       session's transport, an interface's RDMA device and the Environment
//       row's per-device codes — and handed to the functions that word them.
//       Every new field is `#[serde(default)]`, so a glue built before them
//       drops them silently while every stubbed test stays green; that is why
//       this goes through the glue. Its own file: the wasm is initialised
//       BEFORE codec.js is first imported (alert-wire.test.js).
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
const { featureDetail } = await import('./feature-words.js');
const { listenText, isRdmaListen, sessionAddressHtml } = await import('./targets.js');
const { transportOptions, interfaceRdmaGap } = await import('./target-wizard.js');
const { parseRefusal, refusalWords } = await import('./format.js');

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

const nasTarget = (protocol, portals, initiators = []) => ({
  target_id: 't-rdma', name: 'vm-rdma', protocol, wwn: protocol === 'nvmet' ? 'nqn.2026-09.local.tentaflow:storage:vm-rdma' : 'iqn.2026-09.local.tentaflow:storage:vm-rdma',
  enabled: true, luns: [], portals, auth: { method: 'none' }, initiators, port_groups: [], sessions: 1, sessions_known: true,
  state: 'active', state_detail: '', created_at: '2026-09-27T06:00:00Z', updated_at: '2026-09-27T06:00:00Z',
});

test('an iSER portal reaches the screen as two listeners, the RDMA one with its device, and a session with its transport', { skip }, async () => {
  const portal = { interface: 'enp5s0', address: '192.168.11.11', port: 3260, transport: 'iser' };
  const body = await throughTheClient('tentaNasTargetGetRequest', { target_id: 't-rdma' }, 'TargetGetResponse', {
    target: nasTarget('iscsi', [portal], ['iqn.2004-10.com.ubuntu:01:35ed4c1b7a']),
    sessions: [{ client: 'iqn.2004-10.com.ubuntu:01:35ed4c1b7a', user: 'iqn.2004-10.com.ubuntu:01:35ed4c1b7a', connected_at: null, address: '192.168.11.11', state: 'LOGGED_IN', transport: 'iser' }],
    config_preview: '',
    listen: [
      { address: '192.168.11.11', port: 3260, transport: 'tcp', state: 'listening', rdma_devices: [] },
      { address: '192.168.11.11', port: 3260, transport: 'iser', state: 'listening', rdma_devices: ['m27rxe'] },
    ],
  });
  assert.deepEqual(body.listen.map((l) => l.rdmaDevices), [[], ['m27rxe']], 'the decoder keeps rdmaDevices');
  assert.equal(body.sessions[0].transport, 'iser', 'the decoder keeps a session transport');
  assert.equal(listenText(body.listen.filter((l) => !isRdmaListen(l))), 'Nasłuch aktywny');
  assert.equal(listenText(body.listen.filter(isRdmaListen)), 'Nasłuch aktywny · urządzenie RDMA: m27rxe');
  assert.match(sessionAddressHtml(body.sessions[0]), /192\.168\.11\.11<\/span> <span[^>]*>iSER \(RDMA\)</);
});

test('a lost RDMA listener and an address without an RDMA device read as the kernel measured them', { skip }, async () => {
  const body = await throughTheClient('tentaNasTargetGetRequest', { target_id: 't-rdma' }, 'TargetGetResponse', {
    target: nasTarget('nvmet', [{ interface: 'enp5s0', address: '192.168.11.11', port: 4420, transport: 'rdma' }]),
    sessions: [],
    config_preview: '',
    listen: [{ address: '192.168.11.11', port: 4420, transport: 'rdma', state: 'listener_lost', rdma_devices: [] }],
  });
  assert.match(listenText(body.listen), /^Brak nasłuchu — konfiguracja jest w jądrze, ale nasłuchu RDMA nie ma/);
  assert.match(listenText([{ ...body.listen[0], state: 'no_rdma_device' }]), /żadne urządzenie RDMA nie ma tego adresu/);
});

test('an interface without an RDMA device is not offered RDMA, from the decoded capabilities', { skip }, async () => {
  const body = await throughTheClient('tentaNasTargetsListRequest', {}, 'TargetsListResponse', {
    targets: [],
    services: [],
    capabilities: {
      iscsi: true, nvmet: true, iser: true, nvme_rdma: true, dhchap: false,
      interfaces: [
        { name: 'enp5s0', address: '192.168.11.11', rdma: false, rdma_device: '', shared: true, supported: true },
        { name: 'enp4s0np0', address: '10.10.0.5', rdma: false, rdma_device: 'rocep4s0', shared: false, supported: true },
        { name: 'storage0', address: '10.20.0.5', rdma: true, rdma_device: 'mlx5_0', shared: false, supported: true },
      ],
      volumes: [],
    },
  });
  const caps = body.capabilities;
  assert.deepEqual(caps.interfaces.map((i) => i.rdmaDevice), ['', 'rocep4s0', 'mlx5_0'], 'the decoder keeps rdmaDevice');
  assert.deepEqual(transportOptions('iscsi', caps, 'enp5s0').map((t) => t.ok), [true, false]);
  assert.deepEqual(transportOptions('nvmet', caps, 'enp4s0np0').map((t) => t.ok), [true, false, false]);
  assert.deepEqual(transportOptions('nvmet', caps, 'storage0').map((t) => t.ok), [true, true, true]);
  assert.match(interfaceRdmaGap(caps, 'enp5s0'), /^Interfejs enp5s0 nie ma urządzenia RDMA/);
  assert.match(interfaceRdmaGap(caps, 'enp4s0np0'), /rocep4s0 na interfejsie enp4s0np0 ma nieaktywne łącze/);
  assert.equal(interfaceRdmaGap(caps, ''), '', 'every interface at once is not one card');
});

test('the Environment RDMA row says per device whether its interface can carry an RDMA portal', { skip }, async () => {
  const detail = 'rocep4s0 DOWN (enp4s0np0), m27rxe ACTIVE (enp5s0 192.168.11.11) · rpcrdma loaded (provides svcrdma/xprtrdma)';
  const body = await throughTheClient('tentaNasEnvironmentRequest', { refresh: false }, 'EnvironmentResponse', {
    environment: {
      platform: 'linux', full_support: true, os_name: 'Ubuntu', os_version: '', kernel: '7.0.0-34-generic', hostname: 'storage',
      package_manager: 'apt', ram_bytes: 0, uptime_secs: 0,
      features: [{ id: 'rdma', status: 'ok', version: null, required_version: null, binaries: [], kernel_module: 'rpcrdma', packages: [], detail, optional: true }],
      elevation: {
        mode: 'helper', helper_state: 'ok', helper_path: '', helper_version: null, sudoers_path: '', core_user: '', core_version: '',
        armed_until: null, ttl_secs: 900, audit_entries: 0, provisioned_at: null, provisioned_by: null,
      },
      probed_at: '2026-09-27T06:00:00Z',
      feature_reasons: {
        rdma: [
          reason('rdma_device', { device: 'rocep4s0', state: 'DOWN', netdev: 'enp4s0np0', portal: 'no_address' }),
          reason('rdma_device', { device: 'm27rxe', state: 'ACTIVE', netdev: 'enp5s0', addresses: '192.168.11.11', portal: 'yes' }),
          reason('module_loaded', { module: 'rpcrdma' }),
        ],
      },
    },
  });
  const row = featureDetail(body.environment.features[0], body.environment.featureReasons);
  assert.equal(
    row.text,
    'rocep4s0 (enp4s0np0): łącze DOWN, brak adresu IPv4 — nie przyjmie portalu RDMA · m27rxe (enp5s0, 192.168.11.11): łącze ACTIVE — przyjmie portal RDMA · rpcrdma załadowany',
  );
  assert.equal(row.title, detail);
});

test('the refusal of an RDMA portal on an interface without an RDMA device is worded with its parameters', () => {
  const refusal = parseRefusal(new Error('protocol error BadRequest: refusal:target_rdma_no_device?interface=enp5s0&address=192.168.11.11 enp5s0 has no RDMA device, and the kernel refuses an RDMA listener on 192.168.11.11 (ENODEV)'));
  assert.deepEqual(refusal.params, { interface: 'enp5s0', address: '192.168.11.11' });
  assert.equal(refusalWords(refusal), 'Interfejs enp5s0 nie ma urządzenia RDMA — jądro odrzuca nasłuch RDMA na adresie 192.168.11.11. Wybierz TCP albo interfejs z kartą RDMA.');
});
