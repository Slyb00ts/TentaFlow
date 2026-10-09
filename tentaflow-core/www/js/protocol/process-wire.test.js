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
  const head = (major, length) => {
    if (length < 24) return [(major << 5) | length];
    if (length < 0x100) return [(major << 5) | 24, length];
    if (length < 0x10000) return [(major << 5) | 25, length >> 8, length & 0xff];
    return [(major << 5) | 26, length >>> 24, (length >>> 16) & 0xff,
      (length >>> 8) & 0xff, length & 0xff];
  };
  if (typeof value === 'string') {
    const bytes = [...new TextEncoder().encode(value)];
    return [...head(3, bytes.length), ...bytes];
  }
  if (Array.isArray(value)) return [...head(4, value.length), ...value.flatMap(cbor)];
  const entries = Object.entries(value);
  return [...head(5, entries.length), ...entries.flatMap(([key, item]) => [...cbor(key), ...cbor(item)])];
}

test('link definitions preserve exact typed source and target references', { skip }, () => {
  const base = { schemaVersion: 1, processId: 'P_1', variables: {},
    sequenceFlows: [], diagram: { shapes: [], edges: [] } };
  const nodes = [{ id: 'Throw_1', name: 'Go', kind: { LinkThrow: { definition: {
    id: 'Link_Throw', name: 'repeat', sourceRefs: [], targetRef: 'Link_Catch',
  } } } }, { id: 'Catch_1', name: 'Resume', kind: { LinkCatch: { definition: {
    id: 'Link_Catch', name: 'repeat', sourceRefs: ['Link_Throw'], targetRef: null,
  } } } }];
  const saved = request('processDefinitionSaveRequest', { commandId: 'link', definitionId: null,
    expectedRevision: 0, name: 'Link', description: '', model: { ...base, nodes } });
  assert.equal(saved.model.nodes[0].kind.LinkThrow.definition.targetRef, 'Link_Catch');
  assert.deepEqual(saved.model.nodes[1].kind.LinkCatch.definition.sourceRefs, ['Link_Throw']);
  assert.equal(saved.model.nodes[0].kind.LinkThrow.definition.name, 'repeat');
  assert.equal(saved.model.nodes[1].kind.LinkCatch.definition.name, 'repeat');
  assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'bad-link', definitionId: null,
    expectedRevision: 0, name: 'Link', description: '', model: { ...base, nodes: [{ ...nodes[0],
      kind: { LinkThrow: { definition: { ...nodes[0].kind.LinkThrow.definition, sourceRefs: 'Link_Throw' } } },
    }] } }), /link definition requires/);
});

test('escalation wire preserves typed declarations, boundary mapping and diagnostic context', { skip }, () => {
  const model = { schemaVersion: 1, processId: 'P_1', targetNamespace: 'urn:example:review',
    escalations: [{ escalationId: 'Esc_1', name: 'Review & approve', escalationCode: 'NEEDS.HUMAN' }],
    nodes: [{ id: 'Boundary_1', name: 'Review', kind: { BoundaryEscalation: {
      attachedToId: 'Service_1', escalationRef: 'Esc_1', cancelActivity: false,
      outputMapping: { business_key: 'outputs.customer_ID' },
    } } }], sequenceFlows: [], variables: { customer_ID: { attached_to_id: 'kept' } },
    diagram: { shapes: [], edges: [] } };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Review', description: '', model });
  assert.equal(saved.model.escalations[0].escalationCode, 'NEEDS.HUMAN');
  assert.equal(saved.model.nodes[0].kind.BoundaryEscalation.escalationRef, 'Esc_1');
  assert.equal(saved.model.nodes[0].kind.BoundaryEscalation.cancelActivity, false);
  assert.deepEqual(saved.model.nodes[0].kind.BoundaryEscalation.outputMapping,
    { business_key: 'outputs.customer_ID' });
  assert.deepEqual(saved.model.variables, model.variables);
  const diagnostic = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    XmlImportResponse: { model: null, diagnostics: [{ code: 'ESCALATION_IMMEDIATE_PATH_UNSUPPORTED',
      message: 'terminal_before_wait', element_id: 'Flow_1', offset: 42, fatal: true,
      boundary_id: 'Boundary_1', flow_id: 'Flow_1', node_id: 'End_1',
      reason: 'terminal_before_wait' }] },
  } })));
  assert.equal(diagnostic.diagnostics[0].boundaryId, 'Boundary_1');
  assert.equal(diagnostic.diagnostics[0].flowId, 'Flow_1');
  assert.equal(diagnostic.diagnostics[0].reason, 'terminal_before_wait');
});

test('inclusive gateway save keeps the selected default and opaque variables', { skip }, () => {
  const model = {
    schemaVersion: 1, processId: 'P_1',
    nodes: [
      { id: 'OR_Split', name: 'Select', kind: { InclusiveGateway: { defaultFlowId: 'Flow_Default' } } },
      { id: 'OR_Join', name: 'Join', kind: { InclusiveGateway: { defaultFlowId: null } } },
    ],
    sequenceFlows: [
      { id: 'Flow_A', sourceId: 'OR_Split', targetId: 'Task_A', condition: 'vars.business_key == true' },
      { id: 'Flow_Default', sourceId: 'OR_Split', targetId: 'Task_B', condition: null },
    ],
    variables: { business_key: { inner_value: 'Łódź & <ok>' } },
    diagram: { shapes: [], edges: [] },
  };
  const body = request('processDefinitionSaveRequest', {
    commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd', definitionId: null,
    expectedRevision: 0, name: 'Selection', description: '', model,
  });
  assert.deepEqual(body.model.nodes[0].kind, model.nodes[0].kind);
  assert.deepEqual(body.model.nodes[1].kind, model.nodes[1].kind);
  assert.equal(body.model.sequenceFlows[0].condition, 'vars.business_key == true');
  assert.deepEqual(body.model.variables, model.variables);
  assert.throws(() => request('processDefinitionSaveRequest', {
    commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd', definitionId: null,
    expectedRevision: 0, name: 'Selection', description: '',
    model: { ...model, nodes: [{ id: 'Bad', name: '', kind: { InclusiveGateway: { defaultFlowId: null, unknown: true } } }] },
  }), /unknown field|unsupported/i);
});

test('terminate end save keeps a unit node kind and opaque business keys', { skip }, () => {
  const model = { schemaVersion: 1, processId: 'P_1', nodes: [
    { id: 'Start_1', name: 'Start', kind: 'Start' },
    { id: 'Stop_1', name: 'Stop', kind: 'TerminateEnd' },
  ], sequenceFlows: [{ id: 'Flow_1', sourceId: 'Start_1', targetId: 'Stop_1', condition: null }],
  variables: { terminate_end: { attached_to_id: 'business value' } },
  diagram: { shapes: [], edges: [] } };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Stop', description: '', model });
  assert.equal(saved.model.nodes[1].kind, 'TerminateEnd');
  assert.deepEqual(saved.model.variables, model.variables);
});

test('script task save keeps exact body and explicit opaque output mapping', { skip }, () => {
  const base = { schemaVersion: 1, processId: 'P_1', sequenceFlows: [],
    variables: { amount: 2, business_key: null }, diagram: { shapes: [], edges: [] } };
  const kind = { ScriptTask: { script: 'vars.amount + 1', outputMapping: { business_key: 'outputs' } } };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Script', description: '', model: { ...base,
      nodes: [{ id: 'Script_1', name: 'Script', kind }] } });
  assert.deepEqual(saved.model.nodes[0].kind.ScriptTask,
    { script: 'vars.amount + 1', outputMapping: { business_key: 'outputs' } });
  assert.deepEqual(saved.model.variables, base.variables);
  for (const invalid of [
    { script: 'null' },
    { outputMapping: {} },
    { script: 'null', outputMapping: {}, unknown: true },
  ]) {
    assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
      expectedRevision: 0, name: 'Script', description: '', model: { ...base,
        nodes: [{ id: 'Script_1', name: 'Script', kind: { ScriptTask: invalid } }] } }),
    /required|unsupported|unknown|script task/i);
  }
});

