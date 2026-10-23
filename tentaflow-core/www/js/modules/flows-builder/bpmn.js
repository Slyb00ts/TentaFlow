// ============ File: flows-builder/bpmn.js — BPMN presentation and lossless canvas model mapping ============

import { I18n } from '/js/i18n.js';
import { getNodeDisplayTitle } from './node-i18n.js';

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

// The settings a block starts with: the keys the server lists as required,
// empty. Choosing a type replaces the settings, so keys of the previous type
// never linger.
export const processBlockTemplate = (block) => Object.fromEntries((block?.requiredSettings || []).map((key) => [key, '']));

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
  // Flows that share a name are told apart by their occurrence number, in the
  // palette and on the task dropped from it.
  const flows = options.serviceFlows || [];
  const flowNames = processDistinctLabels(flows.map((flow) => [flow.flowId, boundedNodeName(flow.name)]));
  return [
    ...flows.map((flow) =>
      entry('flows', 'flow', flowNames.get(flow.flowId), flow.description || I18n.t('bpmn.palette_flow_hint'),
        { flowId: flow.flowId })),
    ...(options.serviceBlocks || []).map((block) => entry('blocks', block.nodeType === 'llm' ? 'brain'
      : block.nodeType === 'pii_filter' ? 'shield' : 'sparkle',
    localized(`bpmn.block_${block.nodeType}`, block.label),
    localized(`bpmn.block_${block.nodeType}_hint`, block.description),
    { implementation: { Block: { nodeType: block.nodeType, config: processBlockTemplate(block) } } })),
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
  return implementation?.Block ? 'block' : implementation?.AddonTool ? 'tool' : implementation?.Agent ? 'agent' : 'flow';
}

// The settings the server lists as required that a block task has left empty.
// The list comes from the server (`requiredSettings`), so a block that needs
// more than `model` is covered without a client change.
export function processMissingSettings(node, blocks = []) {
  const block = node.type === 'bpmn_service_task' ? node.config?.implementation?.Block : null;
  const listed = block && blocks.find((candidate) => candidate.nodeType === block.nodeType);
  return (listed?.requiredSettings || []).filter((key) => {
    const value = block.config?.[key];
    return typeof value === 'string' ? value.trim() === '' : value == null;
  });
}

// Problems the canvas marks on a node itself. A block with an empty required
// setting, a boundary event attached to nothing and a published Call without
// an exact version are saved or refused only later (publication, or the wire
// codec), so the author sees them here first. `blocks` is the server's block list.
export function processNodeWarnings(node, blocks = []) {
  const warnings = [];
  if (processMissingSettings(node, blocks).length) warnings.push('empty_setting');
  if (processBoundaryKind(node.type) && !node.config?.attachedToId) warnings.push('boundary_unattached');
  if (node.type === 'bpmn_script_task' && node.repeat
    && (node.activityIo?.dataOutputs?.length || node.activityIo?.outputAssociations?.length)) warnings.push('repeated_script_io');
  if (node.type === 'bpmn_call_activity' && !node.config?.localBody
    && !(Number.isInteger(node.config?.calledVersion) && node.config.calledVersion >= 1)) {
    warnings.push('call_version');
  }
  return warnings;
}

export function processWarningText(node, key, blocks = []) {
  return I18n.t(`bpmn.warning_${key}`, { names: processMissingSettings(node, blocks).join(', ') });
}

// Name of an error or escalation handler added from the result tab: the code it
// catches, or "any" for the catch-all handler.
export function processHandlerName(type, code) {
  const kind = type === 'bpmn_boundary_error' ? 'error' : 'escalation';
  return code == null ? I18n.t(`bpmn.handler_name_${kind}_any`) : I18n.t(`bpmn.handler_name_${kind}`, { code });
}

const ESCALATION_WAITS = ['bpmn_user_task', 'bpmn_manual_task', 'bpmn_service_task', 'bpmn_timer_catch', 'bpmn_message_catch',
  'bpmn_receive_task', 'bpmn_signal_catch', 'bpmn_event_based_gateway'];
const ESCALATION_PASS_THROUGH = ['bpmn_message_throw', 'bpmn_send_task', 'bpmn_signal_throw', 'bpmn_script_task'];

/**
 * The first reason an escalation handler's immediate path is refused at
 * publication (processes/model.rs EscalationPrefixProof), for the shapes a
 * designer draws: straight lines and exclusive gateways. Parallel and inclusive
 * gateways are left to the server. Returns `{ reason, nodeId }` or null.
 */
