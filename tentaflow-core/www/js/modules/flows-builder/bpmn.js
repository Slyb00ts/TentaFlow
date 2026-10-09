// ============ File: flows-builder/bpmn.js — BPMN presentation and lossless canvas model mapping ============

import { I18n } from '/js/i18n.js';

const ELEMENTS = [
  ['Start', 'start', 'bpmn-none-start', 'events', 56, 56],
  ['End', 'end', 'bpmn-none-end', 'events', 56, 56],
  ['ErrorEnd', 'error_end', 'bpmn-error-filled', 'events', 56, 56],
  ['TerminateEnd', 'terminate_end', 'bpmn-terminate', 'events', 56, 56],
  ['TimerStart', 'timer_start', 'clock', 'events', 56, 56],
  ['TimerCatch', 'timer_catch', 'clock', 'events', 56, 56],
  ['BoundaryTimer', 'boundary_timer', 'clock', 'events', 56, 56],
  ['MessageStart', 'message_start', 'bpmn-message-catch', 'events', 56, 56],
  ['MessageCatch', 'message_catch', 'bpmn-message-catch', 'events', 56, 56],
  ['MessageThrow', 'message_throw', 'bpmn-message-throw', 'events', 56, 56],
  ['SignalThrow', 'signal_throw', 'bpmn-signal-throw', 'events', 56, 56],
  ['SignalCatch', 'signal_catch', 'bpmn-signal-catch', 'events', 56, 56],
  ['LinkThrow', 'link_throw', 'link-throw', 'events', 56, 56],
  ['LinkCatch', 'link_catch', 'link-catch', 'events', 56, 56],
  ['BoundaryMessage', 'boundary_message', 'bpmn-message-catch', 'events', 56, 56],
  ['BoundaryError', 'boundary_error', 'bpmn-error', 'events', 56, 56],
  ['BoundaryEscalation', 'boundary_escalation', 'bpmn-escalation-catch', 'events', 56, 56],
  ['UserTask', 'user_task', 'user', 'tasks', 240, 96],
  ['ServiceTask', 'service_task', 'bpmn-service-task', 'tasks', 240, 96],
  ['ScriptTask', 'script_task', 'bpmn-script-task', 'tasks', 240, 96],
  ['ManualTask', 'manual_task', 'bpmn-manual-task', 'tasks', 240, 96],
  ['SendTask', 'send_task', 'bpmn-message-throw', 'tasks', 240, 96],
  ['ReceiveTask', 'receive_task', 'bpmn-message-catch', 'tasks', 240, 96],
  ['SubProcess', 'sub_process', 'bpmn-subprocess', 'tasks', 240, 96],
  ['CallActivity', 'call_activity', 'bpmn-subprocess', 'tasks', 240, 96],
  ['ExclusiveGateway', 'exclusive_gateway', 'bpmn-gateway-exclusive', 'gateways', 72, 72],
  ['ParallelGateway', 'parallel_gateway', 'bpmn-gateway-parallel', 'gateways', 72, 72],
  ['InclusiveGateway', 'inclusive_gateway', 'bpmn-gateway-inclusive', 'gateways', 72, 72],
  ['EventBasedGateway', 'event_based_gateway', 'bpmn-gateway-event-based', 'gateways', 72, 72],
];

export function processTemplates() {
  return ELEMENTS.map(([kind, name, icon, group, width, height]) => ({
    node_type: `bpmn_${name}`, label: I18n.t(`bpmn.node_${name}`),
    description: I18n.t(`bpmn.node_${name}_hint`), icon, category: group,
    input_ports: ['Start', 'TimerStart', 'MessageStart', 'LinkCatch', 'BoundaryTimer', 'BoundaryMessage', 'BoundaryError', 'BoundaryEscalation'].includes(kind) ? [] : ['in'],
    output_ports: ['End', 'ErrorEnd', 'TerminateEnd', 'LinkThrow'].includes(kind) ? [] : ['full'],
    width, height,
  }));
}

export function processNodeKind(type) {
  const kind = ELEMENTS.find(([, name]) => type === `bpmn_${name}`)?.[0];
  if (!kind) throw new Error(I18n.t('bpmn.unsupported_element'));
  return kind;
}

