// =============================================================================
// Plik: protocol/tentanas-elastic-codec.test.js
// Opis: Rzeczywisty roundtrip adapterów Elastic przez codec i WASM.
// Przykład: node --test js/protocol/tentanas-elastic-codec.test.js
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const artifact = new URL('./wasm_glue_bg.wasm', import.meta.url);
const skip = existsSync(artifact) ? false : 'Brak wygenerowanego WASM; roundtrip nie został wykonany';
const wasm = skip ? null : await import('./wasm_glue.js');
let codec;
if (!skip) {
  await wasm.default({ module_or_path: readFileSync(artifact) });
  codec = await import('./codec.js');
  await codec.codecReady;
}

const cases = [
  ['ElasticCapabilities', {}, {}],
  ['ElasticArrayPlan', {
    name: 'archiwum', filesystem: 'ext4', dataDiskIds: ['data-a', 'data-b'],
    parityDiskIds: ['parity-a'], cacheDiskIds: [],
  }, {
    name: 'archiwum', filesystem: 'ext4', data_disk_ids: ['data-a', 'data-b'],
    parity_disk_ids: ['parity-a'], cache_disk_ids: [],
  }],
  ['ElasticArrayCreate', {
    name: 'archiwum', filesystem: 'xfs', data_disk_ids: ['data-a'],
    parity_disk_ids: ['parity-a', 'parity-b'], confirm_name: 'archiwum',
    cacheDiskIds: ['cache-a'],
    sudo_password: 'test-secret-not-real',
  }, {
    name: 'archiwum', filesystem: 'xfs', data_disk_ids: ['data-a'],
    parity_disk_ids: ['parity-a', 'parity-b'], confirm_name: 'archiwum',
    cache_disk_ids: ['cache-a'],
    sudo_password: 'test-secret-not-real',
  }],
  ['ElasticArraysList', {}, {}],
  ['ElasticArrayGet', { name: 'archiwum' }, { name: 'archiwum' }],
  ['ElasticArrayRestore', { name: 'archiwum', sudoPassword: 'test-secret-not-real' },
    { name: 'archiwum', sudo_password: 'test-secret-not-real' }],
  ['ElasticArraySync', { name: 'archiwum', sudoPassword: 'test-secret-not-real' },
    { name: 'archiwum', sudo_password: 'test-secret-not-real' }],
  ['ElasticArrayScrub', { name: 'archiwum', sudo_password: 'test-secret-not-real' },
    { name: 'archiwum', sudo_password: 'test-secret-not-real' }],
  // E2-09 shipped this variant with no encoder at all, so the button could not
  // reach the wire; it is pinned here beside its siblings now.
  ['ElasticArrayMover', { name: 'archiwum', sudoPassword: 'test-secret-not-real' },
    { name: 'archiwum', sudo_password: 'test-secret-not-real' }],
  ['ElasticMoverScheduleSet', {
    name: 'archiwum', enabled: true, schedule: { every: '1h', hour: 0, minute: 30, weekday: 0, day: 1 },
    minAgeSecs: 7200, cacheMinFreePct: 20, coupledSync: true,
  }, {
    name: 'archiwum', enabled: true, schedule: { every: '1h', hour: 0, minute: 30, weekday: 0, day: 1 },
    min_age_secs: 7200, cache_min_free_pct: 20, coupled_sync: true,
  }],
  ['ElasticSyncScheduleSet', {
    name: 'archiwum', enabled: true, schedule: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 },
  }, {
    name: 'archiwum', enabled: true, schedule: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 },
  }],
  ['ElasticScrubScheduleSet', {
    name: 'archiwum', enabled: false, schedule: { every: 'weekly', hour: 4, minute: 0, weekday: 0, day: 1 },
  }, {
    name: 'archiwum', enabled: false, schedule: { every: 'weekly', hour: 4, minute: 0, weekday: 0, day: 1 },
  }],
];

for (const [name, request, expected] of cases) {
  test(`${name}Request: rzeczywisty codec → WASM → envelope → payload`, { skip }, () => {
    assert.equal(typeof wasm[`encodeTentaNas${name}Request`], 'function',
      'Istniejący, ale nieaktualny artefakt WASM nie zalicza testu');
    const envelope = wasm.decodeEnvelope(codec.encode[`tentaNas${name}Request`](71, request, 9));
    try {
      assert.equal(envelope.correlation_id, 71n);
      assert.equal(envelope.sequence, 9n);
      assert.equal(envelope.is_forward, false);
      const payload = wasm.decodeMessageBody(envelope.body);
      assert.equal(payload.variant, `TentaNas${name}Request`);
      for (const [key, value] of Object.entries(expected)) {
        assert.deepEqual(payload[key], value, key);
      }
    } finally {
      envelope.free();
    }
  });
}

// n15's row toggle sends the cadence and no rules. On the real wire they must
// arrive ABSENT: a `0` would be stored as "move every file, never trigger" —
// settings the admin never chose.
test('przełącznik movera nie wysyła reguł: nieobecne, nie zerowe', { skip }, () => {
  const envelope = wasm.decodeEnvelope(codec.encode.tentaNasElasticMoverScheduleSetRequest(73, {
    name: 'archiwum', enabled: false, schedule: { every: '1h', hour: 0, minute: 30, weekday: 0, day: 1 },
  }));
  try {
    const payload = wasm.decodeMessageBody(envelope.body);
    assert.equal(payload.enabled, false);
    for (const field of ['min_age_secs', 'cache_min_free_pct', 'coupled_sync']) {
      assert.notEqual(payload[field], 0, `${field} nie może przyjechać jako zero`);
      assert.equal(payload[field] ?? null, null, field);
    }
  } finally {
    envelope.free();
  }
});

test('Create bez hasła zachowuje jawną nazwę potwierdzenia i puste parity', { skip }, () => {
  const envelope = wasm.decodeEnvelope(codec.encode.tentaNasElasticArrayCreateRequest(72, {
    name: 'dane', filesystem: 'ext4', dataDiskIds: ['data-a'], confirmName: 'inna',
  }));
  try {
    const payload = wasm.decodeMessageBody(envelope.body);
    assert.equal(payload.confirm_name, 'inna');
    assert.deepEqual(payload.parity_disk_ids, []);
    assert.equal(Object.hasOwn(payload, 'sudo_password'), false);
  } finally {
    envelope.free();
  }
});
