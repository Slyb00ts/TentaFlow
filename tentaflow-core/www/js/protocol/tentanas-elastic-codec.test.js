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