export function processEscalationPathProblem(graph, boundaryId) {
  const byId = new Map(graph.nodes.map((node) => [node.id, node]));
  const outgoing = (id) => graph.edges.filter((edge) => edge.from_node === id).map((edge) => edge.to_node);
  const starts = outgoing(boundaryId);
  if (starts.length !== 1) return null;
  const seen = new Set();
  const visit = (id) => {
    if (seen.has(id)) return null;
    seen.add(id);
    const node = byId.get(id);
    if (!node) return { reason: 'no_durable_wait', nodeId: id };
    const fail = (reason) => ({ reason, nodeId: id });
    if (ESCALATION_WAITS.includes(node.type)) return null;
    if (ESCALATION_PASS_THROUGH.includes(node.type)) {
      const next = outgoing(id);
      return next.length === 1 ? visit(next[0]) : fail('no_durable_wait');
    }
    if (node.type === 'bpmn_exclusive_gateway') {
      const next = outgoing(id);
      if (!next.length) return fail('no_durable_wait');
      for (const target of next) {
        const problem = visit(target);
        if (problem) return problem;
      }
      return null;
    }
    if (node.type === 'bpmn_parallel_gateway' || node.type === 'bpmn_inclusive_gateway') return null;
    if (['bpmn_end', 'bpmn_error_end', 'bpmn_terminate_end'].includes(node.type)) return fail('terminal_before_wait');
    if (node.type === 'bpmn_sub_process') return fail('scope_entry');
    if (node.type === 'bpmn_call_activity') return fail('call_entry');
    return fail('no_durable_wait');
  };
  return visit(starts[0]);
}

// The error boundary events of the sub-processes around a task, innermost first,
// each with the code it catches (null for the catch-all). `path` lists the
// sub-process ids from the process body inward; a model that does not hold them
// yields nothing.
function enclosingErrorHandlers(model, selectedProcessId, path) {
  const root = selectedProcessId && selectedProcessId !== model.processId
    ? (model.additionalProcesses || []).find((candidate) => candidate.processId === selectedProcessId) : model;
  const bodies = [root ?? null];
  for (const id of path) {
    const holder = bodies.at(-1)?.nodes?.find((node) => node.id === id);
    bodies.push(holder?.kind?.SubProcess?.body ?? null);
  }
  const handlers = [];
  for (let depth = path.length; depth > 0; depth -= 1) {
    const enclosing = bodies[depth - 1];
    const scope = enclosing?.nodes?.find((node) => node.id === path[depth - 1]);
    if (!scope) continue;
    for (const node of enclosing.nodes.filter((candidate) => candidate.kind?.BoundaryError?.attachedToId === scope.id)) {
      const ref = node.kind.BoundaryError.errorRef;
      const declared = ref == null ? null : (model.errors || []).find((entry) => entry.errorId === ref);
      if (ref != null && !declared) continue;
      handlers.push({ depth, code: declared?.errorCode ?? null, catchAll: ref == null,
        name: node.name || I18n.t('bpmn.node_boundary_error'), scope: scope.name || I18n.t('bpmn.node_sub_process') });
    }
  }
  return handlers;
}

// What takes `code` from the handlers above: per scope the exact code before the
// catch-all, the innermost scope first. A null code is "any other error", which
// only a catch-all takes.
function climbingHandler(handlers, code) {
  for (const depth of [...new Set(handlers.map((handler) => handler.depth))]) {
    const here = handlers.filter((handler) => handler.depth === depth);
    const taken = (code != null && here.find((handler) => !handler.catchAll && handler.code === code))
      || here.find((handler) => handler.catchAll);
    if (taken) return taken;
  }
  return null;
}

const ESCALATION_PATH_REASONS = ['terminal_before_wait', 'scope_entry', 'call_entry', 'no_durable_wait'];

// What an author calls each element of a model: its name, else its kind. The
// elements of every body and nested sub-process, keyed by id.
export function processElementNames(model) {
  const names = new Map();
  const visit = (body) => {
    for (const node of body?.nodes || []) {
      const kind = typeof node.kind === 'string' ? node.kind : Object.keys(node.kind || {})[0];
      const element = ELEMENTS.find(([candidate]) => candidate === kind);
      names.set(node.id, String(node.name || '').trim() || (element ? I18n.t(`bpmn.node_${element[1]}`) : node.id));
      if (node.kind?.SubProcess) visit(node.kind.SubProcess.body);
    }
  };
  [model, ...(model?.additionalProcesses || [])].forEach(visit);
  return names;
}