test('repeated task save preserves typed input, loop metadata, and opaque variables', { skip }, () => {
  const base = { schemaVersion: 1, processId: 'P_1', sequenceFlows: [],
    variables: { results: [], items: [{ business_key: 'Łódź & <ok>' }] }, diagram: { shapes: [], edges: [] } };
  const kinds = [
    { MultiInstance: { mode: 'Sequential', input: { Cardinality: { count: 0 } }, outputCollectionVariable: 'results' } },
    { MultiInstance: { mode: 'Parallel', input: { CollectionExpression: { expression: 'vars.items' } }, outputCollectionVariable: 'results' } },
    { StructuredLoop: { condition: 'vars.again', testBefore: true, maxIterations: 32, outputCollectionVariable: 'results' } },
  ];
  for (const repeat of kinds) {
    const model = { ...base, nodes: [{ id: 'Review', name: 'Review', kind: { UserTask: {
      assigneeUserId: null, outputMapping: {} } }, repeat }] };
    const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
      expectedRevision: 0, name: 'Repeat', description: '', model });
    assert.deepEqual(saved.model.nodes[0].repeat, repeat);
    assert.deepEqual(saved.model.variables, base.variables);
  }
  const noRepeat = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Plain', description: '', model: { ...base, nodes: [
      { id: 'Review', name: '', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } }] } });
  assert.equal(Object.hasOwn(noRepeat.model.nodes[0], 'repeat'), false);
  assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Bad', description: '', model: { ...base, nodes: [{ id: 'Review', name: '',
      kind: { UserTask: { assigneeUserId: null, outputMapping: {} } },
      repeat: { StructuredLoop: { condition: 'true', testBefore: true, maxIterations: 1,
        outputCollectionVariable: 'results', unexpected: true } } }] } }), /unknown field|unsupported/i);
});

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

test('embedded subprocess wire preserves nested graph and opaque local variable keys', { skip }, () => {
  const body = { nodes: [
    { id: 'Child_Start', name: 'Enter', kind: 'Start' },
    { id: 'Child_End', name: 'Leave', kind: 'End' },
  ], sequenceFlows: [{ id: 'Child_Flow', sourceId: 'Child_Start', targetId: 'Child_End', condition: null }],
  variables: { customer_ID: { original_key: 7 } }, diagram: { shapes: [], edges: [] } };
  const model = { schemaVersion: 1, processId: 'P_1', nodes: [
    { id: 'Start_1', name: 'Start', kind: 'Start' },
    { id: 'Sub_1', name: 'Review', kind: { SubProcess: { body,
      inputMapping: { local_ID: 'vars.customer_ID' }, outputMapping: { returned_ID: 'outputs.customer_ID' } } } },
    { id: 'End_1', name: 'End', kind: 'End' },
  ], sequenceFlows: [], variables: { customer_ID: { original_key: 3 } }, diagram: { shapes: [], edges: [] } };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Review', description: '', model });
  const nested = saved.model.nodes[1].kind.SubProcess;
  assert.deepEqual(nested.body.variables, body.variables);
  assert.equal(nested.body.sequenceFlows[0].targetId, 'Child_End');
  assert.deepEqual(nested.inputMapping, { local_ID: 'vars.customer_ID' });
  assert.deepEqual(nested.outputMapping, { returned_ID: 'outputs.customer_ID' });
  assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Review', description: '', model: { ...model, nodes: [
      model.nodes[0], { ...model.nodes[1], kind: { SubProcess: { ...model.nodes[1].kind.SubProcess,
        body: { ...body, timerTimezone: 'UTC' } } } }, model.nodes[2],
    ] } }), /unsupported subprocess body field/);
});

test('message model and send wire keep typed structure and opaque business keys', { skip }, () => {
  const model = { schemaVersion: 1, processId: 'P_1', targetNamespace: 'urn:example:orders',
    messages: [{ messageId: 'Message_1', name: 'order.received' }],
    errors: [{ errorId: 'Error_1', name: 'Bad order', errorCode: 'BUSINESS.BAD' }],
    nodes: [
      { id: 'Start_1', name: 'Start', kind: { MessageStart: {
        messageRef: 'Message_1', outputMapping: { customer_ID: 'outputs.customer_ID' },
      } } },
      { id: 'Service_1', name: 'Check', kind: { ServiceTask: {
        flowId: 'flow', inputMapping: { attached_to_id: 'vars.customer_ID' }, outputMapping: {},
        verification: 'Human', timeoutSeconds: 60, resultExpression: 'vars.result',
      } } },
      { id: 'Boundary_1', name: 'Error', kind: { BoundaryError: {
        attachedToId: 'Service_1', errorRef: 'Error_1', outputMapping: {},
      } } },
      { id: 'End_1', name: 'End', kind: 'End' },
    ], sequenceFlows: [], variables: { customer_ID: { attached_to_id: 'kept' } },
    diagram: { shapes: [], edges: [] },
  };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Orders', description: '', model });
  assert.equal(saved.model.targetNamespace, 'urn:example:orders');
  assert.equal(saved.model.messages[0].messageId, 'Message_1');
  assert.equal(saved.model.errors[0].errorCode, 'BUSINESS.BAD');
  assert.deepEqual(saved.model.variables, model.variables);
  assert.deepEqual(saved.model.nodes[0].kind.MessageStart.outputMapping, { customer_ID: 'outputs.customer_ID' });
  assert.equal(saved.model.nodes[1].kind.ServiceTask.resultExpression, 'vars.result');
  const sent = request('processMessageSendRequest', { commandId: 'cmd', messageId: 'msg',
    target: { Catch: { definitionId: 'definition', instanceId: 'instance', subscriptionId: null } },
    messageName: 'order.received', correlationKey: 'case-1',
    payload: { customer_ID: { attached_to_id: null } }, ttlSeconds: 60 });
  assert.equal(sent.target.Catch.instanceId, 'instance');
  assert.deepEqual(sent.payload, { customer_ID: { attached_to_id: null } });
  assert.throws(() => codec.encode.processDefinitionSaveRequest(17, { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Orders', description: '', model: { ...model,
      nodes: [{ ...model.nodes[0], kind: { MessageStart: { messageRef: 'Message_1', outputMapping: {}, hidden: true } } }] } }),
  /unsupported message start field/);
});

