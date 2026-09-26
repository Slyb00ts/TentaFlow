// =============================================================================
// File: modules/tentanas/coded-text-wire.test.js
// Description: The node's wave-6 sentences as a screen really receives them.
//       Every field below is new on the wire — a schedule row's structured
//       outcome, a parked request's coded detail, an Elastic Array's and a
//       target's coded state, the Elastic preview's coded refusals and
//       warnings, the kernel-support reasons — and every one of them is
//       `#[serde(default)]`: a decoder built before them drops them
//       silently, and every screen then falls back to the node's own
//       sentence while every stubbed test stays green.
//
//       So each reply goes through the real glue: a hand-built CBOR frame in
//       the shape tentaflow-protocol writes, decoded by `BinaryWsClient` with
//       the wasm in www/js/protocol, and handed to the very functions the
//       screens word it with. Its own file for the reason alert-wire.test.js
//       gives: the wasm is initialised BEFORE codec.js is first imported.
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
const { T } = await import('./format.js');
const { scheduleOutcome } = await import('./tasks.js');
const { approvalDetail } = await import('./approvals.js');
const { elasticRefusalText, elasticPlanWarnings } = await import('./pool-wizard.js');
const { targetStateText, targetStateTitle } = await import('./targets.js');
const { kernelSupportText } = await import('./target-wizard.js');
const { elasticStateDetail, elasticStateTitle } = await import('./elastic-detail.js');
const { shareStateText } = await import('./shares.js');

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

// Sends `request` through a client with no socket and answers it with
// `TentaNasBody::<variant>`, the way alert-wire.test.js does.
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
const daily = { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 };
const JOB_ID = '0191f2c0-0000-7000-8000-00000000abcd';

test('a schedule row keeps its structured outcome through the real decoder, and no job id is shown', { skip }, async () => {
  const row = (kind, extra) => ({
    kind, subject: 'tank', enabled: true, schedule: daily, last_run_at: '2026-09-24T03:00:00Z',
    last_result: '', next_run_at: null, ...extra,
  });
  const body = await throughTheClient('tentaNasSchedulesListRequest', {}, 'SchedulesListResponse', {
    rows: [
      row('scrub', { last_result: `started job ${JOB_ID}`, last_outcome: 'started', last_job_status: 'failed' }),
      row('trim', { last_result: 'failed to start: busy', last_outcome: 'start_failed', last_detail: 'the pool is busy' }),
      row('elastic_scrub', { last_result: 'pominięto: reason:array_not_created', last_outcome: 'skipped', last_reason: 'array_not_created' }),
    ],
    smart: { enabled: false, short: daily, long: daily, last_short_at: null, last_long_at: null, next_short_at: null, next_long_at: null },
  });
  const [scrub, trim, skipped] = body.rows;
  assert.equal(scrub.lastOutcome, 'started');
  assert.equal(scrub.lastJobStatus, 'failed');
  assert.equal(trim.lastDetail, 'the pool is busy');
  assert.equal(skipped.lastReason, 'array_not_created');

  // The screen reads the structure: the job's status, never by its id.
  const asked = [];
  const statusOf = (id) => { asked.push(id); return null; };
  const scrubText = scheduleOutcome(scrub, statusOf);
  assert.deepEqual({ label: scrubText.label, failed: scrubText.failed }, { label: T('schedules.result_failed'), failed: true });
  assert.deepEqual(asked, [], 'no job is looked up by id');
  const trimText = scheduleOutcome(trim, statusOf);
  assert.equal(trimText.failed, true);
  assert.equal(trimText.title, 'the pool is busy');
  const skipText = scheduleOutcome(skipped, statusOf);
  assert.equal(skipText.skipped, true);
  assert.match(skipText.title, /tworzenie macierzy się nie zakończyło/);
  for (const text of [scrubText, trimText, skipText]) {
    assert.ok(!JSON.stringify(text).includes(JOB_ID), JSON.stringify(text));
  }
});

test('a parked request keeps its coded detail through the real decoder and is worded for the approver', { skip }, async () => {
  const approval = (id, extra) => ({
    request_id: id, operation: 'elastic_schedule', subject: 'media', detail: '', status: 'pending',
    requested_by: 'u-1', requested_at: '2026-09-24T00:00:00Z', expires_at: '2026-09-25T00:00:00Z',
    decided_by: null, decided_at: null, decision_note: '', decision_job_id: null, is_own_request: false, ...extra,
  });
  const body = await throughTheClient('tentaNasApprovalsListRequest', { includeClosed: false }, 'ApprovalsListResponse', {
    approvals: [
      approval('r-1', {
        detail: 'arms the schedule: mover of array media, hourly, at minute :30; files older than 1800 s',
        detail_reasons: [reason('elastic_schedule', {
          task: 'mover', enabled: 'true', every: '1h', hour: '0', minute: '30', weekday: '0', day: '1',
          min_age_secs: '1800', cache_min_free_pct: '35', coupled_sync: 'false',
        })],
      }),
      approval('r-2', {
        operation: 'elastic_fix',
        detail: "writes back from parity the blocks the last scrub marked bad; recorded against disk 'd2'",
        detail_reasons: [reason('elastic_fix', { disk: '', number: '2' })],
      }),
    ],
    settings: { enabled: true, ttl_hours: 24, admin_count: 2, by_default: false },
  });
  const [schedule, fix] = body.approvals;
  assert.equal(schedule.detailReasons[0].code, 'elastic_schedule');
  assert.equal(schedule.detailReasons[0].params.cache_min_free_pct, '35');

  const scheduleText = approvalDetail(schedule);
  assert.match(scheduleText.text, /^Uzbraja harmonogram: mover, co 1 h\. /);
  assert.match(scheduleText.text, /Pliki starsze niż 30 min, próg wolnego cache 35%, sprzężony Sync: nie\.$/);
  assert.match(scheduleText.title, /^arms the schedule/, 'the node’s sentence is the tooltip');
  const fixText = approvalDetail(fix);
  assert.match(fixText.text, /na dysku danych nr 2\.$/);
  assert.ok(!/\bd2\b/.test(fixText.text), 'the slot is never the name');
});