export function processNodeConfig(kind) {
  if (kind === 'UserTask') return { assigneeUserId: null, outputMapping: {} };
  if (kind === 'ServiceTask') return { flowId: '', inputMapping: {}, outputMapping: {}, verification: 'Human', timeoutSeconds: 60 };
  if (kind === 'ScriptTask') return { script: '', outputMapping: {} };
  if (kind === 'ManualTask') return { assigneeUserId: null, instructions: '' };
  if (kind === 'ExclusiveGateway' || kind === 'InclusiveGateway') return { defaultFlowId: null };
  if (kind === 'TimerStart' || kind === 'TimerCatch') return { timer: { Duration: { seconds: 60 } } };
  if (kind === 'BoundaryTimer') return { attachedToId: null, cancelActivity: true, timer: { Duration: { seconds: 60 } } };
  if (kind === 'MessageStart') return { messageRef: '', outputMapping: {} };
  if (kind === 'MessageCatch') return { messageRef: '', correlationExpression: '', outputMapping: {} };
  if (kind === 'MessageThrow') return { messageRef: '', target: { Start: { definitionId: '' } },
    correlationExpression: '', payloadExpression: '', ttlSeconds: 3600 };
  if (kind === 'SignalThrow') return { signalRef: '', payloadExpression: '', ttlSeconds: 3600 };
  if (kind === 'SignalCatch') return { signalRef: '', outputMapping: {} };
  if (kind === 'LinkThrow' || kind === 'LinkCatch') return { definition: {
    id: `LinkDefinition_${crypto.randomUUID().replaceAll('-', '_')}`,
    name: '', sourceRefs: [], targetRef: null,
  } };
  if (kind === 'SendTask') return { messageRef: '', target: { Start: { definitionId: '' } },
    correlationExpression: '', payloadExpression: '', ttlSeconds: 3600 };
  if (kind === 'ReceiveTask') return { messageRef: '', correlationExpression: '', outputMapping: {} };
  if (kind === 'BoundaryMessage') return { attachedToId: '', cancelActivity: true,
    messageRef: '', correlationExpression: '', outputMapping: {} };
  if (kind === 'BoundaryError') return { attachedToId: '', errorRef: null, outputMapping: {} };
  if (kind === 'BoundaryEscalation') return { attachedToId: '', escalationRef: null,
    cancelActivity: true, outputMapping: {} };
  if (kind === 'SubProcess') {
    const suffix = crypto.randomUUID().replaceAll('-', '_');
    const start = `LocalStart_${suffix}`, end = `LocalEnd_${suffix}`;
    return { body: {
      nodes: [{ id: start, name: '', kind: 'Start' }, { id: end, name: '', kind: 'End' }],
      sequenceFlows: [{ id: `LocalFlow_${suffix}`, sourceId: start, targetId: end, condition: null }],
      variables: {}, diagram: { shapes: [], edges: [] },
    }, inputMapping: {}, outputMapping: {} };
  }
  if (kind === 'CallActivity') return { calledDefinitionId: '', calledVersion: 0,
    calledElement: { namespaceUri: '', processId: '' }, inputMapping: {}, outputMapping: {} };
  if (kind === 'ErrorEnd') return { errorRef: '' };
  return {};
}

export function processHasTimerStart(model) {
  return [model, ...(model.additionalProcesses || [])].some((body) =>
    body.nodes.some((node) => typeof node.kind === 'object' && 'TimerStart' in node.kind));
}

export function processHasMessageStart(model) {
  return [model, ...(model.additionalProcesses || [])].some((body) =>
    body.nodes.some((node) => typeof node.kind === 'object' && 'MessageStart' in node.kind));
}

export function processHasTimer(model) {
  const bodyHasTimer = (body) => body.nodes.some((node) => {
    if (typeof node.kind !== 'object') return false;
    const kind = Object.keys(node.kind)[0];
    return ['TimerStart', 'TimerCatch', 'BoundaryTimer'].includes(kind)
      || (kind === 'SubProcess' && bodyHasTimer(node.kind.SubProcess.body));
  });
  return [model, ...(model.additionalProcesses || [])].some(bodyHasTimer);
}

export function processBoundaryKind(type) {
  return ['bpmn_boundary_timer', 'bpmn_boundary_message', 'bpmn_boundary_error', 'bpmn_boundary_escalation'].includes(type);
}

