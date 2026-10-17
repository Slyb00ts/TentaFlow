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

// The settings a block starts with: the keys it requires, empty. Choosing a type
// replaces the settings, so keys of the previous type never linger.
export const processBlockTemplate = (type) => (type === 'pii_filter' ? {} : { model: '' });

// Block names come from the process options; the locale wins when it knows the
// block, the server's English text covers a block this client has no key for.
export function processBlockLabel(block) {
  return localized(`bpmn.block_${block.nodeType}`, block.label || block.nodeType);
}

const SERVICE_TASK_SHAPE = { input_ports: ['in'], output_ports: ['full'], width: 240, height: 96 };

function localized(key, fallback) {
  const text = I18n.t(key);
  return text && text !== key ? text : fallback;
}

const MAX_NODE_NAME_BYTES = 256;
// Bidirectional controls reorder the text around them, so the server refuses
// them in a node name (processes/model.rs valid_node_name).
const BIDI_CONTROLS = /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/gu;
const utf8Length = (char) => {
  const code = char.codePointAt(0);
  return code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
};

// The server refuses a node name over 256 UTF-8 bytes or with a control
// character, and the palette label becomes the node name, so a long or odd
// addon name must be cleaned here rather than fail the save.
export function boundedNodeName(text, limit = MAX_NODE_NAME_BYTES) {
  let out = '';
  let bytes = 0;
  for (const char of String(text ?? '').replace(/[\u0000-\u001f\u007f-\u009f]+/g, ' ').replace(BIDI_CONTROLS, '').trim()) {
    bytes += utf8Length(char);
    if (bytes > limit) break;
    out += char;
  }
  return out.trimEnd();
}

const nameBytes = (text) => [...text].reduce((sum, char) => sum + utf8Length(char), 0);

// A label with nothing but whitespace, control or invisible format characters
// (zero-width space, BOM, soft hyphen) shows nothing, and so do the filler
// letters below that are not category Cf; the server applies the same
// predicate in processes/tool_host.rs.
const BLANK_LABEL = /^[\p{White_Space}\p{Cc}\p{Cf}\u034f\u115f\u1160\u3164\uffa0]*$/u;
export const isBlankLabel = (text) => BLANK_LABEL.test(String(text ?? ''));

// Keeps both ends of an id: instances of one package differ at the end, so a
// cut from the start would make `notes-instance-1` and `-2` read the same.
function shortenId(text, limit) {
  const id = boundedNodeName(text);
  if (nameBytes(id) <= limit) return id;
  const chars = [...id];
  const room = limit - 3;
  let tail = '';
  let tailBytes = 0;
  for (let index = chars.length - 1; index >= 0; index -= 1) {
    const size = utf8Length(chars[index]);
    if (tailBytes + size > Math.ceil(room * 2 / 3)) break;
    tail = chars[index] + tail;
    tailBytes += size;
  }
  const head = boundedNodeName(id, Math.max(0, room - tailBytes));
  return `${head}\u2026${tail}`;
}

const ADDON_NAME_RESERVE_BYTES = 16;
const MIN_ID_BYTES = 8;
const ROW_ID_BYTES = 64;

// "Addon › tool (id)"; the id part is bounded first, with its parentheses kept
// outside the cut, then the tool name yields only if the label is still too
// long, and the addon name takes whatever room is left. A label never starts
// with the separator: an addon name that does not fit is dropped whole.
function toolLabel(tool, withId) {
  let toolName = boundedNodeName(tool.toolName);
  const addonName = isBlankLabel(tool.addonName) ? '' : boundedNodeName(tool.addonName);
  const addonId = withId ? boundedNodeName(tool.addonId) : '';
  const separator = addonName ? ' › ' : '';
  const reserve = addonName ? nameBytes(separator) + Math.min(nameBytes(addonName), ADDON_NAME_RESERVE_BYTES) : 0;
  let idPart = '';
  if (addonId) {
    const frame = 3;
    const idRoom = Math.max(MIN_ID_BYTES, MAX_NODE_NAME_BYTES - frame - reserve - nameBytes(toolName));
    idPart = ` (${shortenId(addonId, idRoom)})`;
  }
  const toolRoom = MAX_NODE_NAME_BYTES - reserve - nameBytes(idPart);
  if (nameBytes(toolName) > toolRoom) toolName = boundedNodeName(toolName, Math.max(1, toolRoom));
  const tail = `${separator}${toolName}${idPart}`;
  const addonRoom = MAX_NODE_NAME_BYTES - nameBytes(tail);
  const addon = addonName ? boundedNodeName(addonName, addonRoom) : '';
  return addon ? `${addon}${tail}` : `${toolName}${idPart}`;
}