test('call activity wire preserves exact QName binding, direct pins and terminal error', { skip }, () => {
  const model = { schemaVersion: 1, processId: 'Caller_1', targetNamespace: 'urn:example:caller',
    errors: [{ errorId: 'Error_1', name: 'Rejected', errorCode: 'BUSINESS.REJECTED' }],
    nodes: [
      { id: 'Start_1', name: 'Start', kind: 'Start' },
      { id: 'Call_1', name: 'Approval', kind: { CallActivity: {
        calledDefinitionId: 'definition-1', calledVersion: 7,
        calledElement: { namespaceUri: 'urn:example:approval', processId: 'Approval_1' },
        inputMapping: { customer_ID: 'vars.customer_ID' },
        outputMapping: { returned_value: 'outputs.business_key' },
      } } },
      { id: 'ErrorEnd_1', name: 'Rejected', kind: { ErrorEnd: { errorRef: 'Error_1' } } },
    ], sequenceFlows: [], variables: { customer_ID: { inner_value: 7 } },
    diagram: { shapes: [], edges: [] } };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Caller', description: '', model });
  const call = saved.model.nodes[1].kind.CallActivity;
  assert.equal(call.calledVersion, 7);
  assert.equal(call.calledElement.namespaceUri, 'urn:example:approval');
  assert.equal(call.calledElement.processId, 'Approval_1');
  assert.deepEqual(call.inputMapping, { customer_ID: 'vars.customer_ID' });
  assert.deepEqual(call.outputMapping, { returned_value: 'outputs.business_key' });
  assert.equal(saved.model.nodes[2].kind.ErrorEnd.errorRef, 'Error_1');
  assert.deepEqual(saved.model.variables, model.variables);
  for (const invalid of [
    { ...call, calledVersion: '7' },
    { ...call, calledElement: { namespaceUri: 'urn:example:approval', processId: 'Approval_1', extra: true } },
  ]) {
    const wrong = structuredClone(model);
    wrong.nodes[1].kind.CallActivity = invalid;
    assert.throws(() => codec.encode.processDefinitionSaveRequest(17, { commandId: 'cmd', definitionId: null,
      expectedRevision: 0, name: 'Caller', description: '', model: wrong }), TypeError);
  }
  const pinnedModel = { schema_version: 1, process_id: 'Caller_1', nodes: [], sequence_flows: [],
    variables: {}, diagram: { shapes: [], edges: [] } };
  const version = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    VersionGetResponse: { version: { definition_id: 'definition-1', version: 1, model: pinnedModel,
      published_at_ms: 1, published_by: 'owner', model_sha256: 'sha', service_flows: [],
      call_activities: [{ node_id: 'Call_1', called_definition_id: 'definition-2', called_version: 7,
        called_element: { namespace_uri: 'urn:example:approval', process_id: 'Approval_1' },
        model_sha256: 'target-sha' }] }, start_catalog: [] },
  } })));
  assert.equal(version.version.callActivities[0].calledElement.namespaceUri, 'urn:example:approval');
  assert.equal(version.version.callActivities[0].calledVersion, 7);
  const terminal = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    InstanceGetResponse: { instance: { instance_id: 'child-1', definition_id: 'definition-2',
      definition_name: 'Child', process_id: 'Approval_1', start_node_id: 'Start_1',
      initiator_user_id: 'owner', version: 7, revision: 2,
      status: 'Error', variables: {}, active_node_ids: [], user_tasks: [], incidents: [],
      created_at_ms: 1, updated_at_ms: 2, can_cancel: false, can_retry: false,
      calls: [{ Incoming: { parent: null } }],
      repetition_groups: [], repetition_occurrences: [], selected_repetition_occurrence: null,
      terminal_error: { error_ref: 'Error_1', error_code: 'BUSINESS.REJECTED',
        source_event_id: 'event-1', source_node_id: 'ErrorEnd_1', source_scope_id: 'child-1' } } },
  } })));
  assert.equal(terminal.instance.status, 'Error');
  assert.equal(terminal.instance.processId, 'Approval_1');
  assert.equal(terminal.instance.startNodeId, 'Start_1');
  assert.deepEqual(terminal.instance.calls, [{ Incoming: { parent: null } }]);
  assert.equal(terminal.instance.terminalError.errorCode, 'BUSINESS.REJECTED');
  assert.equal(Object.hasOwn(terminal.instance.calls[0].Incoming, 'callNodeId'), false);
});

test('selected bodies, modeling, data stores and activity IO survive the typed binary boundary', { skip }, () => {
  const diagram = { shapes: [], edges: [], modelingShapes: [{ diId: 'Shape_StoreRef',
    elementId: 'StoreRef_1', x: 40, y: 60, width: 140, height: 60 }],
  modelingEdges: [{ diId: 'Edge_Association', elementId: 'Association_1',
    waypoints: [{ x: 50, y: 60 }, { x: 90, y: 80 }] }] };
  const modeling = { laneSets: [{ id: 'LaneSet_1', lanes: [{ id: 'Lane_1', name: '',
    flowNodeRefs: ['Script_1'], childLaneSets: [] }] }],
  dataObjects: [{ id: 'Object_1', name: null }],
  dataObjectReferences: [{ id: 'ObjectRef_1', name: '', dataObjectRef: 'Object_1',
    variableBindingKey: 'customer_ID' }],
  textAnnotations: [{ id: 'Note_1', text: 'First line\nŁódź <&>' }],
  associations: [{ id: 'Association_1', sourceRef: 'ObjectRef_1', targetRef: 'Note_1' }],
  dataStoreReferences: [{ id: 'StoreRef_1', name: '', dataStoreRef: 'Store_1' }] };
  const io = { dataInputs: [{ id: 'Input_1', name: '' }],
    dataOutputs: [{ id: 'Output_1', name: null, valueExpression: 'inputs.Input_1' }],
    inputSetId: 'InputSet_1', inputSet: ['Input_1'],
    outputSetId: 'OutputSet_1', outputSet: ['Output_1'],
    inputAssociations: [{ DirectRef: { id: 'InputAssociation_1',
      sourceObjectRefId: 'ObjectRef_1', targetInputId: 'Input_1' } }],
    outputAssociations: [{ id: 'OutputAssociation_1', sourceOutputId: 'Output_1',
      targetObjectRefId: 'ObjectRef_1' }],
    coordinatorOutput: {
      dataOutputs: [{ id: 'CoordinatorOutput_1', name: '', valueExpression: 'outputs.payload' }],
      outputSetId: 'CoordinatorOutputSet_1', outputSet: ['CoordinatorOutput_1'],
      outputAssociations: [{ id: 'CoordinatorAssociation_1',
        sourceOutputId: 'CoordinatorOutput_1', targetObjectRefId: 'ObjectRef_1' }],
    } };
  const model = { schemaVersion: 1, processId: 'Primary_1', processName: '',
    targetNamespace: 'urn:example:document', dataStores: [{ id: 'Store_1', name: null,
      capacity: 12, isUnlimited: false }],
    nodes: [{ id: 'Script_1', name: 'Evaluate', kind: { ScriptTask: {
      script: 'vars.customer_ID', outputMapping: {} } },
      repeat: { MultiInstance: { mode: 'Sequential', input: { Cardinality: { count: 2 } },
        outputCollectionVariable: 'results' } }, activityIo: io }],
    sequenceFlows: [{ id: 'Flow_1', sourceId: 'Script_1', targetId: 'Call_1',
      condition: null, callStartNodeId: 'ChildStart_1' }],
    variables: { customer_ID: { inner_key: null }, results: [] }, diagram, modeling,
    additionalProcesses: [{ processId: 'Child_1', processName: null,
      nodes: [{ id: 'Call_1', name: '', kind: { CallActivity: {
        localBody: { namespaceUri: 'urn:example:document', processId: 'Primary_1' },
        inputMapping: { customer_ID: 'vars.customer_ID' }, outputMapping: {} } } }],
      sequenceFlows: [], variables: {}, diagram: { shapes: [], edges: [] }, modeling: {
        laneSets: [], dataObjects: [], dataObjectReferences: [], textAnnotations: [],
        associations: [], dataStoreReferences: [{ id: 'StoreRef_Child', name: null,
          dataStoreRef: 'Store_1' }] } }],
    collaboration: { id: 'Collaboration_1', name: '',
      participants: [{ id: 'Pool_1', name: '', processRef: {
        namespaceUri: 'urn:example:document', processId: 'Primary_1' } },
      { id: 'Pool_2', name: null, processRef: null }],
      messageFlows: [{ id: 'MessageFlow_1', sourceRef: 'Pool_2', targetRef: 'Script_1',
        messageRef: null }], diagram: { shapes: [], edges: [], modelingShapes: [], modelingEdges: [] } } };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Document', description: '', model });
  assert.equal(saved.model.processName, '');
  assert.equal(Object.hasOwn(saved.model.dataStores[0], 'name'), false);
  assert.equal(saved.model.dataStores[0].capacity, 12);
  assert.equal(saved.model.dataStores[0].isUnlimited, false);
  assert.equal(saved.model.modeling.laneSets[0].lanes[0].name, '');
  assert.equal(saved.model.modeling.dataObjectReferences[0].name, '');
  assert.equal(saved.model.modeling.dataStoreReferences[0].name, '');
  assert.equal(saved.model.modeling.textAnnotations[0].text, 'First line\nŁódź <&>');
  assert.deepEqual(saved.model.diagram.modelingShapes, diagram.modelingShapes);
  assert.deepEqual(saved.model.diagram.modelingEdges, diagram.modelingEdges);
  const savedIo = saved.model.nodes[0].activityIo;
  assert.equal(savedIo.dataInputs[0].name, '');
  assert.equal(Object.hasOwn(savedIo.dataOutputs[0], 'name'), false);
  assert.equal(savedIo.dataOutputs[0].valueExpression, 'inputs.Input_1');
  assert.deepEqual(savedIo.inputAssociations, io.inputAssociations);
  assert.deepEqual(savedIo.outputAssociations, io.outputAssociations);
  assert.deepEqual(savedIo.coordinatorOutput, io.coordinatorOutput);
  assert.equal(saved.model.sequenceFlows[0].callStartNodeId, 'ChildStart_1');
  assert.equal(saved.model.additionalProcesses[0].processId, 'Child_1');
  assert.equal(Object.hasOwn(saved.model.additionalProcesses[0], 'processName'), false);
  assert.equal(saved.model.additionalProcesses[0].modeling.dataStoreReferences[0].dataStoreRef, 'Store_1');
  assert.deepEqual(saved.model.additionalProcesses[0].nodes[0].kind.CallActivity,
    model.additionalProcesses[0].nodes[0].kind.CallActivity);
  assert.equal(saved.model.collaboration.participants[1].processRef ?? null, null);
  assert.equal(saved.model.collaboration.messageFlows[0].targetRef, 'Script_1');
  const maximumModel = { ...model, dataStores: [{ ...model.dataStores[0], capacity: Number.MAX_SAFE_INTEGER }] };
  const maximum = request('processDefinitionSaveRequest', { commandId: 'maximum', definitionId: null,
    expectedRevision: 0, name: 'Maximum', description: '', model: maximumModel });
  assert.equal(maximum.model.dataStores[0].capacity, Number.MAX_SAFE_INTEGER);
  for (const invalid of [
    { ...model, dataStores: [{ ...model.dataStores[0], itemSubjectRef: 'Unsupported_1' }] },
    { ...model, dataStores: [{ ...model.dataStores[0], capacity: Number.MAX_SAFE_INTEGER + 1 }] },
    { ...model, modeling: { ...modeling, silentlyDropped: true } },
    { ...model, modeling: { ...modeling, laneSets: [{ ...modeling.laneSets[0],
      lanes: [{ ...modeling.laneSets[0].lanes[0], childLaneSets: { length: 0 } }] }] } },
    { ...model, nodes: [{ ...model.nodes[0], kind: { ...model.nodes[0].kind, End: {} } }] },
  ]) assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'bad',
    definitionId: null, expectedRevision: 0, name: 'Invalid', description: '', model: invalid }),
  /unsupported|requires one variant|requires node references|safe integer/);
});