export function emptyProcessModel() {
  const processId = `Process_${crypto.randomUUID().replaceAll('-', '_')}`;
  return {
    schemaVersion: 1, processId, variables: {},
    nodes: [{ id: 'Start', name: '', kind: 'Start' }, { id: 'End', name: '', kind: 'End' }],
    sequenceFlows: [{ id: 'Sequence_start_end', sourceId: 'Start', targetId: 'End', condition: null }],
    diagram: { shapes: [
      { elementId: 'Start', x: 80, y: 160, width: 56, height: 56 },
      { elementId: 'End', x: 400, y: 160, width: 56, height: 56 },
    ], edges: [{ sequenceFlowId: 'Sequence_start_end', waypoints: [{ x: 136, y: 188 }, { x: 400, y: 188 }] }] },
  };
}

export function processBody(model, path = [], processId = model.processId) {
  let body = processId === model.processId
    ? model : model.additionalProcesses?.find((candidate) => candidate.processId === processId);
  if (!body) throw new Error(I18n.t('bpmn.unsupported_element'));
  for (const nodeId of path) {
    const node = body.nodes.find((candidate) => candidate.id === nodeId);
    if (!node || typeof node.kind !== 'object' || !node.kind.SubProcess) throw new Error(I18n.t('bpmn.unsupported_element'));
    body = node.kind.SubProcess.body;
  }
  return body;
}

function remapActivityIo(io, remap, objectReference) {
  for (const input of io.dataInputs) input.id = remap(input.id, 'DataInput');
  for (const output of io.dataOutputs) output.id = remap(output.id, 'DataOutput');
  io.inputSetId = remap(io.inputSetId, 'InputSet');
  io.outputSetId = remap(io.outputSetId, 'OutputSet');
  io.inputSet = io.inputSet.map((id) => remap(id, 'DataInput'));
  io.outputSet = io.outputSet.map((id) => remap(id, 'DataOutput'));
  for (const association of io.inputAssociations) {
    const value = association.DirectRef || association.CelAssignment;
    value.id = remap(value.id, 'InputAssociation');
    if (association.DirectRef) value.sourceObjectRefId = objectReference(value.sourceObjectRefId);
    value.targetInputId = remap(value.targetInputId, 'DataInput');
  }
  for (const association of io.outputAssociations) {
    association.id = remap(association.id, 'OutputAssociation');
    association.sourceOutputId = remap(association.sourceOutputId, 'DataOutput');
    association.targetObjectRefId = objectReference(association.targetObjectRefId);
  }
  if (io.coordinatorOutput) {
    const output = io.coordinatorOutput;
    for (const item of output.dataOutputs) item.id = remap(item.id, 'CoordinatorDataOutput');
    output.outputSetId = remap(output.outputSetId, 'CoordinatorOutputSet');
    output.outputSet = output.outputSet.map((id) => remap(id, 'CoordinatorDataOutput'));
    for (const association of output.outputAssociations) {
      association.id = remap(association.id, 'CoordinatorOutputAssociation');
      association.sourceOutputId = remap(association.sourceOutputId, 'CoordinatorDataOutput');
      association.targetObjectRefId = objectReference(association.targetObjectRefId);
    }
  }
}

export function cloneProcessActivityIo(io) {
  const copy = structuredClone(io);
  const ids = new Map();
  const remap = (id, prefix) => {
    if (!ids.has(id)) ids.set(id, `${prefix}_${crypto.randomUUID().replaceAll('-', '_')}`);
    return ids.get(id);
  };
  remapActivityIo(copy, remap, (id) => id);
  return copy;
}