/**
 * Palette entries that drop a ready-made Service task. `preset` is merged over
 * the Service task's default configuration, so the entry never asks which
 * implementation the task has. `subgroup` orders them inside the palette group.
 */
function tentaflowTemplates(options) {
  const entry = (subgroup, icon, label, description, preset, row = {}) => ({
    node_type: 'bpmn_service_task', label, description, icon, category: 'tentaflow',
    subgroup, preset, ...SERVICE_TASK_SHAPE, ...row,
  });
  const tools = options.serviceTools || [];
  // Two instances of one package share addon and tool names; the instance id
  // is the only thing left to tell their rows apart.
  const labelCounts = new Map();
  for (const tool of tools) {
    const label = toolLabel(tool, false);
    labelCounts.set(label, (labelCounts.get(label) ?? 0) + 1);
  }
  const toolRow = (tool) => {
    const id = shortenId(tool.addonId, ROW_ID_BYTES);
    const blank = isBlankLabel(tool.addonName);
    // The id leads, because the two-line clamp cuts the end of a long instance
    // name; a blank name already shows the id, once.
    const duplicate = labelCounts.get(toolLabel(tool, false)) > 1;
    const rowSub = blank ? id : duplicate ? `(${id}) ${boundedNodeName(tool.addonName)}` : boundedNodeName(tool.addonName);
    return { rowName: boundedNodeName(tool.toolName), rowSub };
  };
  return [
    ...(options.serviceFlows || []).map((flow) =>
      entry('flows', 'flow', boundedNodeName(flow.name), flow.description || I18n.t('bpmn.palette_flow_hint'),
        { flowId: flow.flowId })),
    ...(options.serviceBlocks || []).map((block) => entry('blocks', block.nodeType === 'llm' ? 'brain'
      : block.nodeType === 'pii_filter' ? 'shield' : 'sparkle',
    localized(`bpmn.block_${block.nodeType}`, block.label),
    localized(`bpmn.block_${block.nodeType}_hint`, block.description),
    { implementation: { Block: { nodeType: block.nodeType, config: processBlockTemplate(block.nodeType) } } })),
    ...tools.map((tool) => entry('tools', 'puzzle',
      toolLabel(tool, labelCounts.get(toolLabel(tool, false)) > 1),
      tool.description || tool.addonId,
      { implementation: { AddonTool: { addonId: tool.addonId, toolName: tool.toolName } } }, toolRow(tool))),
  ];
}