// The first thing in each body that needs a process time zone and has none: a
// timer element (named), else the work calendar (null). The engine refuses a
// timer or a calendar without an explicit zone and never guesses one, because
// the zone decides when a deadline falls.
export function processTimezoneGaps(model) {
  const names = processElementNames(model);
  const firstTimer = (body) => {
    for (const node of body.nodes) {
      const kind = typeof node.kind === 'object' ? Object.keys(node.kind)[0] : null;
      if (['TimerStart', 'TimerCatch', 'BoundaryTimer'].includes(kind)) return node.id;
      if (kind === 'SubProcess') {
        const nested = firstTimer(node.kind.SubProcess.body);
        if (nested) return nested;
      }
    }
    return null;
  };
  const gaps = [];
  for (const body of [model, ...(model.additionalProcesses || [])]) {
    if (body.timerTimezone?.trim()) continue;
    const timer = firstTimer(body);
    if (timer) gaps.push({ processId: body.processId, element: names.get(timer) });
    else if (body.workCalendar) gaps.push({ processId: body.processId, element: null });
  }
  return gaps;
}

// tzdb names the engine accepts that are not spelt with a capital per part and
// that the runtime's own list may omit ("UTC" is missing from older ICU lists).
// Typed in another letter case they are repaired to this spelling.
const SHOUTED_TIMEZONES = ['CET', 'CST6CDT', 'EET', 'EST', 'EST5EDT', 'GB', 'GB-Eire', 'GMT', 'GMT+0', 'GMT-0', 'GMT0', 'HST', 'MET', 'MST', 'MST7MDT', 'NZ', 'NZ-CHAT', 'PRC', 'PST8PDT', 'ROC', 'ROK', 'UCT', 'UTC', 'W-SU', 'WET'];

// Why a typed time zone cannot be used, or null. The engine compares names
// exactly and spelt in full, but the browser resolves them in any letter case,
// so a name is accepted when the runtime lists it (in any case: the canonical
// spelling repairs that), when it is one of the tzdb names written in capitals
// ("utc" becomes "UTC") or when it is a link such as "US/Eastern" that the
// runtime still resolves AND is written the way tzdb writes every name, each
// part starting with a capital. A link cannot be matched against a list, so a
// capital in the wrong place inside a part is left to the engine to refuse.
export function processTimezoneProblem(value) {
  const zone = String(value ?? '').trim();
  if (!zone) return null;
  if (!/^[A-Za-z][A-Za-z0-9_+-]*(\/[A-Za-z0-9_+-]+)*$/.test(zone)) return 'invalid';
  if (typeof Intl === 'undefined' || typeof Intl.DateTimeFormat !== 'function') return null;
  if (listedTimezone(zone)) return null;
  if (!zone.split('/').every((part) => /^[A-Z]/.test(part))) return 'invalid';
  try { new Intl.DateTimeFormat('en', { timeZone: zone }); return null; } catch (_) { return 'invalid'; }
}

function listedTimezone(zone) {
  const lower = zone.toLowerCase();
  const shouted = SHOUTED_TIMEZONES.find((name) => name.toLowerCase() === lower);
  if (shouted) return shouted;
  if (typeof Intl.supportedValuesOf !== 'function') return null;
  return Intl.supportedValuesOf('timeZone').find((name) => name.toLowerCase() === lower) ?? null;
}

// The spelling the engine lists for a zone: only the letter case is repaired
// ("europe/warsaw" becomes "Europe/Warsaw"). A valid name is otherwise kept as
// typed, because the runtime's own spelling differs from the engine's for links
// ("Europe/Kyiv" resolves to "Europe/Kiev", "EST" to "America/Panama"). '' for an
// empty field and null for a name that cannot be used. The server compares names
// exactly.
export function processTimezoneCanonical(value) {
  const zone = String(value ?? '').trim();
  if (processTimezoneProblem(zone)) return null;
  if (!zone || typeof Intl === 'undefined') return zone;
  return listedTimezone(zone) ?? zone;
}

export function processTimezoneText(gap) {
  if (!gap) return I18n.t('bpmn.timer_timezone_server');
  return gap.element == null ? I18n.t('bpmn.timer_timezone_calendar_missing')
    : I18n.t('bpmn.timer_timezone_missing', { element: gap.element });
}

const DECLARATIONS = [['message', 'messages', 'messageId', 'messageRef'], ['error', 'errors', 'errorId', 'errorRef'],
  ['escalation', 'escalations', 'escalationId', 'escalationRef'], ['signal', 'signals', 'signalId', 'signalRef']];