export function cloneProcessBody(body) {
  const copy = structuredClone(body);
  const ids = new Map();
  const remap = (id, prefix = 'Element') => {
    if (!ids.has(id)) ids.set(id, `${prefix}_${crypto.randomUUID().replaceAll('-', '_')}`);
    return ids.get(id);
  };
  for (const node of copy.nodes) remap(node.id, 'Node');
  for (const node of copy.nodes) {
    const kind = typeof node.kind === 'object' && Object.keys(node.kind)[0];
    if (kind === 'LinkThrow' || kind === 'LinkCatch') {
      remap(node.kind[kind].definition.id, 'LinkDefinition');
    }
  }
  const flows = new Map();
  for (const flow of copy.sequenceFlows) flows.set(flow.id, `Flow_${crypto.randomUUID().replaceAll('-', '_')}`);
  const modeling = copy.modeling;
  const remapLaneSet = (set) => {
    set.id = remap(set.id, 'LaneSet');
    for (const lane of set.lanes) {
      lane.id = remap(lane.id, 'Lane');
      lane.flowNodeRefs = lane.flowNodeRefs.map((id) => ids.get(id));
      for (const child of lane.childLaneSets || []) remapLaneSet(child);
    }
  };
  for (const set of modeling?.laneSets || []) remapLaneSet(set);
  for (const object of modeling?.dataObjects || []) object.id = remap(object.id, 'DataObject');
  for (const reference of modeling?.dataObjectReferences || []) {
    reference.id = remap(reference.id, 'DataObjectRef');
    reference.dataObjectRef = ids.get(reference.dataObjectRef);
  }
  for (const annotation of modeling?.textAnnotations || []) annotation.id = remap(annotation.id, 'Annotation');
  for (const association of modeling?.associations || []) {
    association.id = remap(association.id, 'Association');
    association.sourceRef = ids.get(association.sourceRef);
    association.targetRef = ids.get(association.targetRef);
  }
  for (const node of copy.nodes) {
    node.id = ids.get(node.id);
    if (node.activityIo) {
      remapActivityIo(node.activityIo, remap, (id) => ids.get(id));
    }
    if (typeof node.kind !== 'object') continue;
    const [kind, config] = Object.entries(node.kind)[0];
    if (kind === 'LinkThrow' || kind === 'LinkCatch') {
      config.definition.id = ids.get(config.definition.id);
      config.definition.sourceRefs = config.definition.sourceRefs.map((id) => ids.get(id) || id);
      if (config.definition.targetRef) config.definition.targetRef = ids.get(config.definition.targetRef) || config.definition.targetRef;
    }
    if (kind === 'SubProcess') config.body = cloneProcessBody(config.body);
    if (['ExclusiveGateway', 'InclusiveGateway'].includes(kind) && config.defaultFlowId) config.defaultFlowId = flows.get(config.defaultFlowId);
    if (['BoundaryTimer', 'BoundaryMessage', 'BoundaryError', 'BoundaryEscalation'].includes(kind)) config.attachedToId = ids.get(config.attachedToId);
  }
  for (const flow of copy.sequenceFlows) {
    flow.id = flows.get(flow.id);
    flow.sourceId = ids.get(flow.sourceId);
    flow.targetId = ids.get(flow.targetId);
  }
  for (const shape of copy.diagram.shapes) shape.elementId = ids.get(shape.elementId);
  for (const edge of copy.diagram.edges) edge.sequenceFlowId = flows.get(edge.sequenceFlowId);
  for (const shape of copy.diagram.modelingShapes || []) {
    shape.diId = remap(shape.diId, 'Shape');
    shape.elementId = ids.get(shape.elementId);
  }
  for (const edge of copy.diagram.modelingEdges || []) {
    edge.diId = remap(edge.diId, 'Edge');
    edge.elementId = ids.get(edge.elementId);
  }
  return copy;
}

export function processToCanvas(model, path = [], processId = model.processId) {
  const body = processBody(model, path, processId);
  const shapes = new Map(body.diagram.shapes.map((shape) => [shape.elementId, shape]));
  const routes = new Map(body.diagram.edges.map((edge) => [edge.sequenceFlowId, edge.waypoints]));
  const nodes = body.nodes.map((node, index) => {
    const kind = typeof node.kind === 'string' ? node.kind : Object.keys(node.kind)[0];
    const element = ELEMENTS.find(([value]) => value === kind);
    if (!element) throw new Error(I18n.t('bpmn.unsupported_element'));
    const shape = shapes.get(node.id);
    return { id: node.id, type: `bpmn_${element[1]}`, label: node.name,
      config: typeof node.kind === 'string' ? {} : structuredClone(node.kind[kind]),
      ...(node.repeat == null ? {} : { repeat: structuredClone(node.repeat) }),
      ...(node.activityIo == null ? {} : { activityIo: structuredClone(node.activityIo) }),
      x: shape?.x ?? index * 280, y: shape?.y ?? 160,
      width: shape?.width ?? element[4], height: shape?.height ?? element[5] };
  });
  const byId = new Map(nodes.map((node) => [node.id, node]));
  for (const node of nodes) {
    if (!processBoundaryKind(node.type) || shapes.has(node.id)) continue;
    const parent = byId.get(node.config.attachedToId);
    if (!parent) continue;
    const siblings = nodes.filter((candidate) => processBoundaryKind(candidate.type) && candidate.config.attachedToId === parent.id);
    const angle = Math.PI / 4 + siblings.indexOf(node) * 2 * Math.PI / siblings.length;
    const dx = Math.cos(angle), dy = Math.sin(angle);
    const scale = 1 / Math.max(Math.abs(dx) / (parent.width / 2), Math.abs(dy) / (parent.height / 2));
    node.x = parent.x + parent.width / 2 + dx * scale - node.width / 2;
    node.y = parent.y + parent.height / 2 + dy * scale - node.height / 2;
  }
  const edges = body.sequenceFlows.map((edge) => ({ id: edge.id,
    from_node: edge.sourceId, to_node: edge.targetId, from_port: 'full', to_port: 'in',
    condition: edge.condition, waypoints: structuredClone(routes.get(edge.id) || []),
    ...(edge.callStartNodeId == null ? {} : { callStartNodeId: edge.callStartNodeId }),
    originalEndpoints: [byId.get(edge.sourceId)?.x, byId.get(edge.sourceId)?.y,
      byId.get(edge.targetId)?.x, byId.get(edge.targetId)?.y] }));
  return { nodes, edges };
}