test('optional selected start identity is absent on old requests and explicit when authored', { skip }, () => {
  const old = request('processInstanceStartRequest', { commandId: 'cmd',
    definitionId: 'definition-1', version: 1, variables: {} });
  assert.equal(Object.hasOwn(old, 'processId'), false);
  assert.equal(Object.hasOwn(old, 'startNodeId'), false);
  const selected = request('processInstanceStartRequest', { commandId: 'cmd',
    definitionId: 'definition-1', version: 1, variables: {},
    processId: 'Process_2', startNodeId: 'Start_2' });
  assert.equal(selected.processId, 'Process_2');
  assert.equal(selected.startNodeId, 'Start_2');
  const oldMessage = request('processMessageSendRequest', { commandId: 'cmd', messageId: 'message-1',
    target: { Start: { definitionId: 'definition-1' } }, messageName: 'order.received',
    correlationKey: 'case-1', payload: {}, ttlSeconds: 60 });
  assert.equal(Object.hasOwn(oldMessage.target.Start, 'processId'), false);
  assert.equal(Object.hasOwn(oldMessage.target.Start, 'startNodeId'), false);
  const selectedMessage = request('processMessageSendRequest', { commandId: 'cmd', messageId: 'message-2',
    target: { Start: { definitionId: 'definition-1', processId: 'Process_2', startNodeId: 'MessageStart_2' } },
    messageName: 'order.received', correlationKey: 'case-1', payload: {}, ttlSeconds: 60 });
  assert.equal(selectedMessage.target.Start.processId, 'Process_2');
  assert.equal(selectedMessage.target.Start.startNodeId, 'MessageStart_2');
});

test('instance pages encode exact selectors and message detail preserves null availability', { skip }, () => {
  const body = request('processInstanceGetRequest', { instanceId: 'instance', pages: {
    userTasks: { offset: 20, limit: 20 }, subscriptions: { offset: 0, limit: 2 },
    scopes: { offset: 0, limit: 20 }, calls: { offset: 20, limit: 20 },
    repetitionGroups: { offset: 40, limit: 20 }, repetitionOccurrences: { offset: 20, limit: 20 },
    selectedUserTaskId: 'task-1', selectedIncidentId: 'incident-1',
    selectedRepetitionGroupId: 'group-1', selectedRepetitionOccurrenceId: 'occurrence-2',
    selectedRepetitionValue: 'aggregate',
  } });
  assert.equal(body.pages.userTasks.offset, 20);
  assert.equal(body.pages.subscriptions.limit, 2);
  assert.equal(body.pages.scopes.limit, 20);
  assert.equal(body.pages.calls.offset, 20);
  assert.equal(body.pages.repetitionGroups.offset, 40);
  assert.equal(body.pages.repetitionOccurrences.offset, 20);
  assert.equal(body.pages.selectedRepetitionGroupId, 'group-1');
  assert.equal(body.pages.selectedRepetitionOccurrenceId, 'occurrence-2');
  assert.equal(body.pages.selectedRepetitionValue, 'aggregate');
  assert.equal(body.pages.selectedUserTaskId, 'task-1');
  assert.equal(body.pages.selectedIncidentId, 'incident-1');
  const scope = request('processScopeGetRequest', { instanceId: 'instance', scopeId: 'child-scope' });
  assert.equal(scope.instanceId, 'instance');
  assert.equal(scope.scopeId, 'child-scope');
  const message = { message_id: 'msg', sender_user_id: 'user', origin: 'Api',
    target: { Start: { definition_id: 'def' } }, message_name: 'order.received', correlation_key: 'key',
    revision: 1, status: 'Delivered', received_at_ms: 1, expires_at_ms: 2, updated_at_ms: 2,
    delivered_at_ms: 2, matched_instance_id: 'inst', matched_version: 1, matched_subscription_id: null,
    source_instance_id: null, source_node_id: null, last_reason: null, payload_sha256: 'sha',
    payload_bytes: 4, payload_available: true, can_resolve: false, can_cancel: false };
  const availableNull = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    MessageGetResponse: { message: { message, payload: null } },
  } })));
  assert.equal(availableNull.message.message.payloadAvailable, true);
  assert.equal(availableNull.message.payload, null);
  const unavailable = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    MessageGetResponse: { message: { message: { ...message, payload_available: false } } },
  } })));
  assert.equal(unavailable.message.message.payloadAvailable, false);
  assert.equal(unavailable.message.payload, undefined);
  const nested = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    MessageGetResponse: { message: { message, payload: { customer_ID: { attached_to_id: 1 } } } },
  } })));
  assert.deepEqual(nested.message.payload, { customer_ID: { attached_to_id: 1 } });
});

