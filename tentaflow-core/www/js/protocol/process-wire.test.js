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
        model_sha256: 'target-sha' }] } },
  } })));
  assert.equal(version.version.callActivities[0].calledElement.namespaceUri, 'urn:example:approval');
  assert.equal(version.version.callActivities[0].calledVersion, 7);
  const terminal = wasm.decodeMessageBody(new Uint8Array(cbor({ ProcessBody: {
    InstanceGetResponse: { instance: { instance_id: 'child-1', definition_id: 'definition-2',
      definition_name: 'Child', initiator_user_id: 'owner', version: 7, revision: 2,
      status: 'Error', variables: {}, active_node_ids: [], user_tasks: [], incidents: [],
      created_at_ms: 1, updated_at_ms: 2, can_cancel: false, can_retry: false,
      calls: [{ Incoming: { parent: null } }],
      terminal_error: { error_ref: 'Error_1', error_code: 'BUSINESS.REJECTED',
        source_event_id: 'event-1', source_node_id: 'ErrorEnd_1', source_scope_id: 'child-1' } } },
  } })));
  assert.equal(terminal.instance.status, 'Error');
  assert.deepEqual(terminal.instance.calls, [{ Incoming: { parent: null } }]);
  assert.equal(terminal.instance.terminalError.errorCode, 'BUSINESS.REJECTED');
  assert.equal(Object.hasOwn(terminal.instance.calls[0].Incoming, 'callNodeId'), false);
});

test('instance pages encode exact selectors and message detail preserves null availability', { skip }, () => {
  const body = request('processInstanceGetRequest', { instanceId: 'instance', pages: {
    userTasks: { offset: 20, limit: 20 }, subscriptions: { offset: 0, limit: 2 },
    scopes: { offset: 0, limit: 20 }, calls: { offset: 20, limit: 20 },
    selectedUserTaskId: 'task-1', selectedIncidentId: 'incident-1',
  } });
  assert.equal(body.pages.userTasks.offset, 20);
  assert.equal(body.pages.subscriptions.limit, 2);
  assert.equal(body.pages.scopes.limit, 20);
  assert.equal(body.pages.calls.offset, 20);
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
    initiator_user_id: 'u1', version: 1, revision: 1, status: 'Running',
    variables: { business_key: 'kept' }, active_node_ids: [], user_tasks: [], incidents: [],
    created_at_ms: 1, updated_at_ms: 1, can_cancel: true, can_retry: false,
    can_send_message: true, calls: [],
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
  assert.equal(populated.instance.subscriptions[0].subscriptionId, 's1');
  assert.deepEqual(populated.instance.eventRaces[0].branchSubscriptionIds, ['s1']);
  assert.equal(populated.instance.outgoingMessages[0].messageId, 'm1');
  assert.deepEqual(populated.instance.messageNames, ['order.received']);
  assert.equal(populated.instance.scopes[0].subprocessNodeName, 'Review');
  assert.equal(populated.instance.calls[0].Outgoing.child.instanceId, 'child-1');
  assert.equal(populated.instance.calls[0].Outgoing.child.canOpen, true);
  assert.deepEqual(populated.instance.variables, { business_key: 'kept' });
  const empty = decode({ ...base, subscriptions: [], event_races: [], outgoing_messages: [], message_names: [], scopes: [] });
  for (const field of ['subscriptions', 'eventRaces', 'outgoingMessages', 'messageNames', 'scopes', 'calls']) {
    assert.deepEqual(empty.instance[field], [], `${field} remains an array on an empty page`);
  }
  assert.equal(empty.instance.canSendMessage, true);
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
      initiator_user_id: 'person', version: 1, revision: 1, status: 'Running',
      variables: { attached_to_id: 'business' }, active_node_ids: ['Review_1'],
      user_tasks: [], incidents: [], created_at_ms: 1, updated_at_ms: 1,
      can_cancel: true, can_retry: false, timers: [timerFields],
      calls: [],
    } },
  } })));
  assert.equal(timer.instance.timers[0].attachedToId, 'Review_1');
  assert.equal(timer.instance.timers[0].kind, 'Boundary');
  assert.deepEqual(timer.instance.variables, { attached_to_id: 'business' });
});

test('timer status Error and schedule fields decode to canonical camel keys', { skip }, () => {
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
    }, timer_start: {
      timer_id: 't1', node_id: 'Start_1', node_name: 'Morning', kind: 'Start',
      status: 'Error', due_at_ms: 1, timezone: 'Europe/Warsaw', occurrence: 3,
      total_firings: 3, last_reason: 'calendar horizon exceeded',
    } },
  } })));
  assert.equal(decoded.variant, 'ProcessDefinitionGetResponse');
  assert.equal(decoded.timerStart.status, 'Error');
  assert.equal(decoded.timerStart.totalFirings, 3);
  assert.equal(decoded.timerStart.lastReason, 'calendar horizon exceeded');
  assert.equal(Object.hasOwn(decoded.timerStart, 'total_firings'), false);
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
      revision: 1, can_complete: true, scope_id: 'i1',
    } },
  } })));
  assert.equal(decoded.variant, 'ProcessUserTaskGetResponse');
  assert.equal(decoded.task.userTaskId, 't1');
  assert.equal(decoded.task.scopeId, 'i1');
  assert.deepEqual(decoded.task.outputs, { business_key: { inner_value: 3 } });
  assert.equal(Object.hasOwn(decoded.task, 'user_task_id'), false);
});
