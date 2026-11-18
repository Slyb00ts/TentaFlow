// ============ File: sla-wire.test.js — SLA policy requests encode into the wire's own SlaBody shape ============

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const artifact = new URL('./wasm_glue_bg.wasm', import.meta.url);
const present = existsSync(artifact);
const skip = present ? false : 'wasm_glue_bg.wasm is missing: build tentaflow-core to generate the Wasm codec';
const wasm = skip ? null : await import('./wasm_glue.js');
let codec;
if (!skip) {
  await wasm.default({ module_or_path: readFileSync(artifact) });
  codec = await import('./codec.js');
  await codec.codecReady;
}

// A missing artifact must be loud: the wire tests below are skipped with their reason, and this one fails.
test('the Wasm codec artifact this file encodes with is present', () => {
  assert.ok(present, 'wasm_glue_bg.wasm is missing, so none of the SLA wire tests ran');
});

function request(name, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[name](17, payload));
  try { return wasm.decodeMessageBody(envelope.body); }
  finally { envelope.free(); }
}

const SPEC = {
  name: 'Gold', description: 'd', maxConcurrency: 4, maxQueue: 8, queueTimeoutMs: 10000, callLimit: 60,
  callWindowSecs: 60, timeoutMs: 30000, latencyP95Ms: 500, errorRatePct: 5, warnAtPct: 80, ordering: 'none',
};

test('a policy list request carries no organization unless one is named', { skip }, () => {
  const own = request('slaPolicyListRequest', {});
  assert.equal(own._variant, 'SlaBody.PolicyListRequest');
  assert.equal(own.orgId ?? null, null);
  assert.equal(request('slaPolicyListRequest', { orgId: 'org-b' }).orgId, 'org-b');
});

test('an upsert without a policy id creates and keeps every limit of the spec', { skip }, () => {
  const create = request('slaPolicyUpsertRequest', { spec: SPEC });
  assert.equal(create._variant, 'SlaBody.PolicyUpsertRequest');
  assert.equal(create.policyId ?? null, null);
  assert.deepEqual(
    { ...create.spec },
    { name: 'Gold', description: 'd', maxConcurrency: 4, maxQueue: 8, queueTimeoutMs: 10000, callLimit: 60,
      callWindowSecs: 60, timeoutMs: 30000, latencyP95Ms: 500, errorRatePct: 5, warnAtPct: 80, ordering: 'none' },
  );
  assert.equal(request('slaPolicyUpsertRequest', { policyId: 'p-1', spec: SPEC }).policyId, 'p-1');
});

test('an empty limit travels as null and ordering falls back to partition', { skip }, () => {
  const sent = request('slaPolicyUpsertRequest', { spec: { name: 'Min', maxConcurrency: null, callLimit: undefined } });
  assert.equal(sent.spec.maxConcurrency ?? null, null);
  assert.equal(sent.spec.callLimit ?? null, null);
  assert.equal(sent.spec.ordering, 'partition');
});

test('get, delete, assign and unassign name the policy and the flow', { skip }, () => {
  assert.equal(request('slaPolicyGetRequest', { policyId: 'p-1' }).policyId, 'p-1');
  assert.equal(request('slaPolicyDeleteRequest', { policyId: 'p-2' }).policyId, 'p-2');
  const assign = request('slaAssignRequest', { flowId: 'f-1', policyId: 'p-1' });
  assert.equal(assign._variant, 'SlaBody.AssignRequest');
  assert.deepEqual([assign.flowId, assign.policyId], ['f-1', 'p-1']);
  assert.equal(request('slaUnassignRequest', { flowId: 'f-1' }).flowId, 'f-1');
});

test('status and breach list requests carry their filters', { skip }, () => {
  assert.equal(request('slaStatusRequest', {}).flowId ?? null, null);
  assert.equal(request('slaStatusRequest', { flowId: 'f-1' }).flowId, 'f-1');
  const breaches = request('slaBreachListRequest', { flowId: 'f-1', openOnly: true, offset: 50, limit: 25 });
  assert.equal(breaches._variant, 'SlaBody.BreachListRequest');
  assert.deepEqual([breaches.flowId, breaches.openOnly, breaches.offset, breaches.limit], ['f-1', true, 50, 25]);
  const defaults = request('slaBreachListRequest', {});
  assert.deepEqual([defaults.openOnly, defaults.offset, defaults.limit], [false, 0, 0]);
});

test('the wasm codec refuses a wrongly typed policy field instead of sending it', { skip }, () => {
  assert.throws(() => codec.encode.slaPolicyUpsertRequest(1, { spec: { name: 'x', ordering: 5 } }));
});