test('instance response decodes required empty and populated message collections', { skip }, () => {
  const base = {
    instance_id: 'i1', definition_id: 'd1', definition_name: 'Approval',
    process_id: 'Process_Approval', start_node_id: 'Start_1',
    initiator_user_id: 'u1', version: 1, revision: 1, status: 'Running',
    variables: { business_key: 'kept' }, active_node_ids: [], user_tasks: [], incidents: [],
    created_at_ms: 1, updated_at_ms: 1, can_cancel: true, can_retry: false,
    can_send_message: true, calls: [], repetition_groups: [], repetition_occurrences: [],
    selected_repetition_occurrence: null,
  };
  const decode = (instance) => wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    InstanceGetResponse: { instance },
  } })));
  const populated = decode({ ...base,
    subscriptions: [{ subscription_id: 's1', node_id: 'Wait_1', node_name: 'Wait', token_id: 't1',
      kind: 'MessageCatch', status: 'Open', revision: 1, message_name: 'order.received',
      correlation_key: 'case-1', error_code: null, attached_to_id: null, race_id: 'r1', last_reason: null,
      scope_id: 'child-scope' }],
    event_races: [{ race_id: 'r1', gateway_node_id: 'Race_1', gateway_name: 'First arrival',
      status: 'Open', revision: 1, winner_node_id: null,
      branch_subscription_ids: ['s1'], branch_timer_ids: [], scope_id: 'child-scope' }],
    outgoing_messages: [{ message_id: 'm1', sender_user_id: 'u1', origin: 'Api',
      target: { Start: { definition_id: 'd2' } }, message_name: 'order.received',
      correlation_key: 'case-1', revision: 1, status: 'Pending', received_at_ms: 1,
      expires_at_ms: 2, updated_at_ms: 1, delivered_at_ms: null, matched_instance_id: null,
      matched_version: null, matched_subscription_id: null, source_instance_id: 'i1',
      source_node_id: 'Throw_1', last_reason: null, payload_sha256: 'sha', payload_bytes: 4,
      payload_available: false, can_resolve: false, can_cancel: false }],
    message_names: ['order.received'],
    calls: [{ Outgoing: { call_node_id: 'Call_1', call_node_name: 'Approval', status: 'Waiting',
      child: { instance_id: 'child-1', definition_name: 'Child', version: 7,
        status: 'Waiting', can_open: true } } }],
    scopes: [{ scope_id: 'child-scope', parent_scope_id: 'i1', subprocess_node_id: 'Sub_1',
      subprocess_node_name: 'Review', parent_token_id: 'wait-1', revision: 1, status: 'Running',
      depth: 1, created_at_ms: 1, updated_at_ms: 1 }],
  });
  assert.equal(populated.instance.processId, 'Process_Approval');
  assert.equal(populated.instance.startNodeId, 'Start_1');
  assert.equal(populated.instance.subscriptions[0].subscriptionId, 's1');
  assert.deepEqual(populated.instance.eventRaces[0].branchSubscriptionIds, ['s1']);
  assert.equal(populated.instance.outgoingMessages[0].messageId, 'm1');
  assert.deepEqual(populated.instance.messageNames, ['order.received']);
  assert.equal(populated.instance.scopes[0].subprocessNodeName, 'Review');
  assert.equal(populated.instance.calls[0].Outgoing.child.instanceId, 'child-1');
  assert.equal(populated.instance.calls[0].Outgoing.child.canOpen, true);
  assert.deepEqual(populated.instance.variables, { business_key: 'kept' });
  const empty = decode({ ...base, subscriptions: [], event_races: [], outgoing_messages: [], message_names: [], scopes: [] });
  for (const field of ['subscriptions', 'eventRaces', 'outgoingMessages', 'messageNames', 'scopes', 'calls',
    'repetitionGroups', 'repetitionOccurrences']) {
    assert.deepEqual(empty.instance[field], [], `${field} remains an array on an empty page`);
  }
  assert.equal(empty.instance.canSendMessage, true);
  const summary = { occurrence_id: 'o1', group_id: 'g1', ordinal: 0, status: 'completed', token_id: 't1',
    user_task_id: null, job_id: null, verification_user_task_id: null,
    accepted_source_event_id: 'event-1', approval_event_id: null,
    created_at_ms: 1, updated_at_ms: 2 };
  const repeated = decode({ ...base, subscriptions: [], event_races: [], outgoing_messages: [],
    message_names: [], scopes: [],
    repetition_groups: [{ group_id: 'g1', node_id: 'Review', node_name: 'Review', scope_id: 'i1',
      parent_token_id: 't0', mode: 'structured_loop', status: 'open', total: null,
      created_count: 1, completed: 1, max_iterations: 32, revision: 2,
      created_at_ms: 1, updated_at_ms: 2 }],
    repetition_occurrences: [summary],
    selected_repetition_occurrence: { summary, value_kind: 'aggregate', value_available: true,
      value: { business_key: { inner_value: 'kept' } }, accepted_origin: 'envelope' },
  });
  assert.equal(repeated.instance.repetitionGroups[0].total, null);
  assert.equal(repeated.instance.repetitionOccurrences[0].tokenId, 't1');
  assert.equal(repeated.instance.selectedRepetitionOccurrence.valueAvailable, true);
  assert.equal(repeated.instance.selectedRepetitionOccurrence.acceptedOrigin, 'envelope');
  assert.deepEqual(repeated.instance.selectedRepetitionOccurrence.value,
    { business_key: { inner_value: 'kept' } });
  const missingAggregate = decode({ ...base, subscriptions: [], event_races: [], outgoing_messages: [],
    message_names: [], scopes: [], repetition_groups: [], repetition_occurrences: [summary],
    selected_repetition_occurrence: { summary, value_kind: 'aggregate', value_available: false,
      value: null, accepted_origin: null } });
  assert.equal(missingAggregate.instance.selectedRepetitionOccurrence.valueAvailable, false);
  assert.equal(missingAggregate.instance.selectedRepetitionOccurrence.value, null);
});