test('an Elastic preview keeps its coded refusals and warnings through the real decoder', { skip }, async () => {
  const body = await throughTheClient('tentaNasElasticArrayPlanRequest', { name: 'media', filesystem: 'xfs', dataDiskIds: ['a'], parityDiskIds: ['b'], cacheDiskIds: [] }, 'ElasticArrayPlanResponse', {
    plan: {
      usable_bytes: 0, raw_bytes: 0, parity_bytes: 0, cache_bytes: 0, fault_tolerance: 1,
      refusals: [{
        code: 'parity_too_small', disk_id: 'wwn-0x5000c500a1b2c3d4', disk_name: 'sde',
        detail: 'sde holds 4.0 TB and the largest data disk holds 8.0 TB',
        params: { size: '4000000000000', largest: '8000000000000' },
      }],
      warnings: ['no cache disk: there is nothing for the mover to do'],
      union_path: '/mnt/media', wiped_devices: [], steps_preview: '',
      warning_codes: [reason('no_cache')],
    },
  });
  const [refusal] = body.plan.refusals;
  assert.deepEqual({ ...refusal.params }, { size: '4000000000000', largest: '8000000000000' });
  const worded = elasticRefusalText(refusal);
  assert.match(worded.text, /^sde \(3\.6 TiB\) nie może być parity — jest mniejszy niż największy dysk danych \(7\.3 TiB\)\.$/);
  assert.ok(!worded.text.includes('wwn-'), worded.text);
  assert.equal(worded.title, refusal.detail);
  assert.deepEqual(elasticPlanWarnings(body.plan), ['Bez dysku cache: mover nie ma nic do roboty, a nowe pliki trafiają od razu na dyski danych.']);
});

test('a target, its services and the capabilities keep their coded reasons through the real decoder', { skip }, async () => {
  const target = {
    target_id: 't-1', name: 'scratch', protocol: 'nvmet', wwn: 'nqn.2026-09.local.tentaflow:helios.scratch', enabled: true,
    luns: [], portals: [], auth: { method: 'none' }, initiators: [], port_groups: [], sessions: 0, sessions_known: false,
    state: 'error',
    state_detail: 'portal 10.10.0.7 is not on storage1 any more — storage1 now has 10.10.0.9, and the address moved to bond0',
    created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-06T11:42:00Z',
    state_reasons: [reason('target_portal_moved', { address: '10.10.0.7', interface: 'storage1', current: '10.10.0.9', elsewhere: 'bond0', in_kernel: 'false' })],
  };
  const body = await throughTheClient('tentaNasTargetsListRequest', {}, 'TargetsListResponse', {
    targets: [target],
    services: [{
      protocol: 'nvmet', installed: false, running: false, version: null, config_path: '/sys/kernel/config/nvmet',
      detail: 'this kernel has no nvmet, nvmet-tcp — the nvmet target is not built for it',
      reasons: [reason('modules_missing', { modules: 'nvmet, nvmet-tcp', protocol: 'nvmet' })],
    }],
    capabilities: {
      iscsi: true, nvmet: false, iser: false, nvme_rdma: false, dhchap: false,
      iscsi_detail: '', nvmet_detail: 'this kernel has no nvmet', rdma_detail: '', dhchap_detail: 'this kernel publishes no configuration',
      iscsi_reasons: [], nvmet_reasons: [reason('modules_missing', { modules: 'nvmet', protocol: 'nvmet' })],
      rdma_reasons: [], dhchap_reasons: [reason('kernel_config_missing')],
      interfaces: [], volumes: [], wwn_host: 'helios',
    },
  });
  const [t] = body.targets;
  assert.equal(t.stateReasons[0].params.elsewhere, 'bond0');
  // N19b in the reader's language, the node's English as the tooltip.
  const text = targetStateText(t);
  assert.match(text, /^Portal targetu nie jest już tam, gdzie go przypięto\. Adres 10\.10\.0\.7 należał do interfejsu storage1 \(który ma teraz 10\.10\.0\.9\)\./);
  assert.match(text, /Target nie jest w jądrze, więc pod tym adresem nic nie nasłuchuje — eksport nie jest osiągalny na bond0 i nie zostanie tam przepięty automatycznie\./);
  assert.equal(targetStateTitle(t), t.stateDetail);

  const [service] = body.services;
  assert.equal(kernelSupportText(service.reasons, service.detail), 'to jądro nie ma modułów nvmet, nvmet-tcp — nie zbudowano go z targetem NVMe-oF');
  assert.match(kernelSupportText(body.capabilities.dhchapReasons, body.capabilities.dhchapDetail), /^to jądro nie publikuje swojej konfiguracji/);
  assert.equal(kernelSupportText(body.capabilities.nvmetReasons, body.capabilities.nvmetDetail), 'to jądro nie ma modułów nvmet — nie zbudowano go z targetem NVMe-oF');
});