export function processTemplates(options) {
  const elements = ELEMENTS.map(([kind, name, icon, group, width, height]) => ({
    node_type: `bpmn_${name}`, label: I18n.t(`bpmn.node_${name}`),
    description: I18n.t(`bpmn.node_${name}_hint`), icon, category: group,
    input_ports: ['Start', 'TimerStart', 'MessageStart', 'LinkCatch', 'BoundaryTimer', 'BoundaryMessage', 'BoundaryError', 'BoundaryEscalation'].includes(kind) ? [] : ['in'],
    output_ports: ['End', 'ErrorEnd', 'TerminateEnd', 'LinkThrow'].includes(kind) ? [] : ['full'],
    width, height,
  }));
  return options ? [...elements, ...tentaflowTemplates(options)] : elements;
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

// Which implementation a Service task runs; a task without the field is a flow.
export function processServiceKind(config) {
  const implementation = config?.implementation;
  return implementation?.Block ? 'block' : implementation?.AddonTool ? 'tool' : 'flow';
}

// Problems the canvas marks on a node itself. A block whose template carries a
// `model` key and a published Call without an exact version are both saved or
// refused only later (publication, or the wire codec), so the author sees them
// here first.
export function processNodeWarnings(node) {
  const warnings = [];
  if (node.type === 'bpmn_service_task') {
    const block = node.config?.implementation?.Block;
    if (block && Object.hasOwn(processBlockTemplate(block.nodeType), 'model')
      && (typeof block.config?.model !== 'string' || block.config.model.trim() === '')) {
      warnings.push('empty_model');
    }
  }
  if (node.type === 'bpmn_call_activity' && !node.config?.localBody
    && !(Number.isInteger(node.config?.calledVersion) && node.config.calledVersion >= 1)) {
    warnings.push('call_version');
  }
  return warnings;
}

/**
 * What happens to a Service task's outcomes, derived from the graph alone.
 * There is no server-side rule table: the rules below mirror the engine in
 * processes/runtime.rs (plan_service_result, error_handler) and processes/jobs.rs
 * (classify_result, fail_claim):
 * - only a Contract result (a non-empty `resultExpression`) can be Error,
 *   NeedsHuman or Cancelled; a platform failure (flow, tool or expression
 *   error) is always an incident and no boundary event catches it;
 * - Error goes to the boundary with the exact code, else to the catch-all
 *   boundary, else (inside a sub-process) to the enclosing scope, else an incident;
 * - NeedsHuman goes to the escalation boundary with the exact code, else the
 *   catch-all one, else the initiator's verification task;
 * - Cancelled is an incident; a service timeout after dispatch is an incident
 *   (reason SERVICE_TIMEOUT, outcome uncertain);
 * - a boundary timer runs on its own clock from activation and is not a reaction
 *   to the service timeout.
 */
export function processResultRouting(graph, nodeId) {
  const node = graph.nodes.find((candidate) => candidate.id === nodeId);
  if (!node || node.type !== 'bpmn_service_task') return null;
  const model = graph.processModel || {};
  const config = node.config || {};
  const hasResult = typeof config.resultExpression === 'string' && config.resultExpression.length > 0;
  const nested = (graph.processPath || []).length > 0;
  const byId = new Map(graph.nodes.map((candidate) => [candidate.id, candidate]));
  const targetsOf = (id) => graph.edges.filter((edge) => edge.from_node === id)
    .map((edge) => byId.get(edge.to_node)).filter(Boolean)
    .map((target) => ({ id: target.id, label: target.label || '', type: target.type }));
  const describe = (boundary, code, undeclared) => ({
    id: boundary.id, label: boundary.label || '', type: boundary.type, code, catchAll: code === null && !undeclared,
    undeclared, interrupting: boundary.config.cancelActivity !== false, targets: targetsOf(boundary.id),
  });
  const attached = (type) => graph.nodes.filter((candidate) => candidate.type === type
    && candidate.config.attachedToId === nodeId);
  const declared = (list, idKey, codeKey, ref) => {
    if (ref == null) return { code: null, undeclared: false };
    const found = (list || []).find((entry) => entry[idKey] === ref);
    return { code: found ? found[codeKey] : null, undeclared: !found };
  };
  const errors = attached('bpmn_boundary_error').map((boundary) => {
    const { code, undeclared } = declared(model.errors, 'errorId', 'errorCode', boundary.config.errorRef);
    return describe(boundary, code, undeclared);
  });
  const escalations = attached('bpmn_boundary_escalation').map((boundary) => {
    const { code, undeclared } = declared(model.escalations, 'escalationId', 'escalationCode', boundary.config.escalationRef);
    return describe(boundary, code, undeclared);
  });
  const timers = attached('bpmn_boundary_timer').map((boundary) => ({
    ...describe(boundary, null, false), timer: boundary.config.timer,
  }));
  const catchAll = (list) => list.find((entry) => entry.catchAll) || null;
  const specific = (list) => list.filter((entry) => !entry.catchAll && !entry.undeclared);
  const human = typeof config.verification === 'string';
  const rows = [];
  rows.push({ outcome: 'completed', route: 'next', gate: human ? 'human' : 'condition', targets: targetsOf(nodeId) });
  if (!human) rows.push({ outcome: 'rejected', route: 'incident', code: 'VERIFICATION_FAILED' });
  if (hasResult) {
    for (const entry of specific(errors)) rows.push({ outcome: 'error', route: 'boundary', code: entry.code, boundary: entry, targets: entry.targets });
    const otherError = catchAll(errors);
    rows.push({ outcome: 'error', route: otherError ? 'boundary' : nested ? 'parent' : 'incident', other: true,
      code: null, boundary: otherError, targets: otherError ? otherError.targets : [] });
    for (const entry of specific(escalations)) rows.push({ outcome: 'needs_human', route: 'boundary', code: entry.code, boundary: entry, targets: entry.targets });
    const otherEscalation = catchAll(escalations);
    rows.push({ outcome: 'needs_human', route: otherEscalation ? 'boundary' : 'review', other: true,
      code: null, boundary: otherEscalation, targets: otherEscalation ? otherEscalation.targets : [] });
    rows.push({ outcome: 'cancelled', route: 'incident', code: 'SERVICE_ERROR' });
  } else {
    for (const outcome of ['error', 'needs_human', 'cancelled']) rows.push({ outcome, route: 'unavailable' });
  }
  rows.push({ outcome: 'platform', route: 'incident' });
  rows.push({ outcome: 'timeout', route: 'incident', code: 'SERVICE_TIMEOUT', seconds: config.timeoutSeconds });
  for (const entry of timers) rows.push({ outcome: 'timer', route: 'boundary', boundary: entry, targets: entry.targets, timer: entry.timer });

  const warnings = [];
  if (!rows[0].targets.length) warnings.push({ key: 'no_outgoing' });
  if (!hasResult && errors.length) warnings.push({ key: 'error_boundary_without_result' });
  if (!hasResult && escalations.length) warnings.push({ key: 'escalation_boundary_without_result' });
  for (const entry of [...errors, ...escalations, ...timers]) {
    if (!entry.targets.length) warnings.push({ key: 'boundary_no_flow', name: entry.label || entry.id });
    if (entry.undeclared) warnings.push({ key: 'undeclared_reference', name: entry.label || entry.id });
  }
  for (const list of [errors, escalations]) {
    const seen = new Set();
    for (const entry of list) {
      const key = entry.undeclared ? `u:${entry.id}` : entry.code ?? '';
      if (seen.has(key)) warnings.push({ key: 'duplicate_handler', name: entry.label || entry.id });
      seen.add(key);
    }
  }
  for (const entry of timers) {
    const seconds = entry.timer?.Duration?.seconds;
    if (Number.isInteger(seconds) && Number.isInteger(config.timeoutSeconds) && seconds >= config.timeoutSeconds) {
      warnings.push({ key: 'timer_after_timeout', name: entry.label || entry.id });
    }
  }

  // The first catch-all, then each declared code, that has no handler yet: a
  // second handler for one code is refused at publication.
  const nextRef = (list, declarations, idKey, codeKey) => {
    const used = new Set(list.map((entry) => (entry.catchAll ? null : entry.undeclared ? undefined : entry.code)));
    if (!used.has(null)) return { ref: null };
    const free = (declarations || []).find((entry) => !used.has(entry[codeKey]));
    return free ? { ref: free[idKey] } : null;
  };
  return {
    node, hasResult, verification: human ? 'Human' : 'Condition', rows, warnings,
    boundaries: { errors, escalations, timers },
    add: { error: nextRef(errors, model.errors, 'errorId', 'errorCode'),
      escalation: hasResult ? nextRef(escalations, model.escalations, 'escalationId', 'escalationCode') : null },
  };
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