test('timer model wire preserves each typed rule and opaque variables', { skip }, () => {
  for (const timer of [
    { Date: { at: '2027-01-02T03:04:05+01:00' } },
    { Duration: { seconds: 90061 } },
    { Cycle: { seconds: 300, totalFirings: 3 } },
    { Daily: { hour: 9, minute: 15, totalFirings: null } },
  ]) {
    const model = {
      schemaVersion: 1, processId: 'Timed_1', timerTimezone: 'Europe/Warsaw',
      nodes: [
        { id: 'Start_1', name: 'Start', kind: { TimerStart: { timer } } },
        { id: 'End_1', name: 'End', kind: 'End' },
      ],
      sequenceFlows: [{ id: 'Flow_1', sourceId: 'Start_1', targetId: 'End_1', condition: null }],
      variables: { threshold_value: { nested_key: 'Łódź & <ok>' } },
      diagram: { shapes: [], edges: [] },
    };
    const body = request('processDefinitionSaveRequest', {
      commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd', definitionId: null,
      expectedRevision: 0, name: 'Timed', description: '', model,
    });
    assert.equal(body.model.timerTimezone, 'Europe/Warsaw');
    assert.deepEqual(body.model.variables, model.variables);
    assert.deepEqual(body.model.nodes[0].kind.TimerStart.timer, timer);
  }
  assert.throws(() => codec.encode.processDefinitionSaveRequest(17, {
    commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd', definitionId: null,
    expectedRevision: 0, name: 'Bad', description: '',
    model: { schemaVersion: 1, processId: 'P_1', nodes: [{ id: 'Start_1', name: '', kind: { TimerStart: { timer: { Daily: { hour: 9, minute: 0, extra: true } } } } }],
      sequenceFlows: [], variables: {}, diagram: { shapes: [], edges: [] }, timerTimezone: 'UTC' },
  }), /unsupported timer rule/);
});

test('working calendar wire preserves typed pin and opaque business keys', { skip }, () => {
  const calendar = { name: 'Office', weeklyWindows: [{ weekday: 1, startMinute: 540, endMinute: 1020 }],
    manualDaysOff: [{ date: '2027-05-04', reason: 'Team & family' }], holidayPolicy: 'None' };
  const pin = { calendar, legalRelease: { releaseId: 'PL-statutory-2026-10-02', asOfDate: '2026-10-02',
      validFrom: '2024-01-01', validUntil: '2041-01-01', auditManifestSha256: 'a',
      sources: [{ sourceId: 'DU/2024/1965', url: 'https://example.invalid', sha256: 'b', retrievedOn: '2026-10-02' }],
      rules: [{ ruleId: 'sunday', sourceId: 'DU/2024/1965', effectiveFrom: '2024-01-01',
        kind: { Weekday: { weekday: 7 } } }] },
    timezoneData: { ianaName: 'Europe/Warsaw', releaseId: '2026e', horizonStartMs: 1,
      horizonEndMs: 2, initialOffsetSeconds: 3600, transitions: [{ atUtcMs: 2, offsetSeconds: 7200 }],
      sourceUrl: 'https://example.invalid/tz', sourceSha256: 'c', datasetSha256: 'd' }, sha256: 'e' };
  const model = { schemaVersion: 1, processId: 'P_1', timerTimezone: 'Europe/Warsaw',
    workCalendar: calendar, calendarPin: pin,
    nodes: [{ id: 'Start_1', name: 'Start', kind: { TimerStart: { timer: { WorkingDuration: { seconds: 3600 } } } } }],
    sequenceFlows: [], variables: { business_key: { inner_value: 'Łódź' } }, diagram: { shapes: [], edges: [] } };
  const body = request('processDefinitionSaveRequest', { commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd',
    definitionId: null, expectedRevision: 0, name: 'Working', description: '', model });
  assert.deepEqual(body.model.variables, model.variables);
  assert.deepEqual(body.model.nodes[0].kind.TimerStart.timer, { WorkingDuration: { seconds: 3600 } });
  assert.deepEqual(body.model.workCalendar.weeklyWindows, calendar.weeklyWindows);
  assert.deepEqual(body.model.calendarPin.timezoneData.transitions, pin.timezoneData.transitions);
  assert.deepEqual(body.model.calendarPin.legalRelease.rules[0].kind, { Weekday: { weekday: 7 } });
  assert.throws(() => codec.encode.processDefinitionSaveRequest(17, { commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd',
    definitionId: null, expectedRevision: 0, name: 'Bad', description: '',
    model: { ...model, workCalendar: { ...calendar, secretRule: true } } }), /unsupported work calendar field/);
  const publish = request('processDefinitionPublishRequest', { commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd',
    definitionId: 'definition-1', expectedRevision: 3, repinCalendar: true });
  assert.equal(publish.repinCalendar, true);
});

test('boundary timer wire keeps attachment, cancellation and opaque business keys', { skip }, () => {
  const model = {
    schemaVersion: 1, processId: 'Boundary_1', timerTimezone: 'Europe/Warsaw',
    nodes: [
      { id: 'Start_1', name: 'Start', kind: 'Start' },
      { id: 'Review_1', name: 'Review', kind: { UserTask: { assigneeUserId: null, outputMapping: { business_key: 'outputs.value' } } } },
      { id: 'Timer_1', name: 'Reminder', kind: { BoundaryTimer: {
        attachedToId: 'Review_1', cancelActivity: false, timer: { Duration: { seconds: 90 } },
      } } },
      { id: 'End_1', name: 'End', kind: 'End' },
    ],
    sequenceFlows: [
      { id: 'Flow_1', sourceId: 'Start_1', targetId: 'Review_1', condition: null },
      { id: 'Flow_2', sourceId: 'Review_1', targetId: 'End_1', condition: null },
      { id: 'Flow_3', sourceId: 'Timer_1', targetId: 'End_1', condition: null },
    ],
    variables: { attached_to_id: { user_key: 'Łódź' } }, diagram: { shapes: [], edges: [] },
  };
  const payload = { commandId: '7c865aaa-febd-4621-9ae6-35977200a0fd', definitionId: null,
    expectedRevision: 0, name: 'Boundary', description: '', model };
  const body = request('processDefinitionSaveRequest', payload);
  assert.equal(body.model.nodes[2].kind.BoundaryTimer.attachedToId, 'Review_1');
  assert.equal(body.model.nodes[2].kind.BoundaryTimer.cancelActivity, false);
  assert.deepEqual(body.model.nodes[2].kind.BoundaryTimer.timer, { Duration: { seconds: 90 } });
  assert.deepEqual(body.model.variables, model.variables);
  assert.deepEqual(body.model.nodes[1].kind.UserTask.outputMapping, { business_key: 'outputs.value' });

  for (const bad of [
    { attachedToId: 'Review_1', timer: { Duration: { seconds: 90 } } },
    { attachedToId: 'Review_1', cancelActivity: true, timer: { Cycle: { seconds: 300, totalFirings: 2 } } },
    { attachedToId: 'Review_1', cancelActivity: false, timer: { Duration: { seconds: 90 } }, extra: 1 },
  ]) {
    const invalid = structuredClone(payload);
    invalid.model.nodes[2].kind.BoundaryTimer = bad;
    assert.throws(() => codec.encode.processDefinitionSaveRequest(17, invalid), TypeError);
  }
});