// The declared messages, errors, escalations and signals that no element
// references. Publishing refuses them; the author is told which, by name.
export function processUnusedDeclarations(model) {
  const used = new Set();
  const visit = (body) => {
    for (const node of body?.nodes || []) {
      if (typeof node.kind !== 'object') continue;
      const payload = Object.values(node.kind)[0];
      for (const [, , , ref] of DECLARATIONS) if (payload?.[ref]) used.add(payload[ref]);
      if (node.kind.SubProcess) visit(node.kind.SubProcess.body);
    }
  };
  [model, ...(model?.additionalProcesses || [])].forEach(visit);
  return DECLARATIONS.flatMap(([kind, list, idKey]) => (model?.[list] || [])
    .filter((declaration) => !used.has(declaration[idKey]))
    .map((declaration) => ({ kind, name: String(declaration.name || declaration.errorCode || declaration.escalationCode || '').trim() })));
}

// The names of the elements that repeat, in every body and nested sub-process.
export function processRepeatedElements(model) {
  const names = processElementNames(model);
  const repeated = [];
  const visit = (body) => {
    for (const node of body?.nodes || []) {
      if (node.repeat) repeated.push(names.get(node.id));
      if (node.kind?.SubProcess) visit(node.kind.SubProcess.body);
    }
  };
  [model, ...(model?.additionalProcesses || [])].forEach(visit);
  return repeated;
}

// The repeated elements that have no output collection variable. Saving a draft
// accepts that; publishing does not.
export function processRepeatOutputGaps(model) {
  const names = processElementNames(model);
  const gaps = [];
  const visit = (body) => {
    for (const node of body?.nodes || []) {
      const repeat = node.repeat;
      const output = repeat?.MultiInstance?.outputCollectionVariable ?? repeat?.StructuredLoop?.outputCollectionVariable;
      if (repeat && !/^[A-Za-z][A-Za-z0-9_.-]{0,127}$/.test(output ?? '')) gaps.push({ nodeId: node.id, name: names.get(node.id) });
      if (node.kind?.SubProcess) visit(node.kind.SubProcess.body);
    }
  };
  [model, ...(model?.additionalProcesses || [])].forEach(visit);
  return gaps;
}

// What would make the server refuse a publication, told by name before it is
// asked: the texts are the ones the author reads in the banner.
export function processPublishProblems(model) {
  return [
    ...processUnusedDeclarations(model).map(({ kind, name }) => I18n.t(`bpmn.unused_${kind}`, { name })),
    ...processRepeatOutputGaps(model).map(({ name }) => I18n.t('bpmn.repeat_output_missing', { node: name })),
  ];
}

// Names that tell two entries apart without showing an internal id: a name two
// entries share gets its occurrence number appended, one that is unique stays
// as written. `entries` is [[key, name], ...]; the result maps key to label.
export function processDistinctLabels(entries) {
  const seen = new Map();
  const total = new Map();
  for (const [, name] of entries) total.set(name, (total.get(name) || 0) + 1);
  return new Map(entries.map(([key, name]) => {
    const occurrence = (seen.get(name) || 0) + 1;
    seen.set(name, occurrence);
    return [key, total.get(name) > 1 ? `${name} (${occurrence})` : name];
  }));
}

// The name an author gives each body of a document: its name, else "Process N"
// by position.
export function processBodyLabels(model) {
  return processDistinctLabels([model, ...(model.additionalProcesses || [])].map((body, index) =>
    [body.processId, String(body.processName || '').trim() || I18n.t('bpmn.process_body_unnamed', { number: index + 1 })]));
}

// The title of each canvas node as the author reads it on the canvas; two nodes
// that share a title get their occurrence number, so a list never offers two
// identical lines. `templates` is the canvas' type -> template map.
export function processNodeLabels(nodes, templates = null) {
  return processDistinctLabels(nodes.map((node) => [node.id, getNodeDisplayTitle(node, templates?.get?.(node.type))]));
}

// The entries of a start catalog by the body and the start element an author
// sees, keyed by their index in the catalog. A body without a name is "Process
// N" by position, never the id the server falls back to.
export function processStartEntryLabels(catalog, model) {
  const bodies = processBodyLabels(model);
  const kindOf = (entry) => entry.trigger?.TimerStart ? 'timer_start' : entry.trigger?.MessageStart ? 'message_start' : 'start';
  return processDistinctLabels(catalog.map((entry, index) => [index,
    `${bodies.get(entry.processId) || I18n.t('bpmn.crumb_root')} \u00b7 ${String(entry.startNodeName || '').trim() || I18n.t(`bpmn.node_${kindOf(entry)}`)}`]));
}

// Data inputs, outputs and object references have a name or are told apart by
// their position ("Item 2"); the id they carry stays out of the lists.
export function processItemLabels(items) {
  return processDistinctLabels(items.map((item, index) =>
    [item.id, String(item.name ?? '').trim() || I18n.t('bpmn.activity_io_unnamed', { number: index + 1 })]));
}