export function canvasToProcess(model, nodes, edges, edgePoints, path = [], processId = model.processId) {
  const graph = {
    nodes: nodes.map((node) => {
      const kind = processNodeKind(node.type);
      return { id: node.id, name: node.label || '',
        kind: ['Start', 'End', 'TerminateEnd', 'ParallelGateway', 'EventBasedGateway'].includes(kind) ? kind : { [kind]: structuredClone(node.config) },
        ...(node.repeat == null ? {} : { repeat: structuredClone(node.repeat) }),
        ...(node.activityIo == null ? {} : { activityIo: structuredClone(node.activityIo) }) };
    }),
    sequenceFlows: edges.map((edge) => ({ id: edge.id, sourceId: edge.from_node, targetId: edge.to_node, condition: edge.condition ?? null,
      ...(edge.callStartNodeId == null ? {} : { callStartNodeId: edge.callStartNodeId }) })),
    diagram: { shapes: nodes.map((node) => ({ elementId: node.id, x: node.x, y: node.y, width: node.width, height: node.height })),
      edges: edges.map((edge) => ({ sequenceFlowId: edge.id, waypoints: edgePoints(edge) })),
      ...(processBody(model, path, processId).diagram.modelingShapes == null ? {} : {
        modelingShapes: structuredClone(processBody(model, path, processId).diagram.modelingShapes) }),
      ...(processBody(model, path, processId).diagram.modelingEdges == null ? {} : {
        modelingEdges: structuredClone(processBody(model, path, processId).diagram.modelingEdges) }) },
  };
  const full = structuredClone(model);
  Object.assign(processBody(full, path, processId), graph);
  return full;
}

export function processStatusLabel(status) { return I18n.t(`bpmn.status_${status.toLowerCase()}`); }

export const PROCESS_DOCUMENT_BYTES = 512 * 1024;
export const PROCESS_VALUE_BYTES = 256 * 1024;

export function processEditorLabels() {
  return Object.fromEntries(['editor', 'find', 'replace', 'match_case', 'regex', 'prev', 'next', 'replace_one', 'replace_all', 'matches', 'no_matches', 'bad_regex', 'folded_lines']
    .map((key) => [key, I18n.t(`project_studio.ce_${key}`)])
    .concat([['close', I18n.t('project_studio.action_close')]]));
}

export function processCommand() {
  let previous = null;
  let commandId = null;
  return (payload) => {
    const signature = JSON.stringify(payload);
    if (signature !== previous) {
      previous = signature;
      commandId = crypto.randomUUID();
    }
    return { ...payload, commandId };
  };
}

export function processJson(value) {
  const parsed = JSON.parse(value);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error(I18n.t('bpmn.object_required'));
  checkProcessValue(parsed);
  return parsed;
}

export function checkProcessValue(value) {
  if (new TextEncoder().encode(JSON.stringify(value)).byteLength > PROCESS_VALUE_BYTES) throw new Error(I18n.t('bpmn.value_too_large'));
}

export function checkProcessDocument(value) {
  if (new TextEncoder().encode(value).byteLength > PROCESS_DOCUMENT_BYTES) throw new Error(I18n.t('bpmn.document_too_large'));
}
