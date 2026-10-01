// ============ File: protocol/project-access-wire.test.js — project access through the actual Wasm codec ============

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const artifact = new URL('./wasm_glue_bg.wasm', import.meta.url);
const skip = existsSync(artifact) ? false : 'Build tentaflow-core to generate the Wasm codec';
const wasm = skip ? null : await import('./wasm_glue.js');
let codec;
if (!skip) {
  await wasm.default({ module_or_path: readFileSync(artifact) });
  codec = await import('./codec.js');
  await codec.codecReady;
}

function cbor(value) {
  if (value === null) return [0xf6];
  if (typeof value === 'boolean') return [value ? 0xf5 : 0xf4];
  const head = (major, length) => length < 24 ? [(major << 5) | length] : [(major << 5) | 24, length];
  if (typeof value === 'string') {
    const bytes = [...new TextEncoder().encode(value)];
    return [...head(3, bytes.length), ...bytes];
  }
  if (Array.isArray(value)) return [...head(4, value.length), ...value.flatMap(cbor)];
  const entries = Object.entries(value);
  return [...head(5, entries.length), ...entries.flatMap(([key, item]) => [...cbor(key), ...cbor(item)])];
}

function sent(name, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[name](7, payload, 1));
  try {
    return wasm.decodeMessageBody(envelope.body);
  } finally {
    envelope.free();
  }
}

test('member creation carries multiple functions, a separate administrator flag and an exact expiry', { skip }, () => {
  const member = { userId: 'u1', functions: ['developer', 'tester'], projectAdmin: true, expiresAt: '2026-12-31T23:59:59Z' };
  for (const name of ['projectStudioProjectCreateRequest', 'projectStudioMembersAddRequest']) {
    const decoded = sent(name, { projectId: 'p1', name: 'Project', template: 'custom', modules: ['tasks'], members: [member] });
    assert.equal(decoded.members[0].role, '');
    assert.deepEqual(decoded.members[0].functions, ['developer', 'tester']);
    assert.equal(decoded.members[0].projectAdmin, true);
    assert.equal(decoded.members[0].expiresAt, member.expiresAt);
  }
});

test('nested access requests preserve their pinned names and authoritative fields', { skip }, () => {
  const decoded = sent('projectStudioMemberAccessSetRequest', { projectId: 'p1', userId: 'u1', functions: ['designer'], projectAdmin: false, expiresAt: null });
  assert.equal(decoded.variant, 'ProjectStudioMemberAccessSetRequest');
  assert.equal(decoded.projectId, 'p1');
  assert.deepEqual(decoded.functions, ['designer']);
  assert.equal(decoded.projectAdmin, false);
  assert.equal(decoded.expiresAt, null);
  assert.equal(sent('projectStudioCatalogueGetRequest', { projectId: 'p1' }).variant, 'ProjectStudioCatalogueGetRequest');
  const deleted = sent('projectStudioFunctionDeleteRequest', { projectId: 'p1', functionId: 'custom' });
  assert.equal(deleted.variant, 'ProjectStudioFunctionDeleteRequest');
  assert.equal(deleted.functionId, 'custom');
});

test('function save keeps per-area levels and does not invent legacy roles', { skip }, () => {
  const decoded = sent('projectStudioFunctionSaveRequest', {
    projectId: 'p1', function: { functionId: 'custom', name: 'Review', description: '', builtin: false, grants: [{ area: 'tests', level: 'write' }, { area: 'security.confidential', level: 'none' }] },
  });
  assert.equal(decoded.variant, 'ProjectStudioFunctionSaveRequest');
  assert.deepEqual(decoded.function.grants, [{ area: 'tests', level: 'write' }, { area: 'security.confidential', level: 'none' }]);
  assert.equal(decoded.function.functionId, 'custom');
  assert.equal(Object.hasOwn(decoded.function, 'role'), false);
});

test('server catalogue answers flatten the nested family and retain caller-visible grants', { skip }, () => {
  const answer = { ProjectStudioBody: { Access: { CatalogueGetResponse: { functions: [{ function_id: 'tester', name: 'QA', description: '', builtin: true, grants: [{ area: 'tests', level: 'admin' }] }] } } } };
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor(answer)));
  assert.equal(decoded.variant, 'ProjectStudioCatalogueGetResponse');
  assert.equal(decoded.functions[0].functionId, 'tester');
  assert.equal(decoded.functions[0].name, 'QA');
  assert.equal(decoded.functions[0].grants[0].level, 'admin');
});