test('boundary timer and full task scope identity decode with nullable timer fields', { skip }, () => {
  const timerFields = {
    timer_id: 't1', node_id: 'Timer_1', node_name: 'Reminder', kind: 'Boundary',
    status: 'Pending', due_at_ms: 42, timezone: 'UTC', occurrence: 1,
    total_firings: null, last_reason: null, attached_to_id: 'Review_1',
  };
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    UserTaskGetResponse: { task: {
      user_task_id: 'u1', node_id: 'Review_1', name: 'Review', assignee_user_id: 'person',
      kind: 'Work', status: 'Open', outputs: { business_key: 'kept' },
      revision: 1, can_complete: true, token_id: 'waiting-activation', scope_id: 'i1',
    } },
  } })));
  assert.equal(decoded.task.tokenId, 'waiting-activation');
  assert.equal(decoded.task.scopeId, 'i1');
  assert.deepEqual(decoded.task.outputs, { business_key: 'kept' });
  const timer = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    InstanceGetResponse: { instance: {
      instance_id: 'i1', definition_id: 'd1', definition_name: 'Boundary',
      process_id: 'Process_Boundary', start_node_id: 'Start_1',
      initiator_user_id: 'person', version: 1, revision: 1, status: 'Running',
      variables: { attached_to_id: 'business' }, active_node_ids: ['Review_1'],
      user_tasks: [], incidents: [], created_at_ms: 1, updated_at_ms: 1,
      can_cancel: true, can_retry: false, timers: [timerFields],
      calls: [], repetition_groups: [], repetition_occurrences: [], selected_repetition_occurrence: null,
    } },
  } })));
  assert.equal(timer.instance.processId, 'Process_Boundary');
  assert.equal(timer.instance.startNodeId, 'Start_1');
  assert.equal(timer.instance.timers[0].attachedToId, 'Review_1');
  assert.equal(timer.instance.timers[0].kind, 'Boundary');
  assert.deepEqual(timer.instance.variables, { attached_to_id: 'business' });
});

test('pinned timer catalog decodes authored rule and matching persisted status', { skip }, () => {
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    DefinitionGetResponse: { definition: {
      definition_id: 'd1', name: 'Morning', description: '', owner_user_id: 'u1',
      draft_revision: 1, published_version: 1, archived: false,
      model: { schema_version: 1, process_id: 'P_1', timer_timezone: 'Europe/Warsaw',
        nodes: [
          { id: 'Start_1', name: 'Morning', kind: { TimerStart: { timer: { Duration: { seconds: 1 } } } } },
          { id: 'End_1', name: 'End', kind: 'End' },
        ],
        sequence_flows: [{ id: 'Flow_1', source_id: 'Start_1', target_id: 'End_1', condition: null }],
        variables: {}, diagram: { shapes: [], edges: [] },
      },
    }, start_catalog: [{ process_id: 'P_1', process_name: null,
      start_node_id: 'Start_1', start_node_name: 'Morning', version: 1,
      trigger: { TimerStart: { timer: { Duration: { seconds: 1 } }, timezone: 'Europe/Warsaw',
        working_time: null, persisted_timer: {
          timer_id: 't1', node_id: 'Start_1', node_name: 'Morning', kind: 'Start',
          status: 'Error', due_at_ms: 1, timezone: 'Europe/Warsaw', occurrence: 3,
          total_firings: 3, last_reason: 'calendar horizon exceeded',
        } } } }] },
  } })));
  assert.equal(decoded.variant, 'ProcessDefinitionGetResponse');
  assert.equal(decoded.startCatalog[0].processId, 'P_1');
  assert.equal(decoded.startCatalog[0].processName, null);
  assert.equal(decoded.startCatalog[0].trigger.TimerStart.timer.Duration.seconds, 1);
  const persisted = decoded.startCatalog[0].trigger.TimerStart.persistedTimer;
  assert.equal(persisted.status, 'Error');
  assert.equal(persisted.totalFirings, 3);
  assert.equal(persisted.lastReason, 'calendar horizon exceeded');
  assert.equal(Object.hasOwn(persisted, 'total_firings'), false);
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

test('user task detail transports sixteen near-limit authenticated activity inputs', { skip }, () => {
  const value = 'x'.repeat(256 * 1024 - 64);
  const task = {
    user_task_id: 'large-task', node_id: 'Review_1', name: 'Review', assignee_user_id: 'u1',
    kind: 'Work', status: 'Open', outputs: null, revision: 1, can_complete: true,
    token_id: 'waiting-1', scope_id: 'i1',
    activity_inputs: Array.from({ length: 16 }, (_, position) => ({
      position, declaration_id: `input_${position}`, name: `Input ${position}`,
      value: { Present: value },
    })),
  };
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    UserTaskGetResponse: { task },
  } })));
  assert.equal(decoded.task.activityInputs.length, 16);
  assert.equal(decoded.task.activityInputs[0].value.Present.length, value.length);
  assert.equal(decoded.task.activityInputs[15].declarationId, 'input_15');
  assert.equal(Object.hasOwn(decoded.task, 'activity_inputs'), false);
});

test('user task detail decodes missing and present null activity inputs distinctly', { skip }, () => {
  const task = {
    user_task_id: 'activity-inputs', node_id: 'Review_1', name: 'Review', assignee_user_id: 'u1',
    kind: 'Work', status: 'Open', outputs: null, revision: 1, can_complete: true,
    token_id: 'waiting-1', scope_id: 'i1',
    activity_inputs: [
      { position: 0, declaration_id: 'Required_Value', name: 'Required value', value: 'Missing' },
      { position: 1, declaration_id: 'Nullable_Value', name: 'Nullable value', value: { Present: null } },
    ],
  };
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    UserTaskGetResponse: { task },
  } })));
  assert.equal(decoded.task.activityInputs[0].value, 'Missing');
  assert.deepEqual(decoded.task.activityInputs[1].value, { Present: null });
});

test('manual task uses a distinct acknowledgment request without work outputs', { skip }, () => {
  const base = { schemaVersion: 1, processId: 'P_1', sequenceFlows: [], variables: {},
    diagram: { shapes: [], edges: [] } };
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Manual', description: '', model: { ...base, nodes: [{ id: 'Manual_1',
      name: 'External work', kind: { ManualTask: { assigneeUserId: null,
        instructions: 'Inspect the external register.\nAcknowledge here.' } } }] } });
  assert.deepEqual(saved.model.nodes[0].kind, { ManualTask: { assigneeUserId: null,
    instructions: 'Inspect the external register.\nAcknowledge here.' } });
  assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Manual', description: '', model: { ...base, nodes: [{ id: 'Manual_1',
      name: 'External work', kind: { ManualTask: { assigneeUserId: null, instructions: '', outputs: {} } } }] } }),
  /unsupported|unknown/);
  assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Manual', description: '', model: { ...base, nodes: [{ id: 'Manual_1',
      name: 'External work', kind: { ManualTask: { instructions: 'Inspect' } } }] } }),
  /requires instructions and optional assignee/);
  const ack = request('processManualTaskAcknowledgeRequest', { commandId: 'ack', instanceId: 'i1',
    userTaskId: 't1', expectedRevision: 4 });
  assert.equal(ack.variant, 'ProcessManualTaskAcknowledgeRequest');
  assert.equal(ack.instanceId, 'i1');
  assert.equal(ack.userTaskId, 't1');
  assert.equal(ack.expectedRevision, 4);
  assert.equal(Object.hasOwn(ack, 'outputs'), false);
  assert.equal(Object.hasOwn(ack, 'approved'), false);
  const detail = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    UserTaskGetResponse: { task: { user_task_id: 't1', node_id: 'Manual_1',
      name: 'External work', assignee_user_id: 'u1', kind: 'Manual', status: 'Open',
      outputs: null, revision: 1, can_complete: true, scope_id: 'i1',
      instructions: 'Inspect the external register.\nAcknowledge here.' } },
  } })));
  assert.equal(detail.task.kind, 'Manual');
  assert.equal(detail.task.outputs, null);
  assert.equal(detail.task.instructions, 'Inspect the external register.\nAcknowledge here.');
});