test('an Elastic Array keeps its coded state through the real decoder, members by name or number', { skip }, async () => {
  const array = {
    name: 'media', kind: 'elastic-array', state: 'pending',
    state_detail: 'data disk #1, data disk sdh are present and not mounted yet — the next reconcile mounts them and then the union',
    state_reasons: [reason('branches_mountable', { data: '#1,sdh', parity: 'sdj' })],
    health: 'unknown', health_reason: '', enabled: true, union_path: '/mnt/media', create_policy: 'mfs', filesystem: 'xfs',
    data_disks: [], cache_disks: [], parity_disks: [], folders: [], folders_known: false,
    mover: { enabled: false, schedule: null, min_age_secs: 0, cache_min_free_pct: 20, coupled_sync: true, last_run: null },
    snapraid: {
      installed: true, version: '12.3', config_path: '/etc/snapraid-media.conf', last_sync: null, last_scrub: null, history: [],
      sync_schedule: null, scrub_schedule: null, scrub_percent: 8, scrub_older_than_days: 10, parity_errors: null, parity_errors_window_days: 30,
    },
    protection: { cache_unprotected_bytes: null, moved_unsynced_bytes: null, status: 'unknown', detail: '', fault_tolerance: null, protected_as_of: null },
    usable_bytes: null, used_bytes: null, cache_size_bytes: null, cache_used_bytes: null,
    created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-01T00:00:00Z',
  };
  const body = await throughTheClient('tentaNasElasticArraysListRequest', {}, 'ElasticArraysListResponse', { arrays: [array] });
  const [a] = body.arrays;
  assert.equal(a.stateReasons[0].code, 'branches_mountable');
  assert.equal(elasticStateDetail(a), 'Obecne i jeszcze niezamontowane: dysk danych nr 1, dysk danych sdh, dysk parity sdj. Następne uzgodnienie zamontuje je, a potem unię.');
  assert.equal(elasticStateTitle(a), a.stateDetail);
});

// Wave 8: a share's state detail travels as codes too (`NasShare.state_reasons`,
// `#[serde(default)]`). Through the real decoder: an unmounted source and an
// SMB Direct refusal with the node's own ksmbd reasons, both worded; the
// English — with its path — only in the tooltip.
test('a share keeps its coded state through the real decoder and is worded from it', { skip }, async () => {
  const share = (name, state, detail, reasons) => ({
    share_id: `s-${name}`, name, protocol: 'smb', source_path: `/mnt/tank/${name}`, dataset: `tank/${name}`, enabled: true,
    smb: null, nfs: null, fleet_mount: false, mounts: [], sessions: 0, state, state_detail: detail,
    created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-01T00:00:00Z', state_reasons: reasons,
  });
  const body = await throughTheClient('tentaNasSharesListRequest', {}, 'SharesListResponse', {
    shares: [
      share('projekty', 'error', 'source path is not mounted — the share stays out of the config', [reason('share_source_unmounted')]),
      share('media', 'active', 'SMB Direct is not served on this node: no RDMA interface with an address · EXPERIMENTAL (kernel docs)',
        [reason('smb_direct_not_served'), reason('ksmbd_no_interface'), reason('ksmbd_experimental')]),
      share('stary', 'error', "'/mnt/tank/stary' is not a directory on this node", []),
    ],
    services: [], users: [], mount_root: '/mnt/tentanas',
  });
  const [unmounted, direct, old] = body.shares;
  assert.equal(unmounted.stateReasons[0].code, 'share_source_unmounted', 'the field survives the decoder');
  assert.deepEqual(shareStateText(unmounted), {
    text: 'Źródło udziału nie jest zamontowane — udział nie trafia do konfiguracji',
    title: 'source path is not mounted — the share stays out of the config',
  });
  const worded = shareStateText(direct).text;
  assert.match(worded, /^SMB Direct nie jest obsługiwany na tym węźle \(udział działa dalej przez sieć LAN\) · /);
  assert.doesNotMatch(worded, /is not served|no RDMA interface/, 'no English outside the tooltip');
  // A row judged before the codes: the sentence, as it came.
  assert.deepEqual(shareStateText(old), { text: "'/mnt/tank/stary' is not a directory on this node", title: '' });
});
