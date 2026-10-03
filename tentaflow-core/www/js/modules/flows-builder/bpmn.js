// ============ File: flows-builder/bpmn.js — BPMN presentation and lossless canvas model mapping ============

import { I18n } from '/js/i18n.js';

const ELEMENTS = [
  ['Start', 'start', 'play', 'events', 56, 56],
  ['End', 'end', 'stop', 'events', 56, 56],
  ['TimerStart', 'timer_start', 'clock', 'events', 56, 56],
  ['TimerCatch', 'timer_catch', 'clock', 'events', 56, 56],
  ['BoundaryTimer', 'boundary_timer', 'clock', 'events', 56, 56],
  ['UserTask', 'user_task', 'user', 'tasks', 240, 96],
  ['ServiceTask', 'service_task', 'flow', 'tasks', 240, 96],
  ['ExclusiveGateway', 'exclusive_gateway', 'branch', 'gateways', 72, 72],
  ['ParallelGateway', 'parallel_gateway', 'plus', 'gateways', 72, 72],
];

export function processTemplates() {
  return ELEMENTS.map(([kind, name, icon, group, width, height]) => ({
    node_type: `bpmn_${name}`, label: I18n.t(`bpmn.node_${name}`),
    description: I18n.t(`bpmn.node_${name}_hint`), icon, category: group,
    input_ports: ['Start', 'TimerStart', 'BoundaryTimer'].includes(kind) ? [] : ['in'],
    output_ports: kind === 'End' ? [] : ['full'],
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
  if (kind === 'ExclusiveGateway') return { defaultFlowId: null };
  if (kind === 'TimerStart' || kind === 'TimerCatch') return { timer: { Duration: { seconds: 60 } } };
  if (kind === 'BoundaryTimer') return { attachedToId: null, cancelActivity: true, timer: { Duration: { seconds: 60 } } };
  return {};
}

export function processHasTimerStart(model) {
  return model.nodes.some((node) => typeof node.kind === 'object' && 'TimerStart' in node.kind);
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

export function processToCanvas(model) {
  const shapes = new Map(model.diagram.shapes.map((shape) => [shape.elementId, shape]));
  const routes = new Map(model.diagram.edges.map((edge) => [edge.sequenceFlowId, edge.waypoints]));
  const nodes = model.nodes.map((node, index) => {
    const kind = typeof node.kind === 'string' ? node.kind : Object.keys(node.kind)[0];
    const element = ELEMENTS.find(([value]) => value === kind);
    if (!element) throw new Error(I18n.t('bpmn.unsupported_element'));
    const shape = shapes.get(node.id);
    return { id: node.id, type: `bpmn_${element[1]}`, label: node.name,
      config: typeof node.kind === 'string' ? {} : structuredClone(node.kind[kind]),
      x: shape?.x ?? index * 280, y: shape?.y ?? 160,
      width: shape?.width ?? element[4], height: shape?.height ?? element[5] };
  });
  const byId = new Map(nodes.map((node) => [node.id, node]));
  for (const node of nodes) {
    if (node.type !== 'bpmn_boundary_timer' || shapes.has(node.id)) continue;
    const parent = byId.get(node.config.attachedToId);
    if (!parent) continue;
    const siblings = nodes.filter((candidate) => candidate.type === 'bpmn_boundary_timer' && candidate.config.attachedToId === parent.id);
    const angle = Math.PI / 4 + siblings.indexOf(node) * 2 * Math.PI / siblings.length;
    const dx = Math.cos(angle), dy = Math.sin(angle);
    const scale = 1 / Math.max(Math.abs(dx) / (parent.width / 2), Math.abs(dy) / (parent.height / 2));
    node.x = parent.x + parent.width / 2 + dx * scale - node.width / 2;
    node.y = parent.y + parent.height / 2 + dy * scale - node.height / 2;
  }
  const edges = model.sequenceFlows.map((edge) => ({ id: edge.id,
    from_node: edge.sourceId, to_node: edge.targetId, from_port: 'full', to_port: 'in',
    condition: edge.condition, waypoints: structuredClone(routes.get(edge.id) || []),
    originalEndpoints: [byId.get(edge.sourceId)?.x, byId.get(edge.sourceId)?.y,
      byId.get(edge.targetId)?.x, byId.get(edge.targetId)?.y] }));
  return { nodes, edges };
}

export function canvasToProcess(model, nodes, edges, edgePoints) {
  return { schemaVersion: model.schemaVersion, processId: model.processId,
    variables: structuredClone(model.variables),
    ...(model.timerTimezone == null ? {} : { timerTimezone: model.timerTimezone }),
    ...(model.workCalendar == null ? {} : { workCalendar: structuredClone(model.workCalendar) }),
    ...(model.calendarPin == null ? {} : { calendarPin: structuredClone(model.calendarPin) }),
    nodes: nodes.map((node) => {
      const kind = processNodeKind(node.type);
      return { id: node.id, name: node.label || '',
        kind: ['Start', 'End', 'ParallelGateway'].includes(kind) ? kind : { [kind]: structuredClone(node.config) } };
    }),
    sequenceFlows: edges.map((edge) => ({ id: edge.id, sourceId: edge.from_node, targetId: edge.to_node, condition: edge.condition ?? null })),
    diagram: { shapes: nodes.map((node) => ({ elementId: node.id, x: node.x, y: node.y, width: node.width, height: node.height })),
      edges: edges.map((edge) => ({ sequenceFlowId: edge.id, waypoints: edgePoints(edge) })) },
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
