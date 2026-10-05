// ============ File: flows-builder/bpmn.js — BPMN presentation and lossless canvas model mapping ============

import { I18n } from '/js/i18n.js';

const ELEMENTS = [
  ['Start', 'start', 'play', 'events', 56, 56],
  ['End', 'end', 'stop', 'events', 56, 56],
  ['ErrorEnd', 'error_end', 'alert-triangle', 'events', 56, 56],
  ['TerminateEnd', 'terminate_end', 'ban', 'events', 56, 56],
  ['TimerStart', 'timer_start', 'clock', 'events', 56, 56],
  ['TimerCatch', 'timer_catch', 'clock', 'events', 56, 56],
  ['BoundaryTimer', 'boundary_timer', 'clock', 'events', 56, 56],
  ['MessageStart', 'message_start', 'play', 'events', 56, 56],
  ['MessageCatch', 'message_catch', 'mail', 'events', 56, 56],
  ['MessageThrow', 'message_throw', 'send', 'events', 56, 56],
  ['BoundaryMessage', 'boundary_message', 'mail', 'events', 56, 56],
  ['BoundaryError', 'boundary_error', 'alert-triangle', 'events', 56, 56],
  ['BoundaryEscalation', 'boundary_escalation', 'alert-triangle', 'events', 56, 56],
  ['UserTask', 'user_task', 'user', 'tasks', 240, 96],
  ['ServiceTask', 'service_task', 'flow', 'tasks', 240, 96],
  ['ScriptTask', 'script_task', 'code', 'tasks', 240, 96],
  ['ManualTask', 'manual_task', 'check', 'tasks', 240, 96],
  ['SubProcess', 'sub_process', 'layers', 'tasks', 240, 96],
  ['CallActivity', 'call_activity', 'layers', 'tasks', 240, 96],
  ['ExclusiveGateway', 'exclusive_gateway', 'branch', 'gateways', 72, 72],
  ['ParallelGateway', 'parallel_gateway', 'plus', 'gateways', 72, 72],
  ['InclusiveGateway', 'inclusive_gateway', 'branch', 'gateways', 72, 72],
  ['EventBasedGateway', 'event_based_gateway', 'branch', 'gateways', 72, 72],
];

export function processTemplates() {
  return ELEMENTS.map(([kind, name, icon, group, width, height]) => ({
    node_type: `bpmn_${name}`, label: I18n.t(`bpmn.node_${name}`),
    description: I18n.t(`bpmn.node_${name}_hint`), icon, category: group,
    input_ports: ['Start', 'TimerStart', 'MessageStart', 'BoundaryTimer', 'BoundaryMessage', 'BoundaryError', 'BoundaryEscalation'].includes(kind) ? [] : ['in'],
    output_ports: ['End', 'ErrorEnd', 'TerminateEnd'].includes(kind) ? [] : ['full'],
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
  return model.nodes.some((node) => typeof node.kind === 'object' && 'TimerStart' in node.kind);
}

export function processHasMessageStart(model) {
  return model.nodes.some((node) => typeof node.kind === 'object' && 'MessageStart' in node.kind);
}

export function processHasTimer(model) {
  return model.nodes.some((node) => {
    if (typeof node.kind !== 'object') return false;
    const kind = Object.keys(node.kind)[0];
    return ['TimerStart', 'TimerCatch', 'BoundaryTimer'].includes(kind)
      || (kind === 'SubProcess' && processHasTimer(node.kind.SubProcess.body));
  });
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

export function processBody(model, path = []) {
  let body = model;
  for (const nodeId of path) {
    const node = body.nodes.find((candidate) => candidate.id === nodeId);
    if (!node || typeof node.kind !== 'object' || !node.kind.SubProcess) throw new Error(I18n.t('bpmn.unsupported_element'));
    body = node.kind.SubProcess.body;
  }
  return body;
}

export function cloneProcessBody(body) {
  const copy = structuredClone(body);
  const ids = new Map();
  for (const node of copy.nodes) ids.set(node.id, `Node_${crypto.randomUUID().replaceAll('-', '_')}`);
  const flows = new Map();
  for (const flow of copy.sequenceFlows) flows.set(flow.id, `Flow_${crypto.randomUUID().replaceAll('-', '_')}`);
  for (const node of copy.nodes) {
    node.id = ids.get(node.id);
    if (typeof node.kind !== 'object') continue;
    const [kind, config] = Object.entries(node.kind)[0];
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
  return copy;
}

export function processToCanvas(model, path = []) {
  const body = processBody(model, path);
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
    originalEndpoints: [byId.get(edge.sourceId)?.x, byId.get(edge.sourceId)?.y,
      byId.get(edge.targetId)?.x, byId.get(edge.targetId)?.y] }));
  return { nodes, edges };
}

export function canvasToProcess(model, nodes, edges, edgePoints, path = []) {
  const graph = {
    nodes: nodes.map((node) => {
      const kind = processNodeKind(node.type);
      return { id: node.id, name: node.label || '',
        kind: ['Start', 'End', 'TerminateEnd', 'ParallelGateway', 'EventBasedGateway'].includes(kind) ? kind : { [kind]: structuredClone(node.config) },
        ...(node.repeat == null ? {} : { repeat: structuredClone(node.repeat) }) };
    }),
    sequenceFlows: edges.map((edge) => ({ id: edge.id, sourceId: edge.from_node, targetId: edge.to_node, condition: edge.condition ?? null })),
    diagram: { shapes: nodes.map((node) => ({ elementId: node.id, x: node.x, y: node.y, width: node.width, height: node.height })),
      edges: edges.map((edge) => ({ sequenceFlowId: edge.id, waypoints: edgePoints(edge) })) },
  };
  if (path.length) {
    const full = structuredClone(model);
    Object.assign(processBody(full, path), graph);
    return full;
  }
  return { schemaVersion: model.schemaVersion, processId: model.processId,
    variables: structuredClone(model.variables),
    ...(model.timerTimezone == null ? {} : { timerTimezone: model.timerTimezone }),
    ...(model.workCalendar == null ? {} : { workCalendar: structuredClone(model.workCalendar) }),
    ...(model.calendarPin == null ? {} : { calendarPin: structuredClone(model.calendarPin) }),
    ...(model.messages?.length ? { messages: structuredClone(model.messages) } : {}),
    ...(model.errors?.length ? { errors: structuredClone(model.errors) } : {}),
    ...(model.escalations?.length ? { escalations: structuredClone(model.escalations) } : {}),
    ...(model.targetNamespace == null ? {} : { targetNamespace: model.targetNamespace }),
    ...graph,
  };
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