test('send and receive tasks keep distinct typed model fields', { skip }, () => {
  const base = { schemaVersion: 1, processId: 'P_1', sequenceFlows: [], variables: {},
    diagram: { shapes: [], edges: [] } };
  const nodes = [
    { id: 'Send_1', name: 'Admit locally', kind: { SendTask: { messageRef: 'Message_1',
      target: { Start: { definitionId: 'definition-1' } }, correlationExpression: 'vars.key',
      payloadExpression: 'vars.payload', ttlSeconds: 60 } } },
    { id: 'Receive_1', name: 'Wait for delivery', kind: { ReceiveTask: { messageRef: 'Message_1',
      correlationExpression: 'vars.key', outputMapping: { received: 'outputs' } } } },
  ];
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Message tasks', description: '', model: { ...base, nodes } });
  assert.deepEqual(saved.model.nodes.map((node) => node.kind), nodes.map((node) => node.kind));
  for (const kind of [
    { SendTask: { ...nodes[0].kind.SendTask, unsupported: true } },
    { ReceiveTask: { messageRef: 'Message_1', correlationExpression: 'vars.key' } },
    { ReceiveTask: { ...nodes[1].kind.ReceiveTask, unsupported: true } },
  ]) {
    assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
      expectedRevision: 0, name: 'Message tasks', description: '',
      model: { ...base, nodes: [{ id: 'Task_1', name: 'Invalid', kind }] } }),
    /unsupported|requires|unknown/);
  }
});

test('signal declarations and event kinds use typed camel case without changing absent fields', { skip }, () => {
  const base = { schemaVersion: 1, processId: 'P_1', sequenceFlows: [], variables: {},
    diagram: { shapes: [], edges: [] } };
  const nodes = [
    { id: 'Throw_1', name: 'Admit', kind: { SignalThrow: { signalRef: 'Signal_1',
      payloadExpression: 'vars.payload', ttlSeconds: 3600 } } },
    { id: 'Catch_1', name: 'Wait', kind: { SignalCatch: { signalRef: 'Signal_1',
      outputMapping: { received: 'outputs' } } } },
  ];
  const saved = request('processDefinitionSaveRequest', { commandId: 'cmd', definitionId: null,
    expectedRevision: 0, name: 'Signals', description: '', model: { ...base, targetNamespace: 'urn:orders',
      signals: [{ signalId: 'Signal_1', namespaceUri: 'urn:orders', name: 'Order changed' }], nodes } });
  assert.deepEqual(saved.model.nodes.map((node) => node.kind), nodes.map((node) => node.kind));
  assert.deepEqual(saved.model.signals, [{ signalId: 'Signal_1', namespaceUri: 'urn:orders', name: 'Order changed' }]);
  const old = request('processDefinitionSaveRequest', { commandId: 'old', definitionId: null,
    expectedRevision: 0, name: 'Old', description: '', model: { ...base, nodes: [] } });
  assert.equal(Object.hasOwn(old.model, 'signals'), false);
  for (const kind of [
    { SignalThrow: { signalRef: 'Signal_1', payloadExpression: 'vars.payload' } },
    { SignalCatch: { signalRef: 'Signal_1' } },
    { SignalCatch: { ...nodes[1].kind.SignalCatch, unsupported: true } },
  ]) assert.throws(() => request('processDefinitionSaveRequest', { commandId: 'bad', definitionId: null,
    expectedRevision: 0, name: 'Invalid', description: '',
    model: { ...base, nodes: [{ id: 'Bad_1', name: 'Invalid', kind }] } }), /unsupported|requires|unknown/);
});

test('user task detail response preserves opaque output keys', { skip }, () => {
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    UserTaskGetResponse: { task: {
      user_task_id: 't1', node_id: 'Review_1', name: 'Review', assignee_user_id: 'u1',
      kind: 'Work', status: 'Open', outputs: { business_key: { inner_value: 3 } },
      revision: 1, can_complete: true, scope_id: 'i1',
    } },
  } })));
  assert.equal(decoded.variant, 'ProcessUserTaskGetResponse');
  assert.equal(decoded.task.userTaskId, 't1');
  assert.equal(decoded.task.scopeId, 'i1');
  assert.deepEqual(decoded.task.outputs, { business_key: { inner_value: 3 } });
  assert.equal(Object.hasOwn(decoded.task, 'user_task_id'), false);
});

test('simulation requests encode their typed fields and keep opaque variable keys', { skip }, () => {
  const start = request('processSimulationStartRequest', {
    definitionId: 'definition-1', version: 3, selectedProcessId: 'Process_1',
    startNodeId: 'Start_1', variables: { customer_ID: { inner_value: 7 }, empty: null },
    startMs: 1000, horizonMs: 9000, tickDurationMs: 100,
  });
  assert.equal(start.variant, 'ProcessSimulationStartRequest');
  assert.equal(start.definitionId, 'definition-1');
  assert.equal(start.horizonMs, 9000);
  assert.deepEqual(start.variables, { customer_ID: { inner_value: 7 }, empty: null });

  const complete = request('processSimulationUserTaskCompleteRequest', {
    simulationId: 'simulation-1', userTaskId: 'task-1', outputs: { business_key: { inner_value: 3 } },
  });
  assert.equal(complete.variant, 'ProcessSimulationUserTaskCompleteRequest');
  assert.deepEqual(complete.outputs, { business_key: { inner_value: 3 } });

  for (const [name, variant] of [
    ['processSimulationViewRequest', 'ProcessSimulationViewRequest'],
    ['processSimulationAdvanceRequest', 'ProcessSimulationAdvanceRequest'],
    ['processSimulationReleaseRequest', 'ProcessSimulationReleaseRequest'],
  ]) {
    const body = request(name, { simulationId: 'simulation-1' });
    assert.equal(body.variant, variant);
    assert.equal(body.simulationId, 'simulation-1');
  }
  const acknowledge = request('processSimulationManualTaskAcknowledgeRequest', {
    simulationId: 'simulation-1', userTaskId: 'task-2',
  });
  assert.equal(acknowledge.variant, 'ProcessSimulationManualTaskAcknowledgeRequest');
  assert.equal(acknowledge.userTaskId, 'task-2');
});

test('simulation view response decodes its private clock, trace, and witnesses', { skip }, () => {
  const view = {
    simulation_id: 'simulation-1',
    source: { simulation_id: 'simulation-1', definition_id: 'definition-1', version: 3,
      model_sha256: 'a'.repeat(64), selected_process_id: 'Process_1', start_node_id: 'Start_1' },
    clock: { start_ms: 10, now_ms: 20, horizon_ms: 90, tick_duration_ms: 10,
      step_index: 1, revision: 2 },
    instance: null, user_tasks: [], timers: [], incidents: [],
    events: [{ event_id: 'event-1', seq: 1, at_ms: 10, kind: 'instance_started', node_id: null,
      actor_user_id: 'user-1', data: null, scope_id: 'scope-1' }],
    trace_steps: [{ trace_step_id: 'trace-1', ordinal: 0, action: 'start', at_ms: 10,
      request_sha256: 'b'.repeat(64), result_sha256: 'c'.repeat(64), data: null }],
    activity_io_witnesses: [], events_omitted: 7, trace_steps_omitted: 3,
  };
  const decoded = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    SimulationViewResponse: { view },
  } })));
  assert.equal(decoded.variant, 'ProcessSimulationViewResponse');
  assert.equal(decoded.view.simulationId, 'simulation-1');
  assert.equal(decoded.view.clock.tickDurationMs, 10);
  assert.equal(decoded.view.events[0].kind, 'instance_started');
  assert.equal(decoded.view.traceSteps[0].action, 'start');
  assert.equal(decoded.view.eventsOmitted, 7);
  assert.equal(decoded.view.traceStepsOmitted, 3);
});