const ID_SLOT = /\b(?:task|node|event|gateway|activity|element|boundary|repeat|call|catch|throw|flow|subprocess|id|origin|reaches)[:]?\s+$|["'`\u201c\u2018]$/i;

// The refusals that need no element name, told in plain words; null for any
// other message. The engine's own wording stays for what is not known here.
export function processPlainError(message) {
  const text = String(message ?? '');
  if (/process draft revision conflict or archived definition/.test(text)) return I18n.t('bpmn.error_draft_conflict');
  if (/publication needs a Current verified calendar pin/.test(text)) return I18n.t('bpmn.error_calendar_pin');
  return null;
}

/**
 * Server and codec messages cite elements by internal id; the author knows them
 * by name. An id of `model` found in `message` becomes the element's name (or
 * its kind when it has none), a flow becomes "from → to", and the one refusal a
 * drawn graph reaches often, an escalation path with nothing waiting, is
 * replaced by the text the inspector shows for it. An id is recognised by its
 * shape (generated and imported ids carry a digit, underscore or hyphen) or by
 * the noun the message puts in front of it, so an imported element called
 * "Start" never rewrites the ordinary word.
 */
export function processExplainError(message, model) {
  const names = processElementNames(model);
  const bodies = [model, ...(model?.additionalProcesses || [])];
  const flows = new Map();
  const collect = (body) => {
    for (const flow of body?.sequenceFlows || []) flows.set(flow.id, flow);
    for (const node of body?.nodes || []) if (node.kind?.SubProcess) collect(node.kind.SubProcess.body);
  };
  bodies.forEach(collect);
  const text = String(message ?? '').replace(/^protocol error \w+:\s*/i, '');
  const plain = processPlainError(text);
  if (plain) return plain;
  if (/explicit IANA timezone/.test(text)) return processTimezoneText(processTimezoneGaps(model)[0]);
  if (/timerless process cannot declare a timer timezone/.test(text)) return I18n.t('bpmn.timer_timezone_unused');
  if (/unknown timezone/i.test(text)) {
    const zone = [model, ...(model?.additionalProcesses || [])].map((body) => body?.timerTimezone).find(Boolean);
    return zone ? I18n.t('bpmn.timer_timezone_invalid', { zone }) : I18n.t('bpmn.timer_timezone_server');
  }
  const unused = /unreferenced (message|error|escalation|signal) declaration/.exec(text);
  if (unused) {
    const named = processUnusedDeclarations(model).filter((declaration) => declaration.kind === unused[1]);
    return named.length ? named.map(({ kind, name }) => I18n.t(`bpmn.unused_${kind}`, { name })).join(' ')
      : I18n.t(`bpmn.unused_${unused[1]}_any`);
  }
  const unsimulated = /repetition is unsupported in simulation \(([\w.-]+)\)/.exec(text);
  if (unsimulated && names.has(unsimulated[1])) return I18n.t('bpmn.simulation_repeat_unsupported', { names: names.get(unsimulated[1]) });
  const repeatedBoundary = /repeat ([\w.-]+) has an unsupported boundary event/.exec(text);
  if (repeatedBoundary && names.has(repeatedBoundary[1])) return I18n.t('bpmn.repeat_boundary_unsupported', { node: names.get(repeatedBoundary[1]) });
  const repeated = /repeat ([\w.-]+) has invalid output collection variable/.exec(text);
  if (repeated && names.has(repeated[1])) return I18n.t('bpmn.repeat_output_missing', { node: names.get(repeated[1]) });
  const path = /escalation boundary ([\w-]+) has unsupported immediate path (\w+) at/.exec(text);
  if (path && names.has(path[1])) {
    const key = ESCALATION_PATH_REASONS.includes(path[2]) ? path[2] : 'other';
    return I18n.t(`bpmn.routing_warning_escalation_path_${key}`, { names: names.get(path[1]) });
  }
  const citesId = (token, offset) => /[\d_-]/.test(token) || ID_SLOT.test(text.slice(0, offset));
  return text.replace(/[\w-]+/g, (token, offset) => {
    if (!citesId(token, offset)) return token;
    if (names.has(token)) return `\u201c${names.get(token)}\u201d`;
    const flow = flows.get(token);
    return flow ? I18n.t('bpmn.error_flow', { from: names.get(flow.sourceId) ?? flow.sourceId, to: names.get(flow.targetId) ?? flow.targetId }) : token;
  });
}

// Which i18n texts a row of the routing table shows; the parameters they need
// (targets, handler, code, timer) come from the row itself.
export function processRoutingTextKeys(row, verification) {
  const outcome = row.outcome === 'timeout' ? 'bpmn.routing_outcome_timeout'
    : row.outcome === 'rejected' ? `bpmn.routing_outcome_rejected_${verification === 'Human' ? 'human' : 'condition'}`
      : (row.outcome === 'error' || row.outcome === 'needs_human') && row.route !== 'unavailable'
        ? `bpmn.routing_outcome_${row.outcome}_${row.other ? 'other' : 'code'}`
        : `bpmn.routing_outcome_${row.outcome}`;
  let route = `bpmn.routing_${row.route}`;
  if (row.route === 'next') route = `bpmn.routing_next_${row.gate}`;
  else if (row.route === 'boundary' && row.outcome === 'timer') route = row.boundary.interrupting ? 'bpmn.routing_timer_interrupting' : 'bpmn.routing_timer_parallel';
  else if (row.route === 'incident') {
    route = row.outcome === 'error' && row.other ? 'bpmn.routing_incident_repeated'
      : row.outcome === 'error' || row.outcome === 'cancelled' ? 'bpmn.routing_incident_result'
        : row.outcome === 'platform' ? 'bpmn.routing_incident_platform'
          : row.outcome === 'timeout' ? 'bpmn.routing_incident_timeout' : 'bpmn.routing_incident';
  } else if (row.route === 'review' && row.outcome === 'denied') route = 'bpmn.routing_review_denied';
  return { outcome, route };
}

// The row a stimulus lands on: the handler with the exact code first, then the
// catch-all one. Outcomes with a single row ignore the code.
export function processRowFor(routing, outcome, code = null) {
  const rows = routing.rows.filter((row) => row.outcome === outcome);
  return rows.find((row) => row.route !== 'unavailable' && !row.other && row.code === code)
    ?? rows.find((row) => row.other || row.route === 'unavailable') ?? rows[0];
}

/**
 * What happens to a Service task's outcomes, derived from the graph alone.
 * There is no server-side rule table: the rules below mirror the engine in
 * processes/runtime.rs (plan_job_result, error_handler) and processes/jobs.rs
 * (classify_result, fail_claim, report_denied_tool). The shared fixture
 * tests/fixtures/process_result_routing.json pins every row against both sides:
 * - only a Contract result (a non-empty `resultExpression`) can be Error,
 *   NeedsHuman or Cancelled; a platform failure (flow, tool or expression
 *   error) is always an incident and no boundary event catches it. The one
 *   platform result that is NOT an incident is a denied addon tool permission:
 *   it opens a verification task for the initiator, whatever `resultExpression` says;
 * - Error goes to the boundary with the exact code, else to the catch-all
 *   boundary, else (inside a sub-process) up the enclosing scopes, else an
 *   incident, unless the process runs as a called process whose Call activity
 *   has an error boundary. A repeated task never climbs: it raises the incident
 *   itself, and so does a Cancelled result or a rejecting condition;
 * - NeedsHuman goes to the escalation boundary with the exact code, else the
 *   catch-all one, else the initiator's verification task. A non-interrupting
 *   escalation keeps the task, which still opens its verification task;
 * - a rejected verification is an incident (HUMAN_REJECTED, VERIFICATION_FAILED);
 *   Cancelled is an incident; a service timeout after dispatch records the
 *   uncertain-outcome incident EXTERNAL_OUTCOME_UNCERTAIN (reason SERVICE_TIMEOUT)
 *   and the job stays running, so an interrupting boundary timer still ends the
 *   task and resolves that incident;
 * - a boundary timer runs on its own clock from activation.
 */
export function processResultRouting(graph, nodeId) {
  const node = graph.nodes.find((candidate) => candidate.id === nodeId);
  if (!node || node.type !== 'bpmn_service_task') return null;
  const model = graph.processModel || {};
  const config = node.config || {};
  const hasResult = typeof config.resultExpression === 'string' && config.resultExpression.length > 0;
  const nested = (graph.processPath || []).length > 0;
  const repeated = node.repeat != null;
  const serviceKind = processServiceKind(config);
  const byId = new Map(graph.nodes.map((candidate) => [candidate.id, candidate]));
  const targetsOf = (id) => graph.edges.filter((edge) => edge.from_node === id)
    .map((edge) => byId.get(edge.to_node)).filter(Boolean)
    .map((target) => ({ id: target.id, label: target.label || '', type: target.type }));
  const describe = (boundary, code, undeclared) => ({
    id: boundary.id, label: boundary.label || '', type: boundary.type, code, catchAll: code === null && !undeclared,
    undeclared, interrupting: boundary.config.cancelActivity !== false, targets: targetsOf(boundary.id),
    name: boundary.label || (boundary.type === 'bpmn_boundary_timer' || undeclared
      ? I18n.t(`bpmn.node_${boundary.type.slice(5)}`) : processHandlerName(boundary.type, code)),
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
  const enclosing = nested && !repeated ? enclosingErrorHandlers(model, graph.selectedProcessId, graph.processPath) : [];
  const rows = [];
  rows.push({ outcome: 'completed', route: 'next', gate: human ? 'human' : 'condition', targets: targetsOf(nodeId) });
  rows.push({ outcome: 'rejected', route: 'incident', code: human ? 'HUMAN_REJECTED' : 'VERIFICATION_FAILED' });
  const escalationRoute = (entry) => (entry.interrupting ? 'boundary' : 'boundary_keep');
  if (hasResult) {
    for (const entry of specific(errors)) rows.push({ outcome: 'error', route: 'boundary', code: entry.code, boundary: entry, targets: entry.targets });
    const otherError = catchAll(errors);
    const taskCodes = new Set(specific(errors).map((entry) => entry.code));
    for (const code of otherError ? [] : [...new Set(enclosing.filter((handler) => !handler.catchAll).map((handler) => handler.code))]) {
      const taken = climbingHandler(enclosing, code);
      if (taskCodes.has(code) || !taken || taken.catchAll) continue;
      rows.push({ outcome: 'error', route: 'parent', code, boundary: null, parent: taken, targets: [] });
    }
    // An error no handler on the task takes climbs scope by scope; with no
    // handler in any enclosing scope it ends up where a top-level task's does.
    const climbed = otherError ? null : climbingHandler(enclosing, null);
    rows.push({ outcome: 'error', route: otherError ? 'boundary' : repeated ? 'incident' : climbed ? 'parent' : 'caller', other: true,
      code: null, boundary: otherError, parent: climbed, targets: otherError ? otherError.targets : [] });
    for (const entry of specific(escalations)) rows.push({ outcome: 'needs_human', route: escalationRoute(entry), code: entry.code, boundary: entry, targets: entry.targets });
    const otherEscalation = catchAll(escalations);
    rows.push({ outcome: 'needs_human', route: otherEscalation ? escalationRoute(otherEscalation) : 'review', other: true,
      code: null, boundary: otherEscalation, targets: otherEscalation ? otherEscalation.targets : [] });
    rows.push({ outcome: 'cancelled', route: 'incident' });
  } else {
    for (const outcome of ['error', 'needs_human', 'cancelled']) rows.push({ outcome, route: 'unavailable' });
  }
  if (serviceKind === 'tool') rows.push({ outcome: 'denied', route: 'review', code: 'TOOL_PERMISSION_DENIED' });
  rows.push({ outcome: 'platform', route: 'incident' });
  rows.push({ outcome: 'timeout', route: 'incident', code: 'EXTERNAL_OUTCOME_UNCERTAIN', reason: 'SERVICE_TIMEOUT', seconds: config.timeoutSeconds });
  for (const entry of timers) rows.push({ outcome: 'timer', route: 'boundary', boundary: entry, targets: entry.targets, timer: entry.timer });

  // One entry per kind of problem, listing every element it concerns, so five
  // new handlers do not produce five identical messages.
  const warnings = [];
  const warn = (key, name, tone = 'warning') => {
    let entry = warnings.find((candidate) => candidate.key === key);
    if (!entry) warnings.push(entry = { key, tone, names: [] });
    if (name !== undefined && !entry.names.includes(name)) entry.names.push(name);
  };
  if (!human && !(config.verification?.Condition?.expression || '').trim()) warn('empty_condition');
  if (!rows[0].targets.length) warn('no_outgoing');
  if (!hasResult && errors.length) warn('error_boundary_without_result');
  if (!hasResult && escalations.length) warn('escalation_boundary_without_result');
  for (const entry of [...errors, ...escalations, ...timers]) {
    if (!entry.targets.length) warn('boundary_no_flow', entry.name);
    if (entry.undeclared) warn('undeclared_reference', entry.name);
  }
  // The server refuses to publish an escalation handler whose path reaches an
  // End, a sub-process or a Call activity before anything waits for a person.
  for (const entry of escalations) {
    const problem = processEscalationPathProblem(graph, entry.id);
    if (problem) warn(`escalation_path_${problem.reason}`, entry.name);
  }
  for (const list of [errors, escalations]) {
    const seen = new Set();
    for (const entry of list) {
      const key = entry.undeclared ? `u:${entry.id}` : entry.code ?? '';
      if (seen.has(key)) warn('duplicate_handler', entry.name);
      seen.add(key);
    }
  }
  // An interrupting timer neither replaces nor races the timeout incident: the
  // job stays running behind it, so the timer fires on an incident instance and
  // resolves the uncertain outcome. A shorter timer simply ends the task first.
  for (const entry of timers.filter((candidate) => candidate.interrupting)) {
    const seconds = entry.timer?.Duration?.seconds;
    const shorter = Number.isInteger(seconds) && Number.isInteger(config.timeoutSeconds) && seconds < config.timeoutSeconds;
    warn(shorter ? 'timer_preempts_timeout' : 'timer_resolves_timeout', entry.name, 'info');
  }

  // The first catch-all, then each declared code, that has no handler yet: a
  // second handler for one code is refused at publication.
  const nextRef = (list, declarations, idKey, codeKey) => {
    const used = new Set(list.map((entry) => (entry.catchAll ? null : entry.undeclared ? undefined : entry.code)));
    if (!used.has(null)) return { ref: null };
    const free = (declarations || []).find((entry) => !used.has(entry[codeKey]));
    return free ? { ref: free[idKey] } : null;
  };
  // Both buttons need a result rule: only a rule can return an error or ask a
  // person. The task also has a finite number of places on its border.
  const room = typeof graph.boundaryRoom === 'function' ? graph.boundaryRoom(nodeId) : Infinity;
  const addError = hasResult ? nextRef(errors, model.errors, 'errorId', 'errorCode') : null;
  const addEscalation = hasResult ? nextRef(escalations, model.escalations, 'escalationId', 'escalationCode') : null;
  const reason = (ref) => (!hasResult ? 'no_result' : !ref ? 'all_taken' : room < 1 ? 'no_room' : null);
  const errorReason = reason(addError);
  const escalationReason = reason(addEscalation);
  return {
    node, hasResult, kind: serviceKind, verification: human ? 'Human' : 'Condition', rows, warnings,
    boundaries: { errors, escalations, timers },
    add: { error: errorReason ? null : addError, escalation: escalationReason ? null : addEscalation, errorReason, escalationReason },
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

// The activities a boundary event of `kind` (BoundaryTimer, BoundaryMessage,
// BoundaryError, BoundaryEscalation) may be attached to in the shown scope. The
// inspector's list and a drop from the palette share it. Repetition adds no
// refusal of its own: on a repeating activity the engine takes timers and
// messages on any kind, an error only on a Service task, subprocess or Call
// activity and an escalation only on a Service task, which is exactly what the
// kind rules below already allow (processes/model.rs).
export function processBoundaryHosts(graph, kind) {
  const timerOrMessage = ['BoundaryTimer', 'BoundaryMessage'].includes(kind);
  return graph.nodes.filter((candidate) => (['bpmn_user_task', 'bpmn_service_task', 'bpmn_sub_process', 'bpmn_call_activity'].includes(candidate.type)
    || (timerOrMessage
      && ['bpmn_send_task', 'bpmn_manual_task', 'bpmn_receive_task', 'bpmn_script_task'].includes(candidate.type)))
    && (kind !== 'BoundaryError' || candidate.type !== 'bpmn_user_task')
    && (kind !== 'BoundaryEscalation' || (candidate.type === 'bpmn_service_task' && !!candidate.config.resultExpression))
    && !(candidate.type === 'bpmn_receive_task' && graph.edges.some((edge) =>
      edge.to_node === candidate.id && graph.nodes.some((source) =>
        source.id === edge.from_node && source.type === 'bpmn_event_based_gateway'))));
}

const BOUNDARY_CAPABLE = ['bpmn_user_task', 'bpmn_service_task', 'bpmn_sub_process', 'bpmn_call_activity',
  'bpmn_send_task', 'bpmn_manual_task', 'bpmn_receive_task', 'bpmn_script_task'];

// Every activity that could carry some boundary event in the shown scope, with
// the reason a boundary event of `kind` is refused on it: 'error_type' /
// 'escalation_type' when the kind of activity cannot take that event (whether or
// not it repeats), 'other' for any remaining refusal, null when it is offered.
export function processBoundaryRefusals(graph, kind) {
  const offered = new Set(processBoundaryHosts(graph, kind).map((candidate) => candidate.id));
  return new Map(graph.nodes.filter((node) => BOUNDARY_CAPABLE.includes(node.type)).map((candidate) => [candidate.id,
    offered.has(candidate.id) ? null
      : kind === 'BoundaryError' ? 'error_type' : kind === 'BoundaryEscalation' ? 'escalation_type' : 'other']));
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
