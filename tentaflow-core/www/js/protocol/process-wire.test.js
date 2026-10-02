// ============ File: process-wire.test.js — BPMN B1 typed CBOR and dynamic-key preservation ============

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

function request(name, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[name](17, payload));
  try { return wasm.decodeMessageBody(envelope.body); }
  finally { envelope.free(); }
}

function cbor(value) {
  if (value === null) return [0xf6];
  if (typeof value === 'boolean') return [value ? 0xf5 : 0xf4];
  if (typeof value === 'number') {
    if (value >= 0 && value < 24) return [value];
    if (value >= 0 && value < 256) return [0x18, value];
    throw new RangeError('test CBOR integer is out of fixture bounds');
  }
  const head = (major, length) => length < 24 ? [(major << 5) | length] : [(major << 5) | 24, length];
  if (typeof value === 'string') {
    const bytes = [...new TextEncoder().encode(value)];
    return [...head(3, bytes.length), ...bytes];
  }
  if (Array.isArray(value)) return [...head(4, value.length), ...value.flatMap(cbor)];
  const entries = Object.entries(value);
  return [...head(5, entries.length), ...entries.flatMap(([key, item]) => [...cbor(key), ...cbor(item)])];
}

test('BPMN typed save retains mapping and variable business keys', { skip }, () => {
  const model = {
    schemaVersion: 1, processId: 'P_1', variables: { user_id: 'u1', nested_value: { inner_key: 4 } },
    nodes: [
      { id: 'Start_1', name: 'Start', kind: 'Start' },
      { id: 'Service_1', name: 'Service', kind: { ServiceTask: {
        flowId: 'f1', inputMapping: { special_key: 'vars.user_id' },
        outputMapping: { result_key: 'outputs.payload' }, verification: { Condition: { expression: 'outputs.ok' } }, timeoutSeconds: 60,
      } } },
      { id: 'End_1', name: 'End', kind: 'End' },
    ],
    sequenceFlows: [
      { id: 'Edge_1', sourceId: 'Start_1', targetId: 'Service_1', condition: null },
      { id: 'Edge_2', sourceId: 'Service_1', targetId: 'End_1', condition: null },
    ],
    diagram: { shapes: [], edges: [] },
  };
  const body = request('processDefinitionSaveRequest', {
    commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd', definitionId: null,
    expectedRevision: 0, name: 'BPMN', description: '', model,
  });
  assert.equal(body.variant, 'ProcessDefinitionSaveRequest');
  assert.deepEqual(body.model.variables, model.variables);
  assert.deepEqual(body.model.nodes[1].kind.ServiceTask.inputMapping, { special_key: 'vars.user_id' });
  assert.deepEqual(body.model.nodes[1].kind.ServiceTask.outputMapping, { result_key: 'outputs.payload' });
  assert.equal(body.model.nodes[1].kind.ServiceTask.timeoutSeconds, 60);
  assert.equal(Object.hasOwn(body.model.nodes[1].kind.ServiceTask, 'timeout_seconds'), false);
});

test('paged list request and response use canonical camel keys only', { skip }, () => {
  const requestBody = request('processDefinitionListRequest', { offset: 10, limit: 25 });
  assert.equal(requestBody.variant, 'ProcessDefinitionListRequest');
  assert.equal(requestBody.offset, 10);
  assert.equal(requestBody.limit, 25);
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    DefinitionListResponse: { definitions: [{ definition_id: 'd1', name: 'Test', description: '',
      owner_user_id: 'u1', draft_revision: 1, published_version: null, archived: false }], total: 1, has_more: false },
  } })));
  assert.equal(decoded.variant, 'ProcessDefinitionListResponse');
  assert.equal(decoded.definitions[0].definitionId, 'd1');
  assert.equal(decoded.hasMore, false);
  assert.equal(Object.hasOwn(decoded, 'has_more'), false);
});

test('user task detail request keeps explicit instance and task identity', { skip }, () => {
  const body = request('processUserTaskGetRequest', { instanceId: 'i1', userTaskId: 't1' });
  assert.equal(body.variant, 'ProcessUserTaskGetRequest');
  assert.equal(body.instanceId, 'i1');
  assert.equal(body.userTaskId, 't1');
  assert.equal(Object.hasOwn(body, 'user_task_id'), false);
});

test('user task detail response preserves opaque output keys', { skip }, () => {
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    UserTaskGetResponse: { task: {
      user_task_id: 't1', node_id: 'Review_1', name: 'Review', assignee_user_id: 'u1',
      kind: 'Work', status: 'Open', outputs: { business_key: { inner_value: 3 } },
      revision: 1, can_complete: true,
    } },
  } })));
  assert.equal(decoded.variant, 'ProcessUserTaskGetResponse');
  assert.equal(decoded.task.userTaskId, 't1');
  assert.deepEqual(decoded.task.outputs, { business_key: { inner_value: 3 } });
  assert.equal(Object.hasOwn(decoded.task, 'user_task_id'), false);
});
