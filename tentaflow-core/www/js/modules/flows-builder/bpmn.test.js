// ============ File: flows-builder/bpmn.test.js — real BPMN editor and participant window behavior ============

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import test, { after, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

for (const name of ['MutationObserver', 'ResizeObserver', 'Document', 'CSS', 'navigator']) {
  if (globalThis[name] === undefined) globalThis[name] = window[name];
}
Object.defineProperty(globalThis, 'localStorage', { value: window.localStorage, configurable: true });
window.HTMLCanvasElement.prototype.getContext = () => ({ measureText: (value) => ({ width: String(value).length * 7 }) });
const hostFetch = globalThis.fetch;
globalThis.fetch = (url, options) => {
  const path = String(url);
  if (/^\/(i18n|css)\//.test(path)) return Promise.resolve(new Response(readFileSync(new URL(`../../..${path}`, import.meta.url)), { headers: { 'Content-Type': path.endsWith('.json') ? 'application/json' : 'text/css' } }));
  return hostFetch(url, options);
};
const { I18n } = await import('../../i18n.js');
const { ApiBinary } = await import('../../protocol/api-binary-shim.js');
const { codecReady, encode } = await import('../../protocol/codec.js');
const wasm = await codecReady;
const { Router } = await import('../../router.js');
const { dismissToast } = await import('../../utils.js');
const { TfWindow } = await import('../../components/tf-window.js');
const { emptyProcessModel, processToCanvas, canvasToProcess, cloneProcessBody, processBoundaryKind, processCommand, processJson, processStatusLabel, checkProcessDocument, PROCESS_DOCUMENT_BYTES } = await import('./bpmn.js');
const { FlowCanvas, UNATTACHED_TAG } = await import('./canvas.js');
const { FlowConfig } = await import('./config.js');
const { FlowPalette } = await import('./palette.js');
const { processTemplates, boundedNodeName, isBlankLabel, processResultRouting, processRowFor, processRoutingTextKeys, processEscalationPathProblem, processExplainError, processMissingSettings, processNodeWarnings, processServiceKind, processBlockTemplate, processStartEntryLabels, processTimezoneProblem, processPublishProblems, processNodeLabels, processTimezoneGaps, processTimezoneText, processTimezoneCanonical, processRepeatedElements, processBoundaryRefusals, processBoundaryHosts, processRepeatOutputGaps, processPlainError } = await import('./bpmn.js');
const { openProcessCalendar, openProcessInstance, openProcessInstances, openProcessMessageDetail, openProcessMessageSend, openProcessRun, openProcessSchedule, processEventText, processLifecycleReasonText, processTimerText } = await import('./process-monitor.js');
const { simulationWindow, openProcessSimulation } = await import('./process-simulation.js');
const { default: builder } = await import('../flows-builder.js');
const { default: flows } = await import('../flows.js');
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();

const calls = [];
const navigation = [];
let responder;
ApiBinary.one = async (kind, payload) => {
  calls.push({ kind, payload: structuredClone(payload) });
  if (kind === 'mePreferencesUpdateRequest') return {};
  return responder(kind, payload);
};
ApiBinary.action = ApiBinary.one;
ApiBinary.list = async (kind, options) => {
  const response = await ApiBinary.one(kind, options);
  return options?.arrayKey ? response[options.arrayKey] : response;
};
Router.navigate = async (view, params) => { navigation.push({ view, params }); return true; };
const confirm = TfWindow.confirm;
TfWindow.confirm = async () => true;
const intervals = new Map();
const setIntervalOriginal = globalThis.setInterval;
const clearIntervalOriginal = globalThis.clearInterval;
globalThis.setInterval = (callback, delay, ...args) => {
  if (delay === 3000 || delay === 10000) { const token = {}; intervals.set(token, { callback, delay }); return token; }
  return setIntervalOriginal(callback, delay, ...args);
};
globalThis.clearInterval = (token) => { if (!intervals.delete(token)) clearIntervalOriginal(token); };
const flush = async (count = 5) => { for (let i = 0; i < count; i += 1) await new Promise((resolve) => setTimeout(resolve, 0)); };
const click = (element) => { assert.ok(element, 'the actual control exists'); element.dispatchEvent(new MouseEvent('click', { bubbles: true })); };
const change = (control, value) => { control.value = value; control.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value } })); };
// Toasts live in a container the page replaces on every mount, so the ones made
// are collected as they are created.
function watchToasts() {
  const created = []; const createElement = document.createElement;
  document.createElement = (...args) => { const node = createElement.apply(document, args); created.push(node); return node; };
  return { texts: () => created.filter((node) => String(node.className).startsWith('toast ')).map((node) => node.textContent),
    stop: () => { document.createElement = createElement; } };
}
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const options = { assignees: [{ userId: 'anna', displayName: 'Anna Kowalska' }, { userId: 'lee', displayName: 'Lee Chen' }], serviceFlows: [{ flowId: 'flow-one', name: 'Contract checker' }] };
// The server's allow-list; the inspector offers exactly these block types.
const serviceBlocks = [
  { nodeType: 'llm', label: 'Language model (LLM)', description: 'Generates text', category: 'service', requiredSettings: ['model'] },
  { nodeType: 'embeddings', label: 'Embeddings', description: 'Embeds text', category: 'service', requiredSettings: ['model'] },
  { nodeType: 'pii_filter', label: 'Personal data filter', description: 'Masks personal data', category: 'filter', requiredSettings: [] },
];
function definition(id = 'definition-one', overrides = {}) {
  return { definitionId: id, name: `Process ${id}`, description: '', ownerUserId: 'owner', draftRevision: 4, model: emptyProcessModel(), publishedVersion: null, archived: false, ...overrides };
}
function instance(id = 'instance-one', overrides = {}) {
  const value = { instanceId: id, definitionId: 'definition-one', definitionName: 'Document approval', initiatorUserId: 'owner', version: 2, revision: 11,
    status: 'Waiting', variables: { Purchase_ID: 'PO-7' }, activeNodeIds: ['Review'], userTasks: [], incidents: [], timers: [],
    subscriptions: [], eventRaces: [], outgoingMessages: [], messageNames: [], scopes: [], calls: [],
    repetitionGroups: [], repetitionOccurrences: [], selectedRepetitionOccurrence: null, canSendMessage: false,
    createdAtMs: 1000, updatedAtMs: 2000, canCancel: false, canRetry: false, ...overrides };
  value.pages ??= Object.fromEntries(['userTasks', 'incidents', 'timers', 'subscriptions', 'eventRaces', 'outgoingMessages', 'scopes', 'calls', 'repetitionGroups', 'repetitionOccurrences'].map((name) =>
    [name, { offset: 0, total: value[name].length, nextOffset: null, hasMore: false }]));
  return value;
}
function fixtures(values) {
  responder = (kind, payload) => {
    if (!(kind in values)) throw new Error(`Unexpected request ${kind}`);
    return typeof values[kind] === 'function' ? values[kind](payload) : structuredClone(values[kind]);
  };
}
async function mount(current, additions = {}) {
  await builder.unmount();
  fixtures({ processDefinitionGetRequest: { definition: current, startCatalog: [] }, processOptionsRequest: options, ...additions });
  document.body.innerHTML = `<main>${builder.render({ mode: 'bpmn' })}</main>`;
  await builder.mount({ flowId: current.definitionId, mode: 'bpmn' });
  return builder._state;
}
function canvas(model = emptyProcessModel()) {
  const root = document.createElement('div'); document.body.append(root);
  const graph = new FlowCanvas(root, { mode: 'bpmn', processOptions: { ...options, serviceBlocks } });
  graph.setTemplates(processTemplates()); graph.setData(model);
  return graph;
}
function inspector(graph, readOnly = false) {
  const root = document.createElement('aside'); document.body.append(root);
  return new FlowConfig(root, { mode: 'bpmn', readOnly, processOptions: { ...options, serviceBlocks }, getCanvas: () => graph,
    onConfigChange: (id, value) => graph.updateNodeConfig(id, value), onEdgeChange: (id, value) => graph.updateEdge(id, value),
    onLabelChange: (id, value) => graph.updateNodeLabel(id, value), onDelete: (id) => graph.removeNodes([id]), onDuplicate: (id) => graph.duplicateNodes([id]) });
}
function userNode(graph) { graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_user_task'), 250, 300); return graph.nodes.at(-1); }
function boundaryModel() {
  const model = emptyProcessModel();
  model.timerTimezone = 'Europe/Warsaw';
  model.variables = { business_key: { inner_value: 'R&D Łódź' } };
  model.nodes.splice(1, 0,
    { id: 'Review', name: 'Review contract', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } },
    { id: 'Timer_A', name: 'Deadline', kind: { BoundaryTimer: { attachedToId: 'Review', cancelActivity: true, timer: { Duration: { seconds: 90 } } } } },
    { id: 'Timer_B', name: 'Reminder', kind: { BoundaryTimer: { attachedToId: 'Review', cancelActivity: false, timer: { Date: { at: '2027-01-02T03:04:05+01:00' } } } } });
  model.sequenceFlows = [
    { id: 'Flow_1', sourceId: 'Start', targetId: 'Review', condition: null },
    { id: 'Flow_2', sourceId: 'Review', targetId: 'End', condition: null },
    { id: 'Flow_3', sourceId: 'Timer_A', targetId: 'End', condition: null },
    { id: 'Flow_4', sourceId: 'Timer_B', targetId: 'End', condition: null },
  ];
  model.diagram.shapes = [
    { elementId: 'Start', x: 80, y: 160, width: 56, height: 56 },
    { elementId: 'Review', x: 200, y: 160, width: 240, height: 96 },
    { elementId: 'Timer_A', x: 412, y: 228, width: 56, height: 56 },
    { elementId: 'Timer_B', x: 172, y: 228, width: 56, height: 56 },
    { elementId: 'End', x: 550, y: 160, width: 56, height: 56 },
  ];
  model.diagram.edges = model.sequenceFlows.map((edge) => ({ sequenceFlowId: edge.id,
    waypoints: [{ x: 100, y: 180 }, { x: 300, y: 180 }] }));
  return model;
}
function embeddedModel() {
  const model = emptyProcessModel();
  const body = {
    nodes: [
      { id: 'Local_Start', name: 'Start work', kind: 'Start' },
      { id: 'Local_Review', name: 'Review inside scope', kind: { UserTask: { assigneeUserId: 'anna', outputMapping: {} } } },
      { id: 'Local_End', name: 'Complete work', kind: 'End' },
    ],
    sequenceFlows: [
      { id: 'Local_Flow_1', sourceId: 'Local_Start', targetId: 'Local_Review', condition: null },
      { id: 'Local_Flow_2', sourceId: 'Local_Review', targetId: 'Local_End', condition: null },
    ],
    variables: { local_ID: { business_key: 'kept' } },
    diagram: {
      shapes: [
        { elementId: 'Local_Start', x: 40, y: 160, width: 56, height: 56 },
        { elementId: 'Local_Review', x: 180, y: 140, width: 240, height: 96 },
        { elementId: 'Local_End', x: 500, y: 160, width: 56, height: 56 },
      ],
      edges: [
        { sequenceFlowId: 'Local_Flow_1', waypoints: [{ x: 96, y: 188 }, { x: 180, y: 188 }] },
        { sequenceFlowId: 'Local_Flow_2', waypoints: [{ x: 420, y: 188 }, { x: 500, y: 188 }] },
      ],
    },
  };
  model.nodes.splice(1, 0, { id: 'Scope_Review', name: 'Review department', kind: { SubProcess: {
    body, inputMapping: { local_ID: 'vars.source_ID' }, outputMapping: { accepted_ID: 'outputs.local_ID' },
  } } });
  model.sequenceFlows = [
    { id: 'Root_Flow_1', sourceId: 'Start', targetId: 'Scope_Review', condition: null },
    { id: 'Root_Flow_2', sourceId: 'Scope_Review', targetId: 'End', condition: null },
  ];
  model.diagram.shapes.splice(1, 0, { elementId: 'Scope_Review', x: 190, y: 140, width: 240, height: 96 });
  model.diagram.edges = [
    { sequenceFlowId: 'Root_Flow_1', waypoints: [{ x: 136, y: 188 }, { x: 190, y: 188 }] },
    { sequenceFlowId: 'Root_Flow_2', waypoints: [{ x: 430, y: 188 }, { x: 400, y: 188 }] },
  ];
  return model;
}
function poll() { return [...intervals.values()].find((row) => row.delay === 3000)?.callback(); }
async function monitor(current, additions = {}) {
  fixtures({ processInstanceGetRequest: { instance: current }, processHistoryRequest: { events: [], nextSeq: 0, hasMore: false }, ...additions });
  return openProcessInstance(current.instanceId, current);
}
afterEach(async () => {
  await builder.unmount(); flows.unmount();
  document.body.replaceChildren();
  await flush(2);
  calls.length = 0; navigation.length = 0; intervals.clear();
});
after(async () => { TfWindow.confirm = confirm; globalThis.setInterval = setIntervalOriginal; globalThis.clearInterval = clearIntervalOriginal; await window.happyDOM.close(); });

test('model round trip preserves stable sequence IDs, DI, default path and opaque business keys', () => {
  const model = emptyProcessModel();
  model.variables = { Customer_ID: 9, nested_value: { Preserve_Me: true } };
  model.nodes.splice(1, 0, { id: 'Choice', name: '<Choice>', kind: { ExclusiveGateway: { defaultFlowId: 'Default' } } }, { id: 'Check', name: 'Check contract', kind: { ServiceTask: { flowId: 'flow-one', inputMapping: { Request_ID: 'vars.Customer_ID' }, outputMapping: { approved_result: 'outputs.Approved' }, verification: { Condition: { expression: 'outputs.Approved == true' } }, timeoutSeconds: 75 } } });
  model.sequenceFlows = [{ id: 'Input', sourceId: 'Start', targetId: 'Choice', condition: null }, { id: 'Default', sourceId: 'Choice', targetId: 'End', condition: null }, { id: 'Conditional', sourceId: 'Choice', targetId: 'Check', condition: 'vars.Customer_ID > 0' }];
  model.diagram.shapes.push({ elementId: 'Choice', x: 180, y: 160, width: 72, height: 72 }, { elementId: 'Check', x: 300, y: 20, width: 240, height: 96 });
  model.diagram.shapes.sort((a, b) => model.nodes.findIndex((node) => node.id === a.elementId) - model.nodes.findIndex((node) => node.id === b.elementId));
  model.diagram.edges = model.sequenceFlows.map((edge) => ({ sequenceFlowId: edge.id, waypoints: [{ x: 100, y: 180 }, { x: 180, y: 180 }, { x: 180, y: 60 }] }));
  const data = processToCanvas(model);
  assert.deepEqual(canvasToProcess(model, data.nodes, data.edges, (edge) => edge.waypoints), model);
  const graph = canvas(model);
  assert.deepEqual(graph.getData(), model);
  assert.match(graph.nodesLayer.textContent, /<Choice>/);
  assert.equal(graph.nodesLayer.querySelector('choice'), null, 'names remain text');
  graph.destroy();
});

test('embedded body edits preserve offscreen root graph, local variables and undo context', () => {
  const model = embeddedModel();
  const graph = canvas(model);
  assert.deepEqual(graph.getData(), model);
  graph.navigateProcessBody(['Scope_Review']);
  assert.deepEqual(graph.processPath, ['Scope_Review']);
  assert.deepEqual(graph.nodes.map((node) => node.id), ['Local_Start', 'Local_Review', 'Local_End']);
  graph.updateNodeLabel('Local_Review', 'Review request');
  graph.updateProcessVariables({ local_ID: { business_key: 'updated' } });
  assert.equal(graph.getData().nodes[1].kind.SubProcess.body.variables.local_ID.business_key, 'updated');
  graph.undo();
  assert.deepEqual(graph.processPath, ['Scope_Review']);
  assert.equal(graph.getData().nodes[1].kind.SubProcess.body.variables.local_ID.business_key, 'kept');
  assert.equal(graph.nodes.find((node) => node.id === 'Local_Review').label, 'Review request');
  graph.redo();
  assert.equal(graph.getData().nodes[1].kind.SubProcess.body.variables.local_ID.business_key, 'updated');
  graph.navigateProcessBody([]);
  const saved = graph.getData();
  assert.deepEqual(saved.nodes.map((node) => node.id), ['Start', 'Scope_Review', 'End']);
  assert.deepEqual(saved.sequenceFlows.map((flow) => flow.id), ['Root_Flow_1', 'Root_Flow_2']);
  assert.equal(saved.nodes[1].kind.SubProcess.body.nodes[1].name, 'Review request');
  assert.equal(saved.nodes[1].kind.SubProcess.body.variables.local_ID.business_key, 'updated');
  assert.deepEqual(saved.nodes[1].kind.SubProcess.inputMapping, { local_ID: 'vars.source_ID' });
  graph.destroy();
});

test('editor enters a real subprocess and saves its complete root model after returning', async () => {
  const current = definition('embedded-definition', { model: embeddedModel() });
  const state = await mount(current, {
    processDefinitionSaveRequest: (payload) => ({ definition: { ...current, model: payload.model, draftRevision: 5 } }),
  });
  state.canvas.selectNode('Scope_Review');
  await flush(2);
  click(state.config.root.querySelector('[data-process-enter]'));
  assert.deepEqual(state.canvas.processPath, ['Scope_Review']);
  assert.equal(state.root.querySelector('[data-role="scope-up"]').hidden, false);
  assert.match(state.root.querySelector('[data-role="scope-path"]').textContent, /Review department/);
  state.canvas.updateNodeLabel('Local_Review', 'Reviewed inside scope');
  click(state.root.querySelector('[data-role="scope-up"]'));
  assert.deepEqual(state.canvas.processPath, []);
  assert.equal(state.root.querySelector('[data-role="scope-up"]').hidden, true);
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model;
  assert.deepEqual(saved.nodes.map((node) => node.id), ['Start', 'Scope_Review', 'End']);
  assert.equal(saved.nodes[1].kind.SubProcess.body.nodes[1].name, 'Reviewed inside scope');
  assert.equal(saved.nodes[1].kind.SubProcess.body.variables.local_ID.business_key, 'kept');
});

test('inside a sub-process the breadcrumb reads process, definition, sub-process, then the selected element', async () => {
  const current = definition('crumb-order', { name: 'Order handling', model: embeddedModel() });
  const state = await mount(current);
  const crumbs = () => [...state.root.querySelectorAll('.fb-breadcrumb > :not([hidden]):not(tf-button)')].map((node) => node.textContent.trim()).filter((text) => text !== '›');
  assert.deepEqual(crumbs(), [I18n.t('bpmn.crumb_root'), 'Order handling'], 'at the root the definition is the last crumb');
  state.canvas.selectNode('Scope_Review');
  assert.deepEqual(crumbs(), [I18n.t('bpmn.crumb_root'), 'Review department'], 'unscoped, a selected element takes the definition crumb as before');
  click(state.config.root.querySelector('[data-process-enter]'));
  assert.deepEqual(crumbs(), [I18n.t('bpmn.crumb_root'), 'Order handling › Review department'],
    'with nothing selected the definition comes before the sub-process and no other name follows');
  state.canvas.selectNode('Local_Review');
  assert.deepEqual(crumbs(), [I18n.t('bpmn.crumb_root'), 'Order handling › Review department', 'Review inside scope']);
  state.root.querySelector('[data-role="name"]').value = 'Order handling v2';
  state.root.querySelector('[data-role="name"]').dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.deepEqual(crumbs().slice(0, 2), [I18n.t('bpmn.crumb_root'), 'Order handling v2 › Review department'], 'renaming the definition updates the path');
});

test('undo and redo restore content in the scope on screen, never moving the author between bodies', async () => {
  const current = definition('undo-root-definition', { model: embeddedModel() });
  const state = await mount(current);
  const toasts = watchToasts();
  dismissToast(I18n.t('bpmn.undo_elsewhere'));
  const gateway = processTemplates().find((item) => item.node_type === 'bpmn_inclusive_gateway');
  const scopePath = () => state.root.querySelector('[data-role="scope-path"]');
  state.canvas.selectNode('Scope_Review');
  click(state.config.root.querySelector('[data-process-enter]'));
  const inside = state.canvas.addNodeFromTemplate(gateway, 300, 200);
  assert.equal(state.config.node?.id, inside.id);
  click(state.root.querySelector('[data-role="scope-up"]'));
  const atRoot = state.canvas.addNodeFromTemplate(gateway, 300, 500);

  state.canvas.undo();
  assert.deepEqual(state.canvas.processPath, [], 'undoing a root step at the root stays at the root');
  assert.equal(state.canvas.nodes.some((node) => node.id === atRoot.id), false);
  assert.deepEqual([...state.canvas.selectedIds], []);
  assert.equal(state.config.node, null);
  assert.ok(state.config.root.querySelector('.fb-config-empty'));
  assert.equal(scopePath().hidden, true);
  assert.equal(state.root.querySelector('[data-role="crumb-name"]').textContent, current.name);

  state.canvas.selectNode('Scope_Review');
  click(state.config.root.querySelector('[data-process-enter]'));
  assert.ok(state.canvas.nodes.some((node) => node.id === inside.id));
  state.canvas.undo();
  assert.deepEqual(state.canvas.processPath, ['Scope_Review'], 'undoing a step inside the body stays inside it');
  assert.equal(state.canvas.nodes.some((node) => node.id === inside.id), false);
  assert.equal(scopePath().hidden, false);
  assert.match(scopePath().textContent, /Review department/);
  state.canvas.redo();
  assert.deepEqual(state.canvas.processPath, ['Scope_Review']);
  assert.ok(state.canvas.nodes.some((node) => node.id === inside.id));
  state.canvas.redo();
  assert.deepEqual(state.canvas.processPath, ['Scope_Review'], 'redoing a root step while inside leaves the view where it is');
  assert.equal(state.canvas.nodes.some((node) => node.id === atRoot.id), false, 'the root node is not drawn in the body');
  assert.ok(toasts.texts().some((text) => text.includes(I18n.t('bpmn.undo_elsewhere'))), 'a step that changed nothing on screen says where it happened');
  toasts.stop();
  click(state.root.querySelector('[data-role="scope-up"]'));
  assert.ok(state.canvas.nodes.some((node) => node.id === atRoot.id), 'the redone root step is there at the root');
});

test('undoing the creation of the body on screen climbs to the nearest body that still exists', async () => {
  const state = await mount(definition('undo-vanishing-body'));
  const subProcess = processTemplates().find((item) => item.node_type === 'bpmn_sub_process');
  const created = state.canvas.addNodeFromTemplate(subProcess, 300, 300);
  state.canvas.navigateProcessBody([created.id]);
  state.config.renderEmpty();
  state._syncProcessPath?.();
  builder._syncProcessPath();
  assert.deepEqual(state.canvas.processPath, [created.id]);
  assert.equal(state.root.querySelector('[data-role="scope-path"]').hidden, false);
  state.canvas.undo();
  assert.deepEqual(state.canvas.processPath, [], 'the sub-process is gone, so the view climbs to the root');
  assert.equal(state.root.querySelector('[data-role="scope-path"]').hidden, true);
  assert.equal(state.canvas.nodes.some((node) => node.id === created.id), false);
});

test('participant inspects a scoped run through authorized ScopeGet without definition access', async () => {
  const scope = { scopeId: 'scope-child-1', parentScopeId: 'instance-scoped', subprocessNodeId: 'Scope_Review',
    subprocessNodeName: 'Review department', parentTokenId: 'waiting-parent', revision: 2,
    status: 'Running', depth: 1, createdAtMs: 10, updatedAtMs: 20 };
  const current = instance('instance-scoped', { scopes: [scope], activeNodeIds: ['Local_Review'] });
  const win = await monitor(current, { processScopeGetRequest: (request) => {
    assert.deepEqual(request, { instanceId: 'instance-scoped', scopeId: 'scope-child-1' });
    return { scope, variables: { local_ID: { business_key: '<kept>' } }, activeNodeIds: ['Local_Review'] };
  } });
  const row = win.querySelector('[data-scope-id="scope-child-1"]');
  assert.ok(row);
  assert.match(row.textContent, /Review department/);
  click(row.querySelector('[data-scope-inspect]'));
  await flush(2);
  const detail = win.querySelector('[data-scope-detail]');
  assert.match(detail.textContent, /Local_Review/);
  assert.match(detail.querySelector('tf-code-editor').value, /<kept>/);
  assert.equal(detail.querySelector('kept'), null, 'business values remain text');
  assert.equal(calls.some((call) => call.kind === 'processVersionGetRequest'), false);
});

test('all five locales render persisted subprocess history and interruption reasons', async () => {
  const events = [
    { kind: 'scope_entered', nodeName: null, data: { subprocess_node_id: 'Scope_Review' } },
    { kind: 'scope_completed', nodeName: null, data: { subprocess_node_id: 'Scope_Review' } },
    { kind: 'scope_cancelled', nodeName: null, data: { subprocess_node_id: 'Scope_Review', reason: 'scope_cancelled' } },
    { kind: 'scope_entry_failed', nodeName: '<Review>', data: { subprocess_node_id: 'Scope_Review', reason: 'scope_limit', code: 'SCOPE_LIMIT' } },
    { kind: 'scope_error_propagated', nodeName: '<Handler>', data: { code: 'BUSINESS_409' } },
    { kind: 'incident', nodeName: 'Scope_Review', data: { code: 'SCOPE_LIMIT', message: 'process instance reached its 129-scope lifetime limit' } },
  ];
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    for (const event of events) {
      const rendered = processEventText(event);
      assert.doesNotMatch(rendered, /bpmn\.|scope_(?:entered|completed|cancelled|entry_failed|error_propagated|limit)|SCOPE_LIMIT/);
    }
    assert.match(processEventText(events[0]), /Scope_Review/, 'the stable element ID is labeled by the localized event');
    assert.match(processEventText(events[4]), /BUSINESS_409/, 'the actual business error code remains visible');
    assert.notEqual(processLifecycleReasonText('scope_cancelled'), 'scope_cancelled');
    assert.equal(processLifecycleReasonText('<private>&'), '<private>&', 'arbitrary diagnostics remain intact');
  }
  await I18n.setLanguage('en');
  const history = events.map((event, index) => ({ ...event, seq: index + 1, atMs: 1000 + index }));
  const win = await monitor(instance('scope-history'), {
    processHistoryRequest: { events: history, nextSeq: history.length, hasMore: false },
  });
  assert.match(win.textContent, /Scope_Review/);
  assert.match(win.textContent, /<Review>/);
  assert.equal(win.querySelector('review'), null, 'node names remain escaped text in history');
});

test('inclusive split and join history shows selected paths and a no-job incident without offering Retry', async () => {
  const events = [
    { kind: 'inclusive_split', nodeName: '<Choose>', data: { selected_branch_edge_ids: ['Flow_A', 'Flow_B'], default_selected: false } },
    { kind: 'inclusive_joined', nodeName: '<Join>', data: { selected_branch_edge_ids: ['Flow_A', 'Flow_B'] } },
    { kind: 'inclusive_split', nodeName: '<Choose>', data: { selected_branch_edge_ids: ['Flow_Default'], default_selected: true } },
  ];
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      assert.notEqual(I18n.t('bpmn.node_inclusive_gateway'), 'bpmn.node_inclusive_gateway');
      for (const event of events) {
        const rendered = processEventText(event);
        assert.match(rendered, new RegExp(String(event.data.selected_branch_edge_ids.length)));
        assert.doesNotMatch(rendered, /bpmn\.|Flow_A|Flow_B|Flow_Default|\{(?:node|count|default)\}/);
      }
      assert.ok(processEventText(events[2]).includes(I18n.t('bpmn.inclusive_default_selected')));
      const current = instance(`or-${language}`, { canCancel: true, status: 'Incident', incidents: [{
        incidentId: 'or-incident', nodeId: 'OR_Split', nodeName: '<Choose>', scopeId: `or-${language}`,
        jobId: null, code: 'INCLUSIVE_GATEWAY_ERROR', message: 'non_boolean_condition', canRetry: false,
      }] });
      const history = events.map((event, index) => ({ ...event, seq: index + 1, atMs: 1000 + index }));
      const win = await monitor(current, { processHistoryRequest: { events: history, nextSeq: history.length, hasMore: false } });
      assert.ok(win.querySelector('[data-incidents]').textContent.includes(I18n.t('bpmn.incident_inclusive_guidance')));
      assert.equal(win.querySelector('[data-incidents] [data-retry]'), null);
      assert.ok(win.querySelector('[data-cancel]'), 'the actual initiator retains the existing Cancel action');
      assert.equal(win.querySelector('choose'), null, 'untrusted node names remain text');
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('duplicating a subprocess rekeys its complete local graph without rewriting the original', () => {
  const graph = canvas(embeddedModel());
  graph.duplicateNodes(['Scope_Review']);
  const model = graph.getData();
  const original = model.nodes.find((node) => node.id === 'Scope_Review').kind.SubProcess.body;
  const duplicate = model.nodes.find((node) => node.id !== 'Scope_Review' && node.kind?.SubProcess)?.kind.SubProcess.body;
  assert.ok(duplicate);
  assert.deepEqual(original.nodes.map((node) => node.id), ['Local_Start', 'Local_Review', 'Local_End']);
  assert.deepEqual(duplicate.nodes.map((node) => node.name), original.nodes.map((node) => node.name));
  assert.deepEqual(duplicate.variables, original.variables);
  assert.equal(new Set([...original.nodes, ...duplicate.nodes].map((node) => node.id)).size, 6);
  assert.equal(new Set([...original.sequenceFlows, ...duplicate.sequenceFlows].map((flow) => flow.id)).size, 4);
  assert.ok(duplicate.sequenceFlows.every((flow) => duplicate.nodes.some((node) => node.id === flow.sourceId)
    && duplicate.nodes.some((node) => node.id === flow.targetId)));
  assert.deepEqual(duplicate.diagram.shapes.map((shape) => shape.elementId), duplicate.nodes.map((node) => node.id));
  graph.destroy();
});

test('boundary siblings retain stable attachment, modes, DI and reject incoming sequence flow', () => {
  const model = boundaryModel();
  const graph = canvas(model);
  assert.deepEqual(graph.getData(), model);
  assert.equal(graph.nodesLayer.querySelectorAll('.bpmn_boundary_timer').length, 2);
  assert.equal(graph.nodesLayer.querySelector('.bpmn_boundary_timer.fb-boundary-interrupting').dataset.nodeId, 'Timer_A');
  assert.equal(graph.nodesLayer.querySelector('.bpmn_boundary_timer.fb-boundary-noninterrupting').dataset.nodeId, 'Timer_B');
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="Timer_A"] use').getAttribute('href'), '#i-clock');
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="Timer_B"] use').getAttribute('href'), '#i-clock');
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="Timer_A"] .fb-port-in'), null);
  assert.equal(graph.connectNodes('Start', 'Timer_A'), false);
  assert.deepEqual(graph.getData(), model);
  graph.destroy();
});

test('BPMN canvas renders the official marker mapping for every supported template', () => {
  const expected = new Map([
    ['bpmn_start', null], ['bpmn_end', null],
    ['bpmn_error_end', 'bpmn-error-filled'], ['bpmn_terminate_end', 'bpmn-terminate'],
    ['bpmn_timer_start', 'clock'], ['bpmn_timer_catch', 'clock'], ['bpmn_boundary_timer', 'clock'],
    ['bpmn_message_start', 'bpmn-message-catch'], ['bpmn_message_catch', 'bpmn-message-catch'],
    ['bpmn_message_throw', 'bpmn-message-throw'], ['bpmn_signal_throw', 'bpmn-signal-throw'],
    ['bpmn_signal_catch', 'bpmn-signal-catch'], ['bpmn_link_throw', 'link-throw'],
    ['bpmn_link_catch', 'link-catch'], ['bpmn_boundary_message', 'bpmn-message-catch'],
    ['bpmn_boundary_error', 'bpmn-error'], ['bpmn_boundary_escalation', 'bpmn-escalation-catch'],
    ['bpmn_user_task', 'user'], ['bpmn_service_task', 'bpmn-service-task'],
    ['bpmn_script_task', 'bpmn-script-task'], ['bpmn_manual_task', 'bpmn-manual-task'],
    ['bpmn_send_task', 'bpmn-message-throw'], ['bpmn_receive_task', 'bpmn-message-catch'],
    ['bpmn_sub_process', 'bpmn-subprocess'], ['bpmn_call_activity', 'bpmn-subprocess'],
    ['bpmn_exclusive_gateway', 'bpmn-gateway-exclusive'], ['bpmn_parallel_gateway', 'bpmn-gateway-parallel'],
    ['bpmn_inclusive_gateway', 'bpmn-gateway-inclusive'], ['bpmn_event_based_gateway', 'bpmn-gateway-event-based'],
  ]);
  assert.equal(expected.size, 29);
  assert.equal(processTemplates().length, expected.size);
  for (const [nodeType, marker] of expected) {
    const graph = canvas();
    graph.addNodeFromTemplate(processTemplates().find((template) => template.node_type === nodeType), 250, 300);
    const node = graph.nodes.at(-1);
    const element = graph.nodesLayer.querySelector(`[data-node-id="${node.id}"]`);
    const use = element.querySelector('.fb-process-symbol use');
    assert.equal(use?.getAttribute('href') ?? null, marker == null ? null : `#i-${marker}`, nodeType);
    if (nodeType === 'bpmn_start' || nodeType === 'bpmn_end') assert.equal(element.querySelector('.fb-process-symbol svg'), null, nodeType);
    graph.destroy();
  }
});

test('BPMN marker definitions preserve filled and hollow semantics and boundary rings', () => {
  const sprite = readFileSync(new URL('../../../index.html', import.meta.url), 'utf8');
  const css = readFileSync(new URL('../../../css/flows-builder.css', import.meta.url), 'utf8');
  const symbolFragment = (id) => {
    const match = sprite.match(new RegExp(`<symbol id="i-${id}"[^>]*>([\\s\\S]*?)</symbol>`));
    assert.ok(match, `missing symbol fragment ${id}`);
    return match[1];
  };
  for (const marker of ['bpmn-none-start', 'bpmn-none-end', 'bpmn-message-catch', 'bpmn-message-throw',
    'bpmn-signal-catch', 'bpmn-signal-throw', 'bpmn-error', 'bpmn-error-filled', 'bpmn-terminate', 'bpmn-escalation-catch',
    'bpmn-service-task', 'bpmn-script-task', 'bpmn-manual-task', 'bpmn-subprocess', 'bpmn-gateway-exclusive',
    'bpmn-gateway-parallel', 'bpmn-gateway-inclusive', 'bpmn-gateway-event-based']) {
    assert.match(sprite, new RegExp(`<symbol id="i-${marker}"`), marker);
  }
  const spriteIds = new Set([...sprite.matchAll(/<symbol id="i-([^"]+)"/g)].map((match) => match[1]));
  for (const marker of processTemplates().map((template) => template.icon).filter((icon) => icon.startsWith('bpmn-'))) {
    assert.ok(spriteIds.has(marker), `missing sprite symbol ${marker}`);
  }
  const messageThrow = symbolFragment('bpmn-message-throw');
  assert.match(messageThrow, /<rect[^>]*fill="currentColor"/);
  assert.match(messageThrow, /<path d="M3 6l9 7 9-7"[^>]*stroke="var\(--tf-bg-card/);
  assert.match(symbolFragment('bpmn-signal-throw'), /fill="currentColor"/);
  assert.match(symbolFragment('bpmn-message-catch'), /fill="none"/);
  assert.match(symbolFragment('bpmn-signal-catch'), /fill="none"/);
  assert.match(symbolFragment('bpmn-error'), /fill="none"/);
  assert.match(symbolFragment('bpmn-error-filled'), /fill="currentColor"/);
  assert.match(symbolFragment('bpmn-terminate'), /fill="currentColor"/);
  assert.match(symbolFragment('bpmn-escalation-catch'), /fill="none"/);
  assert.match(css, /bpmn_boundary_timer\.fb-boundary-interrupting::before/);
  assert.match(css, /bpmn_boundary_timer\.fb-boundary-noninterrupting::before/);
});

test('mounted Send boundaries clear both host ports and remain selectable after the host is raised', async () => {
  const model = boundaryModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.nodes.find((node) => node.id === 'Review').kind = { SendTask: {
    messageRef: 'Message_1', target: { Start: { definitionId: 'target-definition' } },
    correlationExpression: 'vars.case_key', payloadExpression: 'vars.payload', ttlSeconds: 60,
  } };
  model.variables.case_key = 'case-1';
  model.variables.payload = { case_key: 'case-1' };
  const parentShape = model.diagram.shapes.find((shape) => shape.elementId === 'Review');
  for (const [id, portX] of [['Timer_A', parentShape.x + parentShape.width], ['Timer_B', parentShape.x]]) {
    const shape = model.diagram.shapes.find((entry) => entry.elementId === id);
    shape.x = portX - shape.width / 2;
    shape.y = parentShape.y + parentShape.height / 2 - shape.height / 2;
  }
  const state = await mount(definition('send-boundary-port-clearance', { model }));
  state.canvas.view.zoom = 0.34;
  for (const id of ['Timer_A', 'Timer_B']) {
    state.canvas.updateNodeConfig(id, { attachedToId: null });
    state.canvas.updateNodeConfig(id, { attachedToId: 'Review' });
  }
  const parent = state.canvas.nodes.find((node) => node.id === 'Review');
  const boundaries = ['Timer_A', 'Timer_B'].map((id) => state.canvas.nodes.find((node) => node.id === id));
  const ports = [state.canvas._getPortWorldPos('Review', 'in', 'in'), state.canvas._getPortWorldPos('Review', 'full', 'out')];
  for (const boundary of boundaries) {
    const center = { x: boundary.x + boundary.width / 2, y: boundary.y + boundary.height / 2 };
    assert.ok(center.x === parent.x || center.x === parent.x + parent.width
      || center.y === parent.y || center.y === parent.y + parent.height);
    assert.ok(ports.every((port) => Math.hypot(center.x - port.x, center.y - port.y) >= boundary.width / 2 + 16));
    assert.equal(state.canvas.getData().nodes.find((node) => node.id === boundary.id).kind.BoundaryTimer.attachedToId, 'Review');
    assert.equal(state.canvas.getData().diagram.shapes.find((shape) => shape.elementId === boundary.id).x, boundary.x);
  }
  assert.ok(Math.hypot(boundaries[0].x - boundaries[1].x, boundaries[0].y - boundaries[1].y) >= boundaries[0].width);
  const host = state.canvas.nodesLayer.querySelector('[data-node-id="Review"]');
  host.querySelector('.fb-process-symbol').dispatchEvent(new window.PointerEvent('pointerdown', {
    bubbles: true, pointerId: 81, button: 0, clientX: 200, clientY: 200,
  }));
  window.dispatchEvent(new window.PointerEvent('pointerup', { pointerId: 81, button: 0 }));
  const hostLayer = Number(host.style.zIndex);
  for (const boundary of boundaries) {
    const element = state.canvas.nodesLayer.querySelector(`[data-node-id="${boundary.id}"]`);
    assert.ok(Number(element.style.zIndex) > hostLayer);
  }
  click(state.canvas.nodesLayer.querySelector('[data-node-id="Timer_A"] .fb-process-symbol'));
  assert.deepEqual([...state.canvas.selectedIds], ['Timer_A']);
  state.canvas.updateNodeLabel('Timer_A', 'Revised deadline');
  const selected = state.canvas.nodesLayer.querySelector('[data-node-id="Timer_A"]');
  assert.ok(Number(selected.style.zIndex) > Number(host.style.zIndex));
  click(selected.querySelector('.fb-process-symbol'));
  assert.deepEqual([...state.canvas.selectedIds], ['Timer_A']);
});

test('boundary inspector uses stable activity selection and an actual checked toggle', async () => {
  const graph = canvas(boundaryModel());
  const boundary = graph.nodes.find((node) => node.id === 'Timer_A');
  const panel = inspector(graph); panel.show(boundary, graph.templates.get(boundary.type));
  await flush(2);
  const attach = panel.root.querySelector('[data-process="attachedToId"]');
  assert.equal(attach.value, 'Review');
  assert.match(panel.root.querySelector('[data-boundary-target]').textContent, /Review contract/);
  assert.deepEqual([...panel.root.querySelectorAll('[data-process="timerType"] option')].map((option) => option.value), ['Date', 'Duration', 'WorkingDuration']);
  const toggle = panel.root.querySelector('[data-process="cancelActivity"]');
  toggle.checked = false;
  toggle.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { checked: false } }));
  assert.equal(boundary.config.cancelActivity, false);
  assert.ok(graph.nodesLayer.querySelector('[data-node-id="Timer_A"]').classList.contains('fb-boundary-noninterrupting'));
  change(attach, '');
  assert.equal(boundary.config.attachedToId, null);
  assert.ok(graph.validate().length > 0);
  change(attach, 'Review');
  assert.equal(boundary.config.attachedToId, 'Review');
  change(panel.root.querySelector('[data-process="timerType"]'), 'Date');
  await flush(2);
  assert.ok(graph.validate().length > 0, 'an empty date is not a valid saved rule');
  change(panel.root.querySelector('[data-process="timerAt"]'), '2027-01-02T03:04:05+01:00');
  assert.deepEqual(boundary.config.timer, { Date: { at: '2027-01-02T03:04:05+01:00' } });
  assert.deepEqual(graph.validate(), []);
  const readonly = inspector(graph, true); readonly.show(boundary, graph.templates.get(boundary.type));
  assert.ok(readonly.root.querySelector('[data-process="attachedToId"]').hasAttribute('disabled'));
  assert.ok(readonly.root.querySelector('[data-process="cancelActivity"]').hasAttribute('disabled'));
  readonly.root.querySelector('[data-process="cancelActivity"]').dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  assert.equal(boundary.config.cancelActivity, false);
  graph.destroy(); panel.destroy(); readonly.destroy();
});

test('process inspector keeps long node and sequence IDs fully available in readonly multiline controls', () => {
  const model = emptyProcessModel();
  const nodeId = `Start_${'N'.repeat(96)}`;
  const edgeId = `Sequence_${'E'.repeat(96)}`;
  model.nodes[0].id = nodeId;
  model.sequenceFlows[0].sourceId = nodeId;
  model.sequenceFlows[0].id = edgeId;
  model.diagram.shapes[0].elementId = nodeId;
  model.diagram.edges[0].sequenceFlowId = edgeId;
  const graph = canvas(model);
  const panel = inspector(graph);
  const assertId = (expected) => {
    const field = panel.root.querySelector('[data-process="elementId"]');
    const control = field.querySelector('textarea');
    assert.equal(field.tagName, 'TF-TEXTAREA');
    assert.equal(field.value, expected);
    assert.equal(field.hasAttribute('autogrow'), true);
    assert.equal(control.value, expected);
    assert.equal(control.readOnly, true);
    assert.equal(control.disabled, false);
  };
  const node = graph.nodes.find((candidate) => candidate.id === nodeId);
  panel.show(node, graph.templates.get(node.type));
  assertId(nodeId);
  panel.showEdge(graph.edges.find((candidate) => candidate.id === edgeId));
  assertId(edgeId);
  assert.deepEqual(graph.getData(), model);
  graph.destroy(); panel.destroy();
});

test('boundary moves with its activity once, cascades on delete and remaps cloned parent', () => {
  const graph = canvas(boundaryModel());
  graph.view.zoom = 1;
  const review = graph.nodes.find((node) => node.id === 'Review');
  const first = graph.nodes.find((node) => node.id === 'Timer_A');
  const original = graph.getData();
  const before = { parentX: review.x, childX: first.x };
  const element = graph.nodesLayer.querySelector('[data-node-id="Review"]');
  element.dispatchEvent(new window.PointerEvent('pointerdown', { bubbles: true, pointerId: 33, button: 0, clientX: 200, clientY: 200 }));
  graph._flushPointerMove(250, 200);
  window.dispatchEvent(new window.PointerEvent('pointerup', { pointerId: 33, button: 0 }));
  assert.equal(review.x - before.parentX, first.x - before.childX);
  graph.undo(); assert.deepEqual(graph.getData(), original);
  graph.redo();
  assert.equal(graph.nodes.find((node) => node.id === 'Review').x - before.parentX,
    graph.nodes.find((node) => node.id === 'Timer_A').x - before.childX);
  graph.duplicateNodes(['Review', 'Timer_A']);
  const clones = graph.nodes.filter((node) => graph.selectedIds.has(node.id));
  const cloneParent = clones.find((node) => node.type === 'bpmn_user_task');
  const cloneBoundary = clones.find((node) => node.type === 'bpmn_boundary_timer');
  assert.equal(cloneBoundary.config.attachedToId, cloneParent.id);
  graph.duplicateNodes(['Timer_B']);
  const reminderClone = graph.nodes.find((node) => graph.selectedIds.has(node.id));
  assert.equal(reminderClone.config.attachedToId, 'Review');
  assert.notEqual(reminderClone.id, 'Timer_B');
  assert.notDeepEqual({ x: reminderClone.x, y: reminderClone.y }, { x: graph.nodes.find((node) => node.id === 'Timer_B').x, y: graph.nodes.find((node) => node.id === 'Timer_B').y });
  graph.removeNodes(['Review']);
  assert.ok(!graph.nodes.some((node) => ['Review', 'Timer_A', 'Timer_B', reminderClone.id].includes(node.id)));
  assert.ok(graph.nodes.some((node) => node.id === 'End'));
  graph.undo(); assert.ok(graph.nodes.some((node) => node.id === 'Review'));
  graph.destroy();
});

test('grid snapping canonicalizes negative-origin creation and drag without changing ordinary coordinates', () => {
  const graph = canvas();
  graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 900, height: 600 });
  graph.view = { x: 0, y: 0, zoom: 1 };
  const template = processTemplates().find((row) => row.node_type === 'bpmn_user_task');
  const added = graph.addNodeFromTemplate(template, template.width / 2 - 0.4, template.height / 2 + 30.4);
  assert.equal(added.x, 0);
  assert.equal(Object.is(added.x, -0), false);
  assert.equal(added.y, 30);

  const nodeElement = graph.nodesLayer.querySelector(`[data-node-id="${CSS.escape(added.id)}"]`);
  nodeElement.dispatchEvent(new window.PointerEvent('pointerdown', {
    bubbles: true, pointerId: 71, button: 0, clientX: 100, clientY: 100,
  }));
  graph._flushPointerMove(95.9, 100);
  assert.equal(added.x, 0);
  assert.equal(Object.is(added.x, -0), false);
  window.dispatchEvent(new window.PointerEvent('pointerup', {
    pointerId: 71, button: 0, clientX: 95.9, clientY: 100,
  }));

  const ordinary = graph.nodes.find((node) => node.id === 'Start');
  const before = { x: ordinary.x, y: ordinary.y };
  const ordinaryElement = graph.nodesLayer.querySelector('[data-node-id="Start"]');
  ordinaryElement.dispatchEvent(new window.PointerEvent('pointerdown', {
    bubbles: true, pointerId: 72, button: 0, clientX: 100, clientY: 100,
  }));
  graph._flushPointerMove(104.1, 104.1);
  assert.deepEqual({ x: ordinary.x, y: ordinary.y }, before);
  window.dispatchEvent(new window.PointerEvent('pointerup', {
    pointerId: 72, button: 0, clientX: 104.1, clientY: 104.1,
  }));
  graph.destroy();
});

test('duplicating an activity preserves its data-object binding with fresh IO element IDs', () => {
  const graph = canvas();
  const original = userNode(graph);
  original.activityIo = {
    dataInputs: [{ id: 'Customer_Input', name: '' }],
    dataOutputs: [{ id: 'Reviewed_Output', name: null, valueExpression: 'inputs.Customer_Input' }],
    inputSetId: 'Input_Set', inputSet: ['Customer_Input'],
    outputSetId: 'Output_Set', outputSet: ['Reviewed_Output'],
    inputAssociations: [{ DirectRef: { id: 'Input_Association', sourceObjectRefId: 'Customer_Ref', targetInputId: 'Customer_Input' } }],
    outputAssociations: [{ id: 'Output_Association', sourceOutputId: 'Reviewed_Output', targetObjectRefId: 'Review_Ref' }],
  };
  const before = structuredClone(original.activityIo);
  graph.duplicateNodes([original.id]);
  const clone = graph.nodes.find((node) => graph.selectedIds.has(node.id));
  assert.ok(clone && clone.id !== original.id);
  assert.deepEqual(original.activityIo, before);
  const saved = graph.getData();
  const copied = saved.nodes.find((node) => node.id === clone.id).activityIo;
  assert.notEqual(copied.dataInputs[0].id, before.dataInputs[0].id);
  assert.notEqual(copied.dataOutputs[0].id, before.dataOutputs[0].id);
  assert.equal(copied.inputSet[0], copied.dataInputs[0].id);
  assert.equal(copied.outputSet[0], copied.dataOutputs[0].id);
  assert.equal(copied.inputAssociations[0].DirectRef.targetInputId, copied.dataInputs[0].id);
  assert.equal(copied.inputAssociations[0].DirectRef.sourceObjectRefId, 'Customer_Ref');
  assert.equal(copied.outputAssociations[0].sourceOutputId, copied.dataOutputs[0].id);
  assert.equal(copied.outputAssociations[0].targetObjectRefId, 'Review_Ref');
  graph.destroy();
});

test('message and error elements retain declarations, target expressions, attachment and DI through real canvas edits', async () => {
  const model = emptyProcessModel();
  model.targetNamespace = 'urn:example:orders';
  model.messages = [{ messageId: 'Message_1', name: `order.received ${'Long declaration '.repeat(14)}<&>` }];
  model.errors = [{ errorId: 'Error_1', name: 'Validation & retry', errorCode: 'BUSINESS.INVALID' }];
  model.variables = { customer_ID: { attached_to_id: 'kept' } };
  model.nodes.splice(1, 0,
    { id: 'Task_1', name: 'Check', kind: { ServiceTask: { flowId: 'flow-one', inputMapping: { customer_ID: 'vars.customer_ID' },
      outputMapping: {}, verification: 'Human', timeoutSeconds: 60, resultExpression: 'outputs.payload' } } },
    { id: 'BoundaryMessage_1', name: 'Message', kind: { BoundaryMessage: { attachedToId: 'Task_1', cancelActivity: false,
      messageRef: 'Message_1', correlationExpression: 'vars.customer_ID', outputMapping: { customer_ID: 'outputs.customer_ID' } } } },
    { id: 'BoundaryError_1', name: 'Error', kind: { BoundaryError: { attachedToId: 'Task_1', errorRef: 'Error_1',
      outputMapping: { customer_ID: 'activity_result.outputs.customer_ID' } } } },
    { id: 'Gateway_1', name: 'First event', kind: 'EventBasedGateway' },
    { id: 'Catch_1', name: 'Wait', kind: { MessageCatch: { messageRef: 'Message_1',
      correlationExpression: 'vars.customer_ID', outputMapping: {} } } },
    { id: 'Throw_1', name: 'Send', kind: { MessageThrow: { messageRef: 'Message_1',
      target: { Catch: { definitionId: 'definition-one', instanceIdExpression: 'vars.instance_ID', subscriptionIdExpression: null } },
      correlationExpression: 'vars.customer_ID', payloadExpression: 'vars.customer_ID', ttlSeconds: 60 } } });
  model.diagram.shapes.splice(1, 0,
    { elementId: 'Task_1', x: 200, y: 160, width: 240, height: 96 },
    { elementId: 'BoundaryMessage_1', x: 412, y: 230, width: 56, height: 56 },
    { elementId: 'BoundaryError_1', x: 172, y: 230, width: 56, height: 56 },
    { elementId: 'Gateway_1', x: 500, y: 160, width: 72, height: 72 },
    { elementId: 'Catch_1', x: 620, y: 160, width: 56, height: 56 },
    { elementId: 'Throw_1', x: 720, y: 160, width: 56, height: 56 });
  const graph = canvas(model);
  assert.deepEqual(graph.getData(), model);
  const config = inspector(graph);
  const messageNode = graph.nodes.find((node) => node.id === 'BoundaryMessage_1');
  config.show(messageNode, graph.templates.get(messageNode.type));
  await flush(2);
  const messageRef = config.root.querySelector('[data-process="messageRef"]');
  assert.ok(messageRef.hasAttribute('wrap-selected'));
  assert.equal(messageRef.querySelector('.tf-select-selected').textContent,
    model.messages[0].name, 'the declaration is shown by its name, not its id');
  assert.ok(config.root.querySelector('[data-process="attachedToId"]').hasAttribute('wrap-selected'));
  assert.ok(config.root.querySelector('[data-process="outputMapping"]').hasAttribute('multiline'));
  const errorNode = graph.nodes.find((node) => node.id === 'BoundaryError_1');
  config.show(errorNode, graph.templates.get(errorNode.type));
  assert.ok(config.root.querySelector('[data-process="errorRef"]').hasAttribute('wrap-selected'));
  const throwNode = graph.nodes.find((node) => node.id === 'Throw_1');
  config.show(throwNode, graph.templates.get(throwNode.type));
  fixtures({
    processDefinitionListRequest: { definitions: [{ definitionId: 'target-definition', name: 'Target' }], hasMore: false },
    processDefinitionGetRequest: { definition: definition('target-definition', { publishedVersion: 1 }), startCatalog: [] },
    processVersionGetRequest: { version: { model }, startCatalog: [] },
  });
  click(config.root.querySelector('[data-load-targets]'));
  await flush(2);
  change(config.root.querySelector('[data-process="targetDefinitionChoice"]'), 'target-definition');
  await flush(2);
  const capability = config.root.querySelector('[data-target-capability]');
  assert.equal(capability.textContent, `${I18n.t('bpmn.target_declared_names')}: ${model.messages[0].name}`);
  assert.equal(capability.querySelector('script'), null);
  graph.undo();
  assert.deepEqual(graph.getData(), model);
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="BoundaryMessage_1"] .fb-port-in'), null);
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="BoundaryError_1"] .fb-port-in'), null);
  assert.equal(graph.connectNodes('Start', 'BoundaryMessage_1'), false);
  graph.duplicateNodes(['Task_1', 'BoundaryMessage_1', 'BoundaryError_1']);
  const clones = graph.nodes.filter((node) => graph.selectedIds.has(node.id));
  const task = clones.find((node) => node.type === 'bpmn_service_task');
  assert.ok(task);
  assert.equal(clones.filter((node) => processBoundaryKind(node.type)).length, 2);
  assert.ok(clones.filter((node) => processBoundaryKind(node.type)).every((node) => node.config.attachedToId === task.id));
  graph.undo();
  assert.deepEqual(graph.getData(), model);
  graph.updateProcessDeclarations({ messages: [{ messageId: 'Message_1', name: 'order.updated' }],
    errors: model.errors, targetNamespace: model.targetNamespace });
  assert.equal(graph.getData().messages[0].name, 'order.updated');
  graph.undo();
  assert.deepEqual(graph.getData(), model);
  config.destroy();
  graph.destroy();
});

test('call picker binds an exact published version and previews it without replacing the unsaved caller', async () => {
  const model = emptyProcessModel();
  model.targetNamespace = 'urn:example:caller';
  model.additionalProcesses = [{ processId: 'Local_Process', processName: 'Local review',
    nodes: [{ id: 'Local_Start', name: '', kind: 'Start' }, { id: 'Local_End', name: '', kind: 'End' }],
    sequenceFlows: [{ id: 'Local_Flow', sourceId: 'Local_Start', targetId: 'Local_End', condition: null }],
    variables: {}, diagram: { shapes: [], edges: [] } }];
  const called = emptyProcessModel();
  called.processId = 'Called_Process';
  called.targetNamespace = 'urn:example:called';
  const extraCalled = { processId: 'Extra_Process', processName: 'Second callable body',
    nodes: [{ id: 'Extra_Start', name: '', kind: 'Start' }, { id: 'Extra_End', name: '', kind: 'End' }],
    sequenceFlows: [{ id: 'Extra_Flow', sourceId: 'Extra_Start', targetId: 'Extra_End', condition: null }],
    variables: {}, diagram: { shapes: [], edges: [] } };
  called.additionalProcesses = [extraCalled];
  const second = emptyProcessModel();
  second.processId = 'Second_Process';
  second.targetNamespace = 'urn:example:second';
  model.nodes.splice(1, 0, { id: 'Call_1', name: 'Request review', kind: { CallActivity: {
    calledDefinitionId: '', calledVersion: 0, calledElement: { namespaceUri: '', processId: '' },
    inputMapping: { customer_ID: 'vars.customer_ID' }, outputMapping: { return_value: 'outputs.return_value' },
  } } });
  model.sequenceFlows = [
    { id: 'Flow_1', sourceId: 'Start', targetId: 'Call_1', condition: null },
    { id: 'Flow_2', sourceId: 'Call_1', targetId: 'End', condition: null },
  ];
  const graph = canvas(model);
  const ownId = 'cccccccc-0000-4000-8000-000000000001';
  const config = inspector(graph);
  const call = graph.nodes.find((node) => node.id === 'Call_1');
  const before = structuredClone(graph.getData());
  const definitionId = '91764f75-dadb-41aa-a252-a8a911fe7a94';
  const secondDefinitionId = 'f020336c-5ae2-4b7d-b7ef-c71497b192db';
  fixtures({
    // The picker lists what can be called: a published, unarchived version. A draft is not offered, and the
    // edited process is just another definition (calling its earlier published version is allowed).
    processDefinitionListRequest: { definitions: [{ definitionId, name: 'Published review', publishedVersion: 7 },
      { definitionId: secondDefinitionId, name: 'Second published process', publishedVersion: 3 },
      { definitionId: 'draft-only', name: 'Never published', publishedVersion: null },
      { definitionId: 'retired', name: 'Archived one', publishedVersion: 1, archived: true },
      { definitionId: ownId, name: 'This very process', publishedVersion: 2 }], hasMore: false },
    processVersionListRequest: ({ definitionId: selected }) => ({ versions: [{ definitionId: selected,
      version: selected === definitionId ? 7 : 3 }], hasMore: false }),
    processVersionGetRequest: ({ definitionId: selected, version }) => ({ version: { definitionId: selected,
      version, model: selected === definitionId ? called : second }, startCatalog: [] }),
  });
  config.show(call, graph.templates.get(call.type));
  await flush(3);
  // Nothing waits behind a Load button: the definitions are listed on their own.
  assert.deepEqual([...config.root.querySelectorAll('[data-process="calledDefinitionChoice"] option')].map((option) => option.value).filter(Boolean),
    [definitionId, secondDefinitionId, ownId]);
  assert.equal(config.root.querySelector('[data-load-calls]').hidden, true, 'no more to load');
  change(config.root.querySelector('[data-process="calledDefinitionChoice"]'), definitionId);
  assert.match(config.root.querySelector('[data-process="calledDefinitionChoice"] .tf-select-selected').textContent,
    /Published review/, 'the selected call uses the published name rather than only its UUID');
  await flush(3);
  assert.deepEqual([...config.root.querySelectorAll('[data-process="calledVersionChoice"] option')].map((option) => option.value).filter(Boolean), ['7'],
    'the versions of the chosen definition are listed on their own');
  assert.equal(config.root.querySelector('[data-load-call-versions]').hidden, true);
  change(config.root.querySelector('[data-process="calledVersionChoice"]'), '7');
  await flush(2);
  change(config.root.querySelector('[data-process="calledProcessChoice"]'), 'Called_Process');
  assert.deepEqual(call.config.calledElement, { namespaceUri: 'urn:example:called', processId: 'Called_Process' });
  assert.equal(call.config.calledVersion, 7);
  assert.equal(call.config.calledDefinitionId, definitionId);
  assert.deepEqual(call.config.inputMapping, before.nodes[1].kind.CallActivity.inputMapping);
  assert.ok(config.root.querySelector('[data-process="calledDefinitionChoice"]').hasAttribute('wrap-selected'));
  click(config.root.querySelector('[data-preview-called]'));
  await flush(2);
  const preview = [...document.querySelectorAll('tf-window')].at(-1);
  assert.equal(preview.querySelector('tf-code-editor').hasAttribute('readonly'), true);
  assert.deepEqual(JSON.parse(preview.querySelector('tf-code-editor').value), called);
  change(config.root.querySelector('[data-process="calledProcessChoice"]'), 'Extra_Process');
  click(config.root.querySelector('[data-preview-called]'));
  await flush(2);
  const selectedPreview = [...document.querySelectorAll('tf-window')].at(-1);
  assert.deepEqual(JSON.parse(selectedPreview.querySelector('tf-code-editor').value), extraCalled);
  change(config.root.querySelector('[data-process="calledProcessChoice"]'), 'Called_Process');
  assert.deepEqual(graph.getData().nodes[0], before.nodes[0]);
  assert.equal(graph.getData().nodes.find((node) => node.id === 'Call_1').kind.CallActivity.calledVersion, 7);
  change(config.root.querySelector('[data-process="calledVersionChoice"]'), '');
  assert.equal(call.config.calledVersion, 0, 'clearing the selected version clears the actual draft binding');
  assert.deepEqual(call.config.calledElement, { namespaceUri: '', processId: '' });
  assert.equal(config.root.querySelector('[data-preview-called]').hasAttribute('disabled'), true);
  const loader = config.root.querySelector('[data-load-call-versions]');
  assert.equal(loader.hidden, true, 'one exhausted page hides the first target loader');
  change(config.root.querySelector('[data-process="calledDefinitionChoice"]'), secondDefinitionId);
  await flush(3);
  assert.equal(loader.hidden, true, 'switching targets loads the versions of the new one by itself');
  change(config.root.querySelector('[data-process="calledVersionChoice"]'), '3');
  await flush(2);
  change(config.root.querySelector('[data-process="calledProcessChoice"]'), 'Second_Process');
  assert.equal(call.config.calledDefinitionId, secondDefinitionId);
  assert.equal(call.config.calledVersion, 3);
  assert.deepEqual(call.config.calledElement, { namespaceUri: 'urn:example:second', processId: 'Second_Process' });
  assert.deepEqual(graph.getData().nodes[0], before.nodes[0], 'switching exact targets retains the unsaved caller');
  change(config.root.querySelector('[data-process="callTargetMode"]'), 'local');
  assert.ok([...config.root.querySelector('[data-process="localBodyChoice"] .tf-select').options]
    .some((option) => option.value === 'Local_Process'));
  change(config.root.querySelector('[data-process="localBodyChoice"]'), 'Local_Process');
  assert.deepEqual(graph.getData().nodes.find((node) => node.id === 'Call_1').kind.CallActivity,
    { localBody: { namespaceUri: 'urn:example:caller', processId: 'Local_Process' },
      inputMapping: before.nodes[1].kind.CallActivity.inputMapping,
      outputMapping: before.nodes[1].kind.CallActivity.outputMapping });
  change(config.root.querySelector('[data-process="callTargetMode"]'), 'published');
  assert.equal(Object.hasOwn(call.config, 'localBody'), false);
  assert.equal(call.config.calledVersion, 0);
  config.destroy(); graph.destroy();
});

function callerModel(call) {
  const model = emptyProcessModel();
  model.nodes.splice(1, 0, { id: 'Call_1', name: 'Request review', kind: { CallActivity: { calledDefinitionId: '', calledVersion: 0,
    calledElement: { namespaceUri: '', processId: '' }, inputMapping: {}, outputMapping: {}, ...call } } });
  model.sequenceFlows = [{ id: 'Flow_1', sourceId: 'Start', targetId: 'Call_1', condition: null }, { id: 'Flow_2', sourceId: 'Call_1', targetId: 'End', condition: null }];
  return model;
}

test('call picker shows names without ids, numbers only names that repeat, and names an unnamed body by its position', async () => {
  const first = '11111111-aaaa-4000-8000-000000000001', second = '22222222-bbbb-4000-8000-000000000002', third = '33333333-cccc-4000-8000-000000000003';
  const called = emptyProcessModel(); called.processId = 'Process_called_0001';
  const graph = canvas(callerModel({}));
  const config = inspector(graph);
  fixtures({
    processDefinitionListRequest: { definitions: [{ definitionId: first, name: 'Review', publishedVersion: 1 },
      { definitionId: second, name: 'Review', publishedVersion: 1 }, { definitionId: third, name: 'Archive', publishedVersion: 1 }], hasMore: false },
    processVersionListRequest: { versions: [{ version: 1 }], hasMore: false },
    processVersionGetRequest: () => ({ version: { model: called }, startCatalog: [] }),
  });
  const call = graph.nodes.find((node) => node.id === 'Call_1');
  config.show(call, graph.templates.get(call.type));
  await flush(3);
  const labels = (role) => [...config.root.querySelectorAll(`[data-process="${role}"] option`)].map((option) => option.textContent).filter((text) => text && !/^Choose/.test(text));
  assert.deepEqual(labels('calledDefinitionChoice'), ['Review (1)', 'Review (2)', 'Archive']);
  for (const text of labels('calledDefinitionChoice')) assert.doesNotMatch(text, /[0-9a-f]{8}-|·/, 'no id in an option');
  change(config.root.querySelector('[data-process="calledDefinitionChoice"]'), first);
  await flush(3);
  change(config.root.querySelector('[data-process="calledVersionChoice"]'), '1');
  await flush(2);
  assert.deepEqual(labels('calledProcessChoice'), ['Process 1'], 'an unnamed body is called by its position, not by Process_called_0001');
  graph.destroy();
});

test('a saved call target that is archived or gone says so, by name, instead of showing a bare id', async () => {
  const archivedId = 'aaaaaaaa-0000-4000-8000-00000000000a', goneId = 'bbbbbbbb-0000-4000-8000-00000000000b', liveId = 'cccccccc-0000-4000-8000-00000000000c';
  const called = emptyProcessModel(); called.processId = 'Called_Process'; called.processName = 'Invoice approval';
  const open = async (definitionId, extra = {}) => {
    const graph = canvas(callerModel({ calledDefinitionId: definitionId, calledVersion: 2, calledElement: { namespaceUri: 'urn:x', processId: 'Called_Process' } }));
    const config = inspector(graph);
    fixtures({
      processDefinitionListRequest: { definitions: [], hasMore: false },
      processVersionListRequest: { versions: [{ version: 2 }], hasMore: false },
      processDefinitionGetRequest: ({ definitionId: asked }) => {
        if (asked === goneId) throw new Error('process definition not found');
        return { definition: { definitionId: asked, name: asked === archivedId ? 'Old approvals' : 'Live approvals', archived: asked === archivedId, publishedVersion: 2 } };
      },
      processVersionGetRequest: () => ({ version: { model: called }, startCatalog: [] }),
      ...extra,
    });
    const call = graph.nodes.find((node) => node.id === 'Call_1');
    config.show(call, graph.templates.get(call.type));
    await flush(4);
    const warning = config.root.querySelector('[data-call-target-warning]');
    const shown = config.root.querySelector('[data-process="calledDefinitionChoice"] .tf-select-selected').textContent;
    graph.destroy();
    return { warning, shown, body: config.root.querySelector('[data-process="calledProcessChoice"] .tf-select-selected').textContent };
  };
  const archived = await open(archivedId);
  assert.equal(archived.shown, 'Old approvals');
  assert.equal(archived.warning.hidden, false);
  assert.equal(archived.warning.getAttribute('message'), I18n.t('bpmn.call_target_archived'));
  assert.equal(archived.body, 'Invoice approval', 'the called body is named too');
  const gone = await open(goneId);
  assert.equal(gone.shown, I18n.t('bpmn.call_definition_unavailable'));
  assert.equal(gone.warning.getAttribute('message'), I18n.t('bpmn.call_target_missing'));
  assert.doesNotMatch(gone.shown, /bbbbbbbb/);
  const live = await open(liveId);
  assert.equal(live.shown, 'Live approvals');
  assert.equal(live.warning.hidden, true, 'a callable target raises no warning');
});

test('choosing a version while its list is still loading keeps that list', async () => {
  const definitionId = 'dddddddd-0000-4000-8000-00000000000d';
  const called = emptyProcessModel(); called.processId = 'Called_Process';
  const graph = canvas(callerModel({ calledDefinitionId: definitionId, calledVersion: 7, calledElement: { namespaceUri: 'urn:x', processId: 'Called_Process' } }));
  const config = inspector(graph);
  const versions = deferred();
  const listed = () => calls.filter((call) => call.kind === 'processVersionListRequest').length;
  fixtures({
    processDefinitionListRequest: { definitions: [{ definitionId, name: 'Reviews', publishedVersion: 8 }], hasMore: false },
    processDefinitionGetRequest: { definition: { definitionId, name: 'Reviews', archived: false, publishedVersion: 8 } },
    processVersionListRequest: () => versions.promise,
    processVersionGetRequest: { version: { model: called }, startCatalog: [] },
  });
  const call = graph.nodes.find((node) => node.id === 'Call_1');
  config.show(call, graph.templates.get(call.type));
  await flush(2);
  const choice = config.root.querySelector('[data-process="calledVersionChoice"]');
  assert.equal(listed(), 1, 'the versions of the saved definition are requested');
  change(choice, '7');
  await flush(2);
  versions.resolve({ versions: [{ version: 8 }, { version: 7 }], hasMore: false });
  await flush(3);
  assert.deepEqual([...choice.querySelectorAll('option')].map((option) => option.value).filter(Boolean).sort(), ['7', '8'],
    'the answer for the same definition is not thrown away because the version was picked meanwhile');
  graph.destroy();
});

test('pickers of the process settings and the boundary inspector show names, never ids', async () => {
  const state = await mount(definition('picker-names'));
  state.canvas.addProcessBody();
  state.canvas.addProcessBody();
  state.canvas.updateSelectedProcessName('Billing');
  builder._syncProcessBodySelect();
  const bodies = [...state.root.querySelectorAll('[data-role="body-select"] option')].map((option) => option.textContent);
  assert.deepEqual(bodies, ['Process 1', 'Process 2', 'Billing']);
  for (const text of bodies) assert.doesNotMatch(text, /Process_[0-9a-f_]{8,}/);

  const graph = canvas();
  const config = inspector(graph);
  const reviewer = (x) => { graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_user_task'), x, 300); const node = graph.nodes.at(-1); graph.updateNodeLabel(node.id, 'Review'); return node; };
  const first = reviewer(300); const second = reviewer(700);
  const unnamed = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 1100, 300);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer'), first.x + first.width / 2, first.y + first.height);
  const timer = graph.nodes.at(-1);
  config.show(timer, graph.templates.get(timer.type));
  await flush(2);
  const options = [...config.root.querySelectorAll('[data-process="attachedToId"] option')].filter((option) => option.value);
  assert.deepEqual(options.map((option) => option.textContent), ['Review (1)', 'Review (2)', graph.templates.get('bpmn_service_task').label]);
  assert.deepEqual(options.map((option) => option.value), [first.id, second.id, unnamed.id]);
  for (const option of options) assert.equal(option.textContent.includes(option.value), false, 'the id is the value, never the text');
  graph.updateNodeConfig(timer.id, { attachedToId: 'gone' });
  config.show(timer, graph.templates.get(timer.type));
  await flush(2);
  const stale = config.root.querySelector('[data-process="attachedToId"] option[disabled]');
  assert.equal(stale.textContent, I18n.t('bpmn.boundary_unavailable_option'));
  graph.destroy();
});

test('error end remains terminal and called process links disclose only authorized related instances', async () => {
  const model = emptyProcessModel();
  model.errors = [{ errorId: 'Error_Business', name: 'Business rejection', errorCode: 'BUSINESS.REJECTED' }];
  model.nodes[1].kind = { ErrorEnd: { errorRef: 'Error_Business' } };
  const graph = canvas(model);
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="End"] .fb-port-out'), null);
  assert.equal(graph.connectNodes('End', 'Start'), false);
  graph.destroy();
  const current = instance('caller', { status: 'Error', terminalError: { errorRef: 'Error_Business', errorCode: 'BUSINESS.REJECTED',
    sourceEventId: 'source-event', sourceNodeId: 'End', sourceScopeId: 'caller' },
  calls: [
    { Outgoing: { callNodeId: 'Call_1', callNodeName: 'Review order', status: 'Returned', child: {
      instanceId: 'child', definitionName: 'Published review', version: 7, status: 'Completed', canOpen: true,
    } } },
    { Incoming: { parent: null } },
  ] });
  const win = await monitor(current);
  assert.equal(win.querySelectorAll('[data-call-rows] .fb-process-work').length, 2);
  assert.match(win.querySelector('[data-terminal-error]').textContent, /BUSINESS\.REJECTED/);
  assert.equal(win.querySelectorAll('[data-open-related]').length, 1);
  assert.doesNotMatch(win.querySelector('[data-call-rows] .fb-process-work:last-child').textContent, /Call_1|caller/);
  click(win.querySelector('[data-open-related]'));
  await flush(2);
  assert.ok(calls.some((entry) => entry.kind === 'processInstanceGetRequest' && entry.payload.instanceId === 'child'));
});

test('terminate end is a unit terminal in the canvas and preserves the full model', () => {
  const model = emptyProcessModel();
  model.nodes[1].kind = 'TerminateEnd';
  model.nodes[1].name = 'Stop all active work';
  const graph = canvas(model);
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="End"] .fb-port-out'), null);
  assert.equal(graph.connectNodes('End', 'Start'), false);
  const config = inspector(graph);
  config.show(graph.nodes.find((node) => node.id === 'End'), graph.templates.get('bpmn_terminate_end'));
  assert.equal(config.root.querySelector('[data-connect]'), null);
  assert.match(config.root.textContent, new RegExp(I18n.t('bpmn.node_terminate_end_hint')));
  const saved = canvasToProcess(model, graph.nodes, graph.edges, () => [], []);
  assert.equal(saved.nodes[1].kind, 'TerminateEnd');
  assert.equal(saved.nodes[1].name, 'Stop all active work');
  graph.destroy(); config.destroy();
});

test('all five locales distinguish termination facts from ordinary completion', async () => {
  const events = [
    { kind: 'terminate_end_reached', nodeName: 'Stop work', data: {
      source_instance_id: 'instance-1', source_event_id: 'event-1', source_token_id: 'token-1',
      source_scope_id: 'instance-1', source_node_id: 'Stop_1', terminated_scope_id: 'instance-1',
    } },
    { kind: 'instance_completed', nodeName: 'Stop work', data: { reason: 'terminate_end' } },
    { kind: 'cancelled', nodeName: 'Called review', data: { reason: 'terminate_end' } },
    { kind: 'scope_completed', nodeName: 'Stop work', data: { reason: 'terminate_end', subprocess_node_id: 'Sub_1' } },
    { kind: 'call_request_cancelled', nodeName: 'Review', data: { reason: 'terminate_end' } },
    { kind: 'subscription_cancelled', nodeName: 'Wait', data: { reason: 'terminate_end' } },
  ];
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    assert.notEqual(processLifecycleReasonText('terminate_end'), 'terminate_end');
    for (const event of events) {
      const rendered = processEventText(event);
      assert.doesNotMatch(rendered, /bpmn\.event_|bpmn\.reason_|\{(?:node|reason|element)\}/);
      assert.ok(rendered.length > 12);
    }
    assert.notEqual(processEventText(events[1]), processEventText({ kind: 'instance_completed', nodeName: 'Stop work', data: {} }));
    const history = events.map((event, index) => ({ ...event, seq: index + 1, atMs: 1000 + index }));
    const win = await monitor(instance(`terminate-${language}`), {
      processHistoryRequest: { events: history, nextSeq: history.length, hasMore: false },
    });
    assert.match(win.querySelector('[data-process-seq="1"] tf-code-editor').value, /event-1/);
    assert.match(win.querySelector('[data-process-seq="5"]').textContent, new RegExp(I18n.t('bpmn.reason_terminate_end')));
    win.remove();
  }
  await I18n.setLanguage('en');
});

test('all five locales render factual call and error history without exposing translation keys', async () => {
  const events = [
    { kind: 'call_requested', nodeName: 'Review', data: { call_id: 'call-1', parent_token_id: 'wait-1' } },
    { kind: 'call_entered', nodeName: 'Review', data: { call_id: 'call-1', child_instance_id: 'child-1', called_version: 7 } },
    { kind: 'call_returned', nodeName: 'Review', data: { call_id: 'call-1', child_instance_id: 'child-1' } },
    { kind: 'call_cancelled', nodeName: 'Review', data: { call_id: 'call-1', reason: 'call_interrupted' } },
    { kind: 'call_error_propagated', nodeName: 'Review', data: { error_code: 'ORDER.REJECTED', source_kind: 'error_end' } },
    { kind: 'call_error_propagated', nodeName: 'Review', data: { error_code: 'ORDER.REJECTED', source_kind: 'contract' } },
    { kind: 'error_end_reached', nodeName: 'Rejected', data: { error_code: 'ORDER.REJECTED', source_node_id: 'ErrorEnd_1' } },
    { kind: 'instance_error', nodeName: 'Rejected', data: { error_code: 'ORDER.REJECTED' } },
  ];
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    assert.doesNotMatch(processStatusLabel('Error'), /bpmn\.status_error/);
    assert.notEqual(processStatusLabel('Error'), processStatusLabel('Completed'));
    for (const event of events) {
      const rendered = processEventText(event);
      assert.doesNotMatch(rendered, /bpmn\.event_|\{(?:node|reason|version|child|code|source)\}/);
      assert.match(rendered, /Review|Rejected/);
    }
    const history = events.map((event, index) => ({ ...event, seq: index + 1, atMs: 1000 + index }));
    const win = await monitor(instance(`call-history-${language}`), {
      processHistoryRequest: { events: history, nextSeq: history.length, hasMore: false },
    });
    const descriptions = [...win.querySelectorAll('[data-process-seq] > div')].map((element) => element.textContent);
    assert.match(descriptions[4], new RegExp(I18n.t('bpmn.node_error_end')));
    assert.match(descriptions[5], new RegExp(I18n.t('bpmn.node_service_task')));
    for (const description of descriptions.slice(4, 6)) {
      assert.match(description, /ORDER\.REJECTED/);
      assert.doesNotMatch(description, /error_end|\bcontract\b/);
    }
    win.remove();
    assert.notEqual(processLifecycleReasonText('child_cancelled'), 'child_cancelled');
    assert.equal(processLifecycleReasonText('CUSTOM_ERROR_<raw>'), 'CUSTOM_ERROR_<raw>');
  }
  await I18n.setLanguage('en');
});

test('sequential message target and TTL edits survive Save and the official process encoder', async () => {
  const model = emptyProcessModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.nodes.splice(1, 0, { id: 'Throw_1', name: 'Send order', kind: { MessageThrow: {
    messageRef: 'Message_1', target: { Start: { definitionId: 'old-main-definition' } },
    correlationExpression: 'vars.order_ID', payloadExpression: 'vars.payload', ttlSeconds: 60,
  } } });
  const current = definition('throw-ttl', { model });
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({
    definition: { ...current, model: payload.model, draftRevision: 5 },
  }) });
  state.canvas.selectNode('Throw_1');
  await flush(2);
  change(state.config.root.querySelector('[data-process="targetType"]'), 'Catch');
  change(state.config.root.querySelector('[data-process="targetDefinitionId"]'), 'receiver-definition');
  change(state.config.root.querySelector('[data-process="instanceIdExpression"]'), 'vars.receiver_instance_ID');
  change(state.config.root.querySelector('[data-process="subscriptionIdExpression"]'), 'vars.receiver_subscription_ID');
  const ttl = state.config.root.querySelector('[data-process="ttlSeconds"]');
  change(ttl, '');
  assert.equal(state.canvas.getData().nodes.find((node) => node.id === 'Throw_1').kind.MessageThrow.ttlSeconds, 0);
  change(ttl, '1.5');
  assert.equal(state.canvas.getData().nodes.find((node) => node.id === 'Throw_1').kind.MessageThrow.ttlSeconds, 1.5);
  change(ttl, '240');
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload;
  const savedMessage = saved.model.nodes.find((node) => node.id === 'Throw_1').kind.MessageThrow;
  assert.deepEqual(savedMessage.target, { Catch: { definitionId: 'receiver-definition',
    instanceIdExpression: 'vars.receiver_instance_ID', subscriptionIdExpression: 'vars.receiver_subscription_ID' } });
  const savedTtl = savedMessage.ttlSeconds;
  assert.equal(savedTtl, 240);
  assert.equal(typeof savedTtl, 'number');
  assert.ok(encode.processDefinitionSaveRequest(17, saved).byteLength > 0);
});

test('distinct send and receive task inspectors save exact message fields and disclose local admission', async () => {
  const model = emptyProcessModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.nodes.splice(1, 0,
    { id: 'Send_1', name: 'Admit locally', kind: { SendTask: { messageRef: 'Message_1',
      target: { Start: { definitionId: '' } }, correlationExpression: 'vars.case_key',
      payloadExpression: 'vars.payload', ttlSeconds: 60 } } },
    { id: 'Receive_1', name: 'Receive later', kind: { ReceiveTask: { messageRef: 'Message_1',
      correlationExpression: 'vars.case_key', outputMapping: {} } } });
  const current = definition('send-receive-draft', { model });
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({
    definition: { ...current, model: payload.model, draftRevision: 5 },
  }) });
  state.canvas.selectNode('Send_1');
  await flush(2);
  assert.ok(state.config.root.querySelector('[data-process="repeatMode"]'));
  assert.ok(state.config.root.querySelector('[data-process="targetType"]'));
  assert.ok(state.config.root.textContent.includes(I18n.t('bpmn.node_send_task_hint')));
  change(state.config.root.querySelector('[data-process="targetDefinitionId"]'), 'target-definition');
  change(state.config.root.querySelector('[data-process="ttlSeconds"]'), '240');
  state.canvas.selectNode('Receive_1');
  await flush(2);
  assert.ok(state.config.root.querySelector('[data-process="messageRef"]'));
  assert.ok(state.config.root.querySelector('[data-process="outputMapping"]'));
  assert.equal(state.config.root.querySelector('[data-process="targetType"]'), null);
  assert.ok(state.config.root.querySelector('[data-process="repeatMode"]'));
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload;
  assert.deepEqual(saved.model.nodes.find((node) => node.id === 'Send_1').kind.SendTask, {
    messageRef: 'Message_1', target: { Start: { definitionId: 'target-definition' } },
    correlationExpression: 'vars.case_key', payloadExpression: 'vars.payload', ttlSeconds: 240,
  });
  assert.deepEqual(saved.model.nodes.find((node) => node.id === 'Receive_1').kind.ReceiveTask, {
    messageRef: 'Message_1', correlationExpression: 'vars.case_key', outputMapping: {},
  });
  const readonly = inspector(state.canvas, true);
  readonly.show(state.canvas.nodes.find((node) => node.id === 'Receive_1'),
    state.canvas.templates.get('bpmn_receive_task'));
  assert.equal(readonly.root.querySelector('[data-process="correlationExpression"]').hasAttribute('disabled'), true);
  readonly.destroy();
  assert.ok(encode.processDefinitionSaveRequest(17, saved).byteLength > 0);
});

test('root Send timer and message boundaries save, publish and retain their pinned read-only version', async () => {
  const model = emptyProcessModel();
  const namePrefix = 'Admit <&> Żółć ';
  const sendName = namePrefix + 'A'.repeat(256 - Buffer.byteLength(namePrefix));
  assert.equal(Buffer.byteLength(sendName), 256);
  model.timerTimezone = 'Europe/Warsaw';
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.variables = { case_key: 'case-1', payload: { opaque_key: 'R&D <Łódź>' } };
  model.nodes.splice(1, 0,
    { id: 'Send_1', name: sendName, kind: { SendTask: { messageRef: 'Message_1',
      target: { Start: { definitionId: 'target-definition' } }, correlationExpression: 'vars.case_key',
      payloadExpression: 'vars.payload', ttlSeconds: 60 } } },
    { id: 'Send_Timer', name: 'Deadline', kind: { BoundaryTimer: { attachedToId: 'Send_1',
      cancelActivity: true, timer: { Duration: { seconds: 60 } } } } },
    { id: 'Send_Message', name: 'Reply', kind: { BoundaryMessage: { attachedToId: 'Send_1',
      cancelActivity: false, messageRef: 'Message_1', correlationExpression: 'vars.case_key', outputMapping: {} } } });
  model.sequenceFlows = [
    { id: 'Flow_Start', sourceId: 'Start', targetId: 'Send_1', condition: null },
    { id: 'Flow_Send', sourceId: 'Send_1', targetId: 'End', condition: null },
    { id: 'Flow_Timer', sourceId: 'Send_Timer', targetId: 'End', condition: null },
    { id: 'Flow_Message', sourceId: 'Send_Message', targetId: 'End', condition: null },
  ];
  const current = definition('send-boundary', { model });
  let published = 0;
  let pinned;
  const state = await mount(current, {
    processDefinitionSaveRequest: ({ model: saved }) => ({ definition: { ...current, model: saved, draftRevision: 5 } }),
    processDefinitionPublishRequest: () => {
      published += 1;
      const version = { version: published, model: structuredClone(state.canvas.getData()) };
      if (published === 1) pinned = structuredClone(version.model);
      return { definition: { ...current, publishedVersion: published, draftRevision: 5 }, version };
    },
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: () => ({ version: { version: 1, model: structuredClone(pinned) }, startCatalog: [] }),
  });
  for (const [nodeId, kind] of [['Send_Timer', 'BoundaryTimer'], ['Send_Message', 'BoundaryMessage']]) {
    state.canvas.selectNode(nodeId); await flush(2);
    const select = state.config.root.querySelector('[data-process="attachedToId"]');
    assert.equal(select.value, 'Send_1');
    assert.equal(select.querySelector('option[value="Send_1"]').textContent.includes(sendName), true);
    assert.equal(state.canvas.getData().nodes.find((node) => node.id === nodeId).kind[kind].attachedToId, 'Send_1');
  }
  assert.deepEqual(state.canvas.validate(), []);
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(published, 1);
  assert.equal(pinned.nodes.find((node) => node.id === 'Send_1').name, sendName);
  assert.deepEqual(pinned.nodes.find((node) => node.id === 'Send_Timer').kind.BoundaryTimer,
    model.nodes.find((node) => node.id === 'Send_Timer').kind.BoundaryTimer);
  assert.deepEqual(pinned.nodes.find((node) => node.id === 'Send_Message').kind.BoundaryMessage,
    model.nodes.find((node) => node.id === 'Send_Message').kind.BoundaryMessage);
  state.canvas.updateNodeLabel('Send_1', 'Revised Send');
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(published, 2);
  await builder._openProcessVersions();
  click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
  await flush(2);
  assert.equal(state.previewVersion, 1);
  assert.equal(state.canvas.readOnly, true);
  assert.deepEqual(state.canvas.getData(), pinned);
  state.canvas.selectNode('Send_Message'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="attachedToId"]').hasAttribute('disabled'), true);
  assert.equal(state.config.root.querySelector('[data-process="messageRef"]').hasAttribute('disabled'), true);
});

test('duplicated Send boundary clears sibling overlap and selects the clone inspector', async () => {
  const model = emptyProcessModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.timerTimezone = 'Europe/Warsaw';
  model.nodes.splice(1, 0,
    { id: 'Send_1', name: 'Admit <&>', kind: { SendTask: { messageRef: 'Message_1',
      target: { Start: { definitionId: 'target-definition' } },
      correlationExpression: 'vars.case_key', payloadExpression: 'vars.payload', ttlSeconds: 60 } } },
    { id: 'Timer_1', name: 'Deadline', kind: { BoundaryTimer: {
      attachedToId: 'Send_1', cancelActivity: true, timer: { Duration: { seconds: 60 } },
    } } },
    { id: 'Message_1', name: 'Reply', kind: { BoundaryMessage: {
      attachedToId: 'Send_1', cancelActivity: false, messageRef: 'Message_1',
      correlationExpression: 'vars.case_key', outputMapping: {},
    } } });
  model.sequenceFlows = [
    { id: 'Flow_Start', sourceId: 'Start', targetId: 'Send_1', condition: null },
    { id: 'Flow_Send', sourceId: 'Send_1', targetId: 'End', condition: null },
    { id: 'Flow_Timer', sourceId: 'Timer_1', targetId: 'End', condition: null },
    { id: 'Flow_Message', sourceId: 'Message_1', targetId: 'End', condition: null },
  ];
  model.diagram.shapes.push(
    { elementId: 'Send_1', x: 240, y: 160, width: 240, height: 96 },
    { elementId: 'Timer_1', x: 420, y: 230, width: 56, height: 56 },
    { elementId: 'Message_1', x: 420, y: 190, width: 56, height: 56 },
  );
  const state = await mount(definition('boundary-clone-selection', { model }));
  const timer = state.canvas.nodes.find((node) => node.id === 'Timer_1');
  const original = state.canvas.nodes.find((node) => node.id === 'Message_1');
  const parent = state.canvas.nodes.find((node) => node.id === 'Send_1');
  state.canvas.selectNode('Message_1');
  assert.equal(state.config.node.id, 'Message_1');
  state.canvas.duplicateNodes(['Message_1']);
  const clone = state.canvas.nodes.find((node) => state.canvas.selectedIds.has(node.id));
  assert.ok(clone && clone.id !== 'Message_1');
  assert.equal(clone.config.attachedToId, 'Send_1');
  const center = (node) => [node.x + node.width / 2, node.y + node.height / 2];
  const [cloneX, cloneY] = center(clone);
  assert.ok(cloneX === parent.x || cloneX === parent.x + parent.width
    || cloneY === parent.y || cloneY === parent.y + parent.height);
  for (const neighbor of [original, timer]) {
    const [neighborX, neighborY] = center(neighbor);
    assert.ok(Math.hypot(cloneX - neighborX, cloneY - neighborY) >= clone.width);
  }
  assert.equal(state.config.node.id, clone.id);
  assert.equal(state.config.root.querySelector('.fb-config-title').textContent, clone.label);
  click(state.config.root.querySelector('[data-process="delete"]'));
  assert.equal(state.canvas.nodes.some((node) => node.id === clone.id), false);
  assert.equal(state.canvas.nodes.some((node) => node.id === 'Message_1'), true);
});

test('embedded Send timer and message attachments survive authoring and pinned read-only versions', async () => {
  const model = embeddedModel();
  const namePrefix = 'Embedded admission <&> Żółć ';
  const sendName = namePrefix + 'A'.repeat(256 - Buffer.byteLength(namePrefix));
  assert.equal(Buffer.byteLength(sendName), 256);
  model.timerTimezone = 'Europe/Warsaw';
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.variables = { source_ID: { business_key: 'kept' } };
  const body = model.nodes.find((node) => node.id === 'Scope_Review').kind.SubProcess.body;
  body.nodes[1] = { id: 'Local_Review', name: sendName, kind: { SendTask: {
    messageRef: 'Message_1', target: { Start: { definitionId: 'target-definition' } },
    correlationExpression: 'vars.local_ID.business_key', payloadExpression: 'vars.local_ID', ttlSeconds: 60,
  } } };
  body.nodes.push({ id: 'Local_Timer', name: 'Deadline', kind: { BoundaryTimer: {
    attachedToId: 'Local_Review', cancelActivity: true, timer: { Duration: { seconds: 60 } },
  } } }, { id: 'Local_Message', name: 'Reply', kind: { BoundaryMessage: {
    attachedToId: 'Local_Review', cancelActivity: false, messageRef: 'Message_1',
    correlationExpression: 'vars.local_ID.business_key', outputMapping: {},
  } } });
  body.sequenceFlows.push({ id: 'Local_Timer_Flow', sourceId: 'Local_Timer', targetId: 'Local_End', condition: null },
    { id: 'Local_Message_Flow', sourceId: 'Local_Message', targetId: 'Local_End', condition: null });
  body.diagram.shapes.push({ elementId: 'Local_Timer', x: 380, y: 218, width: 56, height: 56 },
    { elementId: 'Local_Message', x: 180, y: 218, width: 56, height: 56 });
  body.diagram.edges.push({ sequenceFlowId: 'Local_Timer_Flow',
    waypoints: [{ x: 408, y: 246 }, { x: 500, y: 188 }] },
    { sequenceFlowId: 'Local_Message_Flow', waypoints: [{ x: 208, y: 246 }, { x: 500, y: 188 }] });
  const current = definition('embedded-send-boundary', { model });
  let published = 0;
  let pinned;
  const state = await mount(current, {
    processDefinitionSaveRequest: ({ model: saved }) => ({ definition: { ...current, model: saved, draftRevision: 5 } }),
    processDefinitionPublishRequest: () => {
      published += 1;
      const version = { version: published, model: structuredClone(state.canvas.getData()) };
      if (published === 1) pinned = structuredClone(version.model);
      return { definition: { ...current, publishedVersion: published, draftRevision: 5 }, version };
    },
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: () => ({ version: { version: 1, model: structuredClone(pinned) }, startCatalog: [] }),
  });
  state.canvas.selectNode('Scope_Review'); await flush(2);
  click(state.config.root.querySelector('[data-process-enter]'));
  assert.deepEqual(state.canvas.processPath, ['Scope_Review']);
  for (const [nodeId, kind] of [['Local_Timer', 'BoundaryTimer'], ['Local_Message', 'BoundaryMessage']]) {
    state.canvas.selectNode(nodeId); await flush(2);
    const attachment = state.config.root.querySelector('[data-process="attachedToId"]');
    assert.equal(attachment.value, 'Local_Review');
    assert.equal(attachment.querySelector('option[value="Local_Review"]').disabled, false);
    assert.equal(attachment.querySelector('option[value="Local_Review"]').textContent.includes(sendName), true);
    assert.equal(state.canvas.getData().nodes[1].kind.SubProcess.body.nodes.find((node) => node.id === nodeId)
      .kind[kind].attachedToId, 'Local_Review');
  }
  assert.deepEqual(state.canvas.validate(), []);
  click(state.root.querySelector('[data-role="scope-up"]'));
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(published, 1);
  assert.deepEqual(pinned.nodes[1].kind.SubProcess.body, body);
  state.canvas.navigateProcessBody(['Scope_Review']);
  state.canvas.updateNodeLabel('Local_Review', 'Revised admission');
  state.canvas.navigateProcessBody([]);
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(published, 2);
  await builder._openProcessVersions();
  click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
  await flush(2);
  assert.equal(state.previewVersion, 1);
  assert.equal(state.canvas.readOnly, true);
  assert.deepEqual(state.canvas.getData(), pinned);
  state.canvas.navigateProcessBody(['Scope_Review']);
  state.canvas.selectNode('Local_Message'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="attachedToId"]').hasAttribute('disabled'), true);
  assert.equal(state.config.root.querySelector('[data-process="messageRef"]').hasAttribute('disabled'), true);
});

test('embedded manual and receive boundaries save, publish and retain their pinned read-only task details', async () => {
  for (const kind of ['ManualTask', 'ReceiveTask']) {
    const model = embeddedModel();
    const namePrefix = `${kind} <&> Żółć `;
    const taskName = namePrefix + 'A'.repeat(256 - Buffer.byteLength(namePrefix));
    assert.equal(Buffer.byteLength(taskName), 256);
    model.timerTimezone = 'Europe/Warsaw';
    model.messages = [{ messageId: 'Message_1', name: 'work.received' }];
    model.variables = { source_ID: { business_key: 'case-1' } };
    const body = model.nodes.find((node) => node.id === 'Scope_Review').kind.SubProcess.body;
    body.variables = { local_ID: { business_key: 'case-1' }, received_payload: null, boundary_payload: null };
    const task = kind === 'ManualTask'
      ? { assigneeUserId: 'anna', instructions: 'Complete the external work, then acknowledge it <&> Żółć.' }
      : { messageRef: 'Message_1', correlationExpression: 'vars.local_ID.business_key',
        outputMapping: { received_payload: 'outputs' } };
    body.nodes[1] = { id: 'Local_Review', name: taskName, kind: { [kind]: task } };
    body.nodes.push(
      { id: 'Local_Timer', name: 'Deadline', kind: { BoundaryTimer: {
        attachedToId: 'Local_Review', cancelActivity: true, timer: { Duration: { seconds: 60 } },
      } } },
      { id: 'Local_Message', name: 'Alternative', kind: { BoundaryMessage: {
        attachedToId: 'Local_Review', cancelActivity: false, messageRef: 'Message_1',
        correlationExpression: 'vars.local_ID.business_key',
        outputMapping: { boundary_payload: 'outputs' },
      } } },
    );
    body.sequenceFlows.push(
      { id: 'Local_Timer_Flow', sourceId: 'Local_Timer', targetId: 'Local_End', condition: null },
      { id: 'Local_Message_Flow', sourceId: 'Local_Message', targetId: 'Local_End', condition: null },
    );
    body.diagram.shapes.push(
      { elementId: 'Local_Timer', x: 380, y: 218, width: 56, height: 56 },
      { elementId: 'Local_Message', x: 180, y: 218, width: 56, height: 56 },
    );
    body.diagram.edges.push(
      { sequenceFlowId: 'Local_Timer_Flow', waypoints: [{ x: 408, y: 246 }, { x: 500, y: 188 }] },
      { sequenceFlowId: 'Local_Message_Flow', waypoints: [{ x: 208, y: 246 }, { x: 500, y: 188 }] },
    );
    const current = definition(`embedded-${kind}-boundary`, { model });
    let published = 0;
    let pinned;
    const state = await mount(current, {
      processDefinitionSaveRequest: ({ model: saved }) => ({
        definition: { ...current, model: saved, draftRevision: 5 },
      }),
      processDefinitionPublishRequest: () => {
        published += 1;
        const version = { version: published, model: structuredClone(state.canvas.getData()) };
        if (published === 1) pinned = structuredClone(version.model);
        return { definition: { ...current, publishedVersion: published, draftRevision: 5 }, version };
      },
      processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
      processVersionGetRequest: () => ({ version: { version: 1, model: structuredClone(pinned) }, startCatalog: [] }),
    });
    state.canvas.selectNode('Scope_Review'); await flush(2);
    click(state.config.root.querySelector('[data-process-enter]'));
    assert.deepEqual(state.canvas.processPath, ['Scope_Review']);
    for (const [nodeId, boundaryKind] of [['Local_Timer', 'BoundaryTimer'], ['Local_Message', 'BoundaryMessage']]) {
      state.canvas.selectNode(nodeId); await flush(2);
      const attachment = state.config.root.querySelector('[data-process="attachedToId"]');
      assert.equal(attachment.value, 'Local_Review');
      assert.equal(attachment.querySelector('option[value="Local_Review"]').disabled, false);
      assert.equal(attachment.querySelector('option[value="Local_Review"]').textContent.includes(taskName), true);
      assert.equal(state.canvas.getData().nodes[1].kind.SubProcess.body.nodes.find((node) => node.id === nodeId)
        .kind[boundaryKind].attachedToId, 'Local_Review');
    }
    assert.deepEqual(state.canvas.validate(), []);
    click(state.root.querySelector('[data-role="scope-up"]'));
    assert.equal(await builder._save(), true);
    await builder._publish();
    assert.equal(published, 1);
    assert.deepEqual(pinned.nodes[1].kind.SubProcess.body, body);
    state.canvas.navigateProcessBody(['Scope_Review']);
    state.canvas.updateNodeLabel('Local_Review', 'Revised external wait');
    state.canvas.navigateProcessBody([]);
    assert.equal(await builder._save(), true);
    await builder._publish();
    assert.equal(published, 2);
    await builder._openProcessVersions();
    click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
    await flush(2);
    assert.equal(state.previewVersion, 1);
    assert.equal(state.canvas.readOnly, true);
    assert.deepEqual(state.canvas.getData(), pinned);
    state.canvas.navigateProcessBody(['Scope_Review']);
    state.canvas.selectNode('Local_Review'); await flush(2);
    const detail = state.config.root.querySelector(`[data-process="${kind === 'ManualTask' ? 'instructions' : 'correlationExpression'}"]`);
    assert.equal(detail.hasAttribute('disabled'), true);
    assert.equal(detail.value, kind === 'ManualTask' ? task.instructions : task.correlationExpression);
    state.canvas.selectNode('Local_Message'); await flush(2);
    assert.equal(state.config.root.querySelector('[data-process="attachedToId"]').hasAttribute('disabled'), true);
    assert.equal(state.config.root.querySelector('[data-process="messageRef"]').hasAttribute('disabled'), true);
  }
});

test('event gateway receive and signal alternatives save, publish and retain pinned read-only configuration', async () => {
  const model = emptyProcessModel();
  const namePrefix = 'Receive <&> Żółć ';
  const receiveName = namePrefix + 'A'.repeat(256 - Buffer.byteLength(namePrefix));
  assert.equal(Buffer.byteLength(receiveName), 256);
  model.targetNamespace = 'urn:orders';
  model.timerTimezone = 'Europe/Warsaw';
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.signals = [{ signalId: 'Signal_1', namespaceUri: 'urn:orders', name: 'Order changed' }];
  model.variables = { case_key: 'case-1', received: null };
  model.nodes.splice(1, 0,
    { id: 'Race_1', name: 'First arrival', kind: 'EventBasedGateway' },
    { id: 'Receive_1', name: receiveName, kind: { ReceiveTask: {
      messageRef: 'Message_1', correlationExpression: 'vars.case_key',
      outputMapping: { received: 'outputs' },
    } } },
    { id: 'Signal_1_Catch', name: 'Signal alternative', kind: { SignalCatch: {
      signalRef: 'Signal_1', outputMapping: {},
    } } },
    { id: 'Timer_1', name: 'Timeout', kind: { TimerCatch: { timer: { Duration: { seconds: 60 } } } } });
  model.sequenceFlows = [
    { id: 'Start_Race', sourceId: 'Start', targetId: 'Race_1', condition: null },
    { id: 'To_Receive', sourceId: 'Race_1', targetId: 'Receive_1', condition: null },
    { id: 'To_Signal', sourceId: 'Race_1', targetId: 'Signal_1_Catch', condition: null },
    { id: 'To_Timer', sourceId: 'Race_1', targetId: 'Timer_1', condition: null },
    { id: 'From_Receive', sourceId: 'Receive_1', targetId: 'End', condition: null },
    { id: 'From_Signal', sourceId: 'Signal_1_Catch', targetId: 'End', condition: null },
    { id: 'From_Timer', sourceId: 'Timer_1', targetId: 'End', condition: null },
  ];
  model.diagram.shapes.splice(1, 0,
    { elementId: 'Race_1', x: 180, y: 160, width: 72, height: 72 },
    { elementId: 'Receive_1', x: 320, y: 100, width: 240, height: 96 },
    { elementId: 'Signal_1_Catch', x: 320, y: 220, width: 56, height: 56 },
    { elementId: 'Timer_1', x: 420, y: 220, width: 56, height: 56 });
  model.diagram.edges = model.sequenceFlows.map((flow) => ({
    sequenceFlowId: flow.id, waypoints: [{ x: 120, y: 180 }, { x: 300, y: 180 }],
  }));
  const current = definition('event-receive-signal', { model });
  let published = 0;
  let pinned;
  const state = await mount(current, {
    processDefinitionSaveRequest: ({ model: saved }) => ({
      definition: { ...current, model: saved, draftRevision: 5 },
    }),
    processDefinitionPublishRequest: () => {
      published += 1;
      const version = { version: published, model: structuredClone(state.canvas.getData()) };
      if (published === 1) pinned = structuredClone(version.model);
      return { definition: { ...current, publishedVersion: published, draftRevision: 5 }, version };
    },
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: () => ({ version: { version: 1, model: structuredClone(pinned) }, startCatalog: [] }),
  });
  assert.deepEqual(state.canvas.validate(), []);
  state.canvas.selectNode('Receive_1'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="messageRef"]').value, 'Message_1');
  assert.equal(state.config.root.querySelector('[data-process="correlationExpression"]').value, 'vars.case_key');
  state.canvas.selectNode('Signal_1_Catch'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="signalRef"]').value, 'Signal_1');
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(published, 1);
  assert.equal(pinned.nodes.find((node) => node.id === 'Receive_1').name, receiveName);
  assert.deepEqual(pinned.diagram, model.diagram);

  const mixed = structuredClone(pinned);
  mixed.nodes.find((node) => node.id === 'Timer_1').kind = { MessageCatch: {
    messageRef: 'Message_1', correlationExpression: 'vars.case_key', outputMapping: {},
  } };
  state.canvas.setData(mixed);
  assert.deepEqual(state.canvas.validate(), [
    I18n.t('bpmn.event_gateway_receive_message_mix', { node: 'First arrival' }),
  ]);
  assert.equal(await builder._save(), false);
  assert.equal(published, 1);

  const attached = structuredClone(pinned);
  attached.nodes.push({ id: 'Receive_Boundary', name: 'Deadline', kind: { BoundaryTimer: {
    attachedToId: 'Receive_1', cancelActivity: true, timer: { Duration: { seconds: 30 } },
  } } });
  attached.sequenceFlows.push({ id: 'Boundary_End', sourceId: 'Receive_Boundary', targetId: 'End', condition: null });
  state.canvas.setData(attached);
  state.canvas.selectNode('Receive_Boundary'); await flush(2);
  assert.equal(state.config.root.querySelector('option[value="Receive_1"]').disabled, true);
  assert.deepEqual(state.canvas.validate(), [
    I18n.t('bpmn.event_gateway_receive_boundary', { node: receiveName }),
  ]);
  assert.equal(await builder._save(), false);
  assert.equal(published, 1);

  const extra = structuredClone(pinned);
  extra.sequenceFlows.push({ id: 'Extra_Receive', sourceId: 'Signal_1_Catch', targetId: 'Receive_1', condition: null });
  state.canvas.setData(extra);
  assert.deepEqual(state.canvas.validate(), [
    I18n.t('bpmn.event_gateway_branch_incoming', { node: receiveName }),
    I18n.t('bpmn.event_gateway_branch_incoming', { node: 'Signal alternative' }),
  ]);
  assert.equal(await builder._save(), false);
  assert.equal(published, 1);

  state.canvas.setData(pinned);
  state.canvas.updateNodeLabel('Receive_1', 'Revised receive');
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(published, 2);
  await builder._openProcessVersions();
  click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
  await flush(2);
  assert.equal(state.previewVersion, 1);
  assert.equal(state.canvas.readOnly, true);
  assert.deepEqual(state.canvas.getData(), pinned);
  state.canvas.selectNode('Receive_1'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="messageRef"]').hasAttribute('disabled'), true);
  assert.equal(state.config.root.querySelector('[data-process="correlationExpression"]').value, 'vars.case_key');
  state.canvas.selectNode('Signal_1_Catch'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="signalRef"]').hasAttribute('disabled'), true);
});

test('signal throw and catch inspectors preserve exact declarations, expressions and source history', async () => {
  const model = emptyProcessModel();
  model.targetNamespace = 'urn:orders';
  model.signals = [{ signalId: 'Signal_1', namespaceUri: 'urn:orders', name: 'Order changed <&>' }];
  model.nodes.splice(1, 0,
    { id: 'Throw_1', name: 'Admit signal', kind: { SignalThrow: { signalRef: 'Signal_1',
      payloadExpression: 'vars.payload', ttlSeconds: 60 } } },
    { id: 'Catch_1', name: 'Wait for signal', kind: { SignalCatch: { signalRef: 'Signal_1',
      outputMapping: { received: 'outputs' } } } });
  const current = definition('signal-draft', { model });
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({
    definition: { ...current, model: payload.model, draftRevision: 5 },
  }) });
  state.canvas.selectNode('Throw_1'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="targetType"]'), null);
  assert.equal(state.config.root.querySelector('[data-process="signalRef"]').value, 'Signal_1');
  change(state.config.root.querySelector('[data-process="payloadExpression"]'), 'vars.payload');
  change(state.config.root.querySelector('[data-process="ttlSeconds"]'), '240');
  state.canvas.selectNode('Catch_1'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="signalRef"]').value, 'Signal_1');
  assert.ok(state.config.root.querySelector('[data-process="outputMapping"]'));
  assert.equal(state.config.root.querySelector('[data-process="correlationExpression"]'), null);
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model;
  assert.deepEqual(saved.signals, model.signals);
  assert.deepEqual(saved.nodes.find((node) => node.id === 'Throw_1').kind.SignalThrow,
    { signalRef: 'Signal_1', payloadExpression: 'vars.payload', ttlSeconds: 240 });
  assert.deepEqual(saved.nodes.find((node) => node.id === 'Catch_1').kind.SignalCatch,
    { signalRef: 'Signal_1', outputMapping: { received: 'outputs' } });
  for (const kind of ['signal_admitted', 'signal_catch_opened', 'signal_received']) {
    const label = processEventText({ kind, nodeName: 'Order changed <&>', data: {} });
    assert.match(label, /Order changed <&>/);
    assert.doesNotMatch(label, /recipient_count|subscription_id|signal_id/);
  }
  const win = await monitor(instance('signal-wait', { subscriptions: [{
    subscriptionId: 'subscription-private', nodeId: 'Catch_1', nodeName: 'Order changed <&>',
    tokenId: 'token-private', kind: 'SignalCatch', status: 'Open', revision: 1,
    signalName: 'Order changed <&>', correlationKey: null, scopeId: 'signal-wait',
  }] }));
  const subscription = win.querySelector('[data-subscription-rows]').textContent;
  assert.match(subscription, /Order changed <&>/);
  assert.equal(subscription.includes(I18n.t('bpmn.message_correlation_key')), false);
  win.remove();
});

test('Link throw and catch use reciprocal typed definitions without a drawn jump edge', async () => {
  const model = emptyProcessModel();
  model.nodes.splice(1, 0,
    { id: 'Throw_1', name: 'Again', kind: { LinkThrow: { definition: {
      id: 'Link_Throw', name: 'repeat', sourceRefs: [], targetRef: 'Link_Catch',
    } } } },
    { id: 'Catch_1', name: 'Resume', kind: { LinkCatch: { definition: {
      id: 'Link_Catch', name: 'repeat', sourceRefs: ['Link_Throw'], targetRef: null,
    } } } });
  model.sequenceFlows = [
    { id: 'Flow_Throw', sourceId: 'Start', targetId: 'Throw_1', condition: null },
    { id: 'Flow_End', sourceId: 'Catch_1', targetId: 'End', condition: null },
  ];
  const current = definition('link-draft', { model });
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({
    definition: { ...current, model: payload.model, draftRevision: 5 },
  }) });
  assert.equal(state.canvas.root.querySelector('[data-node-id="Throw_1"] use').getAttribute('href'), '#i-link-throw');
  assert.equal(state.canvas.root.querySelector('[data-node-id="Catch_1"] use').getAttribute('href'), '#i-link-catch');
  assert.match(state.canvas.root.querySelector('[data-node-id="Throw_1"] .fb-process-label').textContent, /Again · repeat/);
  assert.match(state.canvas.root.querySelector('[data-node-id="Catch_1"] .fb-process-label').textContent, /Resume · repeat/);
  const markerIds = [...state.canvas.svg.querySelectorAll('marker')].map((marker) => marker.id);
  assert.equal(markerIds.length, 1);
  const edgePaths = [...state.canvas.svg.querySelectorAll('.fb-edge-path')];
  assert.equal(edgePaths.length, 2);
  assert.equal(edgePaths.every((path) => path.getAttribute('marker-end') === `url(#${state.canvas._edgeMarkerId})`), true);
  assert.equal(new Set(edgePaths.map((path) => path.getAttribute('d'))).size, 2);
  state.canvas.selectEdge('Flow_Throw');
  const selectedPath = state.canvas.svg.querySelector('.fb-edge-path.selected');
  assert.equal(selectedPath.getAttribute('marker-end'), `url(#${state.canvas._edgeMarkerId})`);
  assert.equal(state.canvas.connectNodes('Throw_1', 'Catch_1'), false);
  assert.equal(state.canvas.connectNodes('Start', 'Catch_1'), false);
  assert.equal(state.canvas.getData().sequenceFlows.length, 2);
  state.canvas.selectNode('Throw_1'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="linkDefinitionId"]').value, 'Link_Throw');
  assert.equal(state.config.root.querySelector('[data-process="linkTargetRef"]').value, 'Link_Catch');
  change(state.config.root.querySelector('[data-process="linkName"]'), 'repeat');
  state.canvas.selectNode('Catch_1'); await flush(2);
  assert.equal(state.config.root.querySelector('[data-process="linkSourceRefs"]').value, 'Link_Throw');
  assert.equal(await builder._save(), true);
  const saved = calls.filter((call) => call.kind === 'processDefinitionSaveRequest').at(-1).payload.model;
  assert.deepEqual(saved.nodes.find((node) => node.id === 'Throw_1').kind.LinkThrow.definition,
    { id: 'Link_Throw', name: 'repeat', sourceRefs: [], targetRef: 'Link_Catch' });
  assert.deepEqual(saved.nodes.find((node) => node.id === 'Catch_1').kind.LinkCatch.definition,
    { id: 'Link_Catch', name: 'repeat', sourceRefs: ['Link_Throw'], targetRef: null });
  assert.equal(saved.sequenceFlows.length, 2);
  const cloned = cloneProcessBody(saved);
  const clonedThrow = cloned.nodes.find((node) => node.kind?.LinkThrow).kind.LinkThrow.definition;
  const clonedCatch = cloned.nodes.find((node) => node.kind?.LinkCatch).kind.LinkCatch.definition;
  assert.notEqual(clonedThrow.id, 'Link_Throw');
  assert.notEqual(clonedCatch.id, 'Link_Catch');
  assert.equal(clonedThrow.targetRef, clonedCatch.id);
  assert.deepEqual(clonedCatch.sourceRefs, [clonedThrow.id]);
  assert.match(processEventText({ kind: 'link_thrown', nodeName: 'Again',
    data: { catch_node_id: 'Catch_1' } }), /Again.*Catch_1/);
  assert.match(processEventText({ kind: 'link_caught', nodeName: 'Resume',
    data: { source_token_id: 'token_1' } }), /Resume.*token_1/);
});

test('signal history distinguishes admitted source from received delivery after zero-recipient completion', async () => {
  const nodeName = 'Signal <img src=x onerror="window.__bpmnSignalMarkupExecuted=true">';
  const sourceData = { signal_id: 'source-signal', source_activation_id: 'source-token',
    signal_namespace_uri: 'urn:orders', signal_declaration_id: 'Signal_1' };
  const receivedData = { signal_id: 'source-signal', subscription_id: 'recipient-subscription',
    attached_token_id: 'recipient-token', source_event_id: 'source-event' };
  const event = (seq, kind, data = {}) => ({ seq, atMs: 1000 + seq, kind, nodeName, data });
  const sourceEvents = [event(1, 'instance_started'), event(2, 'node_completed'),
    event(3, 'signal_admitted', sourceData), event(4, 'end_reached'), event(5, 'instance_completed')];
  const recipientEvents = [event(1, 'instance_started'), event(2, 'node_completed'),
    event(3, 'signal_catch_opened', { subscription_id: 'recipient-subscription', attached_token_id: 'recipient-token',
      signal_namespace_uri: 'urn:orders', signal_declaration_id: 'Signal_1' }),
    event(4, 'signal_received', receivedData), event(5, 'end_reached'), event(6, 'instance_completed')];
  const admittedLabels = { en: `${nodeName}: signal admitted`, pl: `${nodeName}: sygnał zapisany`,
    de: `${nodeName}: Signal angenommen`, es: `${nodeName}: señal admitida`, fr: `${nodeName} : signal admis` };
  const receivedLabels = { en: `${nodeName}: signal received`, pl: `${nodeName}: odebrano sygnał`,
    de: `${nodeName}: Signal empfangen`, es: `${nodeName}: señal recibida`, fr: `${nodeName} : signal reçu` };
  const sourceFacts = structuredClone(sourceEvents);
  const recipientFacts = structuredClone(recipientEvents);
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const source = await monitor(instance(`zero-signal-${language}`, { status: 'Completed', activeNodeIds: [] }), {
        processHistoryRequest: { events: sourceEvents, nextSeq: 5, hasMore: false },
      });
      assert.equal(source.querySelectorAll('[data-process-seq]').length, 5);
      assert.equal(source.querySelector('[data-process-seq="3"] > div').textContent, admittedLabels[language]);
      assert.equal(source.textContent.includes(receivedLabels[language]), false);
      assert.equal(source.querySelector('[data-events] img'), null);
      source.remove();
      const recipient = await monitor(instance(`received-signal-${language}`, { status: 'Completed', activeNodeIds: [] }), {
        processHistoryRequest: { events: recipientEvents, nextSeq: 6, hasMore: false },
      });
      assert.equal(recipient.querySelectorAll('[data-process-seq]').length, 6);
      assert.equal(recipient.querySelector('[data-process-seq="4"] > div').textContent, receivedLabels[language]);
      assert.notEqual(admittedLabels[language], receivedLabels[language]);
      assert.equal(recipient.textContent.includes(admittedLabels[language]), false);
      assert.equal(recipient.querySelector('[data-events] img'), null);
      recipient.remove();
      assert.deepEqual(sourceEvents, sourceFacts);
      assert.deepEqual(recipientEvents, recipientFacts);
      assert.deepEqual(Object.keys(sourceData).sort(), ['signal_id', 'source_activation_id', 'signal_namespace_uri', 'signal_declaration_id'].sort());
      assert.deepEqual(Object.keys(receivedData).sort(), ['signal_id', 'subscription_id', 'attached_token_id', 'source_event_id'].sort());
    }
  } finally { await I18n.setLanguage('en'); }
});

test('boundary message switch click saves its checked boolean with a visible accessible label', async () => {
  const model = emptyProcessModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.nodes.splice(1, 0,
    { id: 'Task_1', name: 'Review order', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } },
    { id: 'BoundaryMessage_1', name: 'Receive update', kind: { BoundaryMessage: {
      attachedToId: 'Task_1', cancelActivity: false, messageRef: 'Message_1',
      correlationExpression: 'vars.order_ID', outputMapping: {},
    } } });
  const current = definition('message-boundary-toggle', { model });
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({
    definition: { ...current, model: payload.model, draftRevision: 5 },
  }) });
  state.canvas.selectNode('BoundaryMessage_1');
  await flush(2);
  const toggle = state.config.root.querySelector('[data-process="cancelActivity"]');
  const switchControl = toggle.querySelector('[role="switch"]');
  const label = I18n.t('bpmn.boundary_interrupting');
  assert.equal(toggle.querySelector('.tf-toggle__label').textContent, label);
  assert.equal(switchControl.getAttribute('aria-label'), label);
  assert.equal(toggle.checked, false);
  switchControl.click();
  assert.equal(toggle.checked, true);
  assert.equal(state.canvas.getData().nodes.find((node) => node.id === 'BoundaryMessage_1').kind.BoundaryMessage.cancelActivity, true);
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload;
  assert.equal(saved.model.nodes.find((node) => node.id === 'BoundaryMessage_1').kind.BoundaryMessage.cancelActivity, true);
  assert.ok(encode.processDefinitionSaveRequest(17, saved).byteLength > 0);
});

test('escalation boundary editor saves the exact declaration, attachment, mapping and switch state', async () => {
  const model = emptyProcessModel();
  model.escalations = [{ escalationId: 'Escalation_1', name: 'Needs human', escalationCode: 'NEEDS.HUMAN' }];
  model.nodes.splice(1, 0,
    { id: 'Service_1', name: 'Check order', kind: { ServiceTask: { flowId: 'flow-one',
      inputMapping: {}, outputMapping: {}, verification: 'Human', timeoutSeconds: 60,
      resultExpression: 'outputs.activity_result' } } },
    { id: 'Boundary_1', name: 'Escalate', kind: { BoundaryEscalation: {
      attachedToId: 'Service_1', escalationRef: 'Escalation_1', cancelActivity: true,
      outputMapping: { customer_ID: 'outputs.customer_ID' },
    } } });
  const current = definition('escalation-boundary-editor', { model });
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({
    definition: { ...current, model: payload.model, draftRevision: 5 },
  }) });
  assert.equal(processBoundaryKind('bpmn_boundary_escalation'), true);
  assert.equal(state.canvas.nodesLayer.querySelector('[data-node-id="Boundary_1"] .fb-port-in'), null);
  state.canvas.selectNode('Boundary_1'); await flush(2);
  const fullName = `Actual boundary ${'FullSelectedName'.repeat(12)} <&>`;
  const nameControl = state.config.root.querySelector('[data-process="name"]');
  const nameTextarea = nameControl.querySelector('textarea');
  nameTextarea.focus();
  change(nameControl, fullName);
  await flush(2);
  assert.equal(nameControl.isConnected, true);
  assert.equal(nameControl.querySelector('textarea'), nameTextarea);
  assert.equal(document.activeElement, nameTextarea);
  assert.equal(state.canvas.nodes.find((node) => node.id === 'Boundary_1').label, fullName);
  assert.equal(state.config.root.querySelector('.fb-config-title').textContent, fullName);
  assert.equal(state.root.querySelector('[data-role="crumb-name"]').textContent, fullName);
  assert.equal(state.config.root.querySelector('[data-process="name"] textarea').value, fullName);
  const attachment = state.config.root.querySelector('[data-process="attachedToId"]');
  attachment.focus();
  assert.equal(document.activeElement, attachment.querySelector('select'));
  assert.equal(attachment.value, 'Service_1');
  assert.equal(state.config.root.querySelector('[data-process="escalationRef"]').value, 'Escalation_1');
  const toggle = state.config.root.querySelector('[data-process="cancelActivity"]');
  assert.equal(toggle.checked, true);
  toggle.querySelector('[role="switch"]').click();
  assert.equal(toggle.checked, false);
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload;
  const boundary = saved.model.nodes.find((node) => node.id === 'Boundary_1').kind.BoundaryEscalation;
  assert.equal(saved.model.nodes.find((node) => node.id === 'Boundary_1').name, fullName);
  assert.deepEqual(boundary, { attachedToId: 'Service_1', escalationRef: 'Escalation_1',
    cancelActivity: false, outputMapping: { customer_ID: 'outputs.customer_ID' } });
  assert.deepEqual(saved.model.escalations, model.escalations);
  const beforeClone = state.canvas.getData();
  state.canvas.duplicateNodes(['Service_1', 'Boundary_1']);
  const clones = state.canvas.nodes.filter((node) => state.canvas.selectedIds.has(node.id));
  const clonedService = clones.find((node) => node.type === 'bpmn_service_task');
  const clonedBoundary = clones.find((node) => node.type === 'bpmn_boundary_escalation');
  assert.ok(clonedService && clonedBoundary);
  assert.equal(clonedBoundary.config.attachedToId, clonedService.id);
  state.canvas.undo();
  assert.deepEqual(state.canvas.getData(), beforeClone);
});

test('incomplete boundary attachment or date blocks the real save and publication requests', async () => {
  const original = definition('boundary-save', { model: boundaryModel() });
  const state = await mount(original, {
    processDefinitionSaveRequest: (payload) => ({ definition: { ...original, model: payload.model, draftRevision: 5 } }),
    processDefinitionPublishRequest: () => ({ definition: { ...original, publishedVersion: 1 }, version: { version: 1, model: state.canvas.getData() } }),
  });
  state.canvas.selectNode('Timer_A'); await flush(2);
  change(state.config.root.querySelector('[data-process="attachedToId"]'), '');
  assert.equal(await builder._save(), false);
  await builder._publish();
  assert.equal(calls.some((call) => call.kind === 'processDefinitionSaveRequest' || call.kind === 'processDefinitionPublishRequest'), false);
  change(state.config.root.querySelector('[data-process="attachedToId"]'), 'Review');
  change(state.config.root.querySelector('[data-process="timerType"]'), 'Date');
  await flush(2);
  assert.equal(await builder._save(), false);
  assert.equal(calls.some((call) => call.kind === 'processDefinitionSaveRequest'), false);
  change(state.config.root.querySelector('[data-process="timerAt"]'), '2027-01-02T03:04:05+01:00');
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest');
  assert.deepEqual(saved.payload.model.nodes.find((node) => node.id === 'Timer_A').kind.BoundaryTimer, {
    attachedToId: 'Review', cancelActivity: true, timer: { Date: { at: '2027-01-02T03:04:05+01:00' } },
  });
});

test('canvas connection and undo preserve identity and deletion clears an XOR default', () => {
  const graph = canvas(); const task = userNode(graph);
  assert.equal(graph.connectNodes('Start', task.id), true);
  const edge = graph.edges.at(-1);
  assert.equal(graph.connectNodes('Start', task.id), false, 'the same path cannot be created twice');
  assert.equal(graph.connectNodes('End', task.id), false, 'End cannot have an outgoing path');
  const connected = graph.getData();
  graph.undo(); assert.equal(graph.edges.some((row) => row.id === edge.id), false);
  graph.redo(); assert.deepEqual(graph.getData(), connected);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_exclusive_gateway'), 280, 100);
  const choice = graph.nodes.at(-1); graph.connectNodes(choice.id, 'End');
  const defaultEdge = graph.edges.at(-1);
  graph.updateNodeConfig(choice.id, { defaultFlowId: defaultEdge.id });
  graph.selectEdge(defaultEdge.id); graph.deleteSelected();
  assert.equal(choice.config.defaultFlowId, null);
  graph.destroy();
});

test('inclusive gateway inspector saves every condition and a stable default through clone and deletion', async () => {
  const graph = canvas();
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_inclusive_gateway'), 220, 100);
  const split = graph.nodes.at(-1);
  const review = userNode(graph);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_inclusive_gateway'), 520, 100);
  const join = graph.nodes.at(-1);
  graph.connectNodes(split.id, review.id);
  const conditional = graph.edges.at(-1);
  graph.connectNodes(split.id, join.id);
  const fallback = graph.edges.at(-1);
  assert.ok(graph.edges.some((edge) => edge.id === fallback.id && edge.from_node === split.id));
  const config = inspector(graph);
  config.show(split, graph.templates.get(split.type));
  await flush(1);
  assert.ok(config.root.querySelector('[data-process="defaultFlowId"]').hasAttribute('wrap-selected'));
  const defaultPicker = config.root.querySelector('[data-process="defaultFlowId"]');
  assert.ok(Array.from(defaultPicker._select.options).some((option) => option.value === fallback.id), 'the real select lists the outgoing fallback');
  change(defaultPicker, fallback.id);
  assert.equal(defaultPicker.value, fallback.id, 'the real select retains the selected edge');
  assert.equal(split.config.defaultFlowId, fallback.id, 'selecting the default updates the gateway');
  config.showEdge(conditional);
  assert.equal(config.root.querySelector('[data-process="condition"]').tagName, 'TF-TEXTAREA');
  change(config.root.querySelector('[data-process="condition"]'), 'vars.Request_ID == "yes"');
  assert.equal(conditional.condition, 'vars.Request_ID == "yes"');
  assert.equal(split.config.defaultFlowId, fallback.id);
  const selected = graph.getData();
  const saved = selected.nodes.find((node) => node.id === split.id).kind.InclusiveGateway;
  assert.equal(saved.defaultFlowId, fallback.id);
  assert.equal(selected.sequenceFlows.find((flow) => flow.id === conditional.id).condition, 'vars.Request_ID == "yes"');
  config.show(split, graph.templates.get(split.type));
  await flush(1);
  change(config.root.querySelector('[data-process="defaultFlowId"]'), conditional.id);
  assert.equal(split.config.defaultFlowId, conditional.id);
  assert.equal(conditional.condition, null, 'selecting a default clears its CEL condition');
  config.showEdge(conditional);
  assert.ok(config.root.querySelector('[data-process="condition"]').hasAttribute('disabled'));
  config.show(split, graph.templates.get(split.type));
  await flush(1);
  change(config.root.querySelector('[data-process="defaultFlowId"]'), fallback.id);
  graph.selectNode(split.id);
  graph.duplicateNodes([split.id]);
  assert.equal(graph.nodes.at(-1).config.defaultFlowId, null, 'a cloned gateway must not refer to the original path');
  graph.selectEdge(fallback.id); graph.deleteSelected();
  assert.equal(split.config.defaultFlowId, null, 'deleting a path clears its selected default');
  graph.destroy(); config.destroy();
});

test('inspector selects an eligible person by stable ID and inner blur cannot clear the assignment', () => {
  const graph = canvas(); const node = userNode(graph); const config = inspector(graph);
  config.show(node, graph.templates.get(node.type));
  const picker = config.root.querySelector('tf-person-picker');
  click(picker.querySelector('[data-id="anna"]'));
  assert.equal(node.config.assigneeUserId, 'anna');
  picker.querySelector('input').dispatchEvent(new Event('change', { bubbles: true }));
  assert.equal(node.config.assigneeUserId, 'anna');
  assert.match(picker.textContent, /Anna Kowalska/);
  click(config.root.querySelector('[data-initiator]')); assert.equal(node.config.assigneeUserId, null);
  graph.destroy(); config.destroy();
});

test('manual inspector saves pinned instructions and discloses assigned execution access', async () => {
  const model = emptyProcessModel();
  model.nodes.splice(1, 0, { id: 'Manual_1', name: 'External review',
    kind: { ManualTask: { assigneeUserId: null, instructions: '' } } });
  model.sequenceFlows[0].targetId = 'Manual_1';
  model.sequenceFlows.push({ id: 'ManualExit', sourceId: 'Manual_1', targetId: 'End', condition: null });
  const state = await mount(definition('manual-draft', { model }), {
    processDefinitionSaveRequest: (payload) => ({ definition: definition('manual-draft',
      { model: payload.model, draftRevision: 5 }) }),
  });
  state.canvas.selectNode('Manual_1');
  await flush(2);
  const picker = state.root.querySelector('tf-person-picker');
  const instructions = state.root.querySelector('[data-process="instructions"]');
  assert.equal(instructions.tagName, 'TF-TEXTAREA');
  assert.equal(state.root.querySelector('[data-process="outputMapping"]'), null);
  assert.ok(state.root.querySelector('[data-process="repeatMode"]'));
  click(picker.querySelector('[data-id="lee"]'));
  change(instructions, 'Inspect <register> & report.\nAcknowledge outside work.');
  assert.match(state.root.textContent, new RegExp(I18n.t('bpmn.manual_access_disclosure')));
  assert.equal(await builder._save(), true);
  const saved = calls.find((row) => row.kind === 'processDefinitionSaveRequest').payload.model;
  assert.deepEqual(saved.nodes.find((node) => node.id === 'Manual_1').kind, { ManualTask: {
    assigneeUserId: 'lee', instructions: 'Inspect <register> & report.\nAcknowledge outside work.' } });
  state.canvas.selectNode('Manual_1');
  const readonly = inspector(state.canvas, true);
  readonly.show(state.canvas.nodes.find((node) => node.id === 'Manual_1'), state.canvas.templates.get('bpmn_manual_task'));
  assert.equal(readonly.root.querySelector('[data-process="instructions"]').hasAttribute('disabled'), true);
  assert.equal(readonly.root.querySelector('[data-process="instructions"]').value,
    'Inspect <register> & report.\nAcknowledge outside work.');
  assert.equal(readonly.root.querySelector('[data-process="delete"]'), null);
  readonly.destroy();
});

test('manual acknowledgment uses its own command with pinned instructions and no outputs in five locales', async () => {
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const task = { userTaskId: 'manual-task-1', nodeId: 'Manual_1', name: 'External <&> review',
        assigneeUserId: 'lee', kind: 'Manual', status: 'Open', outputs: null,
        instructions: 'Inspect <register> & report.\nAcknowledge outside work.',
        revision: 1, canComplete: true, scopeId: 'instance-one' };
      const current = instance(`manual-${language}`, { userTasks: [task] });
      const win = await monitor(current, {
        processUserTaskGetRequest: { task },
        processManualTaskAcknowledgeRequest: ({ instanceId }) => ({ instance: instance(instanceId,
          { revision: current.revision + 1, userTasks: [] }) }),
      });
      assert.match(win.querySelector('[data-work]').textContent, new RegExp(I18n.t('bpmn.work_kind_manual')));
      assert.equal(win.querySelector('[data-complete]').textContent, I18n.t('bpmn.acknowledge_manual'));
      click(win.querySelector('[data-complete]')); await flush();
      const form = [...document.querySelectorAll('.tf-act-window')].at(-1);
      assert.equal(form.querySelector('tf-code-editor'), null);
      assert.equal(form.querySelector('[data-approved]'), null);
      assert.equal(form.querySelector('tf-textarea').value, task.instructions);
      assert.ok(form.textContent.includes(I18n.t('bpmn.manual_acknowledgment_notice')));
      assert.equal(form.querySelector('script'), null);
      click(form.querySelector('[data-act="submit"]')); await flush();
      const mutation = calls.filter((row) => row.kind === 'processManualTaskAcknowledgeRequest').at(-1).payload;
      assert.equal(mutation.instanceId, current.instanceId);
      assert.equal(mutation.userTaskId, task.userTaskId);
      assert.equal(mutation.expectedRevision, current.revision);
      assert.equal(Object.hasOwn(mutation, 'outputs'), false);
      assert.equal(Object.hasOwn(mutation, 'approved'), false);
      assert.ok(processEventText({ kind: 'manual_task_opened', nodeName: task.name, data: {} })
        .includes(I18n.t('bpmn.event_manual_task_opened', { node: task.name })));
      assert.ok(processEventText({ kind: 'manual_task_acknowledged', nodeName: task.name, data: {} })
        .includes(I18n.t('bpmn.event_manual_task_acknowledged', { node: task.name })));
      win.remove(); form.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('manual acknowledgment stays unavailable without pinned instructions', async () => {
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      for (const instructions of [undefined, '']) {
        const task = { userTaskId: 'manual-task-1', nodeId: 'Manual_1', name: 'External review',
          assigneeUserId: 'lee', kind: 'Manual', status: 'Open', outputs: null,
          revision: 1, canComplete: true, scopeId: 'instance-one' };
        if (instructions !== undefined) task.instructions = instructions;
        const current = instance(`manual-missing-${language}`, { userTasks: [task] });
        const win = await monitor(current, { processUserTaskGetRequest: { task } });
        const priorForms = document.querySelectorAll('.tf-act-window').length;
        click(win.querySelector('[data-complete]')); await flush();
        assert.equal(document.querySelectorAll('.tf-act-window').length, priorForms);
        const error = win.querySelector('[data-error]');
        assert.equal(error.hidden, false);
        assert.equal(error.getAttribute('message'), I18n.t('bpmn.request_error', {
          error: I18n.t('bpmn.manual_instructions'),
        }));
        assert.equal(calls.some((row) => row.kind === 'processManualTaskAcknowledgeRequest'), false);
        win.remove();
      }
    }
  } finally { await I18n.setLanguage('en'); }
});

test('service inspector edits actual flow, Human/Condition, mappings and timeout without changing mapping keys', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  assert.equal(node.config.verification, 'Human');
  assert.ok(config.root.querySelector('[data-process="flowId"]').hasAttribute('wrap-selected'));
  assert.ok(config.root.querySelector('[data-process="inputMapping"]').hasAttribute('multiline'));
  assert.equal(config.root.querySelector('[data-process="inputMapping"] [data-field="key"]'), null);
  change(config.root.querySelector('[data-process="flowId"]'), 'flow-one');
  change(config.root.querySelector('[data-process="inputMapping"]'), { Request_ID: 'vars.Source_ID' });
  const mapping = config.root.querySelector('[data-process="inputMapping"]');
  assert.equal(mapping.querySelector('[data-field="key"]').tagName, 'TF-TEXTAREA');
  assert.equal(mapping.querySelector('[data-field="key"]').value, 'Request_ID');
  assert.equal(mapping.querySelector('[data-field="value"]').value, 'vars.Source_ID');
  change(config.root.querySelector('[data-process="verification"]'), 'Condition');
  change(config.root.querySelector('[data-process="expression"]'), 'outputs.Result_OK == true');
  change(config.root.querySelector('[data-process="timeoutSeconds"]'), '80');
  assert.deepEqual(node.config, { flowId: 'flow-one', inputMapping: { Request_ID: 'vars.Source_ID' }, outputMapping: {}, verification: { Condition: { expression: 'outputs.Result_OK == true' } }, timeoutSeconds: 80 });
  const readonly = inspector(graph, true); readonly.show(node, graph.templates.get(node.type));
  assert.ok(readonly.root.querySelector('[data-process="flowId"]').hasAttribute('disabled'));
  assert.equal(readonly.root.querySelector('[data-process="delete"]'), null);
  graph.destroy(); config.destroy(); readonly.destroy();
});

test('service inspector configures an addon tool and switches back to a flow', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  assert.equal(config.root.querySelector('[data-process="implementation"]').value, 'flow');
  assert.equal(Object.hasOwn(node.config, 'implementation'), false);
  change(config.root.querySelector('[data-process="flowId"]'), 'flow-one');
  change(config.root.querySelector('[data-process="implementation"]'), 'tool');
  await flush(2);
  assert.deepEqual(node.config.implementation, { AddonTool: { addonId: '', toolName: '' } });
  assert.equal(node.config.flowId, '');
  assert.equal(config.root.querySelector('[data-process="flowId"]'), null);
  change(config.root.querySelector('[data-process="addonId"]'), 'notes-1a2b3c4d');
  change(config.root.querySelector('[data-process="toolName"]'), 'search');
  assert.deepEqual(node.config.implementation, { AddonTool: { addonId: 'notes-1a2b3c4d', toolName: 'search' } });
  const saved = canvasToProcess(graph.processModel, graph.nodes, graph.edges, () => []);
  assert.deepEqual(saved.nodes.find((row) => row.id === node.id).kind.ServiceTask.implementation,
    { AddonTool: { addonId: 'notes-1a2b3c4d', toolName: 'search' } });
  const readonly = inspector(graph, true); readonly.show(node, graph.templates.get(node.type));
  assert.ok(readonly.root.querySelector('[data-process="addonId"]').hasAttribute('disabled'));
  change(config.root.querySelector('[data-process="implementation"]'), 'flow');
  await flush(2);
  assert.equal(node.config.implementation, 'Flow');
  assert.ok(config.root.querySelector('[data-process="flowId"]'));
  assert.equal(config.root.querySelector('[data-process="addonId"]'), null);
  graph.destroy(); config.destroy(); readonly.destroy();
});

test('service inspector configures a block from a valid template and reports invalid settings inline', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  change(config.root.querySelector('[data-process="implementation"]'), 'block');
  await flush(2);
  assert.deepEqual(node.config.implementation, { Block: { nodeType: 'llm', config: { model: '' } } });
  assert.equal(node.config.flowId, '');
  assert.equal(node.config.inputMapping.payload, 'vars.text');
  assert.equal(config.root.querySelector('[data-process="flowId"]'), null);
  assert.equal(config.root.querySelector('[data-process="blockType"]').value, 'llm');
  change(config.root.querySelector('[data-process="blockConfig"]'), '{"model":"m","max_tokens":64}');
  assert.deepEqual(node.config.implementation.Block.config, { model: 'm', max_tokens: 64 });
  // A type brings its own template: the settings of the previous type do not linger.
  change(config.root.querySelector('[data-process="blockType"]'), 'pii_filter');
  await flush(2);
  assert.deepEqual(node.config.implementation, { Block: { nodeType: 'pii_filter', config: {} } });
  assert.equal(config.root.querySelector('[data-process="blockConfig"]').value.trim(), '{}');
  change(config.root.querySelector('[data-process="blockType"]'), 'embeddings');
  await flush(2);
  assert.deepEqual(node.config.implementation.Block, { nodeType: 'embeddings', config: { model: '' } });
  change(config.root.querySelector('[data-process="blockConfig"]'), '{"model":"e"}');
  // Text that is not a JSON object is reported on the field and never reaches the model.
  for (const invalid of ['{', '[]', '7', 'null']) {
    change(config.root.querySelector('[data-process="blockConfig"]'), invalid);
    assert.ok(config.root.querySelector('[data-process="blockConfig"]').getAttribute('error'), invalid);
    assert.deepEqual(node.config.implementation.Block.config, { model: 'e' }, invalid);
  }
  change(config.root.querySelector('[data-process="blockConfig"]'), '{"model":"f"}');
  assert.equal(config.root.querySelector('[data-process="blockConfig"]').getAttribute('error'), null);
  const saved = canvasToProcess(graph.processModel, graph.nodes, graph.edges, () => []);
  assert.deepEqual(saved.nodes.find((row) => row.id === node.id).kind.ServiceTask.implementation,
    { Block: { nodeType: 'embeddings', config: { model: 'f' } } });
  const readonly = inspector(graph, true); readonly.show(node, graph.templates.get(node.type));
  assert.ok(readonly.root.querySelector('[data-process="blockType"]').hasAttribute('disabled'));
  change(config.root.querySelector('[data-process="implementation"]'), 'flow');
  await flush(2);
  assert.equal(node.config.implementation, 'Flow');
  assert.equal(config.root.querySelector('[data-process="blockType"]'), null);
  graph.destroy(); config.destroy(); readonly.destroy();
});

const taskNode = (id, config = {}, label = '') => ({ id, type: 'bpmn_service_task', label, config: { flowId: 'flow-one', inputMapping: {}, outputMapping: {}, verification: 'Human', timeoutSeconds: 60, ...config } });
const boundaryNode = (id, type, attachedToId, config = {}, label = '') => ({ id, type, label, config: { attachedToId, ...config } });
const edge = (from, to) => ({ id: `${from}-${to}`, from_node: from, to_node: to });
const routingGraph = (nodes, edges, extra = {}) => ({ nodes, edges, processModel: { errors: [], escalations: [], ...extra.model }, processPath: extra.path || [] });
const outcomes = (routing) => routing.rows.map((row) => `${row.outcome}${row.code ? `:${row.code}` : ''}${row.other ? ':other' : ''}=${row.route}`);

test('result routing: a task without a result source can only complete or end in an incident', () => {
  const graph = routingGraph([taskNode('T'), { id: 'E', type: 'bpmn_end', label: 'Done', config: {} }], [edge('T', 'E')]);
  const routing = processResultRouting(graph, 'T');
  assert.deepEqual(outcomes(routing), ['completed=next', 'rejected:HUMAN_REJECTED=incident', 'error=unavailable', 'needs_human=unavailable', 'cancelled=unavailable', 'platform=incident', 'timeout:EXTERNAL_OUTCOME_UNCERTAIN=incident']);
  assert.equal(routing.rows[0].gate, 'human');
  assert.deepEqual(routing.rows[0].targets.map((target) => target.id), ['E']);
  assert.deepEqual(routing.warnings, []);
  assert.equal(processResultRouting(graph, 'E'), null);
  assert.equal(processResultRouting(graph, 'missing'), null);
});

test('result routing: a result source without boundary events falls back to incident and verification', () => {
  const graph = routingGraph([taskNode('T', { resultExpression: '{}', verification: { Condition: { expression: 'true' } } }), { id: 'E', type: 'bpmn_end', config: {} }], [edge('T', 'E')]);
  const routing = processResultRouting(graph, 'T');
  assert.deepEqual(outcomes(routing), ['completed=next', 'rejected:VERIFICATION_FAILED=incident', 'error:other=caller', 'needs_human:other=review',
    'cancelled=incident', 'platform=incident', 'timeout:EXTERNAL_OUTCOME_UNCERTAIN=incident']);
  assert.equal(routing.rows[0].gate, 'condition');
  // Inside a sub-process an unhandled error climbs to the enclosing scopes; with no
  // handler in any of them it is an incident like a top-level one.
  const nested = processResultRouting(routingGraph(graph.nodes, graph.edges, { path: ['Sub'] }), 'T');
  assert.equal(nested.rows.find((row) => row.outcome === 'error').route, 'caller');
  assert.equal(nested.rows.find((row) => row.outcome === 'needs_human').route, 'review');
});

test('result routing: a nested task names the enclosing handler an error climbs to, innermost scope first', () => {
  const handler = (id, scope, ref, name) => ({ id, name, kind: { BoundaryError: { attachedToId: scope, errorRef: ref } } });
  const sub = (id, name, nodes) => ({ id, name, kind: { SubProcess: { body: { nodes } } } });
  const model = { processId: 'Root', errors: [{ errorId: 'Err_A', name: 'A', errorCode: 'CODE_A' }, { errorId: 'Err_B', name: 'B', errorCode: 'CODE_B' }],
    nodes: [
      sub('Outer', 'Handle claim', [sub('Inner', 'Check', []), handler('InnerB', 'Inner', 'Err_B', 'Inner B')]),
      handler('OuterA', 'Outer', 'Err_A', 'Claim failed'), handler('OuterB', 'Outer', 'Err_B', 'Outer B'), handler('OuterAny', 'Outer', null, 'Outer catch'),
    ] };
  const graph = routingGraph([taskNode('T', { resultExpression: '{}' }), { id: 'X', type: 'bpmn_end', config: {} }], [edge('T', 'X')], { model, path: ['Outer', 'Inner'] });
  const rows = processResultRouting(graph, 'T').rows.filter((row) => row.outcome === 'error');
  // CODE_B is taken by the inner scope's exact handler, CODE_A by the outer exact one, anything else by the outer catch-all.
  assert.deepEqual(rows.map((row) => [row.code, row.route, row.parent?.name, row.parent?.scope]),
    [['CODE_B', 'parent', 'Inner B', 'Check'], ['CODE_A', 'parent', 'Claim failed', 'Handle claim'], [null, 'parent', 'Outer catch', 'Handle claim']]);
  // A catch-all on the task itself takes everything before any enclosing handler.
  const own = routingGraph([taskNode('T', { resultExpression: '{}' }), boundaryNode('Own', 'bpmn_boundary_error', 'T', { errorRef: null }, 'Own'),
    { id: 'X', type: 'bpmn_end', config: {} }], [edge('T', 'X'), edge('Own', 'X')], { model, path: ['Outer', 'Inner'] });
  assert.deepEqual(processResultRouting(own, 'T').rows.filter((row) => row.outcome === 'error').map((row) => [row.route, row.boundary?.id]), [['boundary', 'Own']]);
  // A repeated task never climbs, whatever surrounds it.
  const repeated = routingGraph([{ ...taskNode('T', { resultExpression: '{}' }), repeat: { MultiInstance: {} } }, { id: 'X', type: 'bpmn_end', config: {} }], [edge('T', 'X')], { model, path: ['Outer', 'Inner'] });
  assert.deepEqual(processResultRouting(repeated, 'T').rows.filter((row) => row.outcome === 'error').map((row) => row.route), ['incident']);
});

test('result routing: exact codes win over catch-all boundary events and every code gets a row', () => {
  const model = { errors: [{ errorId: 'Err_A', name: 'A', errorCode: 'CODE_A' }, { errorId: 'Err_B', name: 'B', errorCode: 'CODE_B' }],
    escalations: [{ escalationId: 'Esc_1', name: 'One', escalationCode: 'REVIEW' }] };
  const graph = routingGraph([
    taskNode('T', { resultExpression: '{}' }),
    boundaryNode('BA', 'bpmn_boundary_error', 'T', { errorRef: 'Err_A' }, 'Handle A'),
    boundaryNode('BB', 'bpmn_boundary_error', 'T', { errorRef: 'Err_B' }),
    boundaryNode('BAll', 'bpmn_boundary_error', 'T', { errorRef: null }, 'Any error'),
    boundaryNode('BE', 'bpmn_boundary_escalation', 'T', { escalationRef: null, cancelActivity: true }),
    { id: 'X', type: 'bpmn_end', config: {} }, { id: 'Y', type: 'bpmn_end', config: {} }, { id: 'W', type: 'bpmn_user_task', config: {} },
  ], [edge('T', 'X'), edge('BA', 'Y'), edge('BB', 'Y'), edge('BAll', 'Y'), edge('BE', 'W'), edge('W', 'Y')], { model });
  const routing = processResultRouting(graph, 'T');
  assert.deepEqual(outcomes(routing), ['completed=next', 'rejected:HUMAN_REJECTED=incident',
    'error:CODE_A=boundary', 'error:CODE_B=boundary', 'error:other=boundary', 'needs_human:other=boundary',
    'cancelled=incident', 'platform=incident', 'timeout:EXTERNAL_OUTCOME_UNCERTAIN=incident']);
  assert.equal(routing.rows.find((row) => row.code === 'CODE_A').boundary.id, 'BA');
  assert.equal(routing.rows.find((row) => row.outcome === 'error' && row.other).boundary.id, 'BAll');
  assert.deepEqual(routing.warnings, []);
  // Every code and the catch-all are taken, so there is nothing left to add.
  assert.equal(routing.add.error, null);
  assert.deepEqual(routing.add.escalation, { ref: 'Esc_1' });
});

test('result routing: an escalation by code replaces the verification task only for that code', () => {
  const model = { escalations: [{ escalationId: 'Esc_1', name: 'One', escalationCode: 'REVIEW' }] };
  const graph = routingGraph([taskNode('T', { resultExpression: '{}' }),
    boundaryNode('BE', 'bpmn_boundary_escalation', 'T', { escalationRef: 'Esc_1' }), { id: 'X', type: 'bpmn_end', config: {} }],
  [edge('T', 'X'), edge('BE', 'X')], { model });
  const routing = processResultRouting(graph, 'T');
  assert.deepEqual(routing.rows.filter((row) => row.outcome === 'needs_human').map((row) => `${row.code}:${row.route}`), ['REVIEW:boundary', 'null:review']);
});

test('result routing: timers run on their own clock and an interrupting one is noted against the timeout', () => {
  const graph = routingGraph([taskNode('T', { timeoutSeconds: 30 }),
    boundaryNode('TA', 'bpmn_boundary_timer', 'T', { cancelActivity: true, timer: { Duration: { seconds: 10 } } }, 'Soon'),
    boundaryNode('TB', 'bpmn_boundary_timer', 'T', { cancelActivity: false, timer: { Duration: { seconds: 45 } } }, 'Late'),
    { id: 'X', type: 'bpmn_end', config: {} }], [edge('T', 'X'), edge('TA', 'X'), edge('TB', 'X')]);
  const routing = processResultRouting(graph, 'T');
  assert.equal(routing.rows.find((row) => row.outcome === 'timeout').route, 'incident');
  const timers = routing.rows.filter((row) => row.outcome === 'timer');
  assert.deepEqual(timers.map((row) => [row.boundary.id, row.boundary.interrupting]), [['TA', true], ['TB', false]]);
  assert.deepEqual(routing.warnings, [{ key: 'timer_preempts_timeout', tone: 'info', names: ['Soon'] }]);
});

test('result routing warns about handlers that cannot work', () => {
  const model = { errors: [{ errorId: 'Err_A', name: 'A', errorCode: 'CODE_A' }] };
  const graph = routingGraph([taskNode('T'),
    boundaryNode('B1', 'bpmn_boundary_error', 'T', { errorRef: 'Err_A' }, 'First'),
    boundaryNode('B2', 'bpmn_boundary_error', 'T', { errorRef: 'Err_A' }, 'Second'),
    boundaryNode('B3', 'bpmn_boundary_error', 'T', { errorRef: 'Err_Gone' }, 'Gone'),
    boundaryNode('B4', 'bpmn_boundary_escalation', 'T', { escalationRef: null }, 'Esc')], [], { model });
  const keys = processResultRouting(graph, 'T').warnings.map((warning) => `${warning.key}:${warning.names.join('|')}`);
  // One message per kind of problem, however many handlers it concerns.
  assert.deepEqual(keys.sort(), ['boundary_no_flow:First|Second|Gone|Esc', 'duplicate_handler:Second', 'error_boundary_without_result:',
    'escalation_boundary_without_result:', 'no_outgoing:', 'undeclared_reference:Gone']);
  // Without a result source neither handler can be added, and both buttons say why.
  const routing = processResultRouting(graph, 'T');
  assert.deepEqual([routing.add.error, routing.add.errorReason, routing.add.escalation, routing.add.escalationReason], [null, 'no_result', null, 'no_result']);
});

// The routing table and the engine read the same cases: tests/fixtures/process_result_routing.json
// is also driven through the real engine by processes::result_routing_tests.
const routingFixture = JSON.parse(readFileSync(new URL('../../../../tests/fixtures/process_result_routing.json', import.meta.url), 'utf8'));
const routingCases = routingFixture.cases;

function graphOfCase(entry) {
  const { task } = entry;
  const model = { errors: [], escalations: [] };
  const nodes = [{ id: 'T', type: 'bpmn_service_task', label: 'Task', config: {
    flowId: task.kind === 'tool' ? '' : 'flow-one', inputMapping: {}, outputMapping: {}, timeoutSeconds: task.timeoutSeconds ?? 60,
    verification: task.verification === 'Human' ? 'Human' : { Condition: { expression: task.conditionAccepts === false ? 'false' : 'true' } },
    ...(task.kind === 'tool' ? { implementation: { AddonTool: { addonId: 'notes-1', toolName: 'search' } } } : {}),
    ...(task.resultExpression ? { resultExpression: 'outputs.result' } : {}) },
  ...(task.repeat ? { repeat: { MultiInstance: { mode: 'Sequential', input: { CollectionExpression: { expression: 'vars.items' } }, outputCollectionVariable: 'results' } } } : {}) },
  { id: 'X', type: 'bpmn_end', label: 'Done', config: {} }];
  const edges = [edge('T', 'X')];
  const attach = (id, type, config) => { nodes.push({ id, type, label: id, config: { attachedToId: 'T', ...config } }); edges.push(edge(id, 'X')); };
  for (const handler of entry.errors || []) {
    if (handler.code !== null) model.errors.push({ errorId: `Decl_${handler.id}`, name: handler.id, errorCode: handler.code });
    attach(handler.id, 'bpmn_boundary_error', { errorRef: handler.code === null ? null : `Decl_${handler.id}` });
  }
  for (const handler of entry.escalations || []) {
    if (handler.code !== null) model.escalations.push({ escalationId: `Decl_${handler.id}`, name: handler.id, escalationCode: handler.code });
    attach(handler.id, 'bpmn_boundary_escalation', { escalationRef: handler.code === null ? null : `Decl_${handler.id}`, cancelActivity: handler.interrupting });
  }
  for (const timer of entry.timers || []) attach(timer.id, 'bpmn_boundary_timer', { cancelActivity: timer.interrupting, timer: { Duration: { seconds: timer.seconds } } });
  // The enclosing sub-process of a nested case, with its catch-all error handler when the case says so.
  if (entry.nested) {
    model.processId = 'Root';
    model.nodes = [{ id: 'Scope', name: 'Scope', kind: { SubProcess: { body: { nodes: [] } } } },
      ...(entry.scopeCatch ? [{ id: 'ScopeCatch', name: 'ScopeCatch', kind: { BoundaryError: { attachedToId: 'Scope', errorRef: null } } }] : [])];
  }
  return routingGraph(nodes, edges, { model, path: entry.nested ? ['Scope'] : [] });
}

for (const entry of routingCases) {
  test(`result routing fixture: ${entry.name}`, () => {
    const routing = processResultRouting(graphOfCase(entry), 'T');
    const stimulus = routingFixture.stimuli[entry.stimulus.type];
    const row = processRowFor(routing, stimulus.row, entry.stimulus.code ?? null);
    assert.ok(row, 'the table has a row for this outcome');
    assert.equal(row.route, entry.expect.route);
    for (const key of ['code', 'reason']) if (entry.expect[key] !== undefined) assert.equal(row[key], entry.expect[key]);
    if (entry.expect.boundary) assert.equal(row.boundary.id, entry.expect.boundary);
    // The texts the row shows are decided by the product code from the row alone.
    assert.deepEqual(processRoutingTextKeys(row, routing.verification), entry.expect.text);
    if (entry.expect.warning) assert.ok(routing.warnings.some((warning) => warning.key === entry.expect.warning));
    else assert.equal(routing.warnings.some((warning) => warning.key.startsWith('timer_')), false);
  });
}

test('result routing fixture covers every route the table can show and pairs each with an engine fact that can honour it', () => {
  const routes = new Set(routingCases.map((entry) => entry.expect.route));
  assert.deepEqual([...routes].sort(), ['boundary', 'boundary_keep', 'caller', 'incident', 'next', 'parent', 'review', 'unavailable']);
  assert.deepEqual(Object.keys(routingFixture.routeFacts).sort(), [...routes].sort());
  assert.ok(routingCases.every((entry) => entry.engine !== undefined && (entry.engine !== null || entry.engineReason)), 'a row without an engine probe says why');
  for (const entry of routingCases.filter((candidate) => candidate.engine)) {
    assert.ok(routingFixture.routeFacts[entry.expect.route].includes(entry.engine.fact),
      `${entry.name}: the table says ${entry.expect.route}, the engine fact ${entry.engine.fact} cannot honour it`);
  }
  for (const [type, stimulus] of Object.entries(routingFixture.stimuli)) assert.ok(stimulus.row, `stimulus ${type} names its table row`);
});

test('result routing texts name both timeout codes, the caller catch, the kept task and working time', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1);
  graph.updateNodeConfig(node.id, { resultExpression: 'outputs.result', implementation: { AddonTool: { addonId: 'notes-1', toolName: 'search' } } });
  const timer = processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer');
  graph.addNodeFromTemplate(timer, 200, 400);
  const boundary = graph.nodes.at(-1);
  graph.updateNodeConfig(boundary.id, { attachedToId: node.id, timer: { WorkingDuration: { seconds: 7200 } } });
  const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  config.root.querySelector('[data-service-tabs]').dispatchEvent(new CustomEvent('change', { detail: { value: 'result' } }));
  const rows = Object.fromEntries(config.root.querySelector('tf-table[data-routing]').rows.map((row) => [row.outcome, row.route]));
  const timeout = Object.entries(rows).find(([outcome]) => outcome.startsWith('Timeout'))[1];
  assert.match(timeout, /EXTERNAL_OUTCOME_UNCERTAIN/);
  assert.match(timeout, /SERVICE_TIMEOUT/);
  assert.match(timeout, /interrupting boundary timer/);
  assert.match(rows['Any other error'], /Call activity has an error boundary event/);
  assert.match(rows['Addon tool permission denied'], /whatever the result rule says/);
  const working = Object.entries(rows).find(([outcome]) => outcome === 'Boundary timer')[1];
  assert.match(working, /7200 seconds of working time/);
  assert.doesNotMatch(working, /after 7200 seconds,/);
  graph.destroy(); config.destroy();
});

test('result routing rows never show a dangling code and warnings name handlers, not their ids', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  config.root.querySelector('[data-service-tabs]').dispatchEvent(new CustomEvent('change', { detail: { value: 'result' } }));
  const rows = () => config.root.querySelector('tf-table[data-routing]').rows;
  for (const row of rows()) assert.doesNotMatch(row.outcome, /[:\s]$/, `no dangling colon or space in "${row.outcome}"`);
  assert.deepEqual(rows().filter((row) => row.route === 'Not possible without a result rule').map((row) => row.outcome), ['Error', 'Needs a human', 'Cancelled']);
  for (let index = 0; index < 3; index += 1) graph.addBoundaryNode(node.id, 'bpmn_boundary_error', null);
  config.refreshRouting();
  const warnings = [...config.root.querySelectorAll('[data-routing-warning="boundary_no_flow"]')];
  assert.equal(warnings.length, 1, 'identical warnings collapse into one');
  const message = warnings[0].getAttribute('message');
  assert.doesNotMatch(message, /n_[a-z0-9]+_[a-z0-9]+/, 'no internal id');
  assert.match(message, /Error: any/);
  assert.match(config.root.querySelector('[data-routing-warning="error_boundary_without_result"]').getAttribute('message'), /result rule/);
  graph.destroy(); config.destroy();
});

test('the Service inspector reads in all five locales without keys, placeholders or clipped tabs', async () => {
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const model = emptyProcessModel();
      model.errors = [{ errorId: 'Err_A', name: 'A', errorCode: 'CODE_A' }];
      model.escalations = [{ escalationId: 'Esc_R', name: 'R', escalationCode: 'REVIEW' }];
      const graph = canvas(model); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
      const node = graph.nodes.at(-1);
      graph.updateNodeConfig(node.id, { resultExpression: 'outputs.result', verification: { Condition: { expression: ' ' } },
        implementation: { AddonTool: { addonId: 'notes-1', toolName: 'search' } }, flowId: '' });
      graph.addBoundaryNode(node.id, 'bpmn_boundary_error', null);
      graph.addBoundaryNode(node.id, 'bpmn_boundary_error', 'Err_A');
      const keep = graph.addBoundaryNode(node.id, 'bpmn_boundary_escalation', 'Esc_R');
      graph.updateNodeConfig(keep.id, { cancelActivity: false });
      graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer'), 300, 500);
      const timer = graph.nodes.at(-1);
      graph.updateNodeConfig(timer.id, { attachedToId: node.id, timer: { WorkingDuration: { seconds: 600 } } });
      const config = inspector(graph); config.show(node, graph.templates.get(node.type));
      await flush(2);
      config.root.querySelector('[data-service-tabs]').dispatchEvent(new CustomEvent('change', { detail: { value: 'result' } }));
      const texts = [
        ...config.root.querySelector('tf-table[data-routing]').rows.flatMap((row) => [row.outcome, row.route]),
        ...[...config.root.querySelectorAll('[data-routing-warning]')].map((alert) => alert.getAttribute('message')),
        config.root.querySelector('[data-routing-add-hint]').textContent,
        ...graph.nodes.filter((candidate) => processBoundaryKind(candidate.type)).map((candidate) => candidate.label),
      ];
      for (const text of texts) {
        assert.doesNotMatch(text, /\{[a-z]+[|}]|bpmn\.[a-z_]+/, `${language}: "${text}" has no placeholder or key`);
        assert.doesNotMatch(text, /[:：]\s*$/, `${language}: "${text}" has no dangling colon`);
      }
      for (const tab of config.root.querySelectorAll('[data-service-tabs] tf-tab')) {
        assert.ok(tab.getAttribute('label').length <= 10, `${language}: "${tab.getAttribute('label')}" fits a 60 px cell`);
        assert.doesNotMatch(tab.getAttribute('title'), /bpmn\./);
      }
      assert.equal(new Set([...config.root.querySelectorAll('[data-service-tabs] tf-tab')].map((tab) => tab.getAttribute('label'))).size, 5);
      graph.destroy(); config.destroy();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('boundary events added from the buttons sit on the bottom edge, spread left to right, clear of the icon and the ports', () => {
  const model = emptyProcessModel();
  model.errors = ['A', 'B', 'C', 'D', 'E'].map((code) => ({ errorId: `Err_${code}`, name: code, errorCode: `CODE_${code}` }));
  const graph = canvas(model); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  graph.updateNodeConfig(task.id, { resultExpression: 'outputs.result' });
  const refs = [null, 'Err_A', 'Err_B', 'Err_C', 'Err_D', 'Err_E'];
  const added = refs.map((ref) => graph.addBoundaryNode(task.id, 'bpmn_boundary_error', ref));
  const centre = (node) => ({ x: node.x + node.width / 2, y: node.y + node.height / 2 });
  const bottom = task.y + task.height;
  const first = added.slice(0, 4);
  assert.ok(first.every((node) => centre(node).y === bottom), 'the first four sit on the bottom border');
  assert.deepEqual(first.map((node) => centre(node).x), [...first.map((node) => centre(node).x)].sort((a, b) => a - b), 'in left-to-right order');
  const overlap = (a, b) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
  for (let i = 0; i < added.length; i += 1) for (let j = i + 1; j < added.length; j += 1) assert.equal(overlap(added[i], added[j]), false, `${i} and ${j} do not overlap`);
  // The incoming port is the left middle, the icon the top left, the kind badge the top right.
  const inPort = { x: task.x, y: task.y + task.height / 2 };
  const icon = { x: task.x, y: task.y, width: 38, height: 38 };
  const badge = { x: task.x + task.width - 54, y: task.y, width: 54, height: 30 };
  for (const node of added) {
    assert.ok(Math.hypot(centre(node).x - inPort.x, centre(node).y - inPort.y) > node.width / 2 + 8, 'clear of the incoming port');
    assert.equal(overlap(node, icon), false, 'clear of the task icon');
    assert.equal(overlap(node, badge), false, 'clear of the kind badge');
  }
  assert.deepEqual(added.map((node) => node.label), ['Error: any', 'Error: CODE_A', 'Error: CODE_B', 'Error: CODE_C', 'Error: CODE_D', 'Error: CODE_E']);
  // The border is full: a seventh event is refused instead of piled on the icon, the ports or another event.
  assert.equal(graph.boundaryRoom(task.id), 0);
  const count = graph.nodes.length;
  assert.equal(graph.addBoundaryNode(task.id, 'bpmn_boundary_escalation', null), null);
  assert.equal(graph.nodes.length, count, 'nothing was added');
  const refusal = processResultRouting(graph, task.id).add;
  assert.equal(refusal.errorReason, 'all_taken', 'every declared error already has its handler, which is reported before the room');
  assert.equal(refusal.escalationReason, 'no_room', 'an escalation handler could be chosen, but the border is full');
  graph.destroy();
});

test('a task with a full border says so on both add buttons, and room returns when a handler goes', () => {
  const model = emptyProcessModel();
  model.errors = ['A', 'B', 'C', 'D', 'E', 'F', 'G'].map((code) => ({ errorId: `Err_${code}`, name: code, errorCode: `CODE_${code}` }));
  model.escalations = [{ escalationId: 'Esc_R', name: 'R', escalationCode: 'REVIEW' }];
  const graph = canvas(model); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  graph.updateNodeConfig(task.id, { resultExpression: 'outputs.result' });
  const added = ['Err_A', 'Err_B', 'Err_C', 'Err_D', 'Err_E', 'Err_F'].map((ref) => graph.addBoundaryNode(task.id, 'bpmn_boundary_error', ref));
  const routing = processResultRouting(graph, task.id);
  assert.deepEqual([routing.add.error, routing.add.errorReason, routing.add.escalation, routing.add.escalationReason], [null, 'no_room', null, 'no_room']);
  graph.removeNodes([added[2].id]);
  const after = processResultRouting(graph, task.id);
  assert.deepEqual([after.add.errorReason, after.add.escalationReason], [null, null]);
  assert.ok(graph.addBoundaryNode(task.id, 'bpmn_boundary_error', 'Err_G'), 'the freed place is taken');
  graph.destroy();
});

test('a boundary event dropped from the palette attaches to the task it lands on, or says it did not', () => {
  const graph = canvas(); const template = (type) => processTemplates().find((row) => row.node_type === type);
  graph.addNodeFromTemplate(template('bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  const [x, y] = [task.x + task.width / 2, task.y + task.height + 10];
  // Dropped on the bottom edge: attached, on a slot of the border near the drop.
  const timer = graph.addNodeFromTemplate(template('bpmn_boundary_timer'), x, y);
  assert.equal(timer.config.attachedToId, task.id);
  assert.equal(timer.y + timer.height / 2, task.y + task.height, 'it sits on the border');
  assert.deepEqual(processNodeWarnings(timer), []);
  assert.deepEqual(graph.validate(), [I18n.t('bpmn.timer_timezone_missing', { element: 'Boundary timer' })], 'a timer needs its zone, and the message names it');
  graph.updateProcessTimezone('Europe/Warsaw');
  assert.deepEqual(graph.validate(), [], 'a saved process keeps it');
  // Dropped in the open: not attached, flagged, and the save names it.
  const loose = graph.addNodeFromTemplate(template('bpmn_boundary_timer'), 900, 900);
  assert.ok(!loose.config.attachedToId);
  assert.deepEqual(processNodeWarnings(loose), ['boundary_unattached']);
  assert.match(graph.validate().join(' '), new RegExp(`“${loose.label || template('bpmn_boundary_timer').label}”|${loose.id}`));
  graph.destroy();
});

test('a boundary event dropped on a task with a full border is moved off it, covering neither the task nor its handlers', () => {
  const model = emptyProcessModel();
  model.errors = ['A', 'B', 'C', 'D', 'E', 'F'].map((code) => ({ errorId: `Err_${code}`, name: code, errorCode: `CODE_${code}` }));
  const graph = canvas(model); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 600, 300);
  const task = graph.nodes.at(-1);
  const handlers = model.errors.map((entry) => graph.addBoundaryNode(task.id, 'bpmn_boundary_error', entry.errorId));
  assert.equal(graph.boundaryRoom(task.id), 0);
  const overlap = (a, b) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
  for (const type of ['bpmn_boundary_timer', 'bpmn_boundary_error']) {
    const refused = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === type), task.x + task.width / 2, task.y + task.height - 4);
    assert.ok(!refused.config.attachedToId, 'it stays unattached');
    for (const other of [task, ...handlers, ...graph.nodes.filter((node) => node !== refused && !handlers.includes(node) && node !== task)]) {
      assert.equal(overlap(refused, other), false, `${type} does not cover ${other.id}`);
    }
    assert.deepEqual(processNodeWarnings(refused), ['boundary_unattached'], 'and it says so');
  }
  graph.destroy();
});

test('a long code in a canvas label may break after its separators and keeps its full text on hover', () => {
  const graph = canvas();
  const code = 'INVALID_CUSTOMER_ADDRESS_FORMAT_FOR_REGION_<b>';
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_end'), 300, 300);
  const end = graph.nodes.at(-1);
  graph.updateNodeLabel(end.id, code);
  const label = graph.nodesLayer.querySelector(`[data-node-id="${end.id}"] .fb-process-label`);
  assert.equal(label.getAttribute('title'), code, 'the whole text is on hover');
  assert.equal(label.textContent, code, 'the marker adds no text and the name stays text');
  assert.equal(label.querySelectorAll('wbr').length, 6, 'a break is offered after each underscore');
  assert.equal(label.querySelector('b'), null);
  graph.destroy();
});

test('an escalation handler the server would refuse carries a warning badge that follows its flows', () => {
  const graph = canvas();
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  graph.updateNodeConfig(task.id, { resultExpression: 'outputs.result' });
  const handler = graph.addBoundaryNode(task.id, 'bpmn_boundary_escalation', null);
  const badge = () => graph.nodesLayer.querySelector(`[data-node-id="${handler.id}"] .fb-process-warn`);
  assert.equal(badge(), null, 'no flow yet, nothing to refuse');
  assert.ok(graph.connectNodes(handler.id, 'End'));
  assert.equal(badge()?.dataset.warning, 'escalation_path', 'running straight into an End is flagged on the canvas');
  assert.match(badge().getAttribute('title'), new RegExp(`^Escalation handler “${handler.label}” runs into an End`), 'and the badge names the handler');
  const edge = graph.edges.find((candidate) => candidate.from_node === handler.id);
  graph.selectEdge(edge.id); graph.deleteSelected();
  assert.equal(badge(), null, 'removing the flow clears it');
  const wait = userNode(graph);
  assert.ok(graph.connectNodes(handler.id, wait.id));
  assert.equal(badge(), null, 'a user task is a wait the server accepts');
  graph.destroy();
});

test('a boundary event is not put on top of a node below the task when a free slot exists', () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_end'), 0, 0);
  const end = graph.nodes.at(-1);
  end.x = task.x + 20; end.y = task.y + task.height + 10;
  const added = graph.addBoundaryNode(task.id, 'bpmn_boundary_error', null);
  const covers = added.x < end.x + end.width && end.x < added.x + added.width && added.y < end.y + end.height && end.y < added.y + added.height;
  assert.equal(covers, false);
  graph.destroy();
});

test('the process settings row folds on a short window and the palette drawer closes after an add', async () => {
  const heights = [];
  for (const height of [700, 1000]) {
    Object.defineProperty(window, 'innerHeight', { value: height, configurable: true });
    const state = await mount(definition(`fold-${height}`));
    const notice = state.root.querySelector('.fb-process-notice');
    const toggle = state.root.querySelector('[data-role="notice-toggle"]');
    heights.push(notice.classList.contains('fb-collapsed'));
    assert.equal(toggle.dataset.expanded, String(!notice.classList.contains('fb-collapsed')));
    click(toggle);
    assert.equal(notice.classList.contains('fb-collapsed'), !heights.at(-1), 'the toggle flips it');
    assert.equal(toggle.dataset.expanded, String(!notice.classList.contains('fb-collapsed')));
    // The state chip stays when everything else folds away.
    assert.ok(notice.querySelector('[data-role="publication"]'));
  }
  assert.deepEqual(heights, [true, false]);
  const state = builder._state;
  const palette = state.root.querySelector('[data-role="palette"]');
  const opener = state.root.querySelector('[data-role="palette-toggle"]');
  click(opener);
  assert.equal(palette.classList.contains('open'), true);
  assert.equal(opener.dataset.expanded, 'true');
  const template = processTemplates().find((row) => row.node_type === 'bpmn_service_task');
  state.palette.opts.onAdd(template);
  assert.equal(palette.classList.contains('open'), false, 'adding a node puts the drawer away');
  click(opener);
  state.palette.opts.onDrop(template, 300, 300);
  assert.equal(palette.classList.contains('open'), false, 'so does dropping one');
  Object.defineProperty(window, 'innerHeight', { value: 768, configurable: true });
});

test('the Service task tab and the routing table survive selecting its handler and follow the canvas', async () => {
  const current = definition('routing-follow-definition');
  const state = await mount(current);
  const graph = state.canvas;
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  graph.updateNodeConfig(task.id, { resultExpression: 'outputs.result' });
  graph.selectNode(task.id);
  await flush(2);
  const tabs = () => state.config.root.querySelector('[data-service-tabs]');
  tabs().dispatchEvent(new CustomEvent('change', { detail: { value: 'result' } }));
  const handler = graph.addBoundaryNode(task.id, 'bpmn_boundary_error', null);
  state.config.refreshRouting();
  assert.ok(state.config.root.querySelector('[data-routing-warning="boundary_no_flow"]'));
  // Looking at the handler and coming back keeps the tab.
  graph.selectNode(handler.id);
  await flush(2);
  assert.equal(state.config.root.querySelector('[data-service-tabs]'), null, 'the handler has its own inspector');
  graph.selectNode(task.id);
  await flush(2);
  assert.equal(tabs().getAttribute('value'), 'result');
  // An edge drawn from the new handler updates the table while the task stays selected.
  const end = graph.nodes.find((node) => node.type === 'bpmn_end');
  assert.equal(graph.connectNodes(handler.id, end.id), true);
  assert.equal(state.config.root.querySelector('[data-routing-warning="boundary_no_flow"]'), null, 'the table follows the canvas');
  const row = state.config.root.querySelector('tf-table[data-routing]').rows.find((entry) => entry.outcome === 'Any other error');
  assert.match(row.route, /Error: any/);
  assert.match(row.route, /then: End/);
  // Another task's handler does not carry the tab over.
  graph.addNodeFromTemplate(processTemplates().find((candidate) => candidate.node_type === 'bpmn_service_task'), 600, 300);
  const other = graph.nodes.at(-1);
  graph.selectNode(other.id);
  await flush(2);
  assert.equal(tabs().getAttribute('value'), 'impl');
});

test('service tabs are labelled panels reachable at a glance with full names on hover', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  const tabs = config.root.querySelector('[data-service-tabs]');
  assert.equal(tabs.getAttribute('variant'), 'bar');
  assert.equal(tabs.getAttribute('layout'), 'stacked', 'equal cells with an icon, no sideways scrolling');
  for (const tab of tabs.querySelectorAll('tf-tab')) {
    const panel = config.root.querySelector(`[data-service-panel="${tab.id}"]`);
    assert.equal(panel.getAttribute('role'), 'tabpanel');
    assert.equal(panel.getAttribute('aria-labelledby'), tab.id);
    assert.equal(tab.getAttribute('panel'), panel.id);
    assert.equal(tab.querySelector('button').getAttribute('aria-controls'), panel.id);
    assert.ok(tab.getAttribute('title'), 'the full name is on hover');
    assert.ok(tab.getAttribute('label').length <= 10, `"${tab.getAttribute('label')}" fits a 60 px cell`);
  }
  assert.equal(tabs.querySelector('tf-tab[id="result"]').getAttribute('title'), 'Result and errors');
  graph.destroy(); config.destroy();
});

test('an empty acceptance condition is named at save, flagged in the table and explained in the autosave status', async () => {
  const current = definition('empty-condition-definition');
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({ definition: { ...current, model: payload.model, draftRevision: 5 } }) });
  const graph = state.canvas;
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  graph.updateNodeLabel(task.id, 'Check invoice');
  graph.updateNodeConfig(task.id, { verification: { Condition: { expression: '  ' } } });
  assert.deepEqual(graph.validate(), ['Check invoice: the acceptance condition is empty. Enter a CEL condition or choose human review.']);
  assert.ok(processResultRouting(graph, task.id).warnings.some((warning) => warning.key === 'empty_condition'));
  assert.equal(await builder._save({ silent: true }), false);
  assert.match(state.root.querySelector('[data-role="autosave"]').getAttribute('title'), /Check invoice/);
  assert.equal(calls.some((call) => call.kind === 'processDefinitionSaveRequest'), false, 'nothing was sent');
  graph.updateNodeConfig(task.id, { verification: { Condition: { expression: 'true' } } });
  assert.equal(await builder._save(), true);
  assert.equal(state.root.querySelector('[data-role="autosave"]').getAttribute('title'), state.root.querySelector('[data-role="autosave-text"]').textContent,
    'a saved chip tells its state by tooltip too, for the bar where it is only an icon');
});

test('saving a process says the process was saved, not a flow', async () => {
  const current = definition('saved-text-definition');
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({ definition: { ...current, model: payload.model, draftRevision: 5 } }) });
  state.canvas.updateNodeLabel('Start', 'Begin');
  // A toast repeated within seconds is merged into the earlier one, so the text
  // the save asks for is read from the translation calls instead of the page.
  const asked = [];
  const translate = I18n.t;
  I18n.t = function record(key, ...rest) { asked.push(key); return translate.call(this, key, ...rest); };
  try { assert.equal(await builder._save(), true); } finally { I18n.t = translate; }
  assert.ok(asked.includes('bpmn.saved'));
  assert.equal(asked.includes('flows_builder.save_success'), false);
  assert.equal(I18n.t('bpmn.saved'), 'Process saved');
});

test('a handler named after its code follows the code, a name the author wrote stays', async () => {
  const model = emptyProcessModel();
  model.errors = [{ errorId: 'Err_A', name: 'A', errorCode: 'CODE_A' }, { errorId: 'Err_B', name: 'B', errorCode: 'CODE_B' }];
  const graph = canvas(model); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  graph.updateNodeConfig(task.id, { resultExpression: 'outputs.result' });
  const auto = graph.addBoundaryNode(task.id, 'bpmn_boundary_error', 'Err_A');
  const written = graph.addBoundaryNode(task.id, 'bpmn_boundary_error', 'Err_B');
  graph.updateNodeLabel(written.id, 'Handle B myself');
  const config = inspector(graph);
  config.show(auto, graph.templates.get(auto.type)); await flush(2);
  change(config.root.querySelector('[data-process="errorRef"]'), 'Err_B');
  assert.equal(auto.label, 'Error: CODE_B');
  change(config.root.querySelector('[data-process="errorRef"]'), '');
  assert.equal(auto.label, 'Error: any');
  config.show(written, graph.templates.get(written.type)); await flush(2);
  change(config.root.querySelector('[data-process="errorRef"]'), 'Err_A');
  assert.equal(written.label, 'Handle B myself');
  graph.destroy(); config.destroy();
});

test('the Call activity version warning leads its inspector', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_call_activity'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  const body = config.root.querySelector('.fb-process-fields');
  const order = [...body.querySelectorAll('[data-call-version-warning], [data-process="callTargetMode"], [data-process="calledVersionChoice"]')];
  assert.equal(order[0].hasAttribute('data-call-version-warning'), true, 'above every picker, within the first screen');
  graph.destroy(); config.destroy();
});

test('new block settings come from the server list, so a block that needs nothing starts empty', async () => {
  const [llm, embeddings, pii] = serviceBlocks;
  assert.deepEqual(processBlockTemplate(llm), { model: '' });
  assert.deepEqual(processBlockTemplate(embeddings), { model: '' });
  assert.deepEqual(processBlockTemplate(pii), {});
  assert.deepEqual(processBlockTemplate({ nodeType: 'future', requiredSettings: ['voice', 'speed'] }), { voice: '', speed: '' });
  assert.deepEqual(processBlockTemplate(undefined), {});
});

test('service inspector groups the task into tabs and keeps every control reachable', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  const tabs = config.root.querySelector('[data-service-tabs]');
  assert.deepEqual([...tabs.querySelectorAll('tf-tab')].map((tab) => tab.id), ['impl', 'io', 'result', 'time', 'advanced']);
  const panelOf = (selector) => config.root.querySelector(selector).closest('[data-service-panel]').dataset.servicePanel;
  assert.equal(panelOf('[data-process="flowId"]'), 'impl');
  assert.equal(panelOf('[data-process="inputMapping"]'), 'io');
  assert.equal(panelOf('[data-process="outputMapping"]'), 'io');
  assert.equal(panelOf('[data-process="resultExpression"]'), 'result');
  assert.equal(panelOf('[data-process="verification"]'), 'result');
  assert.equal(panelOf('[data-process="timeoutSeconds"]'), 'time');
  assert.equal(panelOf('[data-process="repeatMode"]'), 'advanced');
  assert.equal(panelOf('[data-process="elementId"]'), 'advanced');
  const visible = () => [...config.root.querySelectorAll('[data-service-panel]')].filter((panel) => !panel.hidden).map((panel) => panel.dataset.servicePanel);
  assert.deepEqual(visible(), ['impl']);
  tabs.dispatchEvent(new CustomEvent('change', { detail: { value: 'result' } }));
  assert.deepEqual(visible(), ['result']);
  // The same node shown again keeps its tab; another node starts on the first one.
  config.show(node, graph.templates.get(node.type));
  assert.deepEqual(visible(), ['result']);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 400, 300);
  const other = graph.nodes.at(-1); config.show(other, graph.templates.get(other.type));
  assert.deepEqual(visible(), ['impl']);
  const hosts = 'tf-input, tf-textarea, tf-select, tf-keyvalue-editor, tf-table, tf-tabs, tf-button, tf-toggle, tf-person-picker, tf-alert';
  const raw = [...config.root.querySelectorAll('button, input, select, textarea, table')].filter((element) => !element.closest(hosts));
  assert.equal(raw.length, 0, 'the service inspector is built from tf-* components only');
  graph.destroy(); config.destroy();
});

test('result tab shows the derived routing and its buttons add boundary events as one undo step', async () => {
  const model = emptyProcessModel();
  model.errors = [{ errorId: 'Err_A', name: 'A', errorCode: 'CODE_A' }];
  const graph = canvas(model); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  config.root.querySelector('[data-service-tabs]').dispatchEvent(new CustomEvent('change', { detail: { value: 'result' } }));
  const rows = () => config.root.querySelector('tf-table[data-routing]').rows;
  assert.equal(rows().length, 7);
  assert.match(rows()[2].route, /result rule/);
  // Both buttons need a result rule, and say so.
  for (const kind of ['error', 'escalation']) {
    assert.ok(config.root.querySelector(`[data-routing-add="${kind}"]`).hasAttribute('disabled'), `${kind} is disabled without a result rule`);
    assert.match(config.root.querySelector(`[data-routing-add="${kind}"]`).getAttribute('title'), /result rule/);
  }
  assert.match(config.root.querySelector('[data-routing-add-hint]').textContent, /result rule/);
  change(config.root.querySelector('[data-process="resultExpression"]'), '{}');
  for (const kind of ['error', 'escalation']) assert.equal(config.root.querySelector(`[data-routing-add="${kind}"]`).hasAttribute('disabled'), false);
  assert.equal(rows().length, 7);
  assert.doesNotMatch(rows()[2].route, /result rule/, 'the table follows the result rule');
  assert.match(rows()[3].route, /verification task/);
  const history = graph.historyIndex; const count = graph.nodes.length;
  click(config.root.querySelector('[data-routing-add="error"]'));
  assert.equal(graph.nodes.length, count + 1);
  assert.equal(graph.historyIndex, history + 1, 'one undo step');
  const added = graph.nodes.at(-1);
  assert.equal(added.type, 'bpmn_boundary_error');
  assert.deepEqual([added.config.attachedToId, added.config.errorRef], [node.id, null]);
  assert.equal(added.label, 'Error: any', 'a handler added by a button reads as what it catches');
  assert.ok(rows().some((row) => row.outcome === 'Any other error' && /Boundary event/.test(row.route)));
  assert.ok(config.root.querySelector('[data-routing-warning="boundary_no_flow"]'), 'the new event has no flow yet');
  // The catch-all is taken, so the next click attaches the declared code.
  click(config.root.querySelector('[data-routing-add="error"]'));
  assert.deepEqual(graph.nodes.at(-1).config.errorRef, 'Err_A');
  assert.equal(graph.nodes.at(-1).label, 'Error: CODE_A');
  assert.equal(config.root.querySelector('[data-routing-add="error"]').hasAttribute('disabled'), true);
  assert.match(config.root.querySelector('[data-routing-add-hint]').textContent, /Process declarations/, 'a disabled button says where to go');
  const disabledAdd = config.root.querySelector('[data-routing-add="error"]');
  const describedBy = disabledAdd.querySelector('button').getAttribute('aria-describedby');
  assert.equal(describedBy, config.root.querySelector('[data-routing-add-hint]').id, 'the real button is described by the hint that says why it is off');
  click(config.root.querySelector('[data-routing-add="escalation"]'));
  assert.equal(graph.nodes.at(-1).type, 'bpmn_boundary_escalation');
  graph.undo();
  assert.equal(graph.nodes.filter((candidate) => candidate.type === 'bpmn_boundary_escalation').length, 0);
  assert.equal(graph.nodes.filter((candidate) => candidate.type === 'bpmn_boundary_error').length, 2);
  const saved = canvasToProcess(graph.processModel, graph.nodes, graph.edges, () => []);
  assert.equal(saved.nodes.filter((row) => row.kind.BoundaryError).length, 2);
  const readonly = inspector(graph, true); readonly.show(node, graph.templates.get(node.type));
  assert.equal(readonly.root.querySelector('[data-routing-add]'), null);
  graph.destroy(); config.destroy(); readonly.destroy();
});

test('canvas marks the kind of a Service task and an LLM block without a model', async () => {
  const graph = canvas();
  const add = (preset, x, label = '') => { graph.addNodeFromTemplate({ ...processTemplates().find((row) => row.node_type === 'bpmn_service_task'), preset, label }, x, 300); return graph.nodes.at(-1); };
  const element = (node) => graph.nodesLayer.querySelector(`[data-node-id="${node.id}"]`);
  const plain = add(undefined, 100);
  const flow = add({ flowId: 'flow-one' }, 300);
  const block = add({ implementation: { Block: { nodeType: 'llm', config: { model: '' } } } }, 500);
  const pii = add({ implementation: { Block: { nodeType: 'pii_filter', config: {} } } }, 700);
  const tool = add({ implementation: { AddonTool: { addonId: 'notes-1', toolName: 'search' } } }, 900);
  assert.equal(element(plain).querySelector('.fb-process-kind'), null);
  assert.deepEqual([flow, block, pii, tool].map((node) => element(node).querySelector('.fb-process-kind')?.dataset.kind), ['flow', 'block', 'block', 'tool']);
  assert.equal(element(flow).querySelector('.fb-process-kind').getAttribute('aria-label'), I18n.t('bpmn.kind_flow'));
  assert.equal(element(block).querySelector('.fb-process-warn')?.getAttribute('data-warning'), 'empty_setting');
  assert.equal(element(block).querySelector('.fb-process-warn').getAttribute('aria-label'), I18n.t('bpmn.warning_empty_setting', { names: 'model' }));
  for (const node of [plain, flow, pii, tool]) assert.equal(element(node).querySelector('.fb-process-warn'), null);
  const config = inspector(graph); config.show(block, graph.templates.get(block.type));
  await flush(2);
  const alert = config.root.querySelector('[data-service-warning="empty_setting"]');
  assert.equal(alert.hidden, false);
  assert.match(alert.getAttribute('message'), /model/);
  change(config.root.querySelector('[data-process="blockConfig"]'), '{"model":"m"}');
  assert.equal(alert.hidden, true);
  assert.equal(element(block).querySelector('.fb-process-warn'), null, 'the node follows the setting');
  change(config.root.querySelector('[data-process="blockConfig"]'), '{"model":"  "}');
  assert.equal(alert.hidden, false);
  assert.deepEqual(processNodeWarnings(block, serviceBlocks), ['empty_setting']);
  // Which blocks need which settings is the server's word, not a list in the client.
  assert.deepEqual(processNodeWarnings(block, [{ nodeType: 'llm', requiredSettings: [] }]), []);
  const other = [{ nodeType: 'llm', requiredSettings: ['model', 'temperature_mode'] }];
  block.config = { ...block.config, implementation: { Block: { nodeType: 'llm', config: { model: 'm', temperature_mode: '' } } } };
  assert.deepEqual(processMissingSettings(block, other), ['temperature_mode'], 'whatever key the server lists is checked, not only model');
  assert.match(I18n.t('bpmn.warning_empty_setting', { names: processMissingSettings(block, other).join(', ') }), /temperature_mode/);
  assert.deepEqual(processNodeWarnings(block), []);
  assert.equal(processServiceKind(tool.config), 'tool');
  graph.destroy(); config.destroy();
});

test('an agent step is shown as an agent and the implementation control cannot replace its settings', async () => {
  const graph = canvas();
  const stored = { agentId: 'agent-1', goalExpression: '"Summarise"', allowedTools: ['core.skill_view'], maxIterations: 5 };
  graph.addNodeFromTemplate({ ...processTemplates().find((row) => row.node_type === 'bpmn_service_task'), preset: { implementation: { Agent: structuredClone(stored) } } }, 300, 300);
  const agent = graph.nodes.at(-1);
  assert.equal(processServiceKind(agent.config), 'agent');
  const marker = graph.nodesLayer.querySelector(`[data-node-id="${agent.id}"] .fb-process-kind`);
  assert.equal(marker.dataset.kind, 'agent');
  assert.equal(marker.getAttribute('aria-label'), I18n.t('bpmn.kind_agent'));

  const config = inspector(graph); config.show(agent, graph.templates.get(agent.type));
  await flush(2);
  const select = config.root.querySelector('[data-process="implementation"]');
  assert.equal(select.getAttribute('value'), 'agent', 'not shown as a Flow task');
  assert.equal(select.hasAttribute('disabled'), true);
  assert.match(config.root.querySelector('[data-agent-summary]').textContent, /agent-1/);
  assert.equal(config.root.querySelector('[data-process="flowId"]'), null);

  for (const choice of ['flow', 'tool', 'block']) change(select, choice);
  assert.deepEqual(agent.config.implementation.Agent, stored, 'the stored settings are kept');
  assert.equal(processServiceKind(agent.config), 'agent');
  graph.destroy(); config.destroy();
});

test('a new Call activity says it needs an exact version instead of failing at save', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_call_activity'), 200, 200);
  const node = graph.nodes.at(-1);
  assert.deepEqual(processNodeWarnings(node), ['call_version']);
  assert.equal(graph.nodesLayer.querySelector(`[data-node-id="${node.id}"] .fb-process-warn`).getAttribute('data-warning'), 'call_version');
  const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  const alert = config.root.querySelector('[data-call-version-warning]');
  assert.equal(alert.hidden, false);
  assert.equal(alert.getAttribute('message'), I18n.t('bpmn.warning_call_version'));
  graph.updateNodeConfig(node.id, { calledDefinitionId: '8d0e1c0e-0000-4000-8000-000000000001', calledVersion: 2,
    calledElement: { namespaceUri: 'urn:x', processId: 'P' } });
  assert.deepEqual(processNodeWarnings(node), []);
  config.show(node, graph.templates.get(node.type));
  assert.equal(config.root.querySelector('[data-call-version-warning]').hidden, true);
  // A local body needs no version at all.
  graph.updateNodeConfig(node.id, { localBody: { namespaceUri: 'urn:x', processId: '' } });
  assert.deepEqual(processNodeWarnings(node), []);
  graph.destroy(); config.destroy();
});

test('service inspector offers the block types the server lists, not a built-in list', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1);
  const mountWith = async (blocks) => {
    const root = document.createElement('aside'); document.body.append(root);
    const config = new FlowConfig(root, { mode: 'bpmn', processOptions: { ...options, serviceBlocks: blocks }, getCanvas: () => graph,
      onConfigChange: (id, value) => graph.updateNodeConfig(id, value), onEdgeChange: () => {}, onLabelChange: () => {}, onDelete: () => {}, onDuplicate: () => {} });
    config.show(node, graph.templates.get(node.type));
    await flush(2);
    return config;
  };
  const none = await mountWith([]);
  assert.deepEqual([...none.root.querySelectorAll('[data-process="implementation"] option')].map((option) => option.value), ['flow', 'tool']);
  change(none.root.querySelector('[data-process="implementation"]'), 'block');
  assert.equal(node.config.implementation?.Block, undefined, 'no block can be chosen when the server lists none');
  const only = await mountWith([{ nodeType: 'pii_filter', label: 'Personal data filter', description: '', category: 'filter', requiredSettings: [] }]);
  change(only.root.querySelector('[data-process="implementation"]'), 'block');
  await flush(2);
  assert.deepEqual(node.config.implementation, { Block: { nodeType: 'pii_filter', config: {} } });
  assert.deepEqual([...only.root.querySelectorAll('[data-process="blockType"] option')].map((option) => option.value), ['pii_filter']);
  // A saved block the server no longer lists stays visible, disabled.
  const stale = await mountWith([{ nodeType: 'llm', label: 'Language model (LLM)', description: '', category: 'service', requiredSettings: ['model'] }]);
  const staleOption = stale.root.querySelector('[data-process="blockType"] option[value="pii_filter"]');
  assert.ok(staleOption?.hasAttribute('disabled'));
  graph.destroy(); none.destroy(); only.destroy(); stale.destroy();
});

test('choosing a block keeps a payload mapping the author already wrote', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 200, 200);
  const node = graph.nodes.at(-1); const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  change(config.root.querySelector('[data-process="inputMapping"]'), { payload: 'vars.question' });
  change(config.root.querySelector('[data-process="implementation"]'), 'block');
  await flush(2);
  assert.equal(node.config.inputMapping.payload, 'vars.question');
  graph.destroy(); config.destroy();
});

test('activity IO inspector authors ordered inputs, outputs, associations and repetition coordinator', () => {
  const model = emptyProcessModel();
  model.variables = { mapped: null };
  model.modeling = {
    laneSets: [], dataObjects: [{ id: 'Object_1', name: 'Result' }],
    dataObjectReferences: [{ id: 'Ref_1', name: 'Result reference', dataObjectRef: 'Object_1', variableBindingKey: 'mapped' }],
    textAnnotations: [], associations: [], dataStoreReferences: [],
  };
  model.nodes.splice(1, 0, { id: 'Service_1', name: 'Check', kind: { ServiceTask: {
    flowId: 'flow-one', inputMapping: {}, outputMapping: {}, verification: 'Human', timeoutSeconds: 60,
  } }, repeat: { MultiInstance: { mode: 'Sequential', input: { Cardinality: { count: 2 } }, outputCollectionVariable: 'mapped' } } });
  model.sequenceFlows[0].targetId = 'Service_1';
  model.sequenceFlows.push({ id: 'Service_exit', sourceId: 'Service_1', targetId: 'End', condition: null });
  model.diagram.shapes = [
    { elementId: 'Start', x: 80, y: 160, width: 56, height: 56 },
    { elementId: 'Service_1', x: 220, y: 140, width: 240, height: 96 },
    { elementId: 'End', x: 560, y: 160, width: 56, height: 56 },
  ];
  model.diagram.edges = model.sequenceFlows.map((edge) => ({ sequenceFlowId: edge.id,
    waypoints: [{ x: 100, y: 180 }, { x: 600, y: 180 }] }));
  const graph = canvas(model);
  const node = graph.nodes.find((candidate) => candidate.id === 'Service_1');
  const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  click(config.root.querySelector('[data-activity-action="enable"]'));
  click(config.root.querySelector('[data-activity-action="add-input"]'));
  click(config.root.querySelector('[data-activity-action="add-output"]'));
  assert.equal(node.activityIo.dataInputs.length, 1);
  assert.equal(node.activityIo.dataOutputs.length, 1);
  const inputId = node.activityIo.dataInputs[0].id;
  const outputId = node.activityIo.dataOutputs[0].id;
  change(config.root.querySelector('[data-activity-row="input"] [data-activity-field="name"]'), 'Payload');
  change(config.root.querySelector('[data-activity-row="output"] [data-activity-field="expression"]'), 'outputs.answer');
  click(config.root.querySelector('[data-activity-action="add-input-association"]'));
  click(config.root.querySelector('[data-activity-action="add-output-association"]'));
  change(config.root.querySelector('[data-activity-row="input-association"] [data-activity-field="target-input"]'), inputId);
  change(config.root.querySelector('[data-activity-row="input-association"] [data-activity-field="from-expression"]'), 'vars.source');
  change(config.root.querySelector('[data-activity-row="output-association"] [data-activity-field="source-output"]'), outputId);
  change(config.root.querySelector('[data-activity-row="output-association"] [data-activity-field="target-object"]'), 'Ref_1');
  const coordinator = config.root.querySelector('[data-activity-field="coordinator-enabled"]');
  coordinator.checked = true;
  coordinator.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  click(config.root.querySelector('[data-activity-action="add-coordinator-output"]'));
  assert.equal(node.activityIo.dataInputs[0].name, 'Payload');
  assert.equal(node.activityIo.dataOutputs[0].valueExpression, 'outputs.answer');
  assert.deepEqual(node.activityIo.inputAssociations[0].CelAssignment, {
    id: node.activityIo.inputAssociations[0].CelAssignment.id,
    fromExpression: 'vars.source', targetInputId: inputId,
  });
  assert.deepEqual(node.activityIo.outputAssociations[0], {
    id: node.activityIo.outputAssociations[0].id, sourceOutputId: outputId, targetObjectRefId: 'Ref_1',
  });
  assert.equal(node.activityIo.inputSet[0], inputId);
  assert.equal(node.activityIo.outputSet[0], outputId);
  assert.equal(node.activityIo.coordinatorOutput.dataOutputs.length, 1);
  const saved = graph.getData().nodes.find((candidate) => candidate.id === 'Service_1');
  assert.equal(saved.activityIo.coordinatorOutput.outputSet.length, 1);
  config.destroy(); graph.destroy();
});

test('script inspector keeps a direct CEL body and explicit mapping through mounted Save', async () => {
  const model = emptyProcessModel();
  model.variables.answer = null;
  model.nodes.splice(1, 0, { id: 'Script_1', name: 'Calculate',
    kind: { ScriptTask: { script: '', outputMapping: {} } } });
  model.sequenceFlows[0].targetId = 'Script_1';
  model.sequenceFlows.push({ id: 'ScriptExit', sourceId: 'Script_1', targetId: 'End', condition: null });
  const state = await mount(definition('script-draft', { model }), {
    processDefinitionSaveRequest: (payload) => ({ definition: definition('script-draft',
      { model: payload.model, draftRevision: 5 }) }),
  });
  state.canvas.selectNode('Script_1');
  await flush(2);
  const script = state.root.querySelector('[data-process="script"]');
  const mapping = state.root.querySelector('[data-process="outputMapping"]');
  assert.equal(script.tagName, 'TF-TEXTAREA');
  assert.ok(script.hasAttribute('autogrow'));
  assert.equal(mapping.tagName, 'TF-KEYVALUE-EDITOR');
  assert.ok(state.root.querySelector('[data-process="repeatMode"]'));
  change(script, 'vars.amount + 1');
  change(mapping, { answer: 'outputs' });
  const expected = { ScriptTask: { script: 'vars.amount + 1', outputMapping: { answer: 'outputs' } } };
  assert.deepEqual(state.canvas.getData().nodes.find((node) => node.id === 'Script_1').kind, expected);
  state.canvas.undo();
  assert.notDeepEqual(state.canvas.getData().nodes.find((node) => node.id === 'Script_1').kind, expected);
  state.canvas.redo();
  assert.deepEqual(state.canvas.getData().nodes.find((node) => node.id === 'Script_1').kind, expected);
  assert.equal(await builder._save(), true);
  const request = calls.find((row) => row.kind === 'processDefinitionSaveRequest');
  assert.deepEqual(request.payload.model.nodes.find((node) => node.id === 'Script_1').kind, expected);
  state.canvas.selectNode('Script_1');
  const readOnly = inspector(state.canvas, true);
  readOnly.show(state.canvas.nodes.find((node) => node.id === 'Script_1'), state.canvas.templates.get('bpmn_script_task'));
  assert.equal(readOnly.root.querySelector('[data-process="script"]').hasAttribute('disabled'), true);
  assert.equal(readOnly.root.querySelector('[data-process="outputMapping"]').hasAttribute('disabled'), true);
  readOnly.destroy();
});

test('repeated Script inspector saves an editable MI draft and pins empty mapping before loop V2', async () => {
  calls.length = 0;
  const model = emptyProcessModel();
  model.variables = { results: [], items: [null, 7, { case_key: 'Łódź <&>' }], step: 0 };
  model.nodes.splice(1, 0, { id: 'Script_1', name: 'Calculate <&> Łódź',
    kind: { ScriptTask: { script: 'repeat.item', outputMapping: {} } } });
  model.sequenceFlows[0].targetId = 'Script_1';
  model.sequenceFlows.push({ id: 'ScriptExit', sourceId: 'Script_1', targetId: 'End', condition: null });
  const current = definition('script-repeat-draft', { model });
  let draftRevision = 4, publishedVersion = 0, pinnedV1;
  const state = await mount(current, {
    processDefinitionSaveRequest: (payload) => ({ definition: { ...current,
      draftRevision: ++draftRevision, publishedVersion: publishedVersion || null, model: payload.model } }),
    processDefinitionPublishRequest: () => {
      const published = structuredClone(state.canvas.getData());
      publishedVersion += 1;
      if (publishedVersion === 1) pinnedV1 = structuredClone(published);
      return { definition: { ...current, draftRevision, publishedVersion, model: published },
        version: { version: publishedVersion, model: published } };
    },
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: { version: { version: 1, model: null }, startCatalog: [] },
  });
  state.canvas.selectNode('Script_1'); await flush(2);
  const body = state.root.querySelector('[data-process="script"]');
  assert.equal(body.tagName, 'TF-TEXTAREA');
  change(state.root.querySelector('[data-process="repeatMode"]'), 'mi_parallel'); await flush(2);
  change(state.root.querySelector('[data-process="repeatInputMode"]'), 'collection'); await flush(2);
  change(state.root.querySelector('[data-process="repeatCollection"]'), 'vars.items');
  change(state.root.querySelector('[data-process="repeatOutput"]'), 'results');
  change(state.root.querySelector('[data-process="outputMapping"]'), { step: 'outputs' });
  assert.ok(state.root.textContent.includes(I18n.t('bpmn.repeat_script_mi_meaning')));
  assert.equal(await builder._save(), true);
  assert.deepEqual(calls.filter((row) => row.kind === 'processDefinitionSaveRequest').at(-1)
    .payload.model.nodes.find((node) => node.id === 'Script_1').kind.ScriptTask.outputMapping,
    { step: 'outputs' });
  change(state.root.querySelector('[data-process="outputMapping"]'), {});
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(publishedVersion, 1);
  assert.deepEqual(pinnedV1.nodes.find((node) => node.id === 'Script_1').kind.ScriptTask,
    { script: 'repeat.item', outputMapping: {} });
  assert.deepEqual(pinnedV1.nodes.find((node) => node.id === 'Script_1').repeat,
    { MultiInstance: { mode: 'Parallel', input: { CollectionExpression: { expression: 'vars.items' } },
      outputCollectionVariable: 'results' } });
  state.canvas.selectNode('Script_1'); await flush(2);
  change(state.root.querySelector('[data-process="repeatMode"]'), 'loop'); await flush(2);
  change(state.root.querySelector('[data-process="repeatCondition"]'), 'vars.step < 3');
  change(state.root.querySelector('[data-process="repeatMaximum"]'), '32');
  change(state.root.querySelector('[data-process="repeatOutput"]'), 'results');
  change(state.root.querySelector('[data-process="outputMapping"]'), { step: 'outputs' });
  assert.ok(state.root.textContent.includes(I18n.t('bpmn.repeat_script_loop_meaning')));
  assert.equal(await builder._save(), true);
  await builder._publish();
  assert.equal(publishedVersion, 2);
  assert.deepEqual(pinnedV1.nodes.find((node) => node.id === 'Script_1').kind.ScriptTask.outputMapping, {});
  assert.equal(state.canvas.getData().nodes.find((node) => node.id === 'Script_1').repeat.StructuredLoop.maxIterations, 32);
  fixtures({ processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: { version: { version: 1, model: pinnedV1 }, startCatalog: [] } });
  await builder._openProcessVersions();
  click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
  await flush();
  assert.equal(state.canvas.readOnly, true);
  state.canvas.selectNode('Script_1'); await flush(2);
  assert.equal(state.root.querySelector('[data-process="script"]').value, 'repeat.item');
  assert.equal(state.root.querySelector('[data-process="repeatMode"]').value, 'mi_parallel');
  for (const field of ['script', 'outputMapping', 'repeatMode', 'repeatCollection', 'repeatOutput']) {
    assert.equal(state.root.querySelector(`[data-process="${field}"]`).hasAttribute('disabled'), true);
  }
  assert.deepEqual(state.canvas.getData().nodes.find((node) => node.id === 'Script_1'),
    pinnedV1.nodes.find((node) => node.id === 'Script_1'));
});

test('repeat inspector persists MI and loop metadata outside task config and renders the standard marker', async () => {
  const graph = canvas();
  graph.processModel.variables.results = [];
  graph.processModel.variables.items = [{ case_ID: 'A' }];
  const node = userNode(graph);
  const config = inspector(graph); config.show(node, graph.templates.get(node.type));
  await flush(2);
  assert.equal(node.repeat, undefined);
  change(config.root.querySelector('[data-process="repeatMode"]'), 'mi_sequential');
  await flush(2);
  assert.deepEqual(node.repeat, { MultiInstance: { mode: 'Sequential', input: { Cardinality: { count: 1 } }, outputCollectionVariable: '' } });
  change(config.root.querySelector('[data-process="repeatCount"]'), '16');
  change(config.root.querySelector('[data-process="repeatCount"]'), '');
  assert.equal(config.root.querySelector('[data-process="repeatCount"]').value, '16');
  change(config.root.querySelector('[data-process="repeatOutput"]'), 'results');
  assert.equal(node.repeat.MultiInstance.input.Cardinality.count, 16);
  assert.equal(node.config.repeat, undefined);
  assert.ok(graph.root.querySelector('.fb-repeat-marker-sequential'));
  const saved = canvasToProcess(graph.processModel, graph.nodes, graph.edges, (edge) => edge.waypoints);
  assert.deepEqual(saved.nodes.find((entry) => entry.id === node.id).repeat, node.repeat);
  change(config.root.querySelector('[data-process="repeatInputMode"]'), 'collection');
  await flush(2);
  change(config.root.querySelector('[data-process="repeatCollection"]'), 'vars.items');
  assert.equal(node.repeat.MultiInstance.input.CollectionExpression.expression, 'vars.items');
  change(config.root.querySelector('[data-process="repeatMode"]'), 'mi_parallel');
  await flush(2);
  assert.equal(node.repeat.MultiInstance.mode, 'Parallel');
  assert.ok(graph.root.querySelector('.fb-repeat-marker-parallel'));
  change(config.root.querySelector('[data-process="repeatMode"]'), 'loop');
  await flush(2);
  assert.equal(node.repeat.StructuredLoop.testBefore, false);
  change(config.root.querySelector('[data-process="repeatCondition"]'), 'vars.again');
  change(config.root.querySelector('[data-process="repeatMaximum"]'), '32');
  change(config.root.querySelector('[data-process="repeatMaximum"]'), '');
  assert.equal(config.root.querySelector('[data-process="repeatMaximum"]').value, '32');
  change(config.root.querySelector('[data-process="repeatOutput"]'), 'results');
  assert.equal(node.repeat.StructuredLoop.maxIterations, 32);
  assert.ok(graph.root.querySelector('.fb-repeat-marker-loop'));
  const readonly = inspector(graph, true); readonly.show(node, graph.templates.get(node.type));
  for (const field of ['repeatMode', 'repeatCondition', 'repeatMaximum', 'repeatOutput']) {
    assert.ok(readonly.root.querySelector(`[data-process="${field}"]`).hasAttribute('disabled'));
  }
  change(config.root.querySelector('[data-process="repeatMode"]'), 'off');
  assert.equal(node.repeat, undefined);
  assert.equal(graph.root.querySelector('.fb-repeat-marker'), null);
  graph.destroy(); config.destroy(); readonly.destroy();
});

test('repeat inspector changes survive the mounted draft save and undo/redo snapshots', async () => {
  const model = emptyProcessModel();
  model.variables.results = [];
  model.nodes.splice(1, 0, { id: 'RepeatWork', name: 'Review',
    kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } });
  model.sequenceFlows[0].targetId = 'RepeatWork';
  model.sequenceFlows.push({ id: 'RepeatExit', sourceId: 'RepeatWork', targetId: 'End', condition: null });
  const state = await mount(definition('repeat-draft', { model }), {
    processDefinitionSaveRequest: (payload) => ({ definition: definition('repeat-draft', { model: payload.model, draftRevision: 5 }) }),
  });
  state.canvas.selectNode('RepeatWork');
  await flush(2);
  change(state.root.querySelector('[data-process="repeatMode"]'), 'mi_parallel');
  await flush(2);
  change(state.root.querySelector('[data-process="repeatCount"]'), '0');
  change(state.root.querySelector('[data-process="repeatOutput"]'), 'results');
  const expected = { MultiInstance: { mode: 'Parallel', input: { Cardinality: { count: 0 } }, outputCollectionVariable: 'results' } };
  assert.deepEqual(state.canvas.getData().nodes.find((node) => node.id === 'RepeatWork').repeat, expected);
  state.canvas.undo();
  assert.notDeepEqual(state.canvas.getData().nodes.find((node) => node.id === 'RepeatWork').repeat, expected);
  state.canvas.redo();
  assert.deepEqual(state.canvas.getData().nodes.find((node) => node.id === 'RepeatWork').repeat, expected);
  assert.equal(await builder._save(), true);
  const request = calls.find((row) => row.kind === 'processDefinitionSaveRequest');
  assert.deepEqual(request.payload.model.nodes.find((node) => node.id === 'RepeatWork').repeat, expected);
});

test('eight repeated activities retain outer boundaries and exact pinned version metadata', async () => {
  const user = { UserTask: { assigneeUserId: null, outputMapping: {} } };
  const service = { ServiceTask: { flowId: 'flow-one', inputMapping: {}, outputMapping: {},
    verification: 'Human', timeoutSeconds: 60, resultExpression: 'outputs.result' } };
  const script = { ScriptTask: { script: 'repeat.item', outputMapping: {} } };
  const manual = { ManualTask: { assigneeUserId: null, instructions: 'Check the external register <&>' } };
  const send = { SendTask: { messageRef: 'Message_1',
    target: { Start: { definitionId: '91764f75-dadb-41aa-a252-a8a911fe7a94' } },
    correlationExpression: 'vars.case_key', payloadExpression: 'vars.payload', ttlSeconds: 60 } };
  const receive = { ReceiveTask: { messageRef: 'Message_1',
    correlationExpression: 'vars.case_key', outputMapping: {} } };
  const embedded = structuredClone(embeddedModel().nodes.find((node) => node.id === 'Scope_Review').kind);
  embedded.SubProcess.inputMapping = {};
  embedded.SubProcess.outputMapping = {};
  const call = { CallActivity: { calledDefinitionId: '91764f75-dadb-41aa-a252-a8a911fe7a94',
    calledVersion: 2, calledElement: { namespaceUri: 'urn:review', processId: 'Review_Process' },
    inputMapping: {}, outputMapping: {} } };
  for (const [tag, kind] of Object.entries({ UserTask: user, ServiceTask: service, ScriptTask: script,
    ManualTask: manual, SendTask: send,
    ReceiveTask: receive, SubProcess: embedded, CallActivity: call })) {
    calls.length = 0;
    const model = emptyProcessModel();
    model.timerTimezone = 'Europe/Warsaw';
    model.variables = { results: [], items: [null, { case_key: 'Łódź <&>' }],
      case_key: 'case-1', payload: { case_key: 'case-1' }, more: false };
    model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
    model.nodes.splice(1, 0, { id: 'Repeat_1', name: 'Repeat <&> Łódź', kind });
    model.nodes.push({ id: 'GroupDeadline', name: 'Group deadline', kind: { BoundaryTimer: {
      attachedToId: 'Repeat_1', cancelActivity: true, timer: { Duration: { seconds: 90 } },
    } } }, { id: 'GroupReply', name: 'Group reply', kind: { BoundaryMessage: {
      attachedToId: 'Repeat_1', cancelActivity: false, messageRef: 'Message_1',
      correlationExpression: 'vars.case_key', outputMapping: {},
    } } });
    model.sequenceFlows[0].targetId = 'Repeat_1';
    model.sequenceFlows.push({ id: 'RepeatExit', sourceId: 'Repeat_1', targetId: 'End', condition: null });
    model.sequenceFlows.push({ id: 'DeadlineExit', sourceId: 'GroupDeadline', targetId: 'End', condition: null },
      { id: 'ReplyExit', sourceId: 'GroupReply', targetId: 'End', condition: null });
    const current = definition(`repeat-${tag}`, { model });
    let draftRevision = 4, publishedVersion = 0, pinnedV1;
    const state = await mount(current, {
      processDefinitionSaveRequest: (payload) => {
        draftRevision += 1;
        return { definition: { ...current, draftRevision, publishedVersion: publishedVersion || null,
          model: payload.model } };
      },
      processDefinitionPublishRequest: () => {
        publishedVersion += 1;
        const published = state.canvas.getData();
        if (publishedVersion === 1) pinnedV1 = structuredClone(published);
        return { definition: { ...current, draftRevision, publishedVersion, model: published },
          version: { version: publishedVersion, model: published } };
      },
      processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
      processVersionGetRequest: { version: { version: 1, model: null }, startCatalog: [] },
    });
    state.canvas.selectNode('Repeat_1');
    await flush(2);
    const mode = state.root.querySelector('[data-process="repeatMode"]');
    assert.ok(mode, `${tag} exposes the existing repeat inspector`);
    change(mode, 'mi_sequential');
    await flush(2);
    change(state.root.querySelector('[data-process="repeatInputMode"]'), 'collection');
    await flush(2);
    change(state.root.querySelector('[data-process="repeatCollection"]'), 'vars.items');
    change(state.root.querySelector('[data-process="repeatOutput"]'), 'results');
    const expected = { MultiInstance: { mode: 'Sequential',
      input: { CollectionExpression: { expression: 'vars.items' } },
      outputCollectionVariable: 'results' } };
    assert.deepEqual(state.canvas.getData().nodes.find((node) => node.id === 'Repeat_1').repeat, expected);
    const parent = state.canvas.nodes.find((node) => node.id === 'Repeat_1');
    const ports = [state.canvas._getPortWorldPos(parent.id, 'in', 'in'),
      state.canvas._getPortWorldPos(parent.id, 'full', 'out')];
    for (const id of ['GroupDeadline', 'GroupReply']) {
      state.canvas.updateNodeConfig(id, { attachedToId: null });
      state.canvas.updateNodeConfig(id, { attachedToId: 'Repeat_1' });
      const boundary = state.canvas.nodes.find((node) => node.id === id);
      const center = { x: boundary.x + boundary.width / 2, y: boundary.y + boundary.height / 2 };
      assert.ok(center.x === parent.x || center.x === parent.x + parent.width
        || center.y === parent.y || center.y === parent.y + parent.height);
      assert.ok(ports.every((port) => Math.hypot(center.x - port.x, center.y - port.y) >= boundary.width / 2 + 16));
      state.canvas.selectNode(id);
      await flush(2);
      const attachment = state.root.querySelector('[data-process="attachedToId"]');
      assert.equal(attachment.value, 'Repeat_1');
      assert.equal(attachment.querySelector('option[value="Repeat_1"]').disabled, false);
      assert.equal(state.root.querySelector('[data-boundary-scope]').textContent, I18n.t('bpmn.boundary_repeat_group_hint'));
    }
    const [deadline, reply] = ['GroupDeadline', 'GroupReply'].map((id) => state.canvas.nodes.find((node) => node.id === id));
    assert.ok(Math.hypot(deadline.x - reply.x, deadline.y - reply.y) >= deadline.width);
    assert.equal(await builder._save(), true);
    const saved = calls.filter((row) => row.kind === 'processDefinitionSaveRequest').at(-1);
    assert.deepEqual(saved.payload.model.nodes.find((node) => node.id === 'Repeat_1').repeat, expected);
    await builder._publish();
    assert.equal(publishedVersion, 1);
    assert.deepEqual(pinnedV1.nodes.find((node) => node.id === 'Repeat_1').repeat, expected);
    for (const id of ['GroupDeadline', 'GroupReply']) {
      assert.deepEqual(pinnedV1.nodes.find((node) => node.id === id), model.nodes.find((node) => node.id === id));
    }
    state.canvas.updateNodeConfig('Repeat_1', { repeat: { StructuredLoop: {
      condition: 'vars.more', testBefore: false, maxIterations: 3,
      outputCollectionVariable: 'results' } } });
    assert.equal(await builder._save(), true);
    await builder._publish();
    assert.equal(publishedVersion, 2);
    assert.deepEqual(pinnedV1.nodes.find((node) => node.id === 'Repeat_1').repeat, expected);
    fixtures({ processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
      processVersionGetRequest: { version: { version: 1, model: pinnedV1 }, startCatalog: [] } });
    await builder._openProcessVersions();
    click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
    await flush();
    assert.equal(state.previewVersion, 1);
    assert.equal(state.canvas.readOnly, true);
    assert.deepEqual(state.canvas.getData().nodes.find((node) => node.id === 'Repeat_1').repeat, expected);
    state.canvas.selectNode('Repeat_1');
    const readonlyMode = state.root.querySelector('[data-process="repeatMode"]');
    assert.equal(readonlyMode.getAttribute('value'), 'mi_sequential');
    await flush(2);
    assert.equal(readonlyMode.value, 'mi_sequential');
    assert.equal(readonlyMode.hasAttribute('disabled'), true);
    assert.equal(state.root.querySelector('[data-process="repeatCollection"]').hasAttribute('disabled'), true);
    assert.equal(state.root.querySelector('[data-process="repeatOutput"]').hasAttribute('disabled'), true);
    assert.equal(state.root.querySelector('[data-role="save"]').hasAttribute('disabled'), true);
    state.canvas.selectNode('GroupDeadline');
    await flush(2);
    assert.equal(state.root.querySelector('[data-process="attachedToId"]').hasAttribute('disabled'), true);
    assert.equal(state.root.querySelector('[data-boundary-scope]').textContent, I18n.t('bpmn.boundary_repeat_group_hint'));
    assert.equal(calls.filter((row) => row.kind === 'processDefinitionSaveRequest').length, 2);
  }
});

test('repeated manual and receive waits reject an unattached boundary draft before save', async () => {
  for (const kind of [
    { ManualTask: { assigneeUserId: null, instructions: 'Check the external register' } },
    { ReceiveTask: { messageRef: 'Message_1', correlationExpression: 'vars.case_key', outputMapping: {} } },
  ]) {
    calls.length = 0;
    const model = boundaryModel();
    model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
    model.variables.results = [];
    model.variables.case_key = 'case-1';
    model.nodes.find((node) => node.id === 'Review').kind = kind;
    model.nodes.find((node) => node.id === 'Review').repeat = { MultiInstance: {
      mode: 'Sequential', input: { Cardinality: { count: 2 } }, outputCollectionVariable: 'results' } };
    model.nodes.find((node) => node.id === 'Timer_A').kind.BoundaryTimer.attachedToId = 'MissingActivity';
    const state = await mount(definition('repeat-boundary-draft', { model }));
    assert.ok(state.canvas.validate().includes(I18n.t('bpmn.boundary_required_named', { node: 'Deadline' })), 'the refusal names the boundary event');
    assert.equal(await builder._save(), false);
    assert.equal(calls.some((row) => row.kind === 'processDefinitionSaveRequest'), false);
  }
});

test('five locales explain the selected repeated activity without losing its full name', async () => {
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      for (const [template, meaning] of [
        ['bpmn_script_task', 'script_mi'], ['bpmn_manual_task', 'manual'], ['bpmn_send_task', 'send'],
        ['bpmn_receive_task', 'receive'], ['bpmn_sub_process', 'subprocess'],
        ['bpmn_call_activity', 'call'],
      ]) {
        const graph = canvas();
        graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === template), 250, 300);
        const node = graph.nodes.at(-1);
        const fullName = `R&D <Łódź> ${'Long activity '.repeat(12)}`;
        graph.updateNodeLabel(node.id, fullName);
        const config = inspector(graph);
        config.show(node, graph.templates.get(node.type));
        assert.ok(config.root.querySelector('[data-process="repeatMode"]'));
        assert.equal(config.root.querySelector('.fb-config-title').textContent, fullName);
        assert.ok(config.root.textContent.includes(I18n.t(`bpmn.repeat_${meaning}_meaning`)));
        if (meaning === 'script_mi') {
          change(config.root.querySelector('[data-process="repeatMode"]'), 'loop');
          await flush(2);
          assert.ok(graph.nodes.find((candidate) => candidate.id === node.id).repeat?.StructuredLoop);
          assert.ok(config.root.textContent.includes(I18n.t('bpmn.repeat_script_loop_meaning')));
        }
        assert.equal(config.root.querySelector('.fb-config-title').querySelector('Łódź'), null);
        graph.updateNodeConfig(node.id, { repeat: { MultiInstance: { mode: 'Sequential',
          input: { Cardinality: { count: 2 } }, outputCollectionVariable: 'results' } } });
        graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer'), 500, 300);
        const boundary = graph.nodes.at(-1);
        graph.updateNodeConfig(boundary.id, { attachedToId: node.id });
        config.show(boundary, graph.templates.get(boundary.type));
        const attachment = config.root.querySelector('[data-process="attachedToId"]');
        assert.equal(attachment.getAttribute('value'), node.id);
        await flush(2);
        assert.equal(attachment.value, node.id);
        if (meaning === 'script_mi') {
          const target = config.root.querySelector(`[data-process="attachedToId"] option[value="${node.id}"]`);
          assert.ok(target && !target.disabled, 'A repeated Script has one selectable outer Timer host');
          for (const [type, supported] of [['bpmn_boundary_message', true],
            ['bpmn_boundary_error', false], ['bpmn_boundary_escalation', false]]) {
            graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === type), 500, 380);
            const candidate = graph.nodes.at(-1);
            config.show(candidate, graph.templates.get(candidate.type));
            const option = config.root.querySelector(`[data-process="attachedToId"] option[value="${node.id}"]`);
            assert.equal(!!option && !option.disabled, supported,
              `${type} exposes only a supported repeated Script boundary target`);
          }
          config.show(boundary, graph.templates.get(boundary.type));
        }
        assert.equal(config.root.querySelector('[data-boundary-scope]').textContent, I18n.t('bpmn.boundary_repeat_group_hint'));
        assert.ok(config.root.querySelector('[data-boundary-target]').textContent.includes(fullName));
        config.destroy(); graph.destroy();
      }
    }
  } finally { await I18n.setLanguage('en'); }
});

test('palette offers the supported elements and cancels drag/filter work when disposed', async () => {
  const root = document.createElement('aside'); document.body.append(root); let added = 0;
  const palette = new FlowPalette(root, { mode: 'bpmn', onAdd: () => { added += 1; } }); await palette.init();
  assert.equal(root.querySelectorAll('[data-node-type]').length, 29);
  assert.ok(root.querySelector('[data-node-type="bpmn_link_throw"]'));
  assert.ok(root.querySelector('[data-node-type="bpmn_link_catch"]'));
  assert.ok(root.querySelector('[data-node-type="bpmn_boundary_escalation"]'));
  assert.ok(root.querySelector('[data-node-type="bpmn_manual_task"]'));
  assert.ok(root.querySelector('[data-node-type="bpmn_send_task"]'));
  assert.ok(root.querySelector('[data-node-type="bpmn_receive_task"]'));
  assert.ok(root.querySelector('[data-node-type="bpmn_signal_throw"]'));
  assert.ok(root.querySelector('[data-node-type="bpmn_signal_catch"]'));
  assert.equal(root.querySelector('[data-node-type="bpmn_timer_boundary"]'), null);
  const item = root.querySelector('[data-node-type="bpmn_user_task"]');
  item.dispatchEvent(new window.PointerEvent('pointerdown', { bubbles: true, pointerId: 1, button: 0, clientX: 1, clientY: 1 }));
  window.dispatchEvent(new window.PointerEvent('pointercancel', { pointerId: 1 }));
  assert.equal(added, 0);
  root.querySelector('tf-searchbox').dispatchEvent(new CustomEvent('search', { detail: { value: 'human' }, bubbles: true }));
  palette.destroy(); await new Promise((resolve) => setTimeout(resolve, 140));
  assert.equal(root.childElementCount, 0); assert.equal(palette.filter, '');
});

test('command retries retain identity for the same intent and changed values obtain a new identity', () => {
  const command = processCommand();
  const first = command({ expectedRevision: 4, variables: { Source_ID: 'A' } });
  assert.deepEqual(command({ expectedRevision: 4, variables: { Source_ID: 'A' } }), first);
  assert.notEqual(command({ expectedRevision: 4, variables: { Source_ID: 'B' } }).commandId, first.commandId);
});

test('message send retry retains the durable message identity until its content changes', async () => {
  let attempts = 0;
  fixtures({ processMessageSendRequest: (payload) => {
    if (++attempts < 3) throw new Error('Connection lost');
    return { message: { status: 'Delivered' } };
  } });
  const form = openProcessMessageSend({ Start: { definitionId: 'definition-one' } }, ['order.received']);
  assert.ok(form.querySelector('[data-message-name]').hasAttribute('wrap-selected'));
  form.querySelector('[data-message-key]').value = 'case-1';
  const editor = form.querySelector('tf-code-editor');
  editor.value = '{"customer_ID":null}';
  click(form.querySelector('[data-act="submit"]')); await flush();
  click(form.querySelector('[data-act="submit"]')); await flush();
  editor.value = '{"customer_ID":true}';
  click(form.querySelector('[data-act="submit"]')); await flush();
  const sends = calls.filter((call) => call.kind === 'processMessageSendRequest').map((call) => call.payload);
  assert.equal(sends.length, 3);
  assert.equal(sends[0].messageId, sends[1].messageId);
  assert.equal(sends[0].commandId, sends[1].commandId);
  assert.notEqual(sends[1].messageId, sends[2].messageId);
  assert.notEqual(sends[1].commandId, sends[2].commandId);
  assert.deepEqual(sends[2].payload, { customer_ID: true });
});

test('message detail distinguishes an available JSON null from an unavailable payload', async () => {
  const summary = { messageId: 'message-1', senderUserId: 'owner', messageName: '<order.received>',
    correlationKey: 'case-1', status: 'Delivered', revision: 1, payloadAvailable: true,
    canResolve: false, canCancel: false };
  fixtures({ processMessageGetRequest: { message: { message: summary, payload: null } } });
  const available = await openProcessMessageDetail(summary);
  assert.equal(available.querySelector('tf-code-editor').value, 'null');
  assert.equal(available.querySelector('img'), null);
  assert.match(available.textContent, /<order\.received>/);
  available.remove();
  fixtures({ processMessageGetRequest: { message: { message: { ...summary, payloadAvailable: false } } } });
  const unavailable = await openProcessMessageDetail(summary);
  assert.equal(unavailable.querySelector('tf-code-editor'), null);
  assert.ok(unavailable.textContent.includes(I18n.t('bpmn.message_payload_unavailable')));
});

test('revoked message read clears the previously authorized payload and actions on refresh', async () => {
  const summary = { messageId: 'message-1', senderUserId: 'owner', messageName: 'order.received',
    correlationKey: 'case-1', status: 'Pending', revision: 1, payloadAvailable: true,
    canResolve: true, canCancel: true };
  let readable = true;
  fixtures({ processMessageGetRequest: () => {
    if (!readable) throw new Error('Current message access was revoked');
    return { message: { message: summary, payload: { private_ID: 'secret' } } };
  } });
  const win = await openProcessMessageDetail(summary);
  assert.match(win.querySelector('tf-code-editor').value, /private_ID/);
  readable = false;
  await poll(); await flush();
  assert.equal(win.querySelector('tf-code-editor'), null);
  assert.equal(win.querySelector('[data-resolve]'), null);
  assert.equal(win.querySelector('[data-cancel-message]'), null);
  assert.match(win.querySelector('[data-error]').getAttribute('message'), /revoked/);
});

test('message detail clears an action error only after an explicit successful action and current read', async () => {
  const summary = { messageId: 'message-1', senderUserId: 'owner', messageName: 'order.received',
    correlationKey: 'case-1', status: 'Pending', revision: 1, payloadAvailable: true,
    canResolve: false, canCancel: true };
  let current = summary;
  let cancelAttempts = 0;
  let readFails = false;
  fixtures({
    processMessageGetRequest: () => {
      if (readFails) throw new Error('Current message read failed');
      return { message: { message: current, payload: { Purchase_ID: 'PO-7' } } };
    },
    processMessageCancelRequest: () => {
      if (++cancelAttempts === 1) throw new Error('Cancel was rejected');
      current = { ...summary, status: 'Cancelled', revision: 2, canCancel: false };
      return { message: current };
    },
  });
  const win = await openProcessMessageDetail(summary);
  click(win.querySelector('[data-cancel-message]')); await flush();
  assert.equal(win.querySelector('[data-error]').hidden, false);
  assert.match(win.querySelector('[data-error]').getAttribute('message'), /Cancel was rejected/);
  click(win.querySelector('[data-cancel-message]')); await flush();
  assert.equal(win.querySelector('[data-error]').hidden, true);
  assert.equal(win.querySelector('[data-cancel-message]'), null);
  assert.match(win.querySelector('tf-code-editor').value, /Purchase_ID/);
  readFails = true;
  await poll(); await flush();
  assert.equal(win.querySelector('[data-error]').hidden, false);
  assert.match(win.querySelector('[data-error]').getAttribute('message'), /Current message read failed/);
  readFails = false;
  await poll(); await flush();
  assert.equal(win.querySelector('[data-error]').hidden, false);
  assert.match(win.querySelector('[data-error]').getAttribute('message'), /Current message read failed/);
  assert.equal(calls.filter((call) => call.kind === 'processMessageCancelRequest').length, 2);
  assert.equal(calls.filter((call) => call.kind === 'processMessageGetRequest').length, 4);
});

test('message resolution ignores stale subscription pages after the target instance changes', async () => {
  const delayed = deferred();
  const summary = { messageId: 'message-1', senderUserId: 'owner', messageName: 'order.received',
    correlationKey: 'case-1', status: 'Ambiguous', revision: 1, payloadAvailable: false,
    canResolve: true, canCancel: true, target: { Catch: { definitionId: 'definition-one', instanceId: null, subscriptionId: null } } };
  const response = (instanceId) => ({ instance: instance(instanceId, {
    subscriptions: [{ subscriptionId: `subscription-${instanceId}`, nodeId: 'Wait', nodeName: `Wait ${instanceId}`,
      status: 'Open', messageName: 'order.received', correlationKey: 'case-1' }],
  }) });
  fixtures({ processMessageGetRequest: { message: { message: summary } },
    processInstanceGetRequest: ({ instanceId }) => instanceId === 'A' ? delayed.promise : response(instanceId) });
  const detail = await openProcessMessageDetail(summary);
  click(detail.querySelector('[data-resolve]'));
  const form = document.querySelector('.tf-act-window');
  const target = form.querySelector('[data-resolve-instance]');
  change(target, 'A'); click(form.querySelector('[data-load-subscriptions]'));
  change(target, 'B'); click(form.querySelector('[data-load-subscriptions]'));
  await flush();
  const choices = form.querySelector('[data-resolve-subscription]');
  assert.ok(choices.hasAttribute('wrap-selected'));
  assert.ok(choices.querySelector('option[value="subscription-B"]'));
  delayed.resolve(response('A')); await flush();
  assert.equal(choices.querySelector('option[value="subscription-A"]'), null);
  assert.ok(choices.querySelector('option[value="subscription-B"]'));
  change(target, 'C');
  assert.equal(choices.querySelector('option[value="subscription-B"]'), null);
  assert.ok(form.querySelector('[data-act="submit"]').hasAttribute('disabled'));
});

test('the resolve picker names each open subscription by its step and never by its id', async () => {
  const summary = { messageId: 'message-2', senderUserId: 'owner', messageName: 'order.received',
    correlationKey: 'case-1', status: 'Ambiguous', revision: 1, payloadAvailable: false,
    canResolve: true, canCancel: true, target: { Catch: { definitionId: 'definition-one', instanceId: null, subscriptionId: null } } };
  const open = (id, nodeName) => ({ subscriptionId: id, nodeId: 'Wait', nodeName, status: 'Open', messageName: 'order.received', correlationKey: 'case-1' });
  fixtures({ processMessageGetRequest: { message: { message: summary } },
    processInstanceGetRequest: { instance: instance('A', { subscriptions: [open('11111111-2222-4333-8444-555555555555', 'Wait for payment'),
      open('66666666-7777-4888-8999-000000000000', 'Wait for payment'), open('aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee', '')] }) } });
  const detail = await openProcessMessageDetail(summary);
  click(detail.querySelector('[data-resolve]'));
  const form = document.querySelector('.tf-act-window');
  change(form.querySelector('[data-resolve-instance]'), 'A'); click(form.querySelector('[data-load-subscriptions]'));
  await flush();
  const labels = [...form.querySelectorAll('[data-resolve-subscription] option')].filter((option) => option.value).map((option) => option.textContent);
  assert.deepEqual(labels, ['Wait for payment (1)', 'Wait for payment (2)', I18n.t('bpmn.element_unavailable')]);
  assert.doesNotMatch(labels.join('\n'), ID_SHAPE);
});

test('incidents of one repeated task are told apart by their occurrence number', async () => {
  const incident = (id) => ({ incidentId: id, nodeId: 'Flow_task', nodeName: 'Boom flow', code: 'FLOW_FAILED', message: 'failed', jobId: null, canRetry: false });
  const win = await monitor(instance('twin-incidents', { status: 'Incident', incidents: [incident('inc-a'), incident('inc-b')] }));
  const titles = [...win.querySelectorAll('[data-incidents] .fb-process-work strong')].map((title) => title.textContent);
  assert.deepEqual(titles, ['Boom flow (1)', 'Boom flow (2)']);
});

test('message receipts and cancellation history localize finite reasons while preserving diagnostics', async () => {
  const reasons = [
    ['sender_cancelled', 'reason_sender_cancelled'],
    ['ttl_expired', 'reason_ttl_expired'],
    ['activation_closed', 'reason_activation_closed'],
    ['target_instance_closed', 'reason_target_instance_closed'],
  ];
  const markup = '<script>kept as text</script>';
  const diagnostic = `${'x'.repeat(32768 - markup.length)}${markup}`;
  const eventWaitLabels = {
    en: 'Escalate: event wait cancelled — ',
    pl: 'Escalate: anulowano oczekiwanie na zdarzenie — ',
    de: 'Escalate: Warten auf Ereignis beendet — ',
    es: 'Escalate: espera de evento cancelada — ',
    fr: 'Escalate : attente de l’événement annulée — ',
  };
  assert.equal(new TextEncoder().encode(diagnostic).length, 32768);
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      for (const [reason, key] of reasons) {
        const summary = { messageId: 'message-1', senderUserId: 'owner', messageName: 'order.received',
          correlationKey: 'case-1', status: 'Cancelled', revision: 2, payloadAvailable: false,
          canResolve: false, canCancel: false, lastReason: reason };
        fixtures({ processMessageGetRequest: { message: { message: summary } } });
        const win = await openProcessMessageDetail(summary);
        const label = I18n.t(`bpmn.${key}`);
        assert.notEqual(label, `bpmn.${key}`);
        assert.ok(win.querySelector('[data-content]').textContent.includes(label));
        assert.ok(!win.querySelector('[data-content]').textContent.includes(reason));
        win.remove();
        assert.equal(processLifecycleReasonText(reason), label);
        const event = { kind: 'message_cancelled', nodeName: 'Send',
          data: { message_name: 'order.received', reason } };
        const original = structuredClone(event);
        assert.ok(processEventText(event).includes(label));
        assert.deepEqual(event, original);
      }
      for (const [kind, reason, key] of [
        ['event_race_cancelled', 'instance_cancelled', 'event_cancelled'],
        ['subscription_cancelled', 'activity_completed', 'timer_reason_activity_completed'],
        ['subscription_cancelled', 'event_race_lost', 'timer_reason_event_race_lost'],
      ]) {
        const event = { kind, nodeName: 'Wait', data: { reason } };
        const label = I18n.t(`bpmn.${key}`);
        const rendered = processEventText(event);
        assert.ok(rendered.includes(label));
        assert.doesNotMatch(rendered, /bpmn\.|\{node\}|\{reason\}/);
      }
      const escalationCancellation = { kind: 'subscription_cancelled', nodeName: 'Escalate',
        data: { subscription_id: 'escalation-any', reason: 'activity_completed' } };
      const originalCancellation = structuredClone(escalationCancellation);
      const cancellation = processEventText(escalationCancellation);
      assert.equal(cancellation, `${eventWaitLabels[language]}${I18n.t('bpmn.timer_reason_activity_completed')}`);
      assert.doesNotMatch(cancellation, /message|mensaje|Nachricht|wiadomość/i);
      assert.deepEqual(escalationCancellation, originalCancellation);
      const summary = { messageId: 'message-2', senderUserId: 'owner', messageName: 'order.received',
        correlationKey: 'case-2', status: 'Error', revision: 3, payloadAvailable: false,
        canResolve: false, canCancel: false, lastReason: diagnostic };
      fixtures({ processMessageGetRequest: { message: { message: summary } } });
      const win = await openProcessMessageDetail(summary);
      assert.ok(win.querySelector('[data-content]').textContent.includes(diagnostic));
      assert.equal(win.querySelector('[data-content] script'), null);
      win.remove();
      assert.equal(processLifecycleReasonText(diagnostic), diagnostic);
      assert.ok(processEventText({ kind: 'message_error', nodeName: 'Send', data: {
        message_name: 'order.received', reason: diagnostic,
      } }).includes(diagnostic));
    }
  } finally { await I18n.setLanguage('en'); }
});

test('five locales describe message and business-error outcomes without losing real event fields', async () => {
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      for (const key of ['node_message_start', 'node_message_catch', 'node_message_throw',
        'node_boundary_message', 'node_boundary_error', 'node_boundary_escalation',
        'escalation_declarations', 'escalation_reference', 'escalation_any_code',
        'incident_escalation_guidance', 'node_event_based_gateway',
        'message_status_ambiguous', 'subscription_status_open', 'race_status_won']) {
        assert.notEqual(I18n.t(`bpmn.${key}`), `bpmn.${key}`);
      }
      const queued = processEventText({ kind: 'message_queued', nodeName: 'Send',
        data: { message_name: 'order.received', correlation_key: 'Case_ID' } });
      assert.match(queued, /order\.received/);
      assert.match(queued, /Case_ID/);
      const race = processEventText({ kind: 'event_race_won', nodeName: 'First event',
        data: { winner_node_id: 'Catch_1' } });
      assert.match(race, /Catch_1/);
      const winnerLabel = I18n.t('bpmn.race_winning_element_id');
      assert.notEqual(winnerLabel, 'bpmn.race_winning_element_id');
      assert.ok(race.toLocaleLowerCase(language).includes(winnerLabel.toLocaleLowerCase(language)));
      const raceWindow = await monitor(instance(`race-${language}`, { eventRaces: [{
        raceId: 'race-1', gatewayNodeId: 'Gateway_1', gatewayName: 'First event', status: 'Won',
        winnerNodeId: 'Catch_1', branchSubscriptionIds: [], branchTimerIds: [],
      }] }));
      const raceRow = raceWindow.querySelector('[data-race-rows]');
      assert.ok(raceRow.textContent.includes(`${winnerLabel}: Catch_1`));
      assert.equal(raceRow.querySelector('script'), null);
      raceWindow.dispatchEvent(new Event('closed'));
      raceWindow.remove();
      const escalationWindow = await monitor(instance(`escalation-${language}`, {
        status: 'Incident', incidents: [{ incidentId: 'incident-1', nodeId: 'Service_1',
          nodeName: 'Check', scopeId: `escalation-${language}`, code: 'ESCALATION_HANDLER_FAILED',
          message: 'boundary failed', jobId: 'job-1', canRetry: false, status: 'Open' }],
        subscriptions: [{ subscriptionId: 'subscription-1', nodeId: 'Boundary_1', nodeName: 'Escalate',
          tokenId: 'token-1', kind: 'BoundaryEscalation', escalationCode: 'NEEDS.HUMAN',
          status: 'Open', scopeId: `escalation-${language}`, messageName: null,
          correlationKey: null, errorCode: null }],
      }));
      const incidentRow = escalationWindow.querySelector('[data-incidents] .fb-process-work');
      assert.ok(incidentRow.textContent.includes(I18n.t('bpmn.incident_escalation_guidance')));
      assert.equal(incidentRow.querySelector('[data-retry]'), null);
      assert.ok(escalationWindow.querySelector('[data-subscription-rows]').textContent.includes('NEEDS.HUMAN'));
      escalationWindow.dispatchEvent(new Event('closed'));
      escalationWindow.remove();
      const caught = processEventText({ kind: 'business_error_caught', nodeName: 'Check',
        data: { error_code: 'BUSINESS.INVALID' } });
      assert.match(caught, /BUSINESS\.INVALID/);
      const armed = processEventText({ kind: 'error_boundary_armed', nodeName: 'Check',
        data: { subscription_id: 's1', attached_to_id: 'Check', error_code: 'BUSINESS.INVALID' } });
      assert.match(armed, /Check/);
      const escalationArmed = processEventText({ kind: 'escalation_boundary_armed', nodeName: 'Check',
        data: { subscription_id: 's2', attached_to_id: 'Check', escalation_code: 'NEEDS.HUMAN' } });
      const escalationCaught = processEventText({ kind: 'escalation_caught', nodeName: 'Check',
        data: { code: 'RESULT.REVIEW', matched_escalation_code: null } });
      assert.match(escalationArmed, /NEEDS\.HUMAN/);
      assert.match(escalationCaught, /RESULT\.REVIEW/);
      assert.ok(escalationCaught.includes(I18n.t('bpmn.escalation_any_code')));
      for (const value of [queued, race, caught, armed, escalationArmed, escalationCaught])
        assert.doesNotMatch(value, /bpmn\.|\{node\}|\{message\}|\{key\}|\{winner\}|\{code\}|\{matched\}|undefined/);
    }
  } finally { await I18n.setLanguage('en'); }
});

test('declaration editor keeps exact namespace and long names in the real draft save', async () => {
  const current = definition('declarations-draft', { publishedVersion: 1 });
  const state = await mount(current, { processDefinitionSaveRequest: (payload) => ({
    definition: definition('declarations-draft', { model: payload.model, draftRevision: 5, publishedVersion: 1 }),
  }) });
  click(state.root.querySelector('[data-role="declarations"]'));
  const form = document.querySelector('.tf-act-window');
  change(form.querySelector('[data-declaration-namespace]'), 'urn:orders:Łódź');
  click(form.querySelector('[data-add-message]'));
  click(form.querySelector('[data-add-escalation]'));
  click(form.querySelector('[data-add-signal]'));
  const signal = form.querySelector('[data-declaration-kind="signal"]');
  change(signal.querySelector('[data-declaration-id]'), 'Signal_Order');
  change(signal.querySelector('[data-declaration-name]'), 'Order changed <&>');
  const escalation = form.querySelector('[data-declaration-kind="escalation"]');
  change(escalation.querySelector('[data-declaration-id]'), 'Escalation_Order');
  change(escalation.querySelector('[data-declaration-name]'), `${'Review'.repeat(40)}<&>`);
  change(escalation.querySelector('[data-declaration-code]'), 'NEEDS.HUMAN');
  const row = form.querySelector('[data-declaration-kind="message"]');
  change(row.querySelector('[data-declaration-id]'), 'Message_Order');
  const name = `${'Order'.repeat(40)}<&>`;
  change(row.querySelector('[data-declaration-name]'), name);
  assert.equal(row.querySelector('[data-declaration-name]').value, name);
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.equal(state.canvas.processModel.targetNamespace, 'urn:orders:Łódź');
  assert.equal(state.canvas.processModel.messages[0].name, name);
  assert.equal(state.canvas.processModel.escalations[0].escalationCode, 'NEEDS.HUMAN');
  assert.deepEqual(state.canvas.processModel.signals, [{ signalId: 'Signal_Order',
    namespaceUri: 'urn:orders:Łódź', name: 'Order changed <&>' }]);
  state.canvas.undo();
  assert.equal(state.canvas.processModel.signals, undefined);
  state.canvas.redo();
  assert.equal(state.canvas.processModel.signals[0].signalId, 'Signal_Order');
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model;
  assert.equal(saved.targetNamespace, 'urn:orders:Łódź');
  assert.deepEqual(saved.messages, [{ messageId: 'Message_Order', name }]);
  assert.deepEqual(saved.escalations, [{ escalationId: 'Escalation_Order',
    name: `${'Review'.repeat(40)}<&>`, escalationCode: 'NEEDS.HUMAN' }]);
  assert.deepEqual(saved.signals, [{ signalId: 'Signal_Order',
    namespaceUri: 'urn:orders:Łódź', name: 'Order changed <&>' }]);
  assert.deepEqual(saved.variables, {});
  assert.equal(state.definition.publishedVersion, 1, 'draft edits do not mutate the published version');
});

test('mounted modeling editor saves pools, lanes, data, annotations, associations and DI', async () => {
  const model = emptyProcessModel();
  model.variables.review = null;
  const current = definition('modeling-draft', { model });
  const state = await mount(current, {
    processDefinitionSaveRequest: ({ model: saved }) => ({ definition: { ...current, model: saved, draftRevision: 5 } }),
    processDefinitionPublishRequest: () => ({ definition: { ...current, publishedVersion: 1 },
      version: { version: 1, model: state.canvas.getData() } }),
  });
  click(state.root.querySelector('[data-role="modeling"]'));
  const form = document.querySelector('.tf-act-window');
  click(form.querySelector('[data-add-modeling="lane"]'));
  const lane = form.querySelector('[data-modeling-kind="lane"]');
  const laneId = lane.dataset.modelingId;
  change(lane.querySelector('[data-modeling-field="name"]'), 'Review Łódź <&>');
  change(lane.querySelector('[data-modeling-node]'), 'Start');
  click(lane.querySelector('[data-assign-node]'));
  change(form.querySelector(`[data-modeling-id="${CSS.escape(laneId)}"] [data-geometry-field="x"]`), '42');
  click(form.querySelector(`[data-modeling-id="${CSS.escape(laneId)}"] [data-add-child-lane]`));
  const childLane = [...form.querySelectorAll('[data-modeling-kind="lane"]')]
    .find((row) => row.dataset.modelingId !== laneId);
  let deepestLaneId = childLane.dataset.modelingId;
  change(childLane.querySelector('[data-modeling-node]'), 'End');
  click(childLane.querySelector('[data-assign-node]'));
  for (let depth = 2; depth <= 3; depth += 1) {
    click(form.querySelector(`[data-modeling-id="${CSS.escape(deepestLaneId)}"] [data-add-child-lane]`));
    deepestLaneId = [...form.querySelectorAll('[data-modeling-kind="lane"]')].at(-1).dataset.modelingId;
  }
  assert.equal(form.querySelector(`[data-modeling-id="${CSS.escape(deepestLaneId)}"] [data-add-child-lane]`), null,
    'the inspector stops at the model validator lane depth');
  click(form.querySelector('[data-add-modeling="reference"]'));
  const reference = form.querySelector('[data-modeling-kind="reference"]');
  const referenceId = reference.dataset.modelingId;
  change(reference.querySelector('[data-modeling-field="objectName"]'), 'Case data');
  change(reference.querySelector('[data-modeling-field="name"]'), 'Case reference');
  change(reference.querySelector('[data-modeling-field="variableBindingKey"]'), 'review');
  click(form.querySelector('[data-add-modeling="annotation"]'));
  const annotation = form.querySelector('[data-modeling-kind="annotation"]');
  const annotationId = annotation.dataset.modelingId;
  const annotationText = `Explain <&> the decision\n${'Long review detail '.repeat(24)}`;
  change(annotation.querySelector('[data-modeling-field="text"]'), annotationText);
  click(form.querySelector('[data-add-modeling="association"]'));
  const association = form.querySelector('[data-modeling-kind="association"]');
  change(association.querySelector('[data-modeling-field="sourceRef"]'), referenceId);
  change(association.querySelector('[data-modeling-field="targetRef"]'), annotationId);
  change(association.querySelector('[data-point-index="1"] [data-point-field="x"]'), '514');
  click(form.querySelector('[data-add-modeling="participant"]'));
  const pool = form.querySelector('[data-modeling-kind="participant"]');
  change(pool.querySelector('[data-modeling-field="name"]'), 'Approval pool');
  change(pool.querySelector('[data-modeling-field="processRef"]'), model.processId);
  change(pool.querySelector('[data-geometry-field="width"]'), '960');
  click(form.querySelector('[data-add-modeling="participant"]'));
  const blackBox = form.querySelectorAll('[data-modeling-kind="participant"]')[1];
  change(blackBox.querySelector('[data-modeling-field="name"]'), 'External pool');
  click(form.querySelector('[data-add-modeling="messageFlow"]'));
  const messageFlow = form.querySelector('[data-modeling-kind="messageFlow"]');
  change(messageFlow.querySelector('[data-modeling-field="sourceRef"]'), pool.dataset.modelingId);
  change(messageFlow.querySelector('[data-modeling-field="targetRef"]'), blackBox.dataset.modelingId);
  click(form.querySelector('[data-act="submit"]')); await flush();
  const authored = state.canvas.getData();
  assert.deepEqual(authored.modeling.laneSets[0].lanes[0].flowNodeRefs, ['Start']);
  assert.deepEqual(authored.modeling.laneSets[0].lanes[0].childLaneSets[0].lanes[0].flowNodeRefs, ['End']);
  assert.equal(authored.modeling.laneSets[0].lanes[0].name, 'Review Łódź <&>');
  assert.equal(authored.diagram.modelingShapes.find((shape) => shape.elementId === laneId).x, 42);
  assert.equal(authored.modeling.dataObjectReferences[0].variableBindingKey, 'review');
  assert.equal(authored.modeling.textAnnotations[0].text, annotationText);
  assert.equal(authored.modeling.associations[0].sourceRef, referenceId);
  assert.equal(authored.modeling.associations[0].targetRef, annotationId);
  assert.equal(authored.diagram.modelingEdges[0].waypoints[1].x, 514);
  assert.equal(authored.collaboration.participants[0].processRef.processId, model.processId);
  assert.equal(authored.collaboration.participants[1].processRef, undefined);
  assert.equal(authored.collaboration.diagram.modelingShapes[0].width, 960);
  assert.equal(authored.collaboration.messageFlows[0].targetRef, blackBox.dataset.modelingId);
  for (const elementId of [laneId, referenceId, annotationId, pool.dataset.modelingId, blackBox.dataset.modelingId])
    assert.ok(state.canvas.root.querySelector(`[data-modeling-id="${CSS.escape(elementId)}"]`));
  const renderedAnnotation = state.canvas.root.querySelector(`[data-modeling-id="${CSS.escape(annotationId)}"]`);
  assert.equal(renderedAnnotation.style.overflow, 'visible');
  assert.equal(renderedAnnotation.style.whiteSpace, 'pre-wrap');
  assert.equal(renderedAnnotation.textContent, annotationText);
  const regionOrder = [...state.canvas.regionsLayer.querySelectorAll('[data-modeling-id]')]
    .map((element) => element.dataset.modelingId);
  assert.ok(regionOrder.indexOf(pool.dataset.modelingId) < regionOrder.indexOf(laneId),
    'the pool is painted behind its readable lane');
  assert.equal(await builder._save(), true);
  const saved = calls.filter((call) => call.kind === 'processDefinitionSaveRequest').at(-1).payload.model;
  assert.deepEqual(saved.modeling, authored.modeling);
  assert.deepEqual(saved.diagram.modelingShapes, authored.diagram.modelingShapes);
  assert.deepEqual(saved.collaboration, authored.collaboration);
  await builder._publish();
  assert.equal(calls.filter((call) => call.kind === 'processDefinitionPublishRequest').length, 1);
});

test('mounted modeling editor authors document data stores and selected-body references', async () => {
  const model = emptyProcessModel();
  const current = definition('data-store-draft', { model });
  const state = await mount(current, {
    processDefinitionSaveRequest: ({ model: saved }) => ({ definition: { ...current, model: saved, draftRevision: 5 } }),
  });
  click(state.root.querySelector('[data-role="modeling"]'));
  const form = document.querySelector('.tf-act-window');
  assert.ok(!form.querySelector('[data-add-modeling="storeReference"]'),
    'a store reference cannot be added without a document store');
  click(form.querySelector('[data-add-modeling="store"]'));
  const store = form.querySelector('[data-modeling-kind="store"]');
  const storeId = store.dataset.modelingId;
  change(store.querySelector('[data-modeling-field="name"]'), 'Retained metadata Łódź <&>');
  change(store.querySelector('[data-modeling-field="capacity"]'), '12');
  change(store.querySelector('[data-modeling-field="isUnlimited"]'), 'false');
  click(form.querySelector('[data-add-modeling="storeReference"]'));
  const reference = form.querySelector('[data-modeling-kind="storeReference"]');
  const referenceId = reference.dataset.modelingId;
  change(reference.querySelector('[data-modeling-field="name"]'), 'Case store reference');
  change(reference.querySelector('[data-modeling-field="dataStoreRef"]'), storeId);
  assert.ok(!form.querySelector(`[data-modeling-id="${CSS.escape(storeId)}"] [data-remove-modeling]`),
    'a referenced document store cannot be deleted in the mounted editor');
  click(form.querySelector('[data-act="submit"]')); await flush();
  const authored = state.canvas.getData();
  assert.deepEqual(authored.dataStores, [{ id: storeId, name: 'Retained metadata Łódź <&>', capacity: 12, isUnlimited: false }]);
  assert.deepEqual(authored.modeling.dataStoreReferences,
    [{ id: referenceId, name: 'Case store reference', dataStoreRef: storeId }]);
  assert.equal(authored.diagram.modelingShapes.filter((shape) => shape.elementId === referenceId).length, 1);
  assert.equal(state.canvas.root.querySelector(`[data-modeling-id="${CSS.escape(referenceId)}"]`).textContent,
    'Case store reference');
  assert.equal(await builder._save(), true);
  const saved = calls.filter((call) => call.kind === 'processDefinitionSaveRequest').at(-1).payload.model;
  assert.deepEqual(saved.dataStores, authored.dataStores);
  assert.deepEqual(saved.modeling.dataStoreReferences, authored.modeling.dataStoreReferences);
});

test('modeling editor preserves a message flow aimed at a nested subprocess node', async () => {
  const model = embeddedModel();
  model.collaboration = { id: 'Collaboration_External', participants: [{ id: 'Participant_External', name: 'External' }],
    messageFlows: [{ id: 'MessageFlow_Nested', sourceRef: 'Participant_External', targetRef: 'Local_Review' }],
    diagram: { shapes: [], edges: [], modelingShapes: [], modelingEdges: [] } };
  const state = await mount(definition('nested-message-flow', { model }));
  click(state.root.querySelector('[data-role="modeling"]'));
  const form = document.querySelector('.tf-act-window');
  const target = form.querySelector('[data-modeling-kind="messageFlow"] [data-modeling-field="targetRef"]');
  assert.ok(target.querySelector('option[value="Local_Review"]'));
  assert.equal(target.value, 'Local_Review');
  assert.equal(form.querySelector('[data-act="submit"]').hasAttribute('disabled'), false);
  assert.deepEqual(state.canvas.getData().collaboration, model.collaboration);
});

test('pinned published signal declarations remain inspectable without enabling edits', async () => {
  const pinned = { ...emptyProcessModel(), targetNamespace: 'urn:orders:Łódź',
    signals: [{ signalId: 'Signal_Order', namespaceUri: 'urn:orders:Łódź', name: 'Order changed <&>' }] };
  const draft = { ...emptyProcessModel(), targetNamespace: 'urn:orders:revised',
    signals: [{ signalId: 'Signal_Revised', namespaceUri: 'urn:orders:revised', name: 'Revised' }] };
  const current = definition('signal-preview', { model: draft, publishedVersion: 2 });
  const state = await mount(current, {
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: { version: { version: 1, model: pinned }, startCatalog: [] },
  });
  await builder._openProcessVersions();
  click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
  await flush();
  assert.equal(state.previewVersion, 1);
  assert.equal(state.canvas.readOnly, true);
  assert.equal(state.root.querySelector('[data-role="save"]').hasAttribute('disabled'), true);
  const declarations = state.root.querySelector('[data-role="declarations"]');
  assert.equal(declarations.hasAttribute('disabled'), false);
  const before = state.canvas.getData();
  click(declarations);
  const form = document.querySelector('.tf-act-window');
  assert.equal(form.querySelector('[data-declaration-namespace]').value, pinned.targetNamespace);
  const signal = form.querySelector('[data-declaration-kind="signal"]');
  assert.equal(form.querySelectorAll('[data-declaration-kind="signal"]').length, 1);
  assert.equal(signal.querySelector('[data-declaration-id]').value, pinned.signals[0].signalId);
  assert.equal(signal.querySelector('[data-declaration-name]').value, pinned.signals[0].name);
  for (const control of [form.querySelector('[data-declaration-namespace]'),
    signal.querySelector('[data-declaration-id]'), signal.querySelector('[data-declaration-name]')])
    assert.equal(control.hasAttribute('disabled'), true);
  assert.equal(form.querySelector('[data-add-signal]'), null);
  assert.equal(signal.querySelector('[data-remove-declaration]'), null);
  assert.equal(form.querySelector('[data-act="submit"]').hasAttribute('disabled'), true);
  assert.deepEqual(state.canvas.getData(), before);
  assert.equal(state.dirty, false);
  assert.equal(calls.some((call) => call.kind === 'processDefinitionSaveRequest'), false);
  state.operationBusy = 'saving'; builder._syncProcessControls();
  assert.equal(declarations.hasAttribute('disabled'), true);
  state.operationBusy = null; builder._syncProcessControls();
  assert.equal(declarations.hasAttribute('disabled'), false);
});

test('documents and business JSON are bounded before sending without changing business keys', () => {
  assert.doesNotThrow(() => checkProcessDocument('a'.repeat(PROCESS_DOCUMENT_BYTES)));
  assert.throws(() => checkProcessDocument('é'.repeat(PROCESS_DOCUMENT_BYTES)), /512/);
  assert.deepEqual(processJson('{"User_ID":{"Keep_Me":true}}'), { User_ID: { Keep_Me: true } });
  assert.throws(() => processJson('[]'), /JSON object/);
  assert.throws(() => processJson(JSON.stringify({ output: 'a'.repeat(256 * 1024) })), /256/);
});

test('ordinary users have visible Workflows navigation and create their own BPMN without DAG creator privileges', async () => {
  const source = readFileSync(new URL('../../app.js', import.meta.url), 'utf8');
  const nav = source.slice(source.indexOf('const ADMIN_NAV = ['), source.indexOf('// Apps section shared', source.indexOf('const ADMIN_NAV = [')));
  const sections = runInNewContext(`${nav}\nuserVisibleAdminSections()`);
  const ids = sections.flatMap((section) => section.items.map((item) => item.id));
  assert.ok(ids.includes('flows')); for (const id of ['users', 'settings', 'scheduler']) assert.equal(ids.includes(id), false);
  let authRequest;
  fixtures({ authMeRequest: (payload) => {
    const envelope = wasm.decodeEnvelope(encode.authMeRequest(17, ...(payload === undefined ? [] : [payload]), 1));
    try { authRequest = wasm.decodeMessageBody(envelope.body); }
    finally { envelope.free(); }
    return { role: 'user' };
  }, processDefinitionListRequest: { definitions: [], total: 0, hasMore: false }, flowListRequest: [], processDefinitionSaveRequest: (payload) => ({ definition: definition('own-created', { name: payload.name, model: payload.model }) }) });
  document.body.innerHTML = `<main>${flows.render()}</main>`; await flows.mount();
  assert.equal(authRequest?.variant, 'AuthMeRequest');
  assert.equal(document.querySelector('#flows-mode').value, 'bpmn');
  assert.equal(document.querySelector('#flows-subtitle').textContent, I18n.t('bpmn.list_description'));
  click(document.querySelector('#btn-new-flow')); await flush();
  const form = document.querySelector('tf-window'); change(form.querySelector('[data-name]'), 'My approval'); click(form.querySelector('[data-act="submit"]')); await flush();
  const save = calls.find((call) => call.kind === 'processDefinitionSaveRequest');
  assert.equal(save.payload.expectedRevision, 0); assert.equal(save.payload.definitionId, null);
  assert.deepEqual(save.payload.model.nodes.map((node) => node.kind), ['Start', 'End']);
  assert.deepEqual(navigation[0], { view: 'flow-builder', params: { flowId: 'own-created', mode: 'bpmn' } });
  assert.equal(calls.some((call) => call.kind === 'flowCreateRequest'), false);
  change(document.querySelector('#flows-mode'), 'flow'); await flush();
  assert.equal(document.querySelector('#flows-subtitle').textContent, I18n.t('flows.subtitle'));
  assert.equal(document.querySelector('#btn-new-flow').hidden, true);
});

test('definition lists use real offset pagination and ignore a stale list after navigation', async () => {
  const page = deferred();
  fixtures({ authMeRequest: { role: 'user' }, processDefinitionListRequest: ({ offset }) => offset ? page.promise : { definitions: [definition()], total: 40, hasMore: true } });
  document.body.innerHTML = `<main>${flows.render()}</main>`; await flows.mount();
  document.querySelector('tf-table').dispatchEvent(new CustomEvent('page-change', { detail: { page: 2 }, bubbles: true })); await flush();
  assert.equal(calls.at(-1).payload.offset, 25);
  flows.unmount(); document.body.innerHTML = '<main id="different-route">Another view</main>';
  page.resolve({ definitions: [definition('late')], total: 40, hasMore: false }); await flush();
  assert.equal(document.querySelector('#different-route').textContent, 'Another view');
});

test('save and publish persist the draft revision, preserve keys and use the published full model', async () => {
  const saved = definition();
  const state = await mount(saved, {
    processDefinitionGetRequest: { definition: saved, startCatalog: manualStartCatalog(saved.model, 3) },
    processDefinitionSaveRequest: (payload) => ({ definition: { ...saved, name: payload.name, draftRevision: 5, model: payload.model } }),
    processDefinitionPublishRequest: () => ({ definition: { definitionId: saved.definitionId, name: 'Approved name', description: '', ownerUserId: 'owner', draftRevision: 5, publishedVersion: 3, archived: false }, version: { version: 3, model: state.canvas.getData() } }),
  });
  change(state.root.querySelector('[data-role="name"]'), 'Approved name');
  state.root.querySelector('[data-role="name"]').dispatchEvent(new Event('input', { bubbles: true }));
  state.canvas.processModel.variables = { Customer_ID: 4 }; builder._markDirty();
  await builder._publish();
  const savedRequest = calls.find((call) => call.kind === 'processDefinitionSaveRequest');
  assert.equal(savedRequest.payload.expectedRevision, 4); assert.equal(savedRequest.payload.model.variables.Customer_ID, 4);
  const publication = calls.find((call) => call.kind === 'processDefinitionPublishRequest'); assert.equal(publication.payload.expectedRevision, 5);
  assert.equal(state.definition.model.variables.Customer_ID, 4);
  change(state.root.querySelector('[data-role="start-entry"]'), '0');
  assert.equal(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'), false);
});

test('failed save retains edits and an uncertain retry sends the same command', async () => {
  let attempts = 0;
  const state = await mount(definition(), { processDefinitionSaveRequest: (payload) => { if (++attempts === 1) throw new Error('Connection lost'); return { definition: definition('definition-one', { model: payload.model, draftRevision: 5 }) }; } });
  state.canvas.updateNodeLabel('Start', 'Changed');
  assert.equal(await builder._save(), false); assert.equal(state.dirty, true);
  assert.equal(await builder._save(), true);
  const requests = calls.filter((row) => row.kind === 'processDefinitionSaveRequest'); assert.deepEqual(requests[0].payload, requests[1].payload);
});

test('late definition/options or save completion cannot replace another editor', async () => {
  const load = deferred(), optionRead = deferred();
  fixtures({ processDefinitionGetRequest: ({ definitionId }) => definitionId === 'A' ? load.promise : { definition: definition('B'), startCatalog: [] }, processOptionsRequest: () => optionRead.promise });
  document.body.innerHTML = builder.render({ mode: 'bpmn' }); const first = builder.mount({ flowId: 'A', mode: 'bpmn' });
  await builder.unmount(); document.body.innerHTML = builder.render({ mode: 'bpmn' });
  optionRead.resolve(options); const second = builder.mount({ flowId: 'B', mode: 'bpmn' }); await second;
  load.resolve({ definition: definition('A'), startCatalog: [] }); await first;
  assert.equal(builder._state.flowId, 'B'); assert.equal(document.querySelector('[data-role="name"]').value, 'Process B');
  const delayedSave = deferred(); responder = (kind) => kind === 'processDefinitionSaveRequest' ? delayedSave.promise : kind === 'processDefinitionGetRequest' ? { definition: definition('C'), startCatalog: [] } : options;
  builder._state.canvas.updateNodeLabel('Start', 'B edit'); const saving = builder._save();
  await builder.unmount(); document.body.innerHTML = builder.render({ mode: 'bpmn' }); await builder.mount({ flowId: 'C', mode: 'bpmn' });
  delayedSave.resolve({ definition: definition('B', { name: 'Late B' }) }); await saving;
  assert.equal(builder._state.flowId, 'C'); assert.equal(document.querySelector('[data-role="name"]').value, 'Process C');
});

test('late publish failure after leaving is contained in its original editor', async () => {
  const pending = deferred(); await mount(definition(), { processDefinitionPublishRequest: () => pending.promise });
  const action = builder._publish(); await flush(); await builder.unmount(); document.body.innerHTML = '<main id="next-route">Current route</main>';
  pending.reject(new Error('Delayed publish refusal')); await action;
  assert.equal(document.querySelector('#next-route').textContent, 'Current route'); assert.equal(document.querySelector('[data-role="error"]'), null);
});

test('XML import uses actual diagnostics and rejects unsupported input without replacing the draft', async () => {
  const state = await mount(definition(), { processXmlImportRequest: { model: null, diagnostics: [{ code: 'UNSUPPORTED_OR_INVALID_BPMN', message: 'Loop is unsupported', fatal: true }] } });
  const before = state.canvas.getData(); builder._importProcess(); await flush();
  const form = document.querySelector('tf-window'); const editor = form.querySelector('tf-code-editor'); editor.value = '<unsupported />'; editor.dispatchEvent(new Event('input', { bubbles: true }));
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.deepEqual(state.canvas.getData(), before); assert.match(form.querySelector('.tf-act__error').getAttribute('message'), /Loop/);
  assert.equal(calls.some((row) => row.kind === 'processDefinitionSaveRequest'), false);
});

test('archived definitions remain read-only and real unarchive restores editing', async () => {
  const current = definition('archived', { archived: true, publishedVersion: 2 });
  const state = await mount(current, { processDefinitionArchiveRequest: (payload) => ({ definition: { ...current, archived: payload.archived, draftRevision: 5 } }) });
  assert.equal(state.canvas.readOnly, true); assert.equal(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'), true);
  const declarations = state.root.querySelector('[data-role="declarations"]');
  assert.equal(declarations.hasAttribute('disabled'), false);
  click(declarations);
  const form = document.querySelector('.tf-act-window');
  assert.equal(form.querySelector('[data-declaration-namespace]').hasAttribute('disabled'), true);
  assert.equal(form.querySelector('[data-add-signal]'), null);
  assert.equal(form.querySelector('[data-act="submit"]').hasAttribute('disabled'), true);
  click(form.querySelector('[data-act="cancel"]'));
  await builder._archiveProcess();
  assert.equal(calls.find((row) => row.kind === 'processDefinitionArchiveRequest').payload.archived, false);
  assert.equal(state.canvas.readOnly, false); assert.equal(state.root.querySelector('[data-role="save"]').hasAttribute('disabled'), false);
});

test('published version paging and competing previews use the selected current response', async () => {
  const old = deferred(), recent = deferred();
  const state = await mount(definition(), { processVersionListRequest: { versions: [{ version: 2, publishedAtMs: 1000 }, { version: 1, publishedAtMs: 100 }], total: 2, hasMore: false }, processVersionGetRequest: ({ version }) => version === 1 ? old.promise : recent.promise });
  await builder._openProcessVersions(); const table = document.querySelector('tf-window tf-table');
  click(table.shadowRoot.querySelectorAll('tbody tf-button')[1]); click(table.shadowRoot.querySelectorAll('tbody tf-button')[0]);
  recent.resolve({ version: { version: 2, model: emptyProcessModel() }, startCatalog: [] }); await flush();
  old.resolve({ version: { version: 1, model: { ...emptyProcessModel(), variables: { wrong: true } } }, startCatalog: [] }); await flush();
  assert.equal(state.previewVersion, 2); assert.deepEqual(state.canvas.getData().variables, {}); assert.equal(state.canvas.readOnly, true);
});

test('preview shows its pinned calendar state and returning to the draft restores stale state', async () => {
  const original = { name: 'Original', weeklyWindows: [{ weekday: 1, startMinute: 540, endMinute: 1020 }],
    manualDaysOff: [], holidayPolicy: 'None' };
  const pin = { calendar: original, timezoneData: { ianaName: 'Europe/Warsaw' }, sha256: 'pin' };
  const published = { ...emptyProcessModel(), timerTimezone: 'Europe/Warsaw', workCalendar: original, calendarPin: pin };
  const draft = { ...published, workCalendar: { ...original, name: 'Revised' } };
  const current = definition('calendar-preview', { model: draft, publishedVersion: 1, calendarPinState: 'Stale' });
  const state = await mount(current, {
    processDefinitionGetRequest: { definition: current, startCatalog: [] },
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: { version: { version: 1, model: published }, startCatalog: [] },
  });
  assert.equal(state.root.querySelector('[data-role="calendar-state"]').textContent, I18n.t('bpmn.calendar_state_stale'));
  await builder._openProcessVersions();
  click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
  await flush();
  assert.equal(state.previewVersion, 1);
  assert.equal(state.root.querySelector('[data-role="calendar-state"]').textContent, I18n.t('bpmn.calendar_state_current'));
  click(state.root.querySelector('[data-role="draft"]'));
  await flush();
  assert.equal(state.previewVersion, null);
  assert.equal(state.root.querySelector('[data-role="calendar-state"]').textContent, I18n.t('bpmn.calendar_state_stale'));
});

test('human completion fetches full authorized work and sends instance revision with unchanged output keys', async () => {
  const task = { userTaskId: 'work', nodeId: 'Review', name: 'Review contract', assigneeUserId: 'anna', kind: 'Work', status: 'Open', revision: 2, canComplete: true };
  const current = instance('work-run', { userTasks: [task] });
  const win = await monitor(current, { processUserTaskGetRequest: { task: { ...task, outputs: { Existing_KEY: 7 } } }, processUserTaskCompleteRequest: (payload) => ({ instance: { ...current, revision: 12, userTasks: [{ ...task, status: 'Completed', canComplete: false }] } }) });
  click(win.querySelector('[data-complete]')); await flush(); const form = document.querySelector('.tf-act-window');
  const editor = form.querySelector('tf-code-editor'); assert.match(editor.value, /Existing_KEY/); editor.value = '{"Output_ID":{"Customer_ID":8}}'; editor.dispatchEvent(new Event('input', { bubbles: true }));
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.equal(calls.find((row) => row.kind === 'processUserTaskGetRequest').payload.userTaskId, 'work');
  const submitted = calls.find((row) => row.kind === 'processUserTaskCompleteRequest').payload;
  assert.equal(submitted.expectedRevision, 11); assert.notEqual(submitted.expectedRevision, task.revision); assert.deepEqual(submitted.outputs, { Output_ID: { Customer_ID: 8 } }); assert.equal(submitted.approved, null);
  assert.equal(win.querySelector('[data-complete]'), null);
});

test('human task form shows authenticated missing, null and present activity inputs read-only', async () => {
  const task = { userTaskId: 'work-inputs', nodeId: 'Review', name: 'Review contract',
    assigneeUserId: 'anna', kind: 'Work', status: 'Open', revision: 2, canComplete: true };
  const detail = { ...task, outputs: {}, activityInputs: [
    { position: 0, declarationId: 'Customer_ID', name: 'Customer ID', value: { Present: 'C-42' } },
    { position: 1, declarationId: 'Optional_Note', name: 'Optional note', value: 'Missing' },
    { position: 2, declarationId: 'Nullable_Flag', name: 'Nullable flag', value: { Present: null } },
  ] };
  const current = instance('work-inputs-run', { userTasks: [task] });
  const win = await monitor(current, { processUserTaskGetRequest: { task: detail } });
  click(win.querySelector('[data-complete]')); await flush();
  const form = document.querySelector('.tf-act-window');
  const section = form.querySelector('.fb-process-activity-inputs');
  assert.ok(section);
  assert.equal(section.querySelectorAll('.fb-process-activity-input').length, 3);
  assert.match(section.textContent, /Customer ID/);
  assert.match(section.textContent, /Optional note/);
  assert.match(section.textContent, /Missing/);
  const editors = section.querySelectorAll('tf-code-editor');
  assert.equal(editors.length, 2);
  assert.equal(editors[0].value, JSON.stringify('C-42', null, 2));
  assert.equal(editors[1].value, 'null');
  assert.ok([...editors].every((editor) => editor.hasAttribute('readonly')));
  assert.equal(section.querySelectorAll('input').length, 0);
  assert.equal(section.querySelectorAll('button').length, 0);
  win.remove(); form.remove();
});

test('instance pages advance independently and keep the exact incident selection while polling', async () => {
  const task = (number) => ({ userTaskId: `work-${number}`, nodeId: 'Review', name: `Review ${number}`,
    kind: 'Work', status: 'Open', canComplete: false });
  const incident = { incidentId: 'incident-1', nodeId: 'Check', nodeName: 'Check', jobId: null,
    code: 'INTERRUPTED', message: 'Actual failure', canRetry: false };
  const page = (offset, total) => ({ offset, total, nextOffset: offset + 1 < total ? offset + 1 : null,
    hasMore: offset + 1 < total });
  const snapshot = (taskOffset, selected = false) => instance('paged-run', {
    userTasks: [task(taskOffset + 1)], incidents: [incident],
    selectedIncident: selected ? { incident, resolvedAtMs: null } : null,
    pages: { ...instance().pages, userTasks: page(taskOffset, 2), incidents: page(0, 1) },
  });
  const win = await monitor(snapshot(0), { processInstanceGetRequest: ({ pages }) => ({
    instance: snapshot(pages.userTasks.offset, pages.selectedIncidentId === 'incident-1'),
  }) });
  click(win.querySelector('[data-inspect-incident]')); await flush();
  assert.equal(win.querySelector('[data-selected-incident]').dataset.selectedIncident, 'incident-1');
  click(win.querySelector('[data-page-controls="userTasks"] [data-page-next]')); await flush();
  const reads = calls.filter((call) => call.kind === 'processInstanceGetRequest').map((call) => call.payload.pages);
  assert.equal(reads.at(-1).userTasks.offset, 1);
  assert.equal(reads.at(-1).incidents.offset, 0);
  assert.equal(reads.at(-1).selectedIncidentId, 'incident-1');
  assert.match(win.querySelector('[data-work]').textContent, /Review 2/);
  await poll(); await flush();
  assert.equal(calls.filter((call) => call.kind === 'processInstanceGetRequest').at(-1).payload.pages.userTasks.offset, 1);
  assert.equal(win.querySelector('[data-selected-incident]').dataset.selectedIncident, 'incident-1');
});

test('empty collections of an active instance show no heading, no list and no page controls', async () => {
  const current = instance('empty-message-pages', { canSendMessage: true, messageNames: [],
    subscriptions: [], eventRaces: [], outgoingMessages: [] });
  const win = await monitor(current);
  for (const name of ['subscriptions', 'eventRaces', 'outgoingMessages']) {
    assert.equal(win.querySelector(`[data-page-controls="${name}"]`), null, `${name} has nothing to page through`);
  }
  for (const section of ['[data-subscriptions]', '[data-event-races]', '[data-outgoing]', '[data-calls]', '[data-scopes]',
    '[data-repetition-groups]', '[data-repetition-occurrences]']) {
    assert.equal(win.querySelector(section).hidden, true, `${section} is hidden while empty`);
  }
  assert.equal(win.querySelector('[data-subscription-rows]').children.length, 0);
  assert.equal(win.querySelector('[data-race-rows]').children.length, 0);
  assert.equal(win.querySelector('[data-outgoing-rows]').children.length, 0);
  assert.equal(win.querySelector('[data-summary]').textContent.includes('undefined'), false);
});

test('repeat monitor pages and one selected value preserve actual null availability in five locales', async () => {
  const group = { groupId: 'group-1', nodeId: 'RepeatWork', nodeName: 'Review <&> order', scopeId: 'scope-1',
    parentTokenId: 'wait-1', mode: 'structured_loop', status: 'open', total: null,
    createdCount: 1, completed: 0, maxIterations: 32, revision: 1, createdAtMs: 1, updatedAtMs: 1 };
  const occurrence = { occurrenceId: 'occurrence-1', groupId: group.groupId, ordinal: 0,
    status: 'awaiting_verification', tokenId: 'token-1', userTaskId: null, jobId: 'job-1',
    verificationUserTaskId: 'verify-1', acceptedSourceEventId: null, approvalEventId: null,
    createdAtMs: 1, updatedAtMs: 1 };
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const current = instance(`repeat-${language}`, { repetitionGroups: [group] });
      const win = await monitor(current, { processInstanceGetRequest: ({ pages }) => ({ instance: instance(current.instanceId, {
        repetitionGroups: [group], repetitionOccurrences: pages.selectedRepetitionGroupId ? [occurrence] : [],
        selectedRepetitionOccurrence: pages.selectedRepetitionOccurrenceId ? {
          summary: occurrence, valueKind: pages.selectedRepetitionValue,
          valueAvailable: pages.selectedRepetitionValue === 'item',
          value: pages.selectedRepetitionValue === 'item' ? { business_key: { Inner_ID: 'kept' } } : null,
          acceptedOrigin: null,
        } : null,
      }) }) });
      assert.match(win.querySelector('[data-repetition-group-rows]').textContent, /Review <&> order/);
      assert.match(win.querySelector('[data-repetition-group-rows]').textContent, new RegExp(I18n.t('bpmn.repeat_total_unknown')));
      click(win.querySelector('[data-repeat-inspect]')); await flush();
      assert.equal(calls.filter((entry) => entry.kind === 'processInstanceGetRequest').at(-1).payload.pages.selectedRepetitionGroupId, group.groupId);
      assert.match(win.querySelector('[data-repetition-occurrence-rows]').textContent,
        new RegExp(I18n.t('bpmn.repeat_occurrence_status_awaiting_verification')));
      click(win.querySelector('[data-repeat-item]')); await flush();
      assert.deepEqual(calls.filter((entry) => entry.kind === 'processInstanceGetRequest').at(-1).payload.pages.selectedRepetitionValue, 'item');
      assert.deepEqual(JSON.parse(win.querySelector('[data-repetition-detail] tf-code-editor').value),
        { business_key: { Inner_ID: 'kept' } });
      click(win.querySelector('[data-repeat-aggregate]')); await flush();
      assert.equal(win.querySelector('[data-repetition-detail] tf-code-editor'), null);
      assert.match(win.querySelector('[data-repetition-detail]').textContent, new RegExp(I18n.t('bpmn.repeat_value_unavailable')));
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('repeat history kind labels are localized without rewriting factual payloads', async () => {
  const kinds = ['repetition_group_started', 'repetition_occurrence_started',
    'repetition_occurrence_completed', 'repetition_condition_checked', 'repetition_completed'];
  const capacityReasons = ['active_occurrences', 'active_service_jobs', 'group_lifetime',
    'occurrence_lifetime', 'repetition_bytes'];
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const events = kinds.map((kind, index) => ({ seq: index + 1, atMs: 1000 + index,
        kind, nodeName: 'Review <&> order', data: { group_id: 'group-1', business_key: 'kept' } }));
      for (const [index, reason] of capacityReasons.entries()) events.push({
        seq: events.length + 1, atMs: 2000 + index, kind: 'repetition_group_blocked',
        nodeName: 'Review <&> order', data: { group_id: 'group-1', last_completed_ordinal: null,
          last_source_event_id: null, incident_id: `incident-${index}`, phase: 'capacity',
          code: 'REPETITION_LIMIT', reason },
      });
      for (const code of ['REPETITION_AGGREGATE_LIMIT', 'REPETITION_MAPPING_FAILED']) events.push({
        seq: events.length + 1, atMs: 3000 + events.length, kind: 'repetition_group_blocked',
        nodeName: 'Review <&> order', data: { group_id: 'group-1', last_completed_ordinal: 2,
          last_source_event_id: 'source-event-2', incident_id: 'incident-aggregate',
          phase: 'aggregate', code, reason: null },
      });
      events.push({ seq: events.length + 1, atMs: 4000, kind: 'repetition_entry_failed',
        nodeName: 'Review <&> order', data: { group_id: 'group-denied', source_token_id: 'source-token-1',
          parent_token_id: 'parent-token-1', incident_id: 'incident-entry', code: 'REPETITION_INPUT_ERROR' } });
      const factualData = structuredClone(events.map((event) => event.data));
      for (const event of events) {
        const label = processEventText(event);
        assert.ok(label.includes('Review <&> order'));
        assert.doesNotMatch(label, /bpmn\.event_|\{node\}/);
      }
      for (const [index, reason] of capacityReasons.entries()) {
        const label = processEventText(events[kinds.length + index]);
        assert.ok(label.includes(I18n.t(`bpmn.repeat_reason_${reason}`)));
        assert.ok(label.includes(I18n.t('bpmn.incident_repetition_limit')));
      }
      assert.ok(processEventText(events.at(-3)).includes(I18n.t('bpmn.incident_repetition_aggregate_limit')));
      assert.ok(processEventText(events.at(-2)).includes(I18n.t('bpmn.incident_repetition_mapping_failed')));
      assert.ok(processEventText(events.at(-1)).includes(I18n.t('bpmn.incident_repetition_input_error')));
      assert.deepEqual(events.map((event) => event.data), factualData);
      const win = await monitor(instance(`repeat-history-${language}`), {
        processHistoryRequest: { events, nextSeq: events.length, hasMore: false },
      });
      assert.equal(win.querySelectorAll('[data-process-seq]').length, events.length);
      assert.match(win.querySelector('[data-process-seq="1"] tf-code-editor').value, /business_key/);
      assert.match(win.querySelector('[data-process-seq="6"] tf-code-editor').value, /incident_id/);
      assert.match(win.querySelector(`[data-process-seq="${events.length}"] tf-code-editor`).value, /parent_token_id/);
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('repeated Script body result stays distinct from an accepted blocked ordinal in five locales', async () => {
  const group = { groupId: 'script-group', nodeId: 'Script_1', nodeName: 'Calculate <&>',
    scopeId: 'script-scope', parentTokenId: 'wait-1', mode: 'structured_loop', status: 'incident',
    total: null, createdCount: 1, completed: 0, maxIterations: 32, revision: 2,
    createdAtMs: 1, updatedAtMs: 2 };
  const occurrence = { occurrenceId: 'script-occurrence', groupId: group.groupId, ordinal: 0,
    status: 'accepted_blocked', tokenId: 'script-token', userTaskId: null, jobId: null,
    verificationUserTaskId: null, acceptedSourceEventId: 'script-source', approvalEventId: null,
    createdAtMs: 1, updatedAtMs: 2 };
  const events = [
    { seq: 1, atMs: 1000, eventId: 'script-source', kind: 'script_completed',
      nodeName: 'Calculate <&>', data: { outputs: null } },
    { seq: 2, atMs: 1001, eventId: 'script-incident', kind: 'incident',
      nodeName: 'Calculate <&>', data: { code: 'REPETITION_MAPPING_FAILED', message: 'invalid local step' } },
    { seq: 3, atMs: 1002, eventId: 'script-blocked', kind: 'repetition_group_blocked',
      nodeName: 'Calculate <&>', data: { group_id: group.groupId, last_completed_ordinal: null,
        last_source_event_id: 'script-source', incident_id: 'script-incident', phase: 'aggregate',
        code: 'REPETITION_MAPPING_FAILED', reason: null } },
  ];
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const current = instance(`script-blocked-${language}`, { repetitionGroups: [group] });
      const win = await monitor(current, {
        processHistoryRequest: { events, nextSeq: 3, hasMore: false },
        processInstanceGetRequest: ({ pages }) => ({ instance: instance(current.instanceId, {
          repetitionGroups: [group],
          repetitionOccurrences: pages.selectedRepetitionGroupId ? [occurrence] : [],
          selectedRepetitionOccurrence: pages.selectedRepetitionOccurrenceId ? {
            summary: occurrence, valueKind: pages.selectedRepetitionValue,
            valueAvailable: false, value: null, acceptedOrigin: null,
          } : null,
        }) }),
      });
      assert.equal(win.querySelectorAll('[data-process-seq]').length, 3);
      assert.deepEqual(JSON.parse(win.querySelector('[data-process-seq="1"] tf-code-editor').value), null);
      assert.ok(win.querySelector('[data-process-seq="1"]').textContent.includes(I18n.t('bpmn.event_script_completed', { node: 'Calculate <&>' })));
      assert.ok(win.querySelector('[data-process-seq="3"]').textContent.includes(I18n.t('bpmn.incident_repetition_mapping_failed')));
      click(win.querySelector('[data-repeat-inspect]')); await flush();
      assert.ok(win.querySelector('[data-repetition-occurrence-rows]').textContent
        .includes(I18n.t('bpmn.repeat_occurrence_status_accepted_blocked')));
      click(win.querySelector('[data-repeat-item]')); await flush();
      assert.ok(win.querySelector('[data-repetition-detail]').textContent
        .includes(`${I18n.t('bpmn.repeat_accepted_source_event')}: script-source`));
      assert.equal(win.querySelector('[data-repetition-detail] tf-code-editor'), null);
      assert.equal(win.querySelector('[data-events] img'), null);
      assert.equal(win.querySelector('[data-events] script'), null);
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('instance window keeps its localized title while the full long definition name remains visible in the summary', async () => {
  const name = `Quarterly approval <source> & Łódź ${'long process title '.repeat(12)}`;
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const win = await monitor(instance(`long-title-${language}`, { definitionName: name }));
      assert.equal(win.shadowRoot.querySelector('.tf-window-title-text').textContent, I18n.t('bpmn.instance'));
      assert.equal(win.querySelector('[data-summary] h2').textContent, name);
      assert.equal(win.querySelector('[data-summary] h2').querySelector('source'), null);
      assert.ok(win.querySelector('[data-summary]').textContent.includes(I18n.t('bpmn.version_number', { version: 2 })));
      assert.ok(win.querySelector('[data-variables] tf-code-editor').value.includes('Purchase_ID'));
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('human verification reads the persisted ActivityResult and submits only the decision', async () => {
  const task = { userTaskId: 'verify', nodeId: 'Check', name: 'Check contract', assigneeUserId: 'anna', kind: 'Verification', status: 'Open', revision: 1, canComplete: true };
  const current = instance('verify-run', { userTasks: [task] }); const result = { outcome: 'Completed', summary: 'Contract validated', code: null, outputs: { Approved_ID: true }, evidence: ['Actual source evidence'] };
  const win = await monitor(current, { processUserTaskGetRequest: { task: { ...task, outputs: result } }, processUserTaskCompleteRequest: () => ({ instance: { ...current, revision: 12, status: 'Completed', userTasks: [] } }) });
  click(win.querySelector('[data-complete]')); await flush(); const form = document.querySelector('.tf-act-window');
  assert.ok(form.querySelector('tf-code-editor').hasAttribute('readonly')); assert.deepEqual(JSON.parse(form.querySelector('tf-code-editor').value), result);
  assert.ok(form.querySelector('[data-act="submit"]').hasAttribute('disabled'));
  change(form.querySelector('[data-approved]'), 'false'); click(form.querySelector('[data-act="submit"]')); await flush();
  const submitted = calls.find((row) => row.kind === 'processUserTaskCompleteRequest').payload; assert.equal(submitted.approved, false); assert.deepEqual(submitted.outputs, {});
});

test('current capabilities gate cancel/retry/work independently of role or local task revision', async () => {
  const current = instance('owner-run', { canCancel: true, canRetry: true, incidents: [{ incidentId: 'incident', nodeId: 'Check', nodeName: 'Check contract', jobId: 'job', code: 'INTERRUPTED', message: 'Worker error', canRetry: true }] });
  let latest = current;
  const win = await monitor(current, { processInstanceGetRequest: () => ({ instance: latest }), processJobRetryRequest: (payload) => { latest = { ...current, revision: 12, incidents: [] }; return { instance: latest }; }, processInstanceCancelRequest: (payload) => ({ instance: { ...latest, revision: 13, status: 'Cancelled', canCancel: false } }) });
  click(win.querySelector('[data-retry]')); await flush(); const retry = calls.find((row) => row.kind === 'processJobRetryRequest').payload; assert.equal(retry.expectedRevision, 11); assert.equal(retry.jobId, 'job');
  click(win.querySelector('[data-cancel]')); await flush(); assert.equal(calls.find((row) => row.kind === 'processInstanceCancelRequest').payload.expectedRevision, 12);
  assert.equal(win.querySelector('[data-cancel]'), null);
});

test('same-revision capability revocation disables an already open work form after current polling', async () => {
  const task = { userTaskId: 'work', name: 'Review', kind: 'Work', status: 'Open', revision: 1, canComplete: true };
  const current = instance('permissions', { userTasks: [task] });
  let latest = current;
  const win = await monitor(current, { processInstanceGetRequest: () => ({ instance: latest }), processUserTaskGetRequest: { task: { ...task, outputs: {} } } });
  click(win.querySelector('[data-complete]')); await flush(); const form = document.querySelector('.tf-act-window');
  assert.equal(form.querySelector('[data-act="submit"]').hasAttribute('disabled'), false);
  latest = { ...current, userTasks: [{ ...task, canComplete: false }] }; await poll(); await flush();
  assert.equal(win.querySelector('[data-complete]'), null); assert.ok(form.querySelector('[data-act="submit"]').hasAttribute('disabled'));
});

test('stale mutation replay never replaces a newer polled instance and fetches its current state', async () => {
  const task = { userTaskId: 'work', name: 'Review', kind: 'Work', status: 'Open', revision: 1, canComplete: true };
  const current = instance('replay', { userTasks: [task] }); const saved = deferred(); let latest = current;
  const win = await monitor(current, { processInstanceGetRequest: () => ({ instance: latest }), processUserTaskGetRequest: { task: { ...task, outputs: {} } }, processUserTaskCompleteRequest: () => saved.promise });
  click(win.querySelector('[data-complete]')); await flush(); click(document.querySelector('.tf-act-window [data-act="submit"]')); await flush();
  latest = { ...current, revision: 14, status: 'Completed', userTasks: [], variables: { Latest_ID: 14 } }; await poll();
  saved.resolve({ instance: { ...current, revision: 12, status: 'Running', userTasks: [] } }); await flush();
  assert.match(win.querySelector('[data-summary]').textContent, /Completed/); assert.match(win.querySelector('[data-variables] tf-code-editor').value, /Latest_ID/);
  assert.ok(calls.filter((row) => row.kind === 'processInstanceGetRequest').length >= 2);
});

test('delayed poll and history responses after closing cannot edit a new run window', async () => {
  const pending = deferred(), history = deferred(); const first = instance('A');
  const a = await monitor(first, { processInstanceGetRequest: () => pending.promise }); const refreshing = poll();
  a.remove(); await flush();
  const b = await monitor(instance('B', { definitionName: 'Current B' }));
  pending.resolve({ instance: { ...first, definitionName: 'Late A', revision: 50 } }); await refreshing;
  assert.match(b.querySelector('[data-summary]').textContent, /Current B/); assert.doesNotMatch(b.textContent, /Late A/);
  b.remove(); await flush(); assert.equal([...intervals.values()].some((row) => row.delay === 3000), false);
  fixtures({ processInstanceGetRequest: { instance: first }, processHistoryRequest: () => history.promise });
  const opening = openProcessInstance('A'); await flush(); document.querySelector('.fb-process-window').remove();
  history.resolve({ events: [{ seq: 1, atMs: 1, kind: 'instance_started', nodeName: null, data: {} }], nextSeq: 1, hasMore: false }); await opening; await flush();
  assert.equal(document.querySelector('[data-events]'), null); assert.equal([...intervals.values()].some((row) => row.delay === 3000), false);
});

test('history uses authoritative hasMore even when the bounded byte page has fewer events than limit', async () => {
  let pages = 0; const win = await monitor(instance(), { processHistoryRequest: (payload) => (++pages === 1 ? { events: [{ seq: 7, atMs: 1000, kind: 'service_claimed', nodeName: 'Check', data: { job_id: 'internal' } }], nextSeq: 7, hasMore: true } : { events: [{ seq: 8, atMs: 2000, kind: 'instance_completed', nodeName: null, data: null }], nextSeq: 8, hasMore: false }) });
  assert.equal(win.querySelector('[data-more]').hidden, false); click(win.querySelector('[data-more]')); await flush();
  assert.equal(calls.filter((row) => row.kind === 'processHistoryRequest').at(-1).payload.afterSeq, 7);
  assert.equal(win.querySelectorAll('[data-process-seq]').length, 2); assert.equal(win.querySelector('[data-more]').hidden, true); assert.doesNotMatch(win.textContent, /internal/);
});

test('interrupting escalation history shows the full accepted result and origin without Verification in five locales', async () => {
  const result = { outcome: 'NeedsHuman', code: 'NEEDS.HUMAN', summary: 'Actual reviewed result <&>',
    outputs: { customer_ID: 17, nested_value: { Preserve_Me: '<img src=x onerror=alert(1)>' } },
    evidence: ['real-pinned-flow-output'], result_origin: 'contract' };
  const events = [
    { seq: 1, atMs: 1000, eventId: 'result-1', kind: 'service_result', nodeName: 'Check service', data: result },
    { seq: 2, atMs: 1001, eventId: 'catch-1', kind: 'escalation_caught', nodeName: 'Escalate',
      data: { code: 'NEEDS.HUMAN', matched_escalation_code: 'NEEDS.HUMAN', result_event_id: 'result-1', cancel_activity: true } },
  ];
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const current = instance(`interrupting-${language}`, { userTasks: [{ userTaskId: 'handler-1', nodeId: 'Handler',
        name: 'Human handler', kind: 'Work', status: 'Open', scopeId: `interrupting-${language}`, canComplete: false }] });
      const win = await monitor(current, { processHistoryRequest: { events, nextSeq: 2, hasMore: false } });
      assert.equal(current.userTasks.some((task) => task.kind === 'Verification'), false);
      assert.equal(win.querySelectorAll('[data-work] .fb-process-work').length, 1);
      const entries = win.querySelectorAll('[data-events] [data-process-seq]');
      assert.equal(entries.length, 2);
      assert.equal(entries[0].querySelector('h3').textContent, I18n.t('bpmn.actual_outputs'));
      const editor = entries[0].querySelector('tf-code-editor');
      assert.equal(editor.hasAttribute('readonly'), true);
      assert.deepEqual(JSON.parse(editor.value), result);
      assert.equal(entries[1].querySelector('tf-code-editor'), null);
      assert.equal(win.querySelector('[data-events] img'), null);
      assert.equal(win.querySelector('[data-events] script'), null);
      win.dispatchEvent(new Event('closed'));
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('script history renders arbitrary JSON results and nonretryable incidents in five locales', async () => {
  const outputs = [null, 7, { business_key: '<img src=x onerror=alert(1)>', nested: ['Łódź', true] }];
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const events = outputs.map((value, index) => ({ seq: index + 1, atMs: 1000 + index,
        kind: 'script_completed', nodeName: 'Calculate <&>', data: { outputs: value } }));
      events.push({ seq: 4, atMs: 1004, kind: 'incident', nodeName: 'Calculate <&>',
        data: { code: 'SCRIPT_MAPPING_FAILED', message: 'private business <&>' } });
      const current = instance(`script-${language}`, { incidents: [{ incidentId: 'script-failure',
        nodeId: 'Script_1', nodeName: 'Calculate <&>', scopeId: `script-${language}`,
        code: 'SCRIPT_MAPPING_FAILED', message: 'private business <&>', jobId: null,
        canRetry: false, status: 'Open' }] });
      const win = await monitor(current, { processHistoryRequest: { events, nextSeq: 4, hasMore: false } });
      const rows = win.querySelectorAll('[data-events] [data-process-seq]');
      assert.equal(rows.length, 4);
      for (let index = 0; index < outputs.length; index += 1) {
        assert.equal(rows[index].querySelector('h3').textContent, I18n.t('bpmn.script_result'));
        const editor = rows[index].querySelector('tf-code-editor');
        assert.equal(editor.hasAttribute('readonly'), true);
        assert.deepEqual(JSON.parse(editor.value), outputs[index]);
        assert.ok(rows[index].textContent.includes('Calculate <&>'));
      }
      assert.equal(win.querySelector('[data-events] img'), null);
      assert.equal(win.querySelector('[data-events] script'), null);
      assert.equal(win.querySelector('[data-retry]'), null);
      assert.ok(rows[3].textContent.includes(I18n.t('bpmn.incident_script_mapping_failed')));
      assert.ok(rows[3].textContent.includes(I18n.t('bpmn.incident_script_guidance')));
      win.dispatchEvent(new Event('closed'));
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('instance summaries paginate without fetching full opaque business values', async () => {
  fixtures({ processInstanceListRequest: ({ offset }) => ({ instances: [instance(`page-${offset}`)], total: 60, hasMore: true }) });
  const win = await openProcessInstances(); const table = win.querySelector('tf-table'); table.dispatchEvent(new CustomEvent('page-change', { detail: { page: 3 }, bubbles: true })); await flush();
  assert.equal(calls.at(-1).payload.offset, 50); assert.equal(table.rows[0].instanceId, 'page-50'); assert.equal(calls.some((row) => row.kind === 'processInstanceGetRequest'), false);
});

test('run uses immutable selected version variables and blocks oversized input before any request', async () => {
  fixtures({ processInstanceStartRequest: () => { throw new Error('Start should remain blocked'); } });
  const published = definition('published', { publishedVersion: 3, model: { ...emptyProcessModel(), variables: { Version_ID: 'pinned' } } });
  const catalog = manualStartCatalog(published.model, 3);
  const form = openProcessRun(published, [{ version: 3 }], catalog, catalog[0]);
  assert.match(form.querySelector('tf-code-editor').value, /Version_ID/); const editor = form.querySelector('tf-code-editor'); editor.value = JSON.stringify({ Long_Value: 'a'.repeat(256 * 1024) });
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.match(form.querySelector('[data-json-error]').getAttribute('message'), /256/); assert.equal(calls.some((row) => row.kind === 'processInstanceStartRequest'), false);
});

test('all five locales translate supported elements, current statuses and every finite event without exposing internal IDs', async () => {
  const events = ['instance_started', 'node_completed', 'end_reached', 'instance_completed', 'user_task_opened', 'manual_task_opened', 'manual_task_acknowledged', 'exclusive_selected', 'parallel_split', 'parallel_joined', 'inclusive_split', 'inclusive_joined', 'service_queued', 'service_claimed', 'service_result', 'verification_passed', 'user_task_completed', 'verification_approved', 'verification_rejected', 'incident', 'cancelled', 'job_retried', 'job_interrupted', 'job_denied', 'job_failed'];
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    assert.equal(processTemplates().length, 29);
    for (const kind of ['link_throw', 'link_catch']) {
      const template = processTemplates().find((item) => item.node_type === `bpmn_${kind}`);
      assert.equal(template?.label, I18n.t(`bpmn.node_${kind}`));
      assert.equal(template?.description, I18n.t(`bpmn.node_${kind}_hint`));
      assert.doesNotMatch(`${template.label} ${template.description}`, /bpmn\.|LinkDefinition_/);
    }
    assert.equal(processTemplates().find((template) => template.node_type === 'bpmn_manual_task')?.label,
      I18n.t('bpmn.node_manual_task'));
    assert.equal(processTemplates().find((template) => template.node_type === 'bpmn_send_task')?.label,
      I18n.t('bpmn.node_send_task'));
    assert.equal(processTemplates().find((template) => template.node_type === 'bpmn_receive_task')?.label,
      I18n.t('bpmn.node_receive_task'));
    for (const name of ['signal_throw', 'signal_catch']) assert.equal(
      processTemplates().find((template) => template.node_type === `bpmn_${name}`)?.label,
      I18n.t(`bpmn.node_${name}`));
    for (const key of ['event_gateway_receive_message_mix', 'event_gateway_receive_boundary',
      'event_gateway_branch_incoming']) {
      const message = I18n.t(`bpmn.${key}`, { node: 'Receive <&>' });
      assert.match(message, /Receive <&>/);
      assert.doesNotMatch(message, /bpmn\.|\{node\}/);
    }
    for (const template of processTemplates()) assert.doesNotMatch(template.label, /^bpmn\./);
    const linkThrown = processEventText({ kind: 'link_thrown', nodeName: 'Handoff <&>',
      data: { catch_node_id: 'Catch_1' } });
    const linkCaught = processEventText({ kind: 'link_caught', nodeName: 'Resume <&>',
      data: { source_token_id: 'source-token-1' } });
    assert.equal(linkThrown, I18n.t('bpmn.event_link_thrown', { node: 'Handoff <&>', target: 'Catch_1' }));
    assert.equal(linkCaught, I18n.t('bpmn.event_link_caught', { node: 'Resume <&>', source: 'source-token-1' }));
    assert.doesNotMatch(`${linkThrown} ${linkCaught}`, /bpmn\.|catch_node_id|source_token_id/);
    const linkLimit = processEventText({ kind: 'incident', nodeName: 'Handoff <&>',
      data: { code: 'IMMEDIATE_TRANSITION_LIMIT', message: 'raw internal reason' } });
    assert.equal(linkLimit, I18n.t('bpmn.event_incident', { node: 'Handoff <&>',
      message: I18n.t('bpmn.incident_immediate_transition_limit') }));
    assert.doesNotMatch(linkLimit, /raw internal reason|IMMEDIATE_TRANSITION_LIMIT|bpmn\./);
    for (const kind of ['manual_task_opened', 'manual_task_acknowledged']) {
      assert.equal(processEventText({ kind, nodeName: 'External work', data: {} }),
        I18n.t(`bpmn.event_${kind}`, { node: 'External work' }));
    }
    for (const kind of ['signal_admitted', 'signal_catch_opened', 'signal_received']) {
      const output = processEventText({ kind, nodeName: 'Order changed', data: {} });
      assert.equal(output, I18n.t(`bpmn.event_${kind}`, { node: 'Order changed' }));
      assert.doesNotMatch(output, /recipient_count|signal_id|subscription_id|bpmn\./);
    }
    for (const [kind, data] of [
      ['send_task_admitted', { message_name: 'order.received', correlation_key: 'case-1' }],
      ['receive_task_opened', { message_name: 'order.received', correlation_key: 'case-1' }],
      ['receive_task_completed', { message_id: 'message-1' }],
    ]) {
      const output = processEventText({ kind, nodeName: 'Message task', data });
      assert.equal(output, I18n.t(`bpmn.event_${kind}`, {
        node: 'Message task', message: data.message_name || data.message_id, key: data.correlation_key,
      }));
    }
    const pending = processEventText({ kind: 'send_task_pending', nodeName: 'Admit <&>',
      data: { source_activation_id: 'private-source', pending_token_id: 'private-wait' } });
    assert.equal(pending, I18n.t('bpmn.event_send_task_pending', { node: 'Admit <&>' }));
    assert.doesNotMatch(pending, /private-source|private-wait|bpmn\./);
    for (const code of ['MESSAGE_EXPRESSION_ERROR', 'SEND_ADMISSION_AUTHORITY_DENIED',
      'SEND_ADMISSION_TARGET_UNAVAILABLE']) {
      const failure = processEventText({ kind: 'send_admission_failed', nodeName: 'Admit <&>',
        data: { pending_token_id: 'private-wait', pending_event_id: 'private-event',
          incident_id: 'private-incident', code, reason: 'private raw reason' } });
      assert.equal(failure, I18n.t('bpmn.event_send_admission_failed', {
        node: 'Admit <&>', reason: I18n.t(`bpmn.incident_${code.toLowerCase()}`),
      }));
      assert.doesNotMatch(failure, /private-wait|private-event|private-incident|private raw reason|bpmn\./);
    }
    for (const kind of events) {
      const output = processEventText({ kind, nodeName: '<Contract>', data: { summary: 'Actual result', code: 'SOURCE_ACCESS_REVOKED', message: 'Access revoked', job_id: 'raw-job-uuid', user_task_id: 'raw-task-uuid', selected_branch_edge_ids: ['Flow_A', 'Flow_B'], default_selected: false } });
      assert.doesNotMatch(output, /bpmn\.|raw-job|raw-task|SOURCE_ACCESS_REVOKED/);
    }
  }
  await I18n.setLanguage('en');
});

test('DAG mode preserves its graph serialization and a system flow remains read-only', async () => {
  const graphJson = JSON.stringify({ nodes: [{ id: 'trigger', type: 'trigger', position: { x: 20, y: 40 }, config: {} }], edges: [] });
  fixtures({ flowDetailRequest: { id: 'system-flow', name: 'System pipeline', description: null, graphJson, status: 'active', enabled: true, isSystem: true }, flowNodeTemplatesListRequest: { templates: [{ node_type: 'trigger', label: 'Trigger', category: 'trigger', input_ports: [], output_ports: ['full'] }] }, catalogListRequest: { entries: [] } });
  document.body.innerHTML = builder.render(); await builder.mount({ flowId: 'system-flow' });
  assert.equal(builder._state.canvas.readOnly, true); assert.ok(document.querySelector('[data-role="save"]').hidden);
  assert.equal(await builder._save(), false); assert.equal(calls.some((row) => row.kind.includes('process')), false);
  await builder.unmount();
  fixtures({ flowDetailRequest: { id: 'custom-flow', name: 'Custom pipeline', description: null, graphJson, status: 'draft', enabled: false, isSystem: false }, flowNodeTemplatesListRequest: { templates: [{ node_type: 'trigger', label: 'Trigger', category: 'trigger', input_ports: [], output_ports: ['full'] }] }, catalogListRequest: { entries: [] }, flowUpdateRequest: { updated: true } });
  document.body.innerHTML = builder.render(); await builder.mount({ flowId: 'custom-flow' });
  builder._state.canvas.updateNodeLabel('trigger', 'Entry'); assert.equal(await builder._save(), true);
  const saved = calls.find((row) => row.kind === 'flowUpdateRequest').payload;
  assert.equal(saved.flowId, 'custom-flow'); assert.deepEqual(JSON.parse(saved.flowJson), { nodes: [{ id: 'trigger', type: 'trigger', label: 'Entry', position: { x: 20, y: 40 }, config: {} }], edges: [] });
});

test('run obtains the selected immutable version rather than unsaved draft variables', async () => {
  const draft = definition('versioned', { publishedVersion: 4 }); draft.model.variables = { Source_ID: 'draft' };
  const published = { ...emptyProcessModel(), variables: { Source_ID: 'published' } };
  const state = await mount(draft, { processDefinitionGetRequest: { definition: draft, startCatalog: manualStartCatalog(published, 4) }, processVersionGetRequest: (payload) => { assert.equal(payload.version, 4); return { version: { version: 4, model: published }, startCatalog: manualStartCatalog(published, 4) }; } });
  change(state.root.querySelector('[data-role="start-entry"]'), '0');
  await builder._runProcess(); const form = document.querySelector('.tf-act-window');
  assert.deepEqual(JSON.parse(form.querySelector('tf-code-editor').value), { Source_ID: 'published' });
  assert.equal(form.querySelector('[data-version]').value, '4');
});

test('valid XML import is applied to a draft and the resulting canonical model is saved by the existing editor', async () => {
  const imported = emptyProcessModel(); imported.nodes[0].name = 'Imported start'; imported.variables = { Original_Key: 'kept' };
  const state = await mount(definition(), { processXmlImportRequest: { model: imported, diagnostics: [] }, processDefinitionSaveRequest: (payload) => ({ definition: definition('definition-one', { model: payload.model, draftRevision: 5 }) }) });
  builder._importProcess(); await flush(); const form = document.querySelector('.tf-act-window');
  form.querySelector('tf-code-editor').value = '<bpmn:definitions/>'; form.querySelector('tf-code-editor').dispatchEvent(new Event('input', { bubbles: true }));
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.deepEqual(state.canvas.getData(), imported); assert.equal(state.dirty, true); await builder._save();
  assert.deepEqual(calls.find((row) => row.kind === 'processDefinitionSaveRequest').payload.model, imported);
});

test('a stale human completion refreshes the actual instance but retries only on a new manual submit', async () => {
  const task = { userTaskId: 'work', name: 'Review', kind: 'Work', status: 'Open', revision: 1, canComplete: true };
  const current = instance('revision-conflict', { userTasks: [task] }); let latest = current; let attempts = 0;
  const win = await monitor(current, { processInstanceGetRequest: () => ({ instance: latest }), processUserTaskGetRequest: { task: { ...task, outputs: {} } }, processUserTaskCompleteRequest: () => { if (++attempts === 1) { latest = { ...current, revision: 12 }; const error = new Error('Instance revision changed'); error.code = 'BadRequest'; throw error; } return { instance: { ...latest, revision: 13, userTasks: [] } }; } });
  click(win.querySelector('[data-complete]')); await flush(); const form = document.querySelector('.tf-act-window');
  form.querySelector('tf-code-editor').value = '{"Response_ID":"kept"}';
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.equal(attempts, 1, 'a refusal never automatically repeats a completion'); assert.match(form.querySelector('.tf-act__error').getAttribute('message'), /revision/);
  click(form.querySelector('[data-act="submit"]')); await flush();
  const mutations = calls.filter((row) => row.kind === 'processUserTaskCompleteRequest');
  assert.equal(mutations[0].payload.expectedRevision, 11); assert.equal(mutations[1].payload.expectedRevision, 12);
  assert.notEqual(mutations[0].payload.commandId, mutations[1].payload.commandId); assert.deepEqual(mutations[1].payload.outputs, { Response_ID: 'kept' });
});

test('poll refusal disables cached mutation controls and a delayed error after closing stays contained', async () => {
  const current = instance('revoked', { canCancel: true }); const pending = deferred();
  const win = await monitor(current, { processInstanceGetRequest: () => pending.promise }); const loading = poll();
  pending.reject(new Error('Access was revoked')); await loading;
  assert.ok(win.querySelector('[data-cancel]').hasAttribute('disabled')); assert.match(win.querySelector('[data-error]').getAttribute('message'), /revoked/);
  const last = deferred(); responder = () => last.promise; const delayed = poll(); win.remove(); await flush();
  last.reject(new Error('Late refusal')); await delayed;
  assert.equal(document.querySelector('.fb-process-window'), null);
});

function timedModel(kind = 'TimerStart', type = 'Duration', spec = { seconds: 60 }) {
  const model = emptyProcessModel();
  model.timerTimezone = 'Europe/Warsaw';
  model.variables = { Request_ID: { Preserve_Me: true } };
  if (kind === 'TimerStart') {
    model.nodes[0].kind = { TimerStart: { timer: { [type]: spec } } };
  } else {
    model.nodes.splice(1, 0, { id: 'Wait', name: 'Saved deadline', kind: { TimerCatch: { timer: { [type]: spec } } } });
    model.sequenceFlows = [{ id: 'e_before_wait', sourceId: 'Start', targetId: 'Wait', condition: null }, { id: 'e_after_wait', sourceId: 'Wait', targetId: 'End', condition: null }];
    model.diagram.shapes.splice(1, 0, { elementId: 'Wait', x: 240, y: 160, width: 56, height: 56 });
    model.diagram.edges = [{ sequenceFlowId: 'e_before_wait', waypoints: [{ x: 136, y: 188 }, { x: 240, y: 188 }] }, { sequenceFlowId: 'e_after_wait', waypoints: [{ x: 296, y: 188 }, { x: 400, y: 188 }] }];
  }
  return model;
}
function savedTimer(overrides = {}) {
  return { timerId: 'private-timer-id', nodeId: 'Start', nodeName: 'Daily approval', kind: 'Start', status: 'Pending', dueAtMs: Date.UTC(2026, 9, 3, 7), timezone: 'Europe/Warsaw', occurrence: 3, totalFirings: 7, lastReason: null, ...overrides };
}
function manualStartCatalog(model, version) {
  const start = model.nodes.find((node) => node.kind === 'Start');
  return [{ processId: model.processId, processName: model.processName ?? null,
    startNodeId: start.id, startNodeName: start.name ?? null, version, trigger: { Start: { canStart: true } } }];
}
function timerStartCatalog(definition, persistedTimer) {
  if (!persistedTimer) return [];
  const start = definition.model.nodes.find((node) => node.id === persistedTimer.nodeId);
  return [{ processId: definition.model.processId, processName: definition.model.processName ?? null,
    startNodeId: persistedTimer.nodeId, startNodeName: persistedTimer.nodeName,
    version: definition.publishedVersion ?? 1, trigger: { TimerStart: {
      timer: start?.kind?.TimerStart?.timer ?? { Duration: { seconds: 60 } },
      timezone: persistedTimer.timezone, workingTime: null, persistedTimer,
    } } }];
}
function messageStartCatalog(definition, canSend = true) {
  return [{ processId: definition.model.processId, processName: definition.model.processName ?? null,
    startNodeId: 'Start', startNodeName: 'Start', version: definition.publishedVersion,
    trigger: { MessageStart: { messageRef: 'Message_1', messageName: 'order.received', canSend } } }];
}

test('all supported timer literals preserve IANA, variables, stable IDs and DI through the actual canvas', () => {
  const cases = [['TimerStart', 'Date', { at: '2026-10-03T09:00:00.123+02:00' }], ['TimerStart', 'Duration', { seconds: 86400 }], ['TimerStart', 'Cycle', { seconds: 300, totalFirings: 3 }], ['TimerStart', 'Daily', { hour: 9, minute: 30, totalFirings: null }], ['TimerCatch', 'Date', { at: '2026-10-02T12:00:00Z' }], ['TimerCatch', 'Duration', { seconds: 1 }]];
  for (const [kind, type, spec] of cases) {
    const model = timedModel(kind, type, spec);
    const graph = canvas(model);
    assert.deepEqual(graph.getData(), model);
    const event = graph.nodesLayer.querySelector(kind === 'TimerStart' ? '.bpmn_timer_start' : '.bpmn_timer_catch');
    assert.ok(event);
    assert.match(event.querySelector('use').getAttribute('href'), /clock$/);
    assert.equal(event.querySelectorAll('.fb-port').length, kind === 'TimerStart' ? 1 : 2);
    if (kind === 'TimerStart') assert.equal(graph.connectNodes('End', 'Start'), false);
    graph.destroy();
  }
  const legacy = emptyProcessModel();
  assert.equal(Object.hasOwn(canvasToProcess(legacy, processToCanvas(legacy).nodes, processToCanvas(legacy).edges, (edge) => edge.waypoints), 'timerTimezone'), false);
});

test('timer inspector edits Date, elapsed Duration, finite Cycle and Daily without permitting repeating catch', async () => {
  const graph = canvas(timedModel()); const config = inspector(graph);
  const node = graph.nodes[0]; config.show(node, graph.templates.get(node.type));
  await flush(2);
  change(config.root.querySelector('[data-process="timerSeconds"]'), '86400');
  assert.deepEqual(node.config.timer, { Duration: { seconds: 86400 } });
  change(config.root.querySelector('[data-process="timerType"]'), 'Date');
  await flush(2);
  change(config.root.querySelector('[data-process="timerAt"]'), '2026-10-03T09:00:00.123+02:00');
  assert.equal(config.root.querySelector('[data-timer-literal]').textContent, '2026-10-03T09:00:00.123+02:00');
  assert.deepEqual(node.config.timer, { Date: { at: '2026-10-03T09:00:00.123+02:00' } });
  change(config.root.querySelector('[data-process="timerType"]'), 'Cycle');
  await flush(2);
  assert.equal(config.root.querySelector('[data-process="timerSeconds"]').getAttribute('min'), '300');
  change(config.root.querySelector('[data-process="timerSeconds"]'), '600');
  change(config.root.querySelector('[data-process="timerTotal"]'), '3');
  assert.deepEqual(node.config.timer, { Cycle: { seconds: 600, totalFirings: 3 } });
  change(config.root.querySelector('[data-process="timerTotal"]'), '');
  assert.equal(node.config.timer.Cycle.totalFirings, null);
  change(config.root.querySelector('[data-process="timerType"]'), 'Daily');
  await flush(2);
  change(config.root.querySelector('[data-process="timerHour"]'), '14');
  change(config.root.querySelector('[data-process="timerMinute"]'), '32');
  assert.deepEqual(node.config.timer, { Daily: { hour: 14, minute: 32, totalFirings: null } });
  const readonly = inspector(graph, true); readonly.show(node, graph.templates.get(node.type));
  assert.ok(readonly.root.querySelector('[data-process="timerType"]').hasAttribute('disabled'));
  change(readonly.root.querySelector('[data-process="timerHour"]'), '3');
  assert.equal(node.config.timer.Daily.hour, 14);
  const catchGraph = canvas(timedModel('TimerCatch')); const catchConfig = inspector(catchGraph);
  catchConfig.show(catchGraph.nodes[1], catchGraph.templates.get('bpmn_timer_catch'));
  assert.deepEqual([...catchConfig.root.querySelectorAll('[data-process="timerType"] option')].map((option) => option.value), ['Date', 'Duration', 'WorkingDuration']);
  assert.equal(catchConfig.root.querySelector('[data-process="timerTotal"]'), null);
  graph.destroy(); config.destroy(); readonly.destroy(); catchGraph.destroy(); catchConfig.destroy();
});

test('process timezone requires an explicit value and actual undo/redo preserves it through save', async () => {
  const model = timedModel(); delete model.timerTimezone;
  const state = await mount(definition('timezone', { model }), { processDefinitionSaveRequest: (payload) => ({ definition: definition('timezone', { model: payload.model, draftRevision: 5 }) }) });
  const field = state.root.querySelector('[data-role="timer-timezone"]');
  assert.equal(field.hidden, false); assert.equal(field.value, '');
  assert.equal(Object.hasOwn(state.canvas.getData(), 'timerTimezone'), false, 'no UTC default is fabricated');
  state.canvas.updateProcessVariables({ ...state.canvas.getData().variables, Current_Key: 'preserved' });
  change(field, 'Europe/Warsaw');
  assert.equal(state.canvas.getData().timerTimezone, 'Europe/Warsaw');
  state.canvas.undo(); assert.equal(field.value, '');
  assert.equal(state.canvas.getData().variables.Current_Key, 'preserved', 'timezone undo does not revert independently edited variables');
  state.canvas.redo(); assert.equal(field.value, 'Europe/Warsaw');
  await builder._save();
  assert.equal(calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model.timerTimezone, 'Europe/Warsaw');
  state.canvas.updateProcessTimezone('');
  assert.equal(Object.hasOwn(state.canvas.getData(), 'timerTimezone'), false);
});

test('boundary-only processes expose the editable timezone while timerless processes hide it', async () => {
  const boundary = boundaryModel(); delete boundary.timerTimezone;
  const current = definition('boundary-timezone', { model: boundary });
  const state = await mount(current, {
    processDefinitionSaveRequest: (payload) => ({ definition: { ...current, model: payload.model, draftRevision: 5 } }),
  });
  const field = state.root.querySelector('[data-role="timer-timezone"]');
  assert.equal(field.hidden, false);
  assert.equal(field.hasAttribute('disabled'), false);
  assert.equal(field.value, '');
  assert.equal(Object.hasOwn(state.canvas.getData(), 'timerTimezone'), false, 'the editor does not invent a timezone');
  change(field, 'Europe/Warsaw');
  assert.equal(state.canvas.getData().timerTimezone, 'Europe/Warsaw');
  assert.equal(await builder._save(), true);
  assert.equal(calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model.timerTimezone, 'Europe/Warsaw');
  const catchState = await mount(definition('catch-timezone', { model: timedModel('TimerCatch') }));
  assert.equal(catchState.root.querySelector('[data-role="timer-timezone"]').hidden, false);
  const plainState = await mount(definition('plain-timezone'));
  assert.equal(plainState.root.querySelector('[data-role="timer-timezone"]').hidden, true);
});

test('a typed time zone is stored as the engine spells it, and an unusable one blocks the save instead of being dropped', async () => {
  assert.equal(processTimezoneCanonical('europe/warsaw'), 'Europe/Warsaw');
  assert.equal(processTimezoneCanonical(' Europe/Warsaw '), 'Europe/Warsaw');
  assert.equal(processTimezoneCanonical(''), '');
  assert.equal(processTimezoneCanonical('Mars/Olympus'), null);
  for (const lowercase of ['us/pacific', 'utc', 'est', 'US/pacific', 'europe/uTC']) {
    assert.equal(processTimezoneCanonical(lowercase) === lowercase, false, `${lowercase} is never stored in a spelling the engine refuses`);
  }
  for (const kept of ['Europe/Kyiv', 'Asia/Kolkata', 'US/Pacific', 'EST', 'UTC']) {
    assert.equal(processTimezoneCanonical(kept), kept, `${kept} is stored as typed, not as the runtime respells it`);
  }
  const model = timedModel(); delete model.timerTimezone;
  const state = await mount(definition('zone-typed', { model }), { processDefinitionSaveRequest: (payload) => ({ definition: definition('zone-typed', { model: payload.model, draftRevision: 5 }) }) });
  const field = state.root.querySelector('[data-role="timer-timezone"]');
  const saves = () => calls.filter((call) => call.kind === 'processDefinitionSaveRequest').length;
  const before = saves();
  change(field, 'Mars/Olympus');
  assert.match(field.getAttribute('error'), /Mars\/Olympus/);
  assert.equal(await builder._save(), false);
  assert.equal(saves(), before, 'nothing is sent for a zone that cannot be used');
  assert.equal(field.value, 'Mars/Olympus', 'the typed text is not replaced by the saved value');
  assert.match(state.root.querySelector('[data-role="error"]').getAttribute('message'), /Mars\/Olympus/);
  state.canvas.updateProcessVariables({ ...state.canvas.getData().variables, Other_Edit: 'x' });
  builder._syncProcessControls();
  assert.equal(field.value, 'Mars/Olympus', 'a canvas change keeps the text the author is still fixing');
  change(field, 'europe/warsaw');
  assert.equal(field.value, 'Europe/Warsaw');
  assert.equal(field.hasAttribute('error'), false);
  assert.equal(state.canvas.getData().timerTimezone, 'Europe/Warsaw');
  assert.equal(await builder._save(), true);
  assert.equal(calls.filter((call) => call.kind === 'processDefinitionSaveRequest').at(-1).payload.model.timerTimezone, 'Europe/Warsaw');
});

test('a zone stays in the editor when the process has no timer, and is left out of what is saved', async () => {
  const state = await mount(definition('zone-orphan'));
  state.canvas.processModel.timerTimezone = 'Europe/Warsaw';
  assert.equal(Object.hasOwn(state.canvas.getData(), 'timerTimezone'), false);
  assert.equal(state.canvas.processModel.timerTimezone, 'Europe/Warsaw');
});

test('the server refusing a time zone name is told by field, and a simulation refusal by element name', () => {
  const model = { ...timedModel(), timerTimezone: 'Mars/Olympus' };
  assert.equal(processExplainError('protocol error BadRequest: unknown timezone', model), I18n.t('bpmn.timer_timezone_invalid', { zone: 'Mars/Olympus' }));
  const repeated = emptyProcessModel();
  repeated.nodes.push({ id: 'n_abc123_def', name: 'Review step', kind: { ServiceTask: {} }, repeat: { StructuredLoop: { outputCollectionVariable: 'out' } } });
  assert.deepEqual(processRepeatedElements(repeated), ['Review step']);
  assert.equal(processExplainError('protocol error BadRequest: repetition is unsupported in simulation (n_abc123_def)', repeated),
    I18n.t('bpmn.simulation_repeat_unsupported', { names: 'Review step' }));
});

test('calendar-only process exposes timezone and structured editor rejects invalid closures', async () => {
  const state = await mount(definition('calendar-only'));
  assert.equal(state.root.querySelector('[data-role="timer-timezone"]').hidden, true);
  click(state.root.querySelector('[data-role="calendar"]'));
  await flush();
  const form = document.querySelector('.tf-act-window');
  assert.ok(form);
  const enabled = form.querySelector('[data-calendar-enabled]');
  enabled.checked = true;
  enabled.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { checked: true } }));
  change(form.querySelector('[data-calendar-name]'), 'Warsaw office');
  assert.equal(form.querySelector('[data-calendar-windows]').children.length, 5);
  click(form.querySelector('[data-calendar-add-day]'));
  change(form.querySelector('[data-calendar-date]'), '2026-02-30');
  change(form.querySelector('[data-calendar-reason]'), 'Maintenance');
  click(form.querySelector('[data-act="submit"]'));
  await flush();
  assert.ok(form.isConnected, 'nonexistent civil date cannot be saved');
  change(form.querySelector('[data-calendar-date]'), '2026-02-27');
  click(form.querySelector('[data-act="submit"]'));
  await flush();
  assert.ok(form.shadowRoot.querySelector('.tf-window-closing'), 'valid structured calendar closes the form');
  assert.equal(state.canvas.getData().workCalendar.manualDaysOff[0].date, '2026-02-27');
  assert.equal(state.root.querySelector('[data-role="timer-timezone"]').hidden, false);
  assert.equal(state.root.querySelector('[data-role="timer-timezone"]').value, '', 'no timezone is inferred');
});

test('calendar-only draft rejects an absent or blank timezone before saving or publishing', async () => {
  const current = definition('calendar-zone');
  const state = await mount(current, {
    processDefinitionSaveRequest: (payload) => ({ definition: { ...current, model: payload.model, draftRevision: 5 } }),
  });
  state.canvas.updateProcessCalendar({ name: 'Office', weeklyWindows: [{ weekday: 1, startMinute: 540, endMinute: 1020 }],
    manualDaysOff: [], holidayPolicy: 'None' });
  assert.deepEqual(state.canvas.validate(), [I18n.t('bpmn.timer_timezone_calendar_missing')]);
  assert.equal(await builder._save(), false);
  await builder._publish();
  assert.equal(calls.some((call) => ['processDefinitionSaveRequest', 'processDefinitionPublishRequest'].includes(call.kind)), false);
  state.canvas.updateProcessTimezone('   ');
  assert.deepEqual(state.canvas.validate(), [I18n.t('bpmn.timer_timezone_calendar_missing')]);
  assert.equal(await builder._save(), false);
  state.canvas.updateProcessTimezone('Europe/Warsaw');
  assert.deepEqual(state.canvas.validate(), []);
  assert.equal(await builder._save(), true);
  assert.equal(calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model.timerTimezone, 'Europe/Warsaw');
  const plain = canvas();
  assert.deepEqual(plain.validate(), [], 'a process without a calendar or timer needs no timezone');
  plain.destroy();
});

test('a timer without a process time zone refuses the save by name, shows the folded field and focuses it', async () => {
  Object.defineProperty(window, 'innerHeight', { value: 700, configurable: true });
  try {
    const current = definition('timer-zone');
    const state = await mount(current, {
      processDefinitionSaveRequest: (payload) => ({ definition: { ...current, model: payload.model, draftRevision: 5 } }),
    });
    const notice = state.root.querySelector('.fb-process-notice');
    const field = state.root.querySelector('[data-role="timer-timezone"]');
    assert.ok(notice.classList.contains('fb-collapsed'), 'a short window starts with the settings row folded');
    const task = userNode(state.canvas);
    const timer = state.canvas.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer'),
      task.x + task.width / 2, task.y + task.height);
    assert.equal(timer.config.attachedToId, task.id);
    state.canvas.updateNodeLabel(timer.id, 'Deadline');
    const message = I18n.t('bpmn.timer_timezone_missing', { element: 'Deadline' });
    assert.deepEqual(state.canvas.validate(), [message], 'the check runs for any timer, not only with a work calendar');
    const autosave = await builder._save({ silent: true });
    assert.equal(autosave, false);
    assert.ok(notice.classList.contains('fb-collapsed'), 'a silent autosave does not unfold anything');
    assert.equal(await builder._save(), false);
    assert.equal(calls.some((call) => call.kind === 'processDefinitionSaveRequest'), false, 'nothing reaches the server');
    assert.equal(state.root.querySelector('[data-role="error"]').getAttribute('message'), message);
    assert.equal(notice.classList.contains('fb-collapsed'), false, 'the settings row is unfolded');
    assert.equal(field.hidden, false);
    assert.equal(document.activeElement, field.querySelector('input'), 'the field has focus');
    change(field, 'Europe/Warsaw');
    assert.deepEqual(state.canvas.validate(), []);
    assert.equal(await builder._save(), true);
    assert.equal(calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model.timerTimezone, 'Europe/Warsaw');
  } finally {
    Object.defineProperty(window, 'innerHeight', { value: 768, configurable: true });
  }
});

test('the server refusal for a missing time zone is rewritten and a timer in another body is brought on screen', async () => {
  const model = emptyProcessModel();
  model.additionalProcesses = [{ ...emptyProcessModel(), processId: 'Process_second', processName: 'Second',
    nodes: [...emptyProcessModel().nodes.slice(0, 1),
      { id: 'Wait_1', name: '', kind: { TimerCatch: { timer: { Duration: { seconds: 60 } } } } },
      ...emptyProcessModel().nodes.slice(1)] }];
  const raw = 'timed process or calendar requires an explicit IANA timezone';
  assert.equal(processExplainError(raw, model), I18n.t('bpmn.timer_timezone_missing', { element: I18n.t('bpmn.node_timer_catch') }));
  assert.equal(processExplainError(raw, emptyProcessModel()), I18n.t('bpmn.timer_timezone_server'));
  assert.doesNotMatch(processExplainError(raw, model), /explicit IANA timezone|Wait_1/);
  const current = definition('timer-zone-other-body', { model });
  const state = await mount(current);
  assert.equal(state.canvas.selectedProcessId, current.model.processId);
  assert.equal(await builder._save(), false);
  assert.equal(state.canvas.selectedProcessId, 'Process_second', 'the body that needs the zone is shown');
  assert.equal(state.root.querySelector('[data-role="timer-timezone"]').hidden, false);
});

test('no validation message cites the internal id of an unnamed element of any type', () => {
  const graph = canvas();
  const templates = processTemplates();
  const template = (type) => templates.find((row) => row.node_type === type);
  const add = (type, x, y, config = {}) => {
    const node = graph.addNodeFromTemplate(template(type), x, y);
    node.label = '';
    node.config = { ...node.config, ...config };
    return node;
  };
  const task = add('bpmn_service_task', 400, 400, { verification: { Condition: { expression: '' } } });
  const call = add('bpmn_call_activity', 800, 400);
  const timers = [
    add('bpmn_timer_start', 100, 100, { timer: { Date: { at: ' ' } } }),
    add('bpmn_timer_catch', 100, 250, { timer: { Duration: { seconds: 0 } } }),
    add('bpmn_boundary_timer', 400, 150, { attachedToId: 'missing', timer: { Cycle: { seconds: 1 } } }),
  ];
  const loose = ['bpmn_boundary_error', 'bpmn_boundary_escalation', 'bpmn_boundary_message']
    .map((type, index) => add(type, 100 + index * 150, 700, { attachedToId: 'missing' }));
  const gateway = add('bpmn_event_based_gateway', 1000, 100);
  const receive = add('bpmn_receive_task', 1200, 50);
  const message = add('bpmn_message_catch', 1200, 250);
  receive.config.attachedToId = undefined;
  graph.edges.push(
    { id: 'g1', from_node: gateway.id, to_node: receive.id, from_port: 'full', to_port: 'in' },
    { id: 'g2', from_node: gateway.id, to_node: message.id, from_port: 'full', to_port: 'in' });
  const guarded = add('bpmn_boundary_timer', 1200, 50, { attachedToId: receive.id, timer: { Duration: { seconds: 5 } } });
  const messages = graph.validate();
  assert.ok(messages.length >= 8, `the unnamed elements produce many refusals (${messages.length})`);
  const ids = graph.nodes.map((node) => node.id);
  for (const text of messages) {
    for (const id of ids) assert.equal(text.includes(id), false, `“${text}” cites ${id}`);
  }
  for (const element of [task, call, ...timers, ...loose, gateway, guarded]) {
    assert.ok(messages.some((text) => text.includes(graph._nodeName(element))), `${element.type} is named by its kind`);
  }
  graph.destroy();
});

test('calendar name and closure reason show their complete escaped values in read-only multiline controls', () => {
  const name = `Office <private> & ${'Łódź '.repeat(28)}`;
  const reason = `Maintenance <script> & ${'review '.repeat(11)}`;
  assert.ok(new TextEncoder().encode(name).length <= 256);
  assert.ok(new TextEncoder().encode(reason).length <= 128);
  const calendar = { name, weeklyWindows: [{ weekday: 1, startMinute: 540, endMinute: 1020 }],
    manualDaysOff: [{ date: '2026-02-27', reason }], holidayPolicy: 'None' };
  const form = openProcessCalendar(calendar, null, 'Unpinned', true, () => assert.fail('read-only calendar cannot save'));
  const title = form.querySelector('[data-calendar-name]');
  const closure = form.querySelector('[data-calendar-reason]');
  for (const [field, value] of [[title, name], [closure, reason]]) {
    assert.equal(field.tagName, 'TF-TEXTAREA');
    assert.ok(field.hasAttribute('autogrow'));
    assert.ok(field.hasAttribute('disabled'));
    assert.equal(field.value, value);
    assert.equal(field.querySelector('textarea').value, value);
    assert.equal(field.querySelector('script'), null);
  }
  assert.ok(form.querySelector('[data-act="submit"]').hasAttribute('disabled'));
});

test('pinned calendar details show exact exclusive source coverage in all five languages', async () => {
  const calendar = { name: 'Office', weeklyWindows: [{ weekday: 1, startMinute: 540, endMinute: 1020 }],
    manualDaysOff: [], holidayPolicy: 'None' };
  const pin = { legalRelease: { releaseId: 'PL-2026-10', asOfDate: '2026-10-02',
    validFrom: '2024-01-01', validUntil: '2041-01-01' }, timezoneData: { releaseId: '2026e' },
    sha256: 'a'.repeat(64) };
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const form = openProcessCalendar(calendar, pin, 'Current', true, () => assert.fail('read-only calendar cannot save'));
      const coverage = form.querySelector('[data-calendar-coverage]');
      assert.equal(coverage.textContent, I18n.t('bpmn.calendar_coverage', {
        from: '2024-01-01', until: '2041-01-01',
      }));
      assert.ok(coverage.textContent.includes('2024-01-01'));
      assert.ok(coverage.textContent.includes('2041-01-01'));
      assert.equal(coverage.children.length, 0, 'pin values render as text');
      form.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('published calendar pin survives ordinary model edit, undo and autosave without repin', async () => {
  const calendar = { name: 'Office', weeklyWindows: [{ weekday: 1, startMinute: 540, endMinute: 1020 }],
    manualDaysOff: [], holidayPolicy: 'PolandStatutory' };
  const model = emptyProcessModel();
  model.workCalendar = calendar;
  model.timerTimezone = 'Europe/Warsaw';
  model.variables = { Invoice_ID: { preserve_key: true } };
  const current = definition('calendar-publish', { model, calendarPinState: 'Unpinned' });
  const pin = { calendar, sha256: 'a'.repeat(64), legalRelease: { releaseId: 'PL-2026', asOfDate: '2026-10-02' },
    timezoneData: { ianaName: 'Europe/Warsaw', releaseId: '2026e' } };
  const published = { ...model, calendarPin: pin };
  const state = await mount(current, {
    processDefinitionSaveRequest: (payload) => ({ definition: { ...current, draftRevision: payload.expectedRevision + 1,
      name: payload.name, model: payload.model, calendarPinState: payload.model.calendarPin ? 'Current' : 'Unpinned' } }),
    processDefinitionPublishRequest: (payload) => {
      assert.equal(payload.repinCalendar, undefined);
      return { definition: { ...current, draftRevision: payload.expectedRevision, publishedVersion: 1,
        calendarPinState: 'Current' }, version: { version: 1, model: published } };
    },
  });
  await builder._publish();
  assert.deepEqual(state.canvas.getData().calendarPin, pin);
  state.canvas.updateNodeLabel('Start', 'Renamed start');
  state.canvas.undo();
  assert.deepEqual(state.canvas.getData().calendarPin, pin, 'undo does not restore an unpinned pre-publication snapshot');
  state.canvas.redo();
  assert.deepEqual(state.canvas.getData().calendarPin, pin);
  assert.equal(await builder._save(), true);
  const saves = calls.filter((call) => call.kind === 'processDefinitionSaveRequest');
  assert.deepEqual(saves.at(-1).payload.model.calendarPin, pin);
  assert.deepEqual(saves.at(-1).payload.model.variables, { Invoice_ID: { preserve_key: true } });
  assert.equal(state.root.querySelector('[data-role="calendar-state"]').textContent, I18n.t('bpmn.calendar_state_current'));
});

test('stale calendar requires explicit refresh and adopts the returned immutable pin', async () => {
  const calendar = { name: 'Initial', weeklyWindows: [{ weekday: 1, startMinute: 540, endMinute: 1020 }],
    manualDaysOff: [], holidayPolicy: 'None' };
  const model = emptyProcessModel();
  model.workCalendar = calendar;
  model.timerTimezone = 'Europe/Warsaw';
  model.calendarPin = { sha256: 'old-pin', calendar, timezoneData: { ianaName: 'Europe/Warsaw' } };
  const current = definition('stale-calendar', { model, calendarPinState: 'Current' });
  let savedModel = model;
  const state = await mount(current, {
    processDefinitionSaveRequest: ({ model: next, expectedRevision }) => {
      savedModel = next;
      return { definition: { ...current, model: next, draftRevision: expectedRevision + 1,
        calendarPinState: next.workCalendar.name === 'Revised' && next.calendarPin.sha256 !== 'new-pin' ? 'Stale' : 'Current' } };
    },
    processDefinitionPublishRequest: ({ repinCalendar, expectedRevision }) => {
      assert.equal(repinCalendar, true);
      assert.equal(expectedRevision, 5);
      return { definition: { ...current, draftRevision: 5, publishedVersion: 2, calendarPinState: 'Current' },
        version: { version: 2, model: { ...savedModel, calendarPin: { sha256: 'new-pin',
          calendar: savedModel.workCalendar, timezoneData: { ianaName: 'Europe/Warsaw' } } } } };
    },
  });
  state.canvas.updateProcessCalendar({ ...calendar, name: 'Revised' });
  assert.equal(await builder._save(), true);
  assert.equal(state.definition.calendarPinState, 'Stale');
  const priorConfirm = TfWindow.confirm;
  try {
    const titles = { en: 'Refresh calendar', pl: 'Odświeżenie kalendarza',
      de: 'Kalender aktualisieren', es: 'Actualizar calendario', fr: 'Actualiser le calendrier' };
    const actions = { en: 'Refresh calendar data and publish a new version',
      pl: 'Odśwież dane kalendarza i opublikuj nową wersję',
      de: 'Kalenderdaten aktualisieren und neue Version veröffentlichen',
      es: 'Actualizar los datos del calendario y publicar una nueva versión',
      fr: 'Actualiser les données du calendrier et publier une nouvelle version' };
    const cancellations = { en: 'Cancel', pl: 'Anuluj', de: 'Abbrechen', es: 'Cancelar', fr: 'Annuler' };
    for (const [language, title] of Object.entries(titles)) {
      await I18n.setLanguage(language);
      TfWindow.confirm = async (options) => {
        assert.equal(options.title, title);
        assert.equal(options.message, I18n.t('bpmn.calendar_refresh_hint'));
        assert.equal(options.confirmLabel, actions[language]);
        assert.equal(options.cancelLabel, cancellations[language]);
        assert.notEqual(options.title, options.confirmLabel, 'the full action stays in the confirmation button');
        return false;
      };
      await builder._publish();
      assert.equal(calls.some((call) => call.kind === 'processDefinitionPublishRequest'), false);
    }
    await I18n.setLanguage('en');
    TfWindow.confirm = async () => true;
    await builder._publish();
  } finally { TfWindow.confirm = priorConfirm; await I18n.setLanguage('en'); }
  assert.equal(calls.filter((call) => call.kind === 'processDefinitionPublishRequest').length, 1);
  assert.equal(state.canvas.getData().calendarPin.sha256, 'new-pin');
  state.canvas.updateNodeLabel('End', 'Ordinary edit');
  assert.equal(await builder._save(), true);
  assert.equal(calls.filter((call) => call.kind === 'processDefinitionSaveRequest').at(-1).payload.model.calendarPin.sha256, 'new-pin');
});

test('timer publication requires a real arming confirmation and refreshes the authoritative schedule', async () => {
  const current = definition('timer-publish', { model: timedModel('TimerStart', 'Cycle', { seconds: 300, totalFirings: 3 }) });
  let published = false; let confirmations = 0;
  const previousConfirm = TfWindow.confirm;
  try {
    const state = await mount(current, {
      processDefinitionGetRequest: () => ({ definition: { ...current, publishedVersion: published ? 1 : null }, startCatalog: published ? timerStartCatalog(current, savedTimer()) : [] }),
      processDefinitionPublishRequest: () => { published = true; return { definition: { ...current, publishedVersion: 1 }, version: { version: 1, model: current.model } }; },
    });
    TfWindow.confirm = async (options) => { confirmations += 1; assert.match(options.message, /Europe\/Warsaw/); assert.match(options.message, /arms|automatic/); return false; };
    await builder._publish();
    assert.equal(calls.some((call) => call.kind === 'processDefinitionPublishRequest'), false);
    TfWindow.confirm = async () => true;
    await builder._publish();
    assert.equal(confirmations, 1);
    assert.equal(calls.filter((call) => call.kind === 'processDefinitionPublishRequest').length, 1);
    assert.equal(calls.filter((call) => call.kind === 'processDefinitionGetRequest').length, 2);
    assert.ok(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'));
    assert.equal(state.root.querySelector('[data-role="schedule"]').hidden, false);
    assert.match(state.root.querySelector('[data-role="timer-summary"]').textContent, /Scheduled.*Europe\/Warsaw/);
    assert.equal(calls.some((call) => call.kind === 'processInstanceStartRequest'), false);
  } finally { TfWindow.confirm = previousConfirm; }
});

test('BPMN names retain their full text when saved and in immutable inspection', async () => {
  const name = `Long process name ${'Process'.repeat(28)}`;
  const nodeName = `Long timer name ${'Timer'.repeat(40)}`;
  const current = definition('long-names', { name, model: timedModel() });
  current.model.nodes[0].name = nodeName;
  let state = await mount(current, { processDefinitionSaveRequest: { definition: { ...current, draftRevision: 5 } } });
  const control = state.root.querySelector('[data-role="name"]');
  assert.equal(control.tagName, 'TF-TEXTAREA');
  assert.equal(control.querySelector('textarea').value, name);
  assert.equal(control.querySelector('textarea').getAttribute('aria-label'), I18n.t('flows_builder.name_label'));
  state.canvas.selectNode('Start');
  assert.equal(state.root.querySelector('[data-process="name"] textarea').value, nodeName);
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest');
  assert.equal(saved.payload.name, name);
  assert.equal(saved.payload.model.nodes[0].name, nodeName);
  state = await mount({ ...current, publishedVersion: 1 }, {
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: { version: { version: 1, model: current.model }, startCatalog: [] },
  });
  await builder._openProcessVersions();
  click(document.querySelector('tf-window tf-table').shadowRoot.querySelector('tbody tf-button'));
  await flush();
  state.canvas.selectNode('Start');
  const immutable = state.root.querySelector('[data-process="name"]');
  assert.equal(immutable.querySelector('textarea').value, nodeName);
  assert.equal(immutable.querySelector('textarea').disabled, true);
  change(immutable, 'Unauthorized rename');
  assert.equal(state.canvas.getData().nodes[0].name, nodeName);
});

test('BPMN fit includes the rendered event label without altering stored DI', () => {
  const graph = canvas(timedModel());
  const node = graph.nodes[0];
  const label = graph.nodesLayer.querySelector('[data-node-id="Start"] .fb-process-label');
  Object.defineProperties(label, {
    offsetWidth: { value: 200 }, offsetHeight: { value: 400 },
  });
  graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 500, height: 400 });
  const original = graph.getData();
  graph._layoutProcessLabels();
  const bounds = graph._contentBounds();
  assert.equal(bounds.minX, Math.min(...graph.nodes.map((row) => row.x), node.x + parseFloat(label.style.left)));
  assert.equal(bounds.maxY, Math.max(...graph.nodes.map((row) => row.y + row.height), node.y + parseFloat(label.style.top) + 400));
  graph.fitToContent();
  assert.ok(graph.view.y + bounds.maxY * graph.view.zoom <= 400);
  assert.ok(graph.view.y + bounds.minY * graph.view.zoom >= 0);
  assert.deepEqual(graph.getData(), original);
  graph.destroy();
});

test('BPMN fit waits for measurable canvas bounds before assigning a zoom', () => {
  const originalObserver = globalThis.ResizeObserver;
  let onResize;
  globalThis.ResizeObserver = class {
    constructor(callback) { onResize = callback; }
    observe() {}
    disconnect() {}
  };
  let graph;
  try {
    let rect = { left: 0, top: 0, width: 0, height: 0 };
    const root = document.createElement('div');
    root.getBoundingClientRect = () => rect;
    document.body.append(root);
    graph = new FlowCanvas(root, { mode: 'bpmn' });
    graph.setTemplates(processTemplates());
    graph.setData(timedModel());
    const model = graph.getData();
    graph.fitToContent();
    assert.equal(graph.view.zoom, 1);
    assert.equal(graph._pendingFitMinZoom, 0);
    rect = { left: 0, top: 0, width: 500, height: 400 };
    onResize();
    const bounds = graph._contentBounds();
    const expected = Math.min(1, Math.min(
      rect.width / (bounds.maxX - bounds.minX + 80),
      rect.height / (bounds.maxY - bounds.minY + 80),
    ));
    assert.equal(graph.view.zoom, expected);
    assert.ok(Number.isFinite(graph.view.zoom) && graph.view.zoom > 0);
    assert.equal(graph._pendingFitMinZoom, null);
    assert.deepEqual(graph.getData(), model);
    const rendered = graph.world.style.transform;
    graph.view.zoom = 0;
    assert.throws(() => graph._clientToWorld(200, 200), /Canvas zoom must be positive and finite/);
    assert.equal(graph.world.style.transform, rendered);
    graph.fitToContent();
    assert.equal(graph.view.zoom, expected);
    assert.equal(graph.world.style.transform, rendered);
    const point = graph._clientToWorld(200, 200);
    assert.ok(Number.isFinite(point.x) && Number.isFinite(point.y));
  } finally {
    graph?.destroy();
    globalThis.ResizeObserver = originalObserver;
  }
});

test('BPMN full task and sibling boundary names occupy separate measured regions before and after Fit', () => {
  const model = boundaryModel();
  const taskName = `Task A ${'A'.repeat(214)}`;
  const firstName = `Boundary B ${'B'.repeat(204)}`;
  const secondName = `Boundary C ${'C'.repeat(204)}`;
  model.nodes.find((node) => node.id === 'Review').name = taskName;
  model.nodes.find((node) => node.id === 'Timer_A').name = firstName;
  model.nodes.find((node) => node.id === 'Timer_B').name = secondName;
  const parent = model.diagram.shapes.find((shape) => shape.elementId === 'Review');
  for (const [id, centerY] of [['Timer_A', 175], ['Timer_B', 245]]) {
    const shape = model.diagram.shapes.find((row) => row.elementId === id);
    shape.x = parent.x + parent.width - shape.width / 2;
    shape.y = centerY - shape.height / 2;
  }
  const graph = canvas(model);
  const original = graph.getData();
  const taskElement = graph.nodesLayer.querySelector('[data-node-id="Review"]');
  const taskText = taskElement.querySelector('.fb-process-symbol > span');
  Object.defineProperties(taskText, {
    clientHeight: { get: () => taskElement.classList.contains('fb-external-label') ? 0 : 68 },
    scrollHeight: { get: () => taskElement.classList.contains('fb-external-label') ? 0 : 180 },
    clientWidth: { value: 180 }, scrollWidth: { value: 180 },
  });
  for (const [id, height] of [['Review', 180], ['Timer_A', 190], ['Timer_B', 190]]) {
    const label = graph.nodesLayer.querySelector(`[data-node-id="${id}"] .fb-process-label`);
    Object.defineProperties(label, { offsetWidth: { value: 200 }, offsetHeight: { value: height } });
  }
  graph._layoutProcessLabels();
  assert.equal(taskElement.classList.contains('fb-external-label'), true);
  const named = [['Review', taskName], ['Timer_A', firstName], ['Timer_B', secondName]];
  const rects = named.map(([id, name]) => {
    const node = graph.nodes.find((row) => row.id === id);
    const element = graph.nodesLayer.querySelector(`[data-node-id="${id}"]`);
    const label = element.querySelector('.fb-process-label');
    assert.equal(label.hidden, false);
    assert.equal(label.textContent, name);
    assert.equal(element.querySelector('.fb-process-label-leader').hidden, false);
    const left = node.x + parseFloat(label.style.left);
    const top = node.y + parseFloat(label.style.top);
    return { left, top, right: left + label.offsetWidth, bottom: top + label.offsetHeight };
  });
  const crosses = (a, b) => a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
  for (const label of rects) {
    for (const node of graph.nodes) {
      assert.equal(crosses(label, { left: node.x, top: node.y, right: node.x + node.width, bottom: node.y + node.height }), false);
    }
  }
  for (let i = 0; i < rects.length; i += 1) {
    for (let j = i + 1; j < rects.length; j += 1) assert.equal(crosses(rects[i], rects[j]), false);
  }
  graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 900, height: 600 });
  graph.fitToContent();
  assert.equal(taskElement.classList.contains('fb-external-label'), true);
  const bounds = graph._contentBounds();
  for (const label of rects) {
    assert.ok(bounds.minX <= label.left && bounds.maxX >= label.right);
    assert.ok(bounds.minY <= label.top && bounds.maxY >= label.bottom);
  }
  assert.deepEqual(graph.getData(), original);
  graph.updateNodeLabel('Review', 'Review contract');
  // A name that fits stays in the box even with events attached: they sit on the border.
  const shortTask = graph.nodesLayer.querySelector('[data-node-id="Review"]');
  assert.equal(shortTask.classList.contains('fb-external-label'), false);
  assert.equal(shortTask.querySelector('.fb-process-label').hidden, true);
  graph.removeNodes(['Timer_A', 'Timer_B']);
  const unencumberedTask = graph.nodesLayer.querySelector('[data-node-id="Review"]');
  assert.equal(unencumberedTask.classList.contains('fb-external-label'), false);
  assert.equal(unencumberedTask.querySelector('.fb-process-label').hidden, true);
  assert.equal(unencumberedTask.querySelector('.fb-process-symbol > span').textContent.trim(), 'Review contract');
  graph.destroy();
});

test('short Manual and Receive task labels stay inside the box while boundary events sit on its border, without changing DI', () => {
  for (const kind of ['ManualTask', 'ReceiveTask']) {
    const model = emptyProcessModel();
    model.targetNamespace = 'urn:example:boundary-labels';
    model.messages = [{ messageId: 'Message_1', name: 'Update' }];
    model.nodes.splice(1, 0,
      { id: 'Work', name: `Short ${kind} label`, kind: { [kind]: kind === 'ManualTask'
        ? { assigneeUserId: null, instructions: 'Acknowledge external work' }
        : { messageRef: 'Message_1', correlationExpression: 'vars.case_key', outputMapping: {} } } },
      { id: 'Attached', name: 'Update', kind: { BoundaryMessage: { attachedToId: 'Work',
        cancelActivity: true, messageRef: 'Message_1', correlationExpression: 'vars.case_key', outputMapping: {} } } });
    model.sequenceFlows = [
      { id: 'Flow_start_work', sourceId: 'Start', targetId: 'Work', condition: null },
      { id: 'Flow_work_end', sourceId: 'Work', targetId: 'End', condition: null },
      { id: 'Flow_boundary_end', sourceId: 'Attached', targetId: 'End', condition: null },
    ];
    model.diagram.shapes.splice(1, 0,
      { elementId: 'Work', x: 200, y: 160, width: 240, height: 96 },
      { elementId: 'Attached', x: 228, y: 230, width: 56, height: 56 });
    model.diagram.shapes.find((shape) => shape.elementId === 'End').x = 580;
    const graph = canvas(model);
    const before = graph.getData();
    const element = graph.nodesLayer.querySelector('[data-node-id="Work"]');
    const title = element.querySelector('.fb-process-symbol > span');
    Object.defineProperties(title, { clientHeight: { value: 60 }, scrollHeight: { value: 36 },
      clientWidth: { value: 190 }, scrollWidth: { value: 120 } });
    graph._layoutProcessLabels();
    assert.equal(element.classList.contains('fb-external-label'), false, `${kind}: the name stays on the task`);
    assert.equal(element.querySelector('.fb-process-label').hidden, true, kind);
    // The attached event itself has no room for a name inside, so its own label is outside.
    assert.equal(graph.nodesLayer.querySelector('[data-node-id="Attached"] .fb-process-label').hidden, false, kind);
    graph.fitToContent();
    assert.equal(element.classList.contains('fb-external-label'), false, kind);
    assert.deepEqual(graph.getData(), before);
    graph.destroy();
  }
});

test('BPMN minimap confines its real viewport to the measured map area', async () => {
  const state = await mount(definition('measured-minimap', { model: timedModel() }));
  const mini = state.root.querySelector('[data-role="minimap"]');
  Object.defineProperties(mini, { clientWidth: { value: 118 }, clientHeight: { value: 78 } });
  state.canvas.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 1500, height: 1200 });
  state.canvas.view = { x: 0, y: 0, zoom: 1 };
  builder._renderMinimap();
  const viewport = mini.querySelector('[data-role="minimap-viewport"]');
  assert.equal(viewport.hidden, false);
  assert.ok(parseFloat(viewport.style.left) >= 0);
  assert.ok(parseFloat(viewport.style.top) >= 18);
  assert.ok(parseFloat(viewport.style.left) + parseFloat(viewport.style.width) <= 118);
  assert.ok(parseFloat(viewport.style.top) + parseFloat(viewport.style.height) <= 78);
});

test('published timer start cannot submit a manual run even with a direct helper call', async () => {
  const current = definition('timed', { model: timedModel(), publishedVersion: 2 });
  const state = await mount(current, { processDefinitionGetRequest: { definition: current, startCatalog: timerStartCatalog(current, savedTimer()) }, processVersionGetRequest: { version: { version: 2, model: current.model }, startCatalog: timerStartCatalog(current, savedTimer()) } });
  assert.ok(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'));
  change(state.root.querySelector('[data-role="start-entry"]'), '0');
  assert.ok(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'));
  await builder._runProcess();
  assert.equal(document.querySelector('.tf-act-window'), null);
  assert.match(document.querySelector('[data-timers]').textContent, /Daily approval/);
  const form = openProcessRun(current, [{ version: 2, model: current.model }], timerStartCatalog(current, savedTimer()));
  assert.ok(form.querySelector('[data-act="submit"]').hasAttribute('disabled'));
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.equal(calls.some((call) => call.kind === 'processInstanceStartRequest'), false);
});

test('published message start offers the current authorized send and never invokes manual start', async () => {
  const model = emptyProcessModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.nodes[0].kind = { MessageStart: { messageRef: 'Message_1', outputMapping: {} } };
  const current = definition('message-start', { model, publishedVersion: 2 });
  const startCatalog = messageStartCatalog(current);
  const state = await mount(current, { processDefinitionGetRequest: { definition: current, startCatalog },
    processVersionGetRequest: { version: { version: 2, model }, startCatalog } });
  assert.equal(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'), true);
  change(state.root.querySelector('[data-role="start-entry"]'), '0');
  assert.equal(state.root.querySelector('[data-role="send-start"]').hidden, false);
  click(state.root.querySelector('[data-role="send-start"]'));
  assert.ok(document.querySelector('.tf-act-window [data-message-name]'));
  await builder._runProcess();
  assert.equal(calls.some((call) => call.kind === 'processInstanceStartRequest'), false);
  assert.equal(document.querySelectorAll('.tf-act-window [data-message-name]').length, 2);
});

test('schedule reads persisted Pending, Blocked and Missed states and scopes delayed reads to its window', async () => {
  let actual = savedTimer();
  const schedule = definition('schedule', { model: timedModel(), publishedVersion: 2 });
  fixtures({ processDefinitionGetRequest: () => ({ definition: schedule, startCatalog: timerStartCatalog(schedule, actual) }) });
  const win = await openProcessSchedule('schedule');
  assert.match(win.querySelector('[data-timers]').textContent, /Scheduled.*Europe\/Warsaw/);
  assert.doesNotMatch(win.textContent, /private-timer-id/);
  actual = savedTimer({ status: 'Blocked', lastReason: 'Current source access was revoked' }); await poll();
  assert.match(win.querySelector('[data-timers]').textContent, /Blocked.*Europe\/Warsaw/);
  assert.match(win.querySelector('[data-timers]').textContent, /Current source access was revoked/);
  actual = savedTimer({ status: 'Missed', dueAtMs: null }); await poll();
  assert.match(win.querySelector('[data-timers]').textContent, /Missed.*publish.*No next deadline/);
  const pending = deferred(); responder = () => pending.promise; const loading = poll();
  win.remove(); await flush();
  const otherDefinition = definition('other', { model: timedModel(), publishedVersion: 1 });
  fixtures({ processDefinitionGetRequest: { definition: otherDefinition, startCatalog: timerStartCatalog(otherDefinition, savedTimer({ nodeName: 'Current other' })) } });
  const other = await openProcessSchedule('other');
  const late = definition('late', { model: timedModel(), publishedVersion: 1 });
  pending.resolve({ definition: late, startCatalog: timerStartCatalog(late, savedTimer({ nodeName: 'Late private data' })) }); await loading;
  assert.match(other.querySelector('[data-timers]').textContent, /Current other/);
  assert.doesNotMatch(other.textContent, /Late private data/);
});

test('schedule renders a long unbroken definition name and untrusted markup as text', async () => {
  const name = `${'Approval'.repeat(24)}X<img src=x onerror=alert(1)>`;
  assert.equal(name.length, 221);
  const current = definition('long-schedule', { name, model: timedModel(), publishedVersion: 1 });
  fixtures({ processDefinitionGetRequest: { definition: current, startCatalog: timerStartCatalog(current, savedTimer()) } });
  const win = await openProcessSchedule('long-schedule');
  const summary = win.querySelector('[data-schedule-summary]');
  assert.ok(summary.textContent.startsWith(name));
  assert.equal(summary.querySelector('img'), null);
  assert.equal(win.querySelector('[data-timers]').textContent.includes('Daily approval'), true);
});

test('working timer renders the pinned offset and provenance without browser timezone arithmetic', async () => {
  const dueAtMs = Date.UTC(2027, 2, 28, 1, 30);
  const workingTime = { calendarName: '<Warsaw office>', holidayPolicy: 'PolandStatutory', pinSha256: 'a'.repeat(64),
    legalReleaseId: 'PL-2026-10', legalAsOfDate: '2026-10-02', tzdbReleaseId: '2026e', dueOffsetSeconds: 7200 };
  const timer = savedTimer({ kind: 'Catch', nodeName: 'Review', dueAtMs, workingTime });
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const win = await monitor(instance(`working-${language}`, { timers: [timer] }));
      const row = win.querySelector('[data-timers]');
      assert.ok(row.textContent.includes('UTC+02:00'));
      assert.ok(row.textContent.includes(new Date(dueAtMs).toISOString()));
      assert.ok(row.textContent.includes('2026e'));
      assert.ok(row.textContent.includes('PL-2026-10'));
      assert.ok(row.textContent.includes(workingTime.pinSha256));
      assert.ok(row.textContent.includes(I18n.t('bpmn.calendar_policy_poland')));
      assert.equal(row.querySelector('warsaw'), null, 'calendar name is untrusted text');
      const fired = processEventText({ kind: 'timer_fired', nodeName: 'Review', data: {
        kind: 'Catch', planned_due_at_ms: dueAtMs, fired_at_ms: dueAtMs + 1000,
        skipped_count: 0, timezone: 'Europe/Warsaw', working_time: { due_offset_seconds: 7200 },
      } });
      assert.ok(fired.includes(new Date(dueAtMs + 1000).toISOString()));
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('timer arming history uses the persisted IANA zone instead of the browser zone', async () => {
  const priorZone = process.env.TZ;
  const dueAtMs = 1791020287134;
  const event = { kind: 'timer_armed', nodeName: 'TimerWait', data: {
    due_at_ms: dueAtMs, timezone: 'UTC', timer_id: 'timer-1',
  } };
  process.env.TZ = 'Europe/Warsaw';
  try {
    assert.equal(Intl.DateTimeFormat().resolvedOptions().timeZone, 'Europe/Warsaw');
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const zoned = new Date(dueAtMs).toLocaleString(language, { timeZone: 'UTC' });
      const browser = new Date(dueAtMs).toLocaleString(language);
      assert.notEqual(zoned, browser);
      const original = structuredClone(event);
      const rendered = processEventText(event);
      assert.ok(rendered.includes(zoned));
      assert.ok(rendered.includes('UTC'));
      assert.ok(!rendered.includes(browser));
      assert.deepEqual(event, original);
      const working = processEventText({ ...event, data: {
        ...event.data, working_time: { due_offset_seconds: 7200 },
      } });
      assert.ok(working.includes(new Date(dueAtMs).toISOString()));
      assert.ok(working.includes('UTC+02:00'));
    }
  } finally {
    if (priorZone === undefined) delete process.env.TZ;
    else process.env.TZ = priorZone;
    await I18n.setLanguage('en');
  }
});

test('schedule and current start localize known timer reasons in five languages without changing arbitrary errors', async () => {
  const reasons = [
    ['instance_cancelled', 'event_cancelled'],
    ['definition_archived', 'timer_reason_definition_archived'],
    ['missed_during_archive', 'timer_reason_missed_during_archive'],
    ['finite_schedule_exhausted_during_archive', 'timer_reason_finite_schedule_exhausted_during_archive'],
    ['superseded_by_publication', 'timer_reason_superseded_by_publication'],
  ];
  const markup = '<script>kept as text</script>';
  const arbitrary = `${'x'.repeat(32768 - markup.length)}${markup}`;
  assert.equal(new TextEncoder().encode(arbitrary).length, 32768);
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const current = definition(`reason-${language}`, { model: timedModel(), publishedVersion: 1 });
      const timer = savedTimer({ lastReason: reasons[0][0] });
      const state = await mount(current, { processDefinitionGetRequest: () => ({ definition: current, startCatalog: timerStartCatalog(current, timer) }) });
      const win = await openProcessSchedule(current.definitionId);
      for (const [reason, key] of reasons) {
        timer.lastReason = reason;
        state.startCatalog = timerStartCatalog(current, timer);
        builder._syncProcessControls();
        await poll();
        const translated = I18n.t(`bpmn.${key}`);
        assert.notEqual(translated, `bpmn.${key}`);
        assert.equal(processLifecycleReasonText(reason), translated);
        assert.equal(win.querySelector('[data-timers] dd:last-child').textContent, translated);
        assert.ok(state.root.querySelector('[data-role="timer-summary"]').textContent.includes(translated));
        assert.doesNotMatch(win.querySelector('[data-timers]').textContent, new RegExp(reason));
      }
      timer.lastReason = arbitrary;
      state.startCatalog = timerStartCatalog(current, timer);
      builder._syncProcessControls();
      await poll();
      assert.equal(win.querySelector('[data-timers] dd:last-child').textContent, arbitrary);
      assert.equal(win.querySelector('[data-timers] script'), null);
      assert.ok(state.root.querySelector('[data-role="timer-summary"]').textContent.includes(arbitrary));
      assert.equal(processLifecycleReasonText(arbitrary), arbitrary);
      win.dispatchEvent(new Event('closed'));
      win.remove();
    }
  } finally { await I18n.setLanguage('en'); }
});

test('instance monitor renders actual Catch deadlines and later cancellation without a client fire control', async () => {
  const timer = savedTimer({ kind: 'Catch', nodeId: 'Wait', nodeName: '<Saved wait>', totalFirings: null, occurrence: 1 });
  let current = instance('catch', { activeNodeIds: ['Wait'], timers: [timer] });
  const win = await monitor(current, { processInstanceGetRequest: () => ({ instance: current }) });
  assert.equal(win.querySelector('[data-timer-section]').hidden, false);
  assert.match(win.querySelector('[data-timers]').textContent, /<Saved wait>[\s\S]*Scheduled/);
  assert.equal(win.querySelector('[data-timers] saved'), null);
  assert.equal(win.querySelector('[data-fire-timer]'), null);
  current = { ...current, revision: 12, status: 'Cancelled', timers: [{ ...timer, status: 'Cancelled' }] }; await poll();
  assert.match(win.querySelector('[data-timers]').textContent, /Cancelled/);
  assert.equal(calls.some((call) => /Timer(Fire|Complete)/.test(call.kind)), false);
});

test('timer XML response retains server literals, IANA and business keys when imported and saved', async () => {
  const imported = timedModel('TimerStart', 'Daily', { hour: 9, minute: 30, totalFirings: 3 });
  const state = await mount(definition('timer-import'), { processXmlImportRequest: { model: imported, diagnostics: [] }, processDefinitionSaveRequest: (payload) => ({ definition: definition('timer-import', { model: payload.model, draftRevision: 5 }) }) });
  builder._importProcess(); await flush();
  const form = document.querySelector('.tf-act-window');
  form.querySelector('tf-code-editor').value = '<bpmn:definitions/>';
  form.querySelector('tf-code-editor').dispatchEvent(new Event('input', { bubbles: true }));
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.deepEqual(state.canvas.getData(), imported);
  assert.equal(state.root.querySelector('[data-role="timer-timezone"]').hidden, false);
  await builder._save();
  assert.deepEqual(calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model, imported);
});

test('all five locales describe actual timer states and events without raw IDs or unexpanded parameters', async () => {
  const events = [
    { kind: 'timer_armed', data: { timer_id: 'private-id', due_at_ms: 1000, timezone: 'Europe/Warsaw' } },
    { kind: 'timer_fired', data: { timer_id: 'private-id', planned_due_at_ms: 1000, fired_at_ms: 2000, skipped_count: 2 } },
    { kind: 'timer_blocked', data: { timer_id: 'private-id', reason: 'Permission changed', due_at_ms: 1000, next_check_at_ms: 62000 } },
    { kind: 'timer_cancelled', data: { timer_id: 'private-id', reason: 'instance_cancelled' } },
    { kind: 'timer_error', data: { timer_id: 'private-id', reason: 'Next deadline exceeds the supported range', due_at_ms: 1000 } },
  ];
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    for (const status of ['Pending', 'Fired', 'Cancelled', 'Archived', 'Blocked', 'Missed', 'Error']) {
      assert.doesNotMatch(processTimerText(savedTimer({ status })), /bpmn\.|\{status\}|\{due\}|\{timezone\}/);
    }
    for (const event of events) assert.doesNotMatch(processEventText({ ...event, nodeName: 'Saved deadline' }), /bpmn\.|private-id|instance_cancelled|\{node\}|\{message\}|\{retry\}|\{due\}|\{actual\}|\{count\}/);
  }
  await I18n.setLanguage('en');
});

test('all five locales render boundary cancellation truth and preserve arbitrary reasons', async () => {
  const arbitrary = '<untrusted>' + 'reason_'.repeat(4600);
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      for (const reason of ['activity_completed', 'sibling_interrupted', 'event_race_lost']) {
        const label = processLifecycleReasonText(reason);
        assert.equal(label, I18n.t(`bpmn.timer_reason_${reason}`));
        assert.doesNotMatch(label, /bpmn\.|activity_completed|sibling_interrupted|event_race_lost/);
        const event = processEventText({ kind: 'timer_cancelled', nodeName: 'Deadline', data: { kind: 'Boundary', reason, timer_id: 'private-id' } });
        assert.ok(event.includes(label));
        assert.doesNotMatch(event, /private-id|\{reason\}|\{node\}/);
      }
      const interruptedByMessage = processEventText({ kind: 'timer_cancelled', nodeName: 'Deadline',
        data: { kind: 'Boundary', reason: 'sibling_interrupted', winning_timer_id: 'message-subscription-1' } });
      assert.ok(interruptedByMessage.includes(I18n.t('bpmn.timer_reason_sibling_interrupted')));
      assert.doesNotMatch(interruptedByMessage, /message-subscription-1|sibling_interrupted/);
      const noninterrupting = processEventText({ kind: 'timer_fired', nodeName: 'Reminder', data: {
        kind: 'Boundary', cancel_activity: false, planned_due_at_ms: 1000, fired_at_ms: 2000,
      } });
      assert.ok(noninterrupting.includes(I18n.t('bpmn.boundary_noninterrupting')));
      const interrupting = processEventText({ kind: 'timer_fired', nodeName: 'Deadline', data: {
        kind: 'Boundary', cancel_activity: true, planned_due_at_ms: 1000, fired_at_ms: 2000,
      } });
      assert.ok(interrupting.includes(I18n.t('bpmn.boundary_interrupting')));
      assert.equal(processLifecycleReasonText(arbitrary), arbitrary);
      assert.ok(processEventText({ kind: 'timer_cancelled', nodeName: 'Deadline', data: { reason: arbitrary } }).includes(arbitrary));
    }
  } finally { await I18n.setLanguage('en'); }
  const current = instance('boundary-empty-name', { timers: [{ ...savedTimer({ kind: 'Boundary', nodeName: '' }), attachedToId: 'n_review_a1b2' }] });
  const named = emptyProcessModel();
  named.nodes.push({ id: 'n_review_a1b2', name: 'Check documents', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } });
  const win = await monitor(current, { processVersionGetRequest: { version: { model: named }, startCatalog: [] } });
  await flush(2);
  assert.ok(win.querySelector('[data-timers]').textContent.includes(I18n.t('bpmn.node_boundary_timer')));
  assert.ok(win.querySelector('[data-timers]').textContent.includes('Check documents'));
  assert.doesNotMatch(win.querySelector('[data-timers]').textContent, /n_review_a1b2/);
  assert.equal(win.querySelector('[data-timers] script'), null);
});

test('a boundary timer names its attached activity in five locales, escaped, and never by its element id', async () => {
  const captions = { en: 'Attached activity', pl: 'Przypięta czynność', de: 'Angehängte Aktivität', es: 'Actividad adjunta', fr: 'Activité attachée' };
  const modelWith = (name) => {
    const model = emptyProcessModel();
    model.nodes.push({ id: 'Review_1', name, kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } });
    return { version: { model }, startCatalog: [] };
  };
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const graph = canvas(boundaryModel());
      const boundary = graph.nodes.find((node) => node.id === 'Timer_A');
      const panel = inspector(graph);
      panel.show(boundary, graph.templates.get(boundary.type));
      assert.equal(panel.root.querySelector('[data-process="attachedToId"]').getAttribute('label'), captions[language]);
      const timer = savedTimer({ kind: 'Boundary', nodeName: 'Deadline', attachedToId: 'Review_1' });
      const win = await monitor(instance(`boundary-id-${language}`, { timers: [timer] }), { processVersionGetRequest: modelWith('Check documents') });
      await flush(2);
      const terms = [...win.querySelectorAll('[data-timers] dt')].map((term) => term.textContent);
      const values = [...win.querySelectorAll('[data-timers] dd')].map((value) => value.textContent);
      assert.deepEqual(terms, [I18n.t('bpmn.timer_occurrence'), captions[language]]);
      assert.deepEqual(values, [I18n.t('bpmn.timer_slot', { occurrence: 3, total: 7 }), 'Check documents']);
      assert.doesNotMatch(terms.join(' '), /bpmn\.|\{occurrence\}|\{total\}|ID/);
      win.remove();
      graph.destroy();
    }
    await I18n.setLanguage('en');
    const untrusted = '<img src=x onerror=alert(1)>';
    const timer = savedTimer({ kind: 'Boundary', attachedToId: 'Review_1', totalFirings: null });
    const win = await monitor(instance('boundary-escaped-name', { timers: [timer] }), { processVersionGetRequest: modelWith(untrusted) });
    await flush(2);
    const values = [...win.querySelectorAll('[data-timers] dd')].map((value) => value.textContent);
    assert.deepEqual(values, ['3', untrusted]);
    assert.equal(win.querySelector('[data-timers] img'), null);
    const lost = await monitor(instance('boundary-lost-model', { timers: [{ ...timer, attachedToId: 'n_gone_zz99' }] }));
    await flush(2);
    assert.deepEqual([...lost.querySelectorAll('[data-timers] dd')].map((value) => value.textContent), ['3', I18n.t('bpmn.element_unavailable')]);
  } finally { await I18n.setLanguage('en'); }
});

test('terminal timer Error displays its full reason and real incident without offering a service retry', async () => {
  const reason = 'Daily time has no supported UTC instant in the selected time zone; publication must use a supported time.';
  const timer = savedTimer({ kind: 'Catch', status: 'Error', nodeName: 'Waiting step', dueAtMs: null, lastReason: reason });
  const current = instance('timer-error', { status: 'Incident', timers: [timer], incidents: [{ incidentId: 'incident', nodeId: 'Wait', nodeName: 'Waiting step', code: 'TIMER_ERROR', message: reason, jobId: null, canRetry: false }] });
  const win = await monitor(current);
  assert.match(win.querySelector('[data-timers]').textContent, /Timer error.*No next deadline/);
  assert.ok(win.querySelector('[data-timers]').textContent.includes(reason));
  assert.ok(win.querySelector('[data-incidents]').textContent.includes(reason));
  assert.equal(win.querySelector('[data-retry]'), null);
  const actual = definition('start-error', { model: timedModel(), publishedVersion: 4 });
  const state = await mount(actual, { processDefinitionGetRequest: { definition: actual, startCatalog: timerStartCatalog(actual, { ...timer, kind: 'Start' }) } });
  assert.ok(state.root.querySelector('[data-role="timer-summary"]').textContent.includes(reason));
  assert.match(state.root.querySelector('[data-role="timer-summary"]').textContent, /Current start.*4.*Timer error/);
  assert.ok(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'));
});

test('current schedule access denial clears persisted private details and disables its run-list action', async () => {
  let revoked = false;
  fixtures({ processDefinitionGetRequest: () => { if (revoked) throw new Error('Current access was revoked'); const current = definition('private-schedule', { model: timedModel(), publishedVersion: 1 }); return { definition: current, startCatalog: timerStartCatalog(current, savedTimer()) }; } });
  const win = await openProcessSchedule('private-schedule');
  assert.equal(win.querySelectorAll('[data-timer-id]').length, 1);
  revoked = true; await poll();
  assert.equal(win.querySelectorAll('[data-timer-id]').length, 0);
  assert.ok(win.querySelector('[data-schedule-instances]').hasAttribute('disabled'));
  assert.match(win.querySelector('[data-error]').getAttribute('message'), /revoked/);
  revoked = false; await poll();
  assert.equal(win.querySelectorAll('[data-timer-id]').length, 1);
  assert.equal(win.querySelector('[data-schedule-instances]').hasAttribute('disabled'), false);
  assert.equal(win.querySelector('[data-error]').hidden, true);
});

test('blank required timer numbers block draft save and publication instead of silently scheduling zero', async () => {
  for (const [type, spec, controlKey, field, valid, invalid] of [
    ['Daily', { hour: 14, minute: 32, totalFirings: null }, 'timerHour', 'hour', '0', '24'],
    ['Daily', { hour: 14, minute: 32, totalFirings: null }, 'timerMinute', 'minute', '0', '60'],
    ['Duration', { seconds: 60 }, 'timerSeconds', 'seconds', '1', '0'],
    ['Cycle', { seconds: 300, totalFirings: null }, 'timerSeconds', 'seconds', '300', '299'],
  ]) {
    const id = `required-${type}-${field}`;
    const original = definition(id, { model: timedModel('TimerStart', type, spec) });
    const state = await mount(original, {
      processDefinitionSaveRequest: (payload) => ({ definition: { ...original, model: payload.model, draftRevision: 5 } }),
      processDefinitionPublishRequest: () => ({ definition: { ...original, publishedVersion: 1 }, version: { version: 1, model: state.canvas.getData() } }),
    });
    state.canvas.selectNode('Start'); await flush(2);
    const control = state.config.root.querySelector(`[data-process="${controlKey}"]`);
    change(control, '');
    const serialized = state.canvas.getData();
    assert.equal(await builder._save(), false, `A blank ${field} must not save ${JSON.stringify(serialized.nodes[0].kind.TimerStart.timer)}`);
    assert.equal(serialized.nodes[0].kind.TimerStart.timer[type][field], null);
    assert.equal(control.value, '');
    assert.ok(state.canvas.validate().length > 0);
    await builder._publish();
    assert.equal(calls.some((call) => call.kind === 'processDefinitionSaveRequest' && call.payload.definitionId === id), false);
    assert.equal(calls.some((call) => call.kind === 'processDefinitionPublishRequest' && call.payload.definitionId === id), false);
    for (const value of [invalid, '1.5']) {
      change(control, value);
      assert.equal(await builder._save(), false);
    }
    change(control, valid);
    assert.equal(state.canvas.validate().length, 0);
    const total = state.config.root.querySelector('[data-process="timerTotal"]');
    if (total) {
      for (const value of ['0', '4294967296', '1.5']) {
        change(total, value);
        assert.equal(await builder._save(), false);
      }
      change(total, '');
      assert.equal(state.canvas.getData().nodes[0].kind.TimerStart.timer[type].totalFirings, null);
      assert.equal(state.canvas.validate().length, 0);
    }
    assert.equal(await builder._save(), true);
    assert.equal(calls.find((call) => call.kind === 'processDefinitionSaveRequest' && call.payload.definitionId === id).payload.model.nodes[0].kind.TimerStart.timer[type][field], Number(valid));
  }
});

test('mounted body selector authors and publishes two exact executable process bodies', async () => {
  const current = definition('body-selection');
  const state = await mount(current, {
    processDefinitionGetRequest: { definition: current, startCatalog: [] },
    processDefinitionSaveRequest: ({ model }) => ({ definition: { ...current, model, draftRevision: 5 } }),
    processDefinitionPublishRequest: () => ({ definition: { ...current, publishedVersion: 1 },
      version: { version: 1, model: state.canvas.getData() } }),
  });
  click(state.root.querySelector('[data-role="add-body"]'));
  const added = state.canvas.selectedProcessId;
  assert.notEqual(added, current.model.processId);
  change(state.root.querySelector('[data-role="body-name"]'), 'Review & sign-off');
  const authored = state.canvas.getData();
  assert.equal(authored.additionalProcesses.length, 1);
  assert.equal(authored.additionalProcesses[0].processId, added);
  assert.equal(authored.additionalProcesses[0].processName, 'Review & sign-off');
  assert.deepEqual(authored.nodes, current.model.nodes);
  change(state.root.querySelector('[data-role="body-select"]'), current.model.processId);
  assert.equal(state.canvas.selectedProcessId, current.model.processId);
  change(state.root.querySelector('[data-role="body-select"]'), added);
  assert.equal(state.canvas.selectedProcessId, added);
  assert.equal(await builder._save(), true);
  const saved = calls.filter((row) => row.kind === 'processDefinitionSaveRequest').at(-1).payload.model;
  assert.equal(saved.additionalProcesses[0].processName, 'Review & sign-off');
  await builder._publish();
  assert.equal(calls.filter((row) => row.kind === 'processDefinitionPublishRequest').length, 1);
  assert.equal(state.canvas.getData().additionalProcesses[0].processId, added);
});

test('activity result inspector edits the localized CEL result and exposes no output port', async () => {
  const expression = "{'outcome':'Completed','code':null,'summary':'Completed','outputs':payload,'evidence':[]}";
  const template = {
    node_type: 'activity_result', category: 'output', label: 'Activity result',
    default_config: JSON.stringify({ result_expression: expression }),
    params_schema: JSON.stringify({ properties: { result_expression: {
      type: 'string', title: 'Activity result (CEL)', title_key: 'flows_config.activity_result_expression',
      description: 'Return the complete result object.', description_key: 'flows_config.activity_result_expression_hint',
      format: 'textarea',
    } }, required: ['result_expression'], order: ['result_expression'] }),
    input_ports: ['in'], output_ports: [],
  };
  const graphRoot = document.createElement('div'); document.body.append(graphRoot);
  const graph = new FlowCanvas(graphRoot, { mode: 'flow' });
  graph.setTemplates([template]);
  graph.setData([{ id: 'Result', type: 'activity_result', label: '',
    position: { x: 240, y: 180 }, config: { result_expression: expression } }], []);
  const configRoot = document.createElement('aside'); document.body.append(configRoot);
  const config = new FlowConfig(configRoot, { mode: 'flow', getCanvas: () => graph,
    onConfigChange: (id, patch) => graph.updateNodeConfig(id, patch) });
  try {
    assert.equal(graph.nodesLayer.querySelector('[data-node-id="Result"] .fb-port-out'), null);
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      config.show(graph.nodes[0], template);
      const field = config.root.querySelector('tf-textarea[data-bind="result_expression"]');
      assert.ok(field, 'the schema uses the project textarea component');
      assert.equal(field.value, expression);
      assert.equal(field.getAttribute('label'), `${I18n.t('flows_config.activity_result_expression')} *`);
      assert.equal(field.getAttribute('hint'), I18n.t('flows_config.activity_result_expression_hint'));
      const tabs = config.root.querySelector('tf-tabs');
      tabs.value = 'ports';
      tabs.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: 'ports' } }));
      assert.ok(config.root.querySelector('[data-role="body"]').textContent
        .includes(I18n.t('flows_config.no_outputs')));
      assert.equal(config.root.querySelectorAll('.fb-port-list li').length, 1, 'only the factual input port remains');
    }
    await I18n.setLanguage('en');
    config.show(graph.nodes[0], template);
    const field = config.root.querySelector('tf-textarea[data-bind="result_expression"]');
    const edited = "{'outcome':'NeedsHuman','code':'REVIEW','summary':'Review','outputs':{},'evidence':[]}";
    change(field, edited);
    assert.equal(graph.nodes[0].config.result_expression, edited);
    assert.equal(graph.nodesLayer.querySelector('[data-node-id="Result"] .fb-port-out'), null);
  } finally {
    await I18n.setLanguage('en');
    config.destroy();
    graph.destroy();
  }
});

test('simulation event list counts the server omissions and the rows the view itself drops', async () => {
  fixtures({ processSimulationReleaseRequest: {} });
  const events = Array.from({ length: 120 }, (_, index) => ({ atMs: index, kind: 'node_completed', nodeId: `Node_${index}` }));
  const win = simulationWindow({ simulationId: 'simulation-one', clock: { nowMs: 5, horizonMs: 10, stepIndex: 2 },
    instance: null, userTasks: [], timers: [], events, eventsOmitted: 7, traceStepsOmitted: 3 },
  { nodes: events.map((event, index) => ({ id: event.nodeId, name: `Step ${index}`, kind: 'ServiceTask' })), additionalProcesses: [] });
  const content = win.querySelector('[data-simulation-content]');
  assert.equal(content.querySelectorAll('.fb-simulation-events li').length, 50);
  assert.match(content.querySelector('.fb-simulation-events li').textContent, /Step 70/);
  assert.doesNotMatch(content.querySelector('.fb-simulation-events').textContent, /Node_\d+/, 'events name their elements, never cite ids');
  assert.equal(content.querySelector('[data-simulation-events-omitted]').textContent.trim(), '77 earlier events not shown');
  assert.equal(content.querySelector('[data-simulation-trace-omitted]').textContent.trim(), '3 earlier trace steps not shown');
  win.remove();

  const complete = simulationWindow({ simulationId: 'simulation-two', clock: { nowMs: 5, horizonMs: 10, stepIndex: 2 },
    instance: null, userTasks: [], timers: [], events: events.slice(0, 3), eventsOmitted: 0, traceStepsOmitted: 0 });
  assert.equal(complete.querySelector('[data-simulation-events-omitted]'), null);
  assert.equal(complete.querySelector('[data-simulation-trace-omitted]'), null);
  complete.remove();
});

const tentaflowOptions = {
  ...options,
  serviceBlocks: [
    { nodeType: 'llm', label: 'Language model (LLM)', description: 'Generates text', category: 'service', requiredSettings: ['model'] },
    { nodeType: 'pii_filter', label: 'Personal data filter', description: 'Masks personal data', category: 'filter', requiredSettings: [] },
  ],
  serviceTools: [
    { addonId: 'notes-1a2b3c4d', toolName: 'search', description: 'Searches notes', addonName: 'Notes <b>A</b>' },
    { addonId: 'notes-5e6f7a8b', toolName: 'search', description: 'Searches notes', addonName: 'Notes B' },
  ],
};

test('TentaFlow palette group lists flows, blocks and addon tools as ready-made Service tasks', async () => {
  const templates = processTemplates(tentaflowOptions);
  assert.equal(processTemplates().length, 29);
  assert.equal(templates.length, 29 + 1 + 2 + 2);
  const dynamic = templates.filter((row) => row.category === 'tentaflow');
  assert.deepEqual(dynamic.map((row) => row.subgroup), ['flows', 'blocks', 'blocks', 'tools', 'tools']);
  assert.ok(dynamic.every((row) => row.node_type === 'bpmn_service_task' && row.preset));
  assert.deepEqual(dynamic.map((row) => row.label), ['Contract checker', 'Language model (LLM)', 'Personal data filter', 'Notes <b>A</b> › search', 'Notes B › search']);

  const root = document.createElement('aside'); document.body.append(root);
  const palette = new FlowPalette(root, { mode: 'bpmn', processOptions: tentaflowOptions }); await palette.init();
  const group = root.querySelector('[data-cat="tentaflow"]');
  assert.ok(group);
  assert.deepEqual([...group.querySelectorAll('[data-subgroup]')].map((row) => row.dataset.subgroup), ['flows', 'blocks', 'tools']);
  assert.equal(group.querySelectorAll('[data-node-type="bpmn_service_task"]').length, 5);
  assert.equal(root.querySelectorAll('[data-node-type="bpmn_service_task"]').length, 6);
  const toolRows = [...group.querySelectorAll('.fb-node-item')].slice(-2);
  assert.deepEqual(toolRows.map((row) => row.querySelector('.fb-node-name').textContent), ['search', 'search']);
  assert.deepEqual(toolRows.map((row) => row.querySelector('.fb-node-desc').textContent), ['Notes <b>A</b>', 'Notes B']);
  assert.equal(group.querySelector('.fb-node-name b'), null, 'the addon name is escaped');
  assert.match(toolRows[0].getAttribute('title'), /Searches notes/);
  palette.destroy();
});

test('TentaFlow palette group is absent without options and explains an empty catalogue', async () => {
  const unavailable = document.createElement('aside'); document.body.append(unavailable);
  const first = new FlowPalette(unavailable, { mode: 'bpmn' }); await first.init();
  assert.equal(unavailable.querySelector('[data-cat="tentaflow"]'), null);
  assert.equal(unavailable.querySelectorAll('[data-node-type]').length, 29);
  first.destroy();

  const empty = document.createElement('aside'); document.body.append(empty);
  const second = new FlowPalette(empty, { mode: 'bpmn', processOptions: { ...options, serviceFlows: [], serviceBlocks: [], serviceTools: [] } });
  await second.init();
  assert.equal(empty.querySelectorAll('[data-cat="tentaflow"] .fb-palette-note').length, 3);
  second.destroy();
});

test('dropping a TentaFlow entry creates a configured, named Service task without altering the plain element', () => {
  const graph = canvas(); graph.setTemplates(processTemplates(tentaflowOptions));
  const find = (label) => graph.templates.get('bpmn_service_task') && processTemplates(tentaflowOptions).find((row) => row.label === label);
  graph.addNodeFromTemplate(find('Contract checker'), 200, 100);
  graph.addNodeFromTemplate(find('Language model (LLM)'), 200, 300);
  graph.addNodeFromTemplate(find('Personal data filter'), 200, 500);
  graph.addNodeFromTemplate(find('Notes B › search'), 200, 700);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 600, 100);
  const [flow, llm, pii, tool, plain] = graph.nodes.slice(-5);
  assert.equal(flow.label, 'Contract checker');
  assert.equal(flow.config.flowId, 'flow-one');
  assert.equal(flow.config.implementation, undefined);
  assert.deepEqual(llm.config.implementation, { Block: { nodeType: 'llm', config: { model: '' } } });
  assert.deepEqual(pii.config.implementation, { Block: { nodeType: 'pii_filter', config: {} } });
  assert.equal(tool.label, 'Notes B › search');
  assert.deepEqual(tool.config.implementation, { AddonTool: { addonId: 'notes-5e6f7a8b', toolName: 'search' } });
  assert.equal(tool.config.verification, 'Human');
  assert.equal(plain.label, '');
  assert.equal(plain.config.flowId, '');
  assert.equal(plain.config.implementation, undefined);
  assert.equal(graph.templates.get('bpmn_service_task').preset, undefined);
  const saved = graph.getData();
  const kinds = saved.nodes.filter((node) => node.kind.ServiceTask).map((node) => [node.name, node.kind.ServiceTask.implementation]);
  assert.deepEqual(kinds.slice(-5).map(([name]) => name), ['Contract checker', 'Language model (LLM)', 'Personal data filter', 'Notes B › search', '']);
  graph.destroy();
});

const utf8Bytes = (text) => new TextEncoder().encode(text).length;
const validNodeName = (name) => utf8Bytes(name) <= 256 && !/[\u0000-\u001f\u007f-\u009f]/.test(name);

test('a palette tool label stays a valid node name for a long, multibyte or control-laden addon name', () => {
  const hostile = `${'Ż'.repeat(400)}\u0000\u0007`;
  const templates = processTemplates({ ...options, serviceTools: [
    { addonId: 'notes-1', toolName: 'search', description: '', addonName: hostile },
    { addonId: 'notes-2', toolName: 't'.repeat(128), description: '', addonName: `😀\n${'x'.repeat(600)}` },
  ] });
  const labels = templates.filter((row) => row.subgroup === 'tools').map((row) => row.label);
  assert.equal(labels.length, 2);
  for (const label of labels) assert.ok(validNodeName(label), `${label.length} chars, ${utf8Bytes(label)} bytes`);
  assert.ok(labels[0].endsWith(' › search'), 'the tool name survives the cut');
  assert.ok(labels[1].endsWith(`› ${'t'.repeat(128)}`));
  assert.equal(boundedNodeName('ab\u0001cd'), 'ab cd');
  assert.equal(utf8Bytes(boundedNodeName('ż'.repeat(200))), 256);

  const graph = canvas(); graph.setTemplates(templates);
  graph.addNodeFromTemplate(templates.find((row) => row.label === labels[0]), 300, 300);
  const stored = graph.getData().nodes.find((node) => node.kind.ServiceTask?.implementation?.AddonTool);
  assert.ok(validNodeName(stored.name));
  graph.destroy();
});

test('two instances that read the same get their instance id in the label', () => {
  const templates = processTemplates({ ...options, serviceTools: [
    { addonId: 'notes-aaaa', toolName: 'search', description: '', addonName: 'Notes' },
    { addonId: 'notes-bbbb', toolName: 'search', description: '', addonName: 'Notes' },
    { addonId: 'wiki-cccc', toolName: 'search', description: '', addonName: 'Wiki' },
  ] });
  assert.deepEqual(templates.filter((row) => row.subgroup === 'tools').map((row) => row.label),
    ['Notes › search (notes-aaaa)', 'Notes › search (notes-bbbb)', 'Wiki › search']);
});

test('a flow row shows the flow description and falls back to the generic hint', () => {
  const templates = processTemplates({ ...options, serviceFlows: [
    { flowId: 'a', name: 'Checker', description: 'Checks contracts for missing clauses' },
    { flowId: 'b', name: 'Plain', description: '' },
  ] });
  const [described, plain] = templates.filter((row) => row.subgroup === 'flows');
  assert.equal(described.description, 'Checks contracts for missing clauses');
  assert.equal(plain.description, I18n.t('bpmn.palette_flow_hint'));
});

test('palette lists the TentaFlow group first, labels its subgroups and trims the search', async () => {
  const root = document.createElement('aside'); document.body.append(root);
  const palette = new FlowPalette(root, { mode: 'bpmn', processOptions: tentaflowOptions }); await palette.init();
  assert.equal(root.querySelector('.fb-palette-category').dataset.cat, 'tentaflow');
  for (const heading of root.querySelectorAll('.fb-palette-subgroup')) {
    assert.equal(heading.getAttribute('role'), 'heading');
    assert.ok(Number(heading.getAttribute('aria-level')) > 0);
  }
  const shown = () => root.querySelectorAll('.fb-node-item').length;
  const all = shown();
  const search = root.querySelector('tf-searchbox');
  search.dispatchEvent(new window.CustomEvent('search', { detail: { value: ' ' } }));
  await new Promise((resolve) => setTimeout(resolve, 200));
  assert.equal(shown(), all, 'a lone space filters nothing');
  search.dispatchEvent(new window.CustomEvent('search', { detail: { value: '  contract  ' } }));
  await new Promise((resolve) => setTimeout(resolve, 200));
  assert.equal(shown(), 1);
  palette.destroy();
});

test('row title carries the full name and description for clamped rows', async () => {
  const root = document.createElement('aside'); document.body.append(root);
  const palette = new FlowPalette(root, { mode: 'bpmn', processOptions: tentaflowOptions }); await palette.init();
  const row = root.querySelector('.fb-node-item[data-node-type="bpmn_service_task"]');
  assert.match(row.getAttribute('title'), /Contract checker\n/);
  palette.destroy();
});

test('keyboard add keeps focus on the palette row after the palette re-renders', async () => {
  const root = document.createElement('aside'); document.body.append(root);
  let palette;
  palette = new FlowPalette(root, { mode: 'bpmn', processOptions: tentaflowOptions, onAdd: () => palette._render() });
  await palette.init();
  const index = root.querySelector('.fb-node-item[data-node-type="bpmn_service_task"]').dataset.index;
  root.querySelector(`.fb-node-item[data-index="${index}"] button`).focus();
  root.querySelector(`.fb-node-item[data-index="${index}"]`).dispatchEvent(new window.MouseEvent('click', { bubbles: true, detail: 0 }));
  assert.equal(document.activeElement, root.querySelector(`.fb-node-item[data-index="${index}"] button`));
  palette.destroy();
});

test('successive adds without a position cascade instead of stacking', () => {
  const graph = canvas();
  const template = processTemplates().find((row) => row.node_type === 'bpmn_user_task');
  const spots = [];
  for (let index = 0; index < 20; index += 1) {
    const node = graph.addNodeFromTemplate(template, 500, 400, { cascade: true });
    spots.push(`${node.x},${node.y}`);
  }
  assert.equal(new Set(spots).size, 20);
  graph.destroy();
  const dropping = canvas();
  const plain = dropping.addNodeFromTemplate(template, 900, 400);
  const centre = (node) => [node.x + node.width / 2, node.y + node.height / 2];
  const world = dropping._clientToWorld(900, 400);
  assert.ok(Math.abs(centre(plain)[0] - world.x) <= 12 && Math.abs(centre(plain)[1] - world.y) <= 12, 'a drop on free ground keeps the position the user chose');
  const dropped = dropping.addNodeFromTemplate(template, 900, 400);
  const overlap = plain.x < dropped.x + dropped.width && dropped.x < plain.x + plain.width
    && plain.y < dropped.y + dropped.height && dropped.y < plain.y + plain.height;
  assert.equal(overlap, false, 'a drop onto a node does not hide it');
  assert.ok(Math.hypot(centre(dropped)[0] - world.x, centre(dropped)[1] - world.y) < 500, 'it takes the free spot nearest the drop');
  dropping.destroy();
});

test('the dedupe suffix keeps the addon name, closes its parenthesis and never leads with the separator', () => {
  const tool = 't'.repeat(128);
  const rows = ['a', 'b'].map((first) => ({
    addonId: `${first}${'i'.repeat(127)}`, toolName: tool, description: '', addonName: 'Notes',
  }));
  const labels = processTemplates({ ...options, serviceTools: rows }).filter((row) => row.subgroup === 'tools').map((row) => row.label);
  assert.equal(new Set(labels).size, 2);
  for (const label of labels) {
    assert.ok(validNodeName(label));
    assert.ok(label.startsWith('Notes › '), label.slice(0, 20));
    assert.ok(label.endsWith(')'));
    assert.ok(label.includes(` ${tool} (`), 'the tool name is not cut while the id can yield');
  }
  const huge = processTemplates({ ...options, serviceTools: ['a', 'b'].map((first) => ({
    addonId: `${first}${'i'.repeat(127)}`, toolName: 't'.repeat(250), description: '', addonName: 'N'.repeat(300) })) })
    .filter((row) => row.subgroup === 'tools').map((row) => row.label);
  for (const label of huge) {
    assert.ok(validNodeName(label));
    assert.ok(!label.startsWith('›') && !label.startsWith(' '));
    assert.ok(label.endsWith(')'));
  }
});

test('rows with an empty cleaned addon name are still told apart by their instance id', () => {
  const labels = processTemplates({ ...options, serviceTools: [
    { addonId: 'x-1', toolName: 'search', description: '', addonName: '\u0001 ' },
    { addonId: 'x-2', toolName: 'search', description: '', addonName: '' },
  ] }).filter((row) => row.subgroup === 'tools').map((row) => row.label);
  assert.deepEqual(labels, ['search (x-1)', 'search (x-2)']);
});

test('Delete and undo still reach the canvas after focus rests on a button, but not in text fields or dialogs', () => {
  const calls = [];
  const state = { readOnly: false, canvas: { deleteSelected: () => calls.push('delete'), undo: () => calls.push('undo'), selectedIds: new Set() } };
  const press = (element, init) => {
    const handler = (ev) => builder._onKey.call({ _state: state }, ev);
    document.addEventListener('keydown', handler);
    element.dispatchEvent(new window.KeyboardEvent('keydown', { bubbles: true, composed: true, cancelable: true, ...init }));
    document.removeEventListener('keydown', handler);
  };
  const host = document.createElement('div'); document.body.append(host);
  host.innerHTML = '<tf-button id="save"></tf-button><tf-input id="field"></tf-input><tf-searchbox id="find"></tf-searchbox>'
    + '<tf-modal><tf-button id="inside"></tf-button></tf-modal><textarea id="plain"></textarea>';
  const at = (id) => host.querySelector(`#${id}`);
  press(at('save'), { key: 'Delete' });
  press(at('save'), { key: 'z', ctrlKey: true });
  assert.deepEqual(calls, ['delete', 'undo']);
  for (const id of ['field', 'find', 'inside', 'plain']) press(at(id), { key: 'Delete' });
  assert.deepEqual(calls, ['delete', 'undo'], 'text entry and dialogs keep their own keys');
  host.remove();
});

test('only the TentaFlow group rows are clamped to two lines and every row keeps its full title', async () => {
  const root = document.createElement('aside'); document.body.append(root);
  const palette = new FlowPalette(root, { mode: 'bpmn', processOptions: tentaflowOptions }); await palette.init();
  const rows = [...root.querySelectorAll('.fb-node-item')];
  const clamped = rows.filter((row) => row.classList.contains('fb-node-clamped'));
  assert.ok(clamped.length > 0 && clamped.length < rows.length);
  for (const row of clamped) assert.equal(row.dataset.nodeType, 'bpmn_service_task');
  assert.ok(rows.filter((row) => row.dataset.nodeType !== 'bpmn_service_task').every((row) => !row.classList.contains('fb-node-clamped')));
  assert.ok(rows.every((row) => row.getAttribute('title')));
  const css = readFileSync(new URL('../../../css/flows-builder.css', import.meta.url), 'utf8');
  assert.ok(!/^\.fb-node-desc\s*\{[^}]*line-clamp/m.test(css), 'the clamp is scoped to .fb-node-clamped');
  palette.destroy();
});

test('the process toolbar is clipped, never a scroll box, at every width', () => {
  const css = readFileSync(new URL('../../../css/flows-builder.css', import.meta.url), 'utf8');
  const rules = [...css.matchAll(/\.fb-process-shell \.fb-topbar \{([^}]*)\}/g)].map((match) => match[1]);
  assert.ok(rules.length >= 2, 'the base rule and the tablet rule');
  for (const body of rules.filter((rule) => /overflow:/.test(rule))) {
    const last = [...body.matchAll(/(?<![-\w])overflow:\s*([^;]+);/g)].pop()[1].trim();
    assert.equal(last, 'clip', 'a focus must not be able to scroll the bar');
  }
});

test('the omitted-trace notes agree in gender and number in German, one and many', async () => {
  try {
    await I18n.setLanguage('de');
    assert.equal(I18n.t('bpmn.simulation_events_omitted', { count: 1 }), '1 älteres Ereignis nicht angezeigt');
    assert.equal(I18n.t('bpmn.simulation_events_omitted', { count: 3 }), '3 ältere Ereignisse nicht angezeigt');
    assert.equal(I18n.t('bpmn.simulation_trace_steps_omitted', { count: 1 }), '1 älterer Ablaufschritt nicht angezeigt');
    assert.equal(I18n.t('bpmn.simulation_trace_steps_omitted', { count: 2 }), '2 ältere Ablaufschritte nicht angezeigt');
  } finally { await I18n.setLanguage('en'); }
});

const overlaps = (a, b, size) => {
  const sa = size(a); const sb = size(b);
  return a.x < b.x + sb.w && a.x + sa.w > b.x && a.y < b.y + sb.h && a.y + sa.h > b.y;
};
const noOverlaps = (nodes, size) => nodes.every((a, i) => nodes.slice(i + 1).every((b) => !overlaps(a, b, size)));

test('a click add on a crowded process canvas intersects no existing node', () => {
  const graph = canvas();
  const template = processTemplates().find((row) => row.node_type === 'bpmn_service_task');
  const size = (n) => ({ w: n.width, h: n.height });
  graph.nodes = [];
  for (let row = 0; row < 8; row += 1) for (let col = 0; col < 8; col += 1) {
    graph.nodes.push({ id: `d${row}_${col}`, type: 'bpmn_service_task', x: 40 + col * 280, y: 40 + row * 130, width: 240, height: 96, config: {} });
  }
  const crowded = graph.nodes.length;
  for (let index = 0; index < 12; index += 1) graph.addNodeFromTemplate(template, 700, 400, { cascade: true });
  assert.equal(graph.nodes.length, crowded + 12);
  assert.ok(noOverlaps(graph.nodes, size), 'no pair of nodes intersects');
  graph.destroy();
});

test('twelve successive keyboard adds on an empty canvas never overlap, for BPMN and classic nodes', () => {
  const graph = canvas();
  const template = processTemplates().find((row) => row.node_type === 'bpmn_user_task');
  for (let index = 0; index < 12; index += 1) graph.addNodeFromTemplate(template, 500, 400, { cascade: true });
  assert.ok(noOverlaps(graph.nodes.slice(-12), (n) => ({ w: n.width, h: n.height })));

  const root = document.createElement('div'); document.body.append(root);
  const classic = new FlowCanvas(root, { mode: 'flow' });
  const tpl = { node_type: 'activity_result', category: 'output', label: 'Result', default_config: '{}', input_ports: ['in'], output_ports: [] };
  classic.setTemplates([tpl]);
  classic.setData([{ id: 'Seed', type: 'activity_result', label: '', position: { x: 380, y: 300 }, config: {} }], []);
  for (let index = 0; index < 12; index += 1) classic.addNodeFromTemplate(tpl, 520, 355, { cascade: true });
  assert.equal(classic.nodes.length, 13);
  assert.ok(noOverlaps(classic.nodes, () => ({ w: 280, h: 110 })));
  const dropped = classic.addNodeFromTemplate(tpl, 520, 355);
  assert.deepEqual([dropped.x, dropped.y], [380, 300], 'a drop keeps the exact position');
  classic.destroy(); graph.destroy();
});

// `height` is a number for every node or a function of the node id.
function stubClassicNodeHeight(height) {
  const proto = window.HTMLElement.prototype;
  const original = Object.getOwnPropertyDescriptor(proto, 'offsetHeight');
  Object.defineProperty(proto, 'offsetHeight', {
    configurable: true,
    get() {
      if (!this.classList?.contains('fb-node')) return 0;
      return typeof height === 'function' ? height(this.dataset.nodeId) : height;
    },
  });
  return () => {
    if (original) Object.defineProperty(proto, 'offsetHeight', original);
    else delete proto.offsetHeight;
  };
}

function classicCanvas(width, height) {
  const root = document.createElement('div'); document.body.append(root);
  root.getBoundingClientRect = () => ({ left: 0, top: 0, width, height, right: width, bottom: height });
  const graph = new FlowCanvas(root, { mode: 'flow' });
  const tpl = { node_type: 'activity_result', category: 'output', label: 'Result', default_config: '{}', input_ports: ['in'], output_ports: [] };
  graph.setTemplates([tpl]);
  return { graph, tpl, root };
}

test('classic click adds avoid nodes rendered taller than the estimate and a drop is centred on the measured height', () => {
  const restore = stubClassicNodeHeight(190);
  try {
    const { graph, tpl } = classicCanvas(1600, 950);
    const seed = [];
    for (let i = 0; i < 12; i += 1) seed.push({ id: `Seed${i}`, type: 'activity_result', label: '', position: { x: 40 + (i % 5) * 300, y: 40 + Math.floor(i / 5) * 220 }, config: {} });
    graph.setData(seed, []);
    graph.view = { x: 0, y: 0, zoom: 0.75 };
    for (let i = 0; i < 5; i += 1) graph.addNodeFromTemplate(tpl, 800, 400, { cascade: true });
    assert.ok(noOverlaps(graph.nodes, () => ({ w: 280, h: 190 })), 'no pair intersects at the rendered height');
    const dropped = graph.addNodeFromTemplate(tpl, 900, 700);
    const centre = graph._clientToWorld(900, 700);
    assert.equal(dropped.y, Math.round((centre.y - 95) / 10) * 10, 'the drop is centred using the measured height');
    graph.destroy();
  } finally { restore(); }
});

test('a cascade add is exactly one undo entry', () => {
  const restore = stubClassicNodeHeight(190);
  try {
    const { graph, tpl } = classicCanvas(1600, 950);
    graph.setData([{ id: 'Seed', type: 'activity_result', label: '', position: { x: 40, y: 40 }, config: {} }], []);
    const before = graph.history.length;
    graph.addNodeFromTemplate(tpl, 100, 100, { cascade: true });
    assert.equal(graph.history.length, before + 1);
    graph.undo();
    assert.equal(graph.nodes.length, 1);
    graph.destroy();
  } finally { restore(); }
});

test('node rectangles take the rendered height as world units whatever the zoom', () => {
  const restore = stubClassicNodeHeight(200);
  try {
    const { graph } = classicCanvas(1600, 950);
    graph.setData([{ id: 'Seed', type: 'activity_result', label: '', position: { x: 40, y: 40 }, config: {} }], []);
    graph.view = { x: 0, y: 0, zoom: 0.5 };
    assert.equal(graph._nodeRects()[0].h, 200);
    graph.destroy();
  } finally { restore(); }
});

test('a click add into a full viewport pans so the whole new node is visible', () => {
  const graph = canvas();
  graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600 });
  const template = processTemplates().find((row) => row.node_type === 'bpmn_service_task');
  graph.nodes = [];
  for (let row = 0; row < 6; row += 1) for (let col = 0; col < 3; col += 1) {
    graph.nodes.push({ id: `d${row}_${col}`, type: 'bpmn_service_task', x: 10 + col * 270, y: 10 + row * 100, width: 240, height: 96, config: {} });
  }
  graph.view = { x: 0, y: 0, zoom: 1 };
  const node = graph.addNodeFromTemplate(template, 400, 300, { cascade: true });
  const zoom = graph.view.zoom;
  const left = node.x * zoom + graph.view.x;
  const top = node.y * zoom + graph.view.y;
  assert.ok(left >= 0 && left + node.width * zoom <= 800, `horizontally visible at ${left}`);
  assert.ok(top >= 0 && top + node.height * zoom <= 600, `vertically visible at ${top}`);
  assert.ok(noOverlaps(graph.nodes, (n) => ({ w: n.width, h: n.height })));
  graph.destroy();
});

test('free-spot search reads the DOM once per node, not once per candidate', () => {
  const restore = stubClassicNodeHeight(190);
  try {
    const { graph, tpl } = classicCanvas(1600, 950);
    const seed = [];
    for (let i = 0; i < 40; i += 1) seed.push({ id: `Seed${i}`, type: 'activity_result', label: '', position: { x: 40 + (i % 5) * 300, y: 40 + Math.floor(i / 5) * 220 }, config: {} });
    graph.setData(seed, []);
    let reads = 0;
    const query = graph.nodesLayer.querySelector.bind(graph.nodesLayer);
    graph.nodesLayer.querySelector = (selector) => { reads += 1; return query(selector); };
    graph.addNodeFromTemplate(tpl, 800, 400, { cascade: true });
    assert.ok(reads < 40 * 12, `${reads} DOM reads`);
    graph.destroy();
  } finally { restore(); }
});

test('a cascade add re-measures the rendered node and moves off a spot that only fit the estimate', () => {
  const restore = stubClassicNodeHeight((id) => (id.startsWith('Seed') ? 110 : 190));
  try {
    const { graph, tpl } = classicCanvas(340, 600);
    // Only y 160 on the candidate grid fits a 110 px node between the seeds; a 190 px one fits nowhere in view.
    graph.setData([
      { id: 'SeedA', type: 'activity_result', label: '', position: { x: 20, y: 20 }, config: {} },
      { id: 'SeedB', type: 'activity_result', label: '', position: { x: 20, y: 290 }, config: {} },
    ], []);
    graph.view = { x: 0, y: 0, zoom: 1 };
    const before = graph.history.length;
    const spots = [];
    const origins = [];
    const search = graph._moveToFreeSpot.bind(graph);
    graph._moveToFreeSpot = (...args) => { const size = search(...args); spots.push([args[0].x, args[0].y, size.h]); origins.push({ ...args[3] }); return size; };
    const node = graph.addNodeFromTemplate(tpl, 160, 225, { cascade: true });
    assert.deepEqual(spots[0], [20, 160, 110], 'the first pass fits the estimate');
    assert.equal(spots.length, 2, 'the rendered 190 px height triggered one more search');
    assert.deepEqual(origins[1], origins[0], 'the second search starts from the original target, not the first spot');
    assert.deepEqual(origins[0], { x: 20, y: 170 }, 'the target is off the candidate grid, so it differs from the first spot');
    assert.notDeepEqual([node.x, node.y], [20, 160], 'the node left the spot that fit the estimate');
    const height = (n) => (n.id.startsWith('Seed') ? 110 : 190);
    assert.ok(noOverlaps(graph.nodes, (n) => ({ w: 280, h: height(n) })), 'nothing overlaps at the rendered heights');
    assert.equal(graph.history.length, before + 1);
    graph.destroy();
  } finally { restore(); }
});

test('a service task with an external label keeps the label inside the canvas after placement', () => {
  const proto = window.HTMLElement.prototype;
  const saved = ['offsetWidth', 'offsetHeight', 'scrollHeight'].map((name) => [name, Object.getOwnPropertyDescriptor(proto, name)]);
  Object.defineProperty(proto, 'offsetWidth', { configurable: true, get() { return this.classList?.contains('fb-process-label') ? 200 : 0; } });
  Object.defineProperty(proto, 'offsetHeight', { configurable: true, get() { return this.classList?.contains('fb-process-label') ? 40 : 0; } });
  Object.defineProperty(proto, 'scrollHeight', { configurable: true, get() { return this.parentElement?.classList?.contains('fb-process-symbol') ? 90 : 0; } });
  try {
    const graph = canvas();
    graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600 });
    graph.nodes = [];
    graph.view = { x: 0, y: 0, zoom: 1 };
    // The content clamp counts labels too and would hide a placement that ignores them.
    graph._clampView = () => {};
    const template = processTemplates().find((row) => row.node_type === 'bpmn_service_task');
    const node = graph.addNodeFromTemplate({ ...template, label: 'A very long tool name' , preset: {} }, 400, 20, { cascade: true });
    const label = graph.nodesLayer.querySelector(`[data-node-id="${node.id}"] .fb-process-label`);
    assert.equal(label.hidden, false, 'the label is outside the box');
    const labelTop = (node.y + parseFloat(label.style.top)) * graph.view.zoom + graph.view.y;
    assert.ok(labelTop >= 20, `the label top is ${labelTop} px from the canvas top`);
    graph.destroy();
  } finally {
    for (const [name, descriptor] of saved) {
      if (descriptor) Object.defineProperty(proto, name, descriptor); else delete proto[name];
    }
  }
});

test('a spot found while zoomed out is visible without a further pan, and the pan maths hold at zoom 0.6', () => {
  const graph = canvas();
  graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600 });
  graph.nodes = [];
  // The content clamp (a separate 40 px rule) is not what is measured here.
  graph._clampView = () => {};
  graph.view = { x: -300, y: 150, zoom: 0.6 };
  const template = processTemplates().find((row) => row.node_type === 'bpmn_service_task');
  const node = graph.addNodeFromTemplate(template, 0, 0, { cascade: true });
  assert.deepEqual([graph.view.x, graph.view.y], [-300, 150], 'a visible spot never pans');
  const left = node.x * 0.6 - 300;
  const top = node.y * 0.6 + 150;
  assert.ok(left >= 20 && top >= 20 && left + node.width * 0.6 <= 780 && top + node.height * 0.6 <= 580);

  const far = { x: 2000, y: -400 };
  graph._applyView = () => {};
  graph._panToReveal(far, { w: 240, h: 96, ox: 0, oy: 0 });
  assert.equal(graph.view.x, -300 + 800 - 20 - (2240 * 0.6 - 300), 'right edge lands on the right margin');
  assert.equal(graph.view.y, 150 + 20 - (-400 * 0.6 + 150), 'top edge lands on the top margin');
  graph.destroy();
});

test('clicking the canvas or a node takes focus from a text field outside it', () => {
  const graph = canvas();
  const field = document.createElement('input'); document.body.append(field);
  const pointerdown = (target, id) => target.dispatchEvent(new window.PointerEvent('pointerdown', {
    bubbles: true, pointerId: id, button: 0, clientX: 10, clientY: 10 }));
  field.focus();
  assert.equal(document.activeElement, field);
  pointerdown(graph.root, 61);
  assert.notEqual(document.activeElement, field, 'an empty-canvas click blurs the field');
  assert.equal(document.activeElement, graph.root);
  window.dispatchEvent(new window.PointerEvent('pointerup', { pointerId: 61, button: 0 }));

  const node = userNode(graph);
  field.focus();
  pointerdown(graph.nodesLayer.querySelector(`[data-node-id="${node.id}"]`), 62);
  assert.notEqual(document.activeElement, field, 'a node click blurs the field');
  window.dispatchEvent(new window.PointerEvent('pointerup', { pointerId: 62, button: 0 }));
  field.remove(); graph.destroy();
});

test('palette labels drop bidi controls and the blank list matches the server', () => {
  for (const code of [0x61c, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069]) {
    const bidi = String.fromCodePoint(code);
    assert.equal(boundedNodeName(`Notes${bidi}search`), 'Notessearch', `U+${code.toString(16)}`);
    assert.ok(validNodeName(boundedNodeName(`a${bidi}b`)));
  }
  // Same list as the_blank_label_list_is_shared_with_the_designer in tool_host.rs.
  const blank = [0x34f, 0x115f, 0x1160, 0x3164, 0xffa0, 0x61c, 0x200e, 0x200f, 0x202a, 0x202e,
    0x2066, 0x2069, 0x200b, 0xfeff, 0xad, 0x20, 0x09];
  for (const code of blank) assert.ok(isBlankLabel(String.fromCodePoint(code)), `U+${code.toString(16)} is blank`);
  for (const code of [0x41, 0x3042, 0x1161]) assert.ok(!isBlankLabel(String.fromCodePoint(code)), `U+${code.toString(16)} is not blank`);
});

test('duplicate tool rows lead with the instance id so a long shared name cannot hide it', async () => {
  const root = document.createElement('aside'); document.body.append(root);
  const shared = 'Shared instance name '.repeat(9);
  const palette = new FlowPalette(root, { mode: 'bpmn', processOptions: { ...options, serviceTools: ['inst-aaa', 'inst-bbb'].map((addonId) => ({
    addonId, toolName: 'search', description: '', addonName: shared })) } });
  await palette.init();
  const subs = [...root.querySelectorAll('.fb-node-item[data-node-type="bpmn_service_task"]')]
    .filter((row) => row.querySelector('.fb-node-name').textContent.trim() === 'search')
    .map((row) => row.querySelector('.fb-node-desc').textContent.trim());
  assert.ok(shared.length > 150);
  assert.ok(subs[0].startsWith('(inst-aaa) ') && subs[1].startsWith('(inst-bbb) '), subs.join('|'));
  palette.destroy();
});

test('dedupe keeps the end of a long instance id so sibling instances stay apart', () => {
  const labels = processTemplates({ ...options, serviceTools: ['notes-instance-1', 'notes-instance-2'].map((addonId) => ({
    addonId, toolName: 't'.repeat(250), description: '', addonName: 'Notes' })) })
    .filter((row) => row.subgroup === 'tools').map((row) => row.label);
  assert.equal(new Set(labels).size, 2);
  assert.ok(labels[0].endsWith('1)') && labels[1].endsWith('2)'), labels.map((l) => l.slice(-12)).join('|'));
  for (const label of labels) assert.ok(validNodeName(label));
  const sub = processTemplates({ ...options, serviceTools: [{ addonId: 'é'.repeat(200), toolName: 't'.repeat(250), description: '', addonName: 'N' },
    { addonId: `${'é'.repeat(199)}x`, toolName: 't'.repeat(250), description: '', addonName: 'N' }] })
    .filter((row) => row.subgroup === 'tools').map((row) => row.label);
  for (const label of sub) assert.ok(validNodeName(label) && !label.includes('�'));
});

test('invisible-only addon names fall back to the instance id like blank ones', () => {
  const labels = processTemplates({ ...options, serviceTools: [
    { addonId: 'x-1', toolName: 'search', description: '', addonName: '​⁠' },
    { addonId: 'x-2', toolName: 'search', description: '', addonName: '﻿­ ' },
  ] }).filter((row) => row.subgroup === 'tools');
  assert.deepEqual(labels.map((row) => row.label), ['search (x-1)', 'search (x-2)']);
  assert.deepEqual(labels.map((row) => row.rowSub), ['x-1', 'x-2']);
});

test('a tool row names the tool on its first line and the addon on a separate line', async () => {
  const root = document.createElement('aside'); document.body.append(root);
  const long = 'Very long instance name '.repeat(8);
  const palette = new FlowPalette(root, { mode: 'bpmn', processOptions: { ...options, serviceTools: [
    { addonId: 'a-1', toolName: 'search', description: 'Searches', addonName: long },
    { addonId: 'b-1', toolName: 'lookup', description: '', addonName: 'Wiki' },
    { addonId: 'b-2', toolName: 'lookup', description: '', addonName: 'Wiki' },
  ] } });
  await palette.init();
  const rows = [...root.querySelectorAll('.fb-node-item[data-node-type="bpmn_service_task"]')];
  const text = (row, cls) => row.querySelector(cls).textContent.trim();
  const search = rows.find((row) => text(row, '.fb-node-name') === 'search');
  assert.ok(search, 'the tool name is the name line');
  assert.ok(text(search, '.fb-node-desc').startsWith('Very long instance name'));
  const lookups = rows.filter((row) => text(row, '.fb-node-name') === 'lookup').map((row) => text(row, '.fb-node-desc'));
  assert.deepEqual(lookups, ['(b-1) Wiki', '(b-2) Wiki']);
  assert.match(search.getAttribute('title'), /›.*search/s, 'the title keeps the node name');
  palette.destroy();
});

test('edges saved with from/to or source/target open drawable and save as from_node/to_node', () => {
  const { graph } = classicCanvas(1600, 950);
  graph.setData([
    { id: 'a', type: 'activity_result', label: '', position: { x: 0, y: 0 }, config: {} },
    { id: 'b', type: 'activity_result', label: '', position: { x: 400, y: 0 }, config: {} },
  ], [{ from: 'a', to: 'b' }, { source: 'a', target: 'b', from_port: 'x' }]);
  assert.deepEqual(graph.edges.map((e) => [e.from_node, e.to_node]), [['a', 'b'], ['a', 'b']]);
  for (const edge of graph.getData().edges) {
    assert.equal(edge.from_node, 'a');
    assert.equal(edge.to_node, 'b');
    assert.ok(!('from' in edge) && !('to' in edge) && !('source' in edge) && !('target' in edge));
  }
  graph.destroy();
});

test('keys typed in an unlisted component with an inner input are left to that input', () => {
  const calls = [];
  const state = { readOnly: false, canvas: { deleteSelected: () => calls.push('delete'), undo: () => calls.push('undo'), selectedIds: new Set() } };
  const host = document.createElement('tf-tag-input'); document.body.append(host);
  const inner = host.attachShadow({ mode: 'open' }).appendChild(document.createElement('input'));
  // A browser retargets the document-level event to the shadow host, which no
  // tag list names; only the composed path still reveals the inner input.
  builder._onKey.call({ _state: state }, {
    key: 'Delete', target: host, preventDefault: () => calls.push('prevented'),
    composedPath: () => [inner, inner.getRootNode(), host, document.body, document, window],
  });
  host.remove();
  assert.deepEqual(calls, [], 'Delete typed into the inner input does not delete canvas nodes');
});

// ---------------------------------------------------------------------------
// Round 2 of the Service task inspector: what the live critic and the code
// review found, each pinned where it was seen.
// ---------------------------------------------------------------------------

const graphWithEscalation = (...path) => {
  const nodes = [taskNode('T', { resultExpression: '{}' }), boundaryNode('H', 'bpmn_boundary_escalation', 'T', { escalationRef: null }, 'Handle review'),
    ...path.map(([id, type]) => ({ id, type, label: id, config: {} }))];
  const edges = [edge('T', path.at(-1)[0]), edge('H', path[0][0]), ...path.slice(1).map(([id], index) => edge(path[index][0], id))];
  return routingGraph(nodes, edges);
};

test('an escalation handler path is judged like the server judges it before anything waits for a person', () => {
  const reason = (...path) => processEscalationPathProblem(graphWithEscalation(...path), 'H')?.reason ?? null;
  assert.equal(reason(['E', 'bpmn_end']), 'terminal_before_wait');
  assert.equal(reason(['E', 'bpmn_terminate_end']), 'terminal_before_wait');
  assert.equal(reason(['S', 'bpmn_sub_process'], ['E', 'bpmn_end']), 'scope_entry');
  assert.equal(reason(['C', 'bpmn_call_activity'], ['E', 'bpmn_end']), 'call_entry');
  // A wait ends the search; a script, a send or a throw is passed through.
  assert.equal(reason(['U', 'bpmn_user_task'], ['E', 'bpmn_end']), null);
  assert.equal(reason(['R', 'bpmn_script_task'], ['U', 'bpmn_user_task'], ['E', 'bpmn_end']), null);
  assert.equal(reason(['R', 'bpmn_script_task'], ['E', 'bpmn_end']), 'terminal_before_wait');
  assert.equal(reason(['G', 'bpmn_parallel_gateway'], ['E', 'bpmn_end']), null, 'gateways with joins are left to the server');
  // Every branch of an exclusive gateway counts.
  const branches = routingGraph([taskNode('T', { resultExpression: '{}' }), boundaryNode('H', 'bpmn_boundary_escalation', 'T', { escalationRef: null }),
    { id: 'G', type: 'bpmn_exclusive_gateway', config: {} }, { id: 'U', type: 'bpmn_user_task', config: {} }, { id: 'E', type: 'bpmn_end', config: {} }],
  [edge('H', 'G'), edge('G', 'U'), edge('G', 'E')]);
  assert.equal(processEscalationPathProblem(branches, 'H').reason, 'terminal_before_wait');
  // The table names the handler, the save is refused by its name, and a path with no outgoing flow is only "no flow yet".
  const routing = processResultRouting(graphWithEscalation(['E', 'bpmn_end']), 'T');
  assert.deepEqual(routing.warnings.filter((warning) => warning.key.startsWith('escalation_path')).map((warning) => [warning.key, warning.names]),
    [['escalation_path_terminal_before_wait', ['Handle review']]]);
  assert.equal(processEscalationPathProblem(routingGraph([taskNode('T'), boundaryNode('H', 'bpmn_boundary_escalation', 'T', { escalationRef: null })], []), 'H'), null);
});

test('an escalation handler that runs into an End blocks the save with its name', async () => {
  const model = emptyProcessModel();
  model.escalations = [{ escalationId: 'Esc_1', name: 'Review', escalationCode: 'REVIEW' }];
  model.nodes.splice(1, 0,
    { id: 'Work', name: 'Check claim', kind: { ServiceTask: { flowId: 'flow-one', inputMapping: {}, outputMapping: {}, verification: 'Human', timeoutSeconds: 60, resultExpression: '{}' } } },
    { id: 'Esc', name: 'Needs review', kind: { BoundaryEscalation: { attachedToId: 'Work', escalationRef: 'Esc_1', cancelActivity: true, outputMapping: {} } } });
  model.sequenceFlows = [{ id: 'F1', sourceId: 'Start', targetId: 'Work', condition: null }, { id: 'F2', sourceId: 'Work', targetId: 'End', condition: null },
    { id: 'F3', sourceId: 'Esc', targetId: 'End', condition: null }];
  const state = await mount(definition('escalation-end', { model }));
  assert.equal(await builder._save({ silent: true }), false);
  const banner = state.root.querySelector('[data-role="error"]');
  assert.equal(banner.hidden, false, 'the problem is on the screen, not only in a tooltip');
  assert.match(banner.getAttribute('message'), /Needs review/);
  assert.match(state.root.querySelector('[data-role="autosave"]').getAttribute('title'), /Needs review/);
  assert.equal(calls.some((call) => call.kind === 'processDefinitionSaveRequest'), false);
});

test('server and codec messages name elements instead of citing their ids', () => {
  const model = emptyProcessModel();
  model.nodes.splice(1, 0, { id: 'n_k1_ab12', name: 'Handle claim', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } },
    { id: 'n_k1_cd34', name: '', kind: { BoundaryEscalation: { attachedToId: 'n_k1_ab12', escalationRef: null, cancelActivity: true, outputMapping: {} } } });
  model.sequenceFlows = [{ id: 'e_k1_ef56', sourceId: 'Start', targetId: 'n_k1_ab12', condition: null }];
  assert.match(processExplainError('escalation boundary n_k1_cd34 has unsupported immediate path terminal_before_wait at e_k1_ef56', model), /^Escalation handler “Boundary escalation” runs into an End/,
    'the refusal the inspector already explains is explained the same way, with the handler named by its kind when it has no name');
  const flow = processExplainError('flow e_k1_ef56 leaves n_k1_ab12', model);
  assert.equal(flow, 'flow the flow “Start” → “Handle claim” leaves “Handle claim”');
  assert.equal(processExplainError('call activity requires one exact local or published target', model), 'call activity requires one exact local or published target');
  assert.doesNotMatch(processExplainError('node n_k1_ab12 and n_unknown_xx', model), /n_k1_ab12/);
  assert.match(processExplainError('node n_unknown_xx', model), /n_unknown_xx/, 'an id the model does not know is left as it is');
});

test('an imported element called like an ordinary word never rewrites that word in a server message', () => {
  const model = emptyProcessModel();
  model.nodes.splice(1, 0, { id: 'Review', name: 'Review contract', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } },
    { id: 'Task_1', name: 'Approve', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } });
  model.sequenceFlows = [{ id: 'Flow_1', sourceId: 'Start', targetId: 'Review', condition: null }];
  const text = 'task Review has invalid instructions; Start must lead to End, and Task_1 too';
  assert.equal(processExplainError(text, model), 'task “Review contract” has invalid instructions; Start must lead to End, and “Approve” too',
    'an id after the noun that introduces it, or one shaped like an id, is named; the words Start and End stay words');
  assert.equal(processExplainError('invalid node ID: Start', model), 'invalid node ID: “Start”');
  assert.equal(processExplainError('the Review of the End is quoted "Review"', model), 'the Review of the End is quoted "“Review contract”"');
});

test('a server refusal is shown by name in the banner and the status, and a silent autosave does not toast', async () => {
  const current = definition('server-refusal');
  const state = await mount(current, { processDefinitionSaveRequest: () => { throw new Error('escalation boundary Start has unsupported immediate path no_durable_wait at End'); } });
  // The toast container is replaced with the page by every mount, so the toasts made are counted as they are created.
  const created = []; const createElement = document.createElement.bind(document);
  document.createElement = (...args) => { const node = createElement(...args); created.push(node); return node; };
  const toasts = () => created.filter((node) => node.className === 'toast toast-error').length;
  const before = toasts();
  assert.equal(await builder._save({ silent: true }), false);
  assert.equal(toasts(), before, 'ten seconds later the same refusal would toast again; the banner carries it instead');
  const banner = state.root.querySelector('[data-role="error"]');
  assert.equal(banner.hidden, false);
  assert.match(banner.getAttribute('message'), /^Escalation handler “Start” has a path/, 'the element is named in visible text');
  assert.match(state.root.querySelector('[data-role="autosave"]').getAttribute('title'), /Escalation handler/);
  // An asked-for save does toast, and a save that works clears the banner.
  assert.equal(await builder._save(), false);
  assert.ok(toasts() > before, 'a save the author asked for says so');
  delete document.createElement;
  fixtures({ processDefinitionSaveRequest: (payload) => ({ definition: { ...current, model: payload.model, draftRevision: 5 } }) });
  assert.equal(await builder._save({ silent: true }), true);
  assert.equal(banner.hidden, true);
});

test('a Call activity without an exact target blocks the save naming it, and the picker explains an empty list', async () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_call_activity'), 200, 200);
  const node = graph.nodes.at(-1); graph.updateNodeLabel(node.id, 'Ask the bank');
  assert.deepEqual(graph.validate(), [I18n.t('bpmn.call_target_required', { node: 'Ask the bank' })]);
  graph.updateNodeConfig(node.id, { calledDefinitionId: 'd1', calledVersion: 2, calledElement: { namespaceUri: 'urn:x', processId: '' } });
  assert.equal(graph.validate().length, 1, 'a version without the process is not exact either');
  graph.updateNodeConfig(node.id, { calledElement: { namespaceUri: 'urn:x', processId: 'P' } });
  assert.deepEqual(graph.validate(), []);
  graph.updateNodeConfig(node.id, { localBody: { namespaceUri: 'urn:x', processId: '' } });
  assert.equal(graph.validate().length, 1, 'a local call needs its body');
  graph.destroy();

  const empty = canvas(); empty.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_call_activity'), 200, 200);
  const call = empty.nodes.at(-1);
  fixtures({ processDefinitionListRequest: { definitions: [{ definitionId: 'draft', name: 'Draft only', publishedVersion: null }], hasMore: false } });
  const config = inspector(empty); config.show(call, empty.templates.get(call.type));
  await flush(3);
  assert.equal(config.root.querySelector('[data-call-capability]').textContent, I18n.t('bpmn.call_no_published'));
  fixtures({ processDefinitionListRequest: { definitions: [{ definitionId: 'd1', name: 'Callee', publishedVersion: 1 }], hasMore: false },
    processVersionListRequest: { versions: [], hasMore: false } });
  config.show(call, empty.templates.get(call.type)); await flush(3);
  change(config.root.querySelector('[data-process="calledDefinitionChoice"]'), 'd1'); await flush(3);
  assert.equal(config.root.querySelector('[data-call-capability]').textContent, I18n.t('bpmn.call_no_versions'));
  empty.destroy(); config.destroy();
});

test('an unattached boundary event is flagged on the canvas, and the save names it', () => {
  const graph = canvas();
  const timer = processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer');
  graph.addNodeFromTemplate(timer, 400, 400);
  const loose = graph.nodes.at(-1);
  assert.deepEqual(processNodeWarnings(loose), ['boundary_unattached']);
  assert.ok(graph.nodesLayer.querySelector(`[data-node-id="${loose.id}"] .fb-process-warn[data-warning="boundary_unattached"]`));
  assert.match(graph.validate().join('\n'), new RegExp(`“${loose.label || graph._nodeName(loose)}” is not attached`));
  for (const type of ['bpmn_boundary_error', 'bpmn_boundary_escalation']) {
    const bare = { id: `bare_${type}`, type, label: 'Bare handler', x: 100, y: 100, width: 56, height: 56, config: { attachedToId: '', ...(type === 'bpmn_boundary_error' ? { errorRef: null } : { escalationRef: null }) } };
    graph.nodes.push(bare);
    assert.ok(graph.validate().some((message) => /“Bare handler” is not attached/.test(message)), `${type} is refused by name`);
    graph.nodes.pop();
  }
  graph.destroy();
});

test('a new boundary event does not land on a handler of the task next to it', () => {
  const graph = canvas(); const template = (type) => processTemplates().find((row) => row.node_type === type);
  graph.addNodeFromTemplate(template('bpmn_service_task'), 300, 300);
  graph.addNodeFromTemplate(template('bpmn_service_task'), 300, 700);
  const [above, below] = graph.nodes.filter((node) => node.type === 'bpmn_service_task');
  // `below` sits 32 px under `above`: clear of the task itself, but a handler on its top edge reaches into
  // the handlers on the bottom edge of `above`.
  below.x = above.x; below.y = above.y + above.height + 32;
  for (const task of [above, below]) graph.updateNodeConfig(task.id, { resultExpression: '{}' });
  const neighbour = graph.addBoundaryNode(above.id, 'bpmn_boundary_error', null);
  for (let count = 0; count < 4; count += 1) graph.addBoundaryNode(below.id, 'bpmn_boundary_error', null);
  // The bottom edge of `below` is full, so the fifth goes to its top edge: the first place there is under
  // the neighbour's handler, the second is clear.
  const fifth = graph.addBoundaryNode(below.id, 'bpmn_boundary_error', null);
  const crosses = (a, b) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
  assert.equal(fifth.y + fifth.height / 2, below.y, 'on the top edge');
  assert.equal(crosses(fifth, neighbour), false, 'it does not sit on the neighbour\'s handler');
  graph.destroy();
});

test('boundary slots follow the sizes the stylesheet gives the task decorations', () => {
  const graph = canvas(); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const task = graph.nodes.at(-1);
  const size = { width: 56, height: 56 };
  const before = graph._boundarySlots(task, size).filter(([, y]) => y === task.y).length;
  // A wider reserved corner (bigger badges) leaves fewer places along the top edge.
  const element = graph.nodesLayer.querySelector(`[data-node-id="${task.id}"]`);
  element.style.setProperty('--fb-deco-size', '40px');
  const original = globalThis.getComputedStyle;
  globalThis.getComputedStyle = (target) => (target === element ? { getPropertyValue: (name) => (name === '--fb-deco-size' ? '40px' : '') } : original(target));
  try {
    const after = graph._boundarySlots(task, size).filter(([, y]) => y === task.y).length;
    assert.ok(after < before, `${after} places with 40px badges against ${before} with the default ones`);
  } finally { globalThis.getComputedStyle = original; }
  graph.destroy();
});

test('the process toolbar keeps the main actions on the bar, the rest behind a menu, and gives every control its own icon', async () => {
  const state = await mount(definition('toolbar-menu'));
  const bar = state.root.querySelector('.fb-topbar');
  const buttons = [...bar.querySelectorAll(':scope > tf-button, :scope > .fb-run-wrap > tf-button')].map((button) => button.dataset.role);
  assert.deepEqual(buttons, ['back', 'publish', 'run', 'save', 'more']);
  const menu = bar.querySelector('tf-menu[data-role="more-menu"]');
  const inMenu = [...menu.querySelectorAll('tf-menu-item')].map((item) => item.dataset.role);
  assert.deepEqual(inMenu, ['zoom-fit-more', 'variables', 'import', 'export', 'calendar', 'refresh-calendar', 'declarations', 'modeling',
    'simulate', 'send-start', 'schedule', 'instances', 'messages', 'history']);
  assert.equal(menu.querySelector('[data-role="zoom-fit-more"]').hidden, true, 'the menu offers zoom to fit only once the bar has dropped its own button');
  const icons = [...bar.querySelectorAll('[icon]')].map((control) => control.getAttribute('icon'));
  assert.equal(new Set(icons).size, icons.length, `every control has its own icon (${icons.join(', ')})`);
  const more = bar.querySelector('[data-role="more"]');
  assert.equal(more.dataset.expanded, 'false');
  click(more);
  assert.equal(menu.hasAttribute('open'), true);
  assert.equal(more.dataset.expanded, 'true');
  click(more);
  assert.equal(menu.hasAttribute('open'), false, 'the trigger closes what it opened');
  assert.equal(more.dataset.expanded, 'false');
  click(bar.querySelector('[data-role="calendar"]'));
  assert.ok([...document.querySelectorAll('tf-window')].length > 0, 'a menu item runs its action');
});

test('Escape deselects an element, and the palette drawer takes it only when nothing sits above the drawer', async () => {
  const state = await mount(definition('escape-keys'));
  const escape = (target = document.body) => target.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true, composed: true, cancelable: true }));
  const task = userNode(state.canvas);
  state.canvas.selectNode(task.id);
  assert.deepEqual([...state.canvas.selectedIds], [task.id]);
  const field = document.createElement('tf-input'); document.body.append(field);
  escape(field);
  assert.deepEqual([...state.canvas.selectedIds], [task.id], 'a text field keeps its Escape');
  field.remove();
  escape();
  assert.deepEqual([...state.canvas.selectedIds], [], 'Escape clears the selection');
  assert.equal(state.config.node, null, 'and the inspector goes with it');

  const palette = state.root.querySelector('[data-role="palette"]');
  const opener = state.root.querySelector('[data-role="palette-toggle"]');
  const menu = state.root.querySelector('[data-role="more-menu"]');
  Object.defineProperty(opener, 'offsetParent', { value: document.body, configurable: true });
  click(opener);
  assert.equal(palette.classList.contains('open'), true);
  const dialog = document.createElement('div'); dialog.setAttribute('role', 'dialog'); document.body.append(dialog);
  escape(dialog);
  assert.equal(palette.classList.contains('open'), true, 'a dialog keeps its Escape');
  dialog.remove();
  menu.open();
  escape();
  assert.equal(menu.hasAttribute('open'), false, 'the open menu received the key');
  assert.equal(palette.classList.contains('open'), true, 'so the drawer stayed');
  escape(palette.querySelector('tf-searchbox'));
  assert.equal(palette.classList.contains('open'), false, 'Escape typed in the drawer\'s own search closes the drawer');
  click(opener);
  assert.equal(palette.classList.contains('open'), true);
  escape();
  assert.equal(palette.classList.contains('open'), false, 'from anywhere else, with nothing above it, the drawer takes the key');
});

test('the settings row follows the window until the author uses its toggle, a phone opens the inspector on selection and the root crumb says process', async () => {
  const resize = (width, height) => {
    Object.defineProperty(window, 'innerWidth', { value: width, configurable: true });
    Object.defineProperty(window, 'innerHeight', { value: height, configurable: true });
    window.dispatchEvent(new window.Event('resize'));
  };
  resize(1400, 1000);
  const state = await mount(definition('responsive-settings'));
  const notice = state.root.querySelector('.fb-process-notice');
  assert.equal(notice.classList.contains('fb-collapsed'), false);
  resize(1400, 700);
  assert.equal(notice.classList.contains('fb-collapsed'), true, 'a window made shorter folds the row');
  resize(390, 1000);
  assert.equal(notice.classList.contains('fb-collapsed'), true, 'a phone never opens it by height alone');
  resize(1400, 1000);
  assert.equal(notice.classList.contains('fb-collapsed'), false);
  click(state.root.querySelector('[data-role="notice-toggle"]'));
  assert.equal(notice.classList.contains('fb-collapsed'), true, 'the toggle folds it although the window is roomy');
  resize(1400, 1100);
  assert.equal(notice.classList.contains('fb-collapsed'), true, 'a resize does not undo the choice');
  click(state.root.querySelector('[data-role="notice-toggle"]'));
  assert.equal(notice.classList.contains('fb-collapsed'), false);
  resize(1400, 650);
  assert.equal(notice.classList.contains('fb-collapsed'), false, 'what the author chose is kept on a short window');
  assert.equal(state.root.querySelector('.fb-breadcrumb .fb-crumb').textContent, I18n.t('bpmn.crumb_root'));
  assert.notEqual(I18n.t('bpmn.crumb_root'), I18n.t('flows_builder.crumb_root'));
  // The secondary toolbar buttons keep their name as a tooltip when the stylesheet shrinks them to an icon.
  const compact = [...state.root.querySelectorAll('.fb-topbar tf-button[data-compact]')];
  assert.deepEqual(compact.map((button) => button.dataset.role), ['publish', 'run']);
  for (const button of compact) {
    assert.ok(button.getAttribute('title').startsWith(button.textContent.trim()), `${button.dataset.role} keeps its name in the tooltip`);
    assert.equal(button.getAttribute('data-aria-label'), button.textContent.trim(), `${button.dataset.role} keeps its name for assistive technology`);
  }
  Object.defineProperty(window, 'innerWidth', { value: 1024, configurable: true });
  Object.defineProperty(window, 'innerHeight', { value: 768, configurable: true });
});

test('the lines from six handlers to their names never run through another name', () => {
  const model = emptyProcessModel();
  model.errors = ['A', 'B', 'C', 'D', 'E', 'F'].map((code) => ({ errorId: `Err_${code}`, name: code, errorCode: `CODE_${code}` }));
  const graph = canvas(model); graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 600, 300);
  const task = graph.nodes.at(-1);
  graph.updateNodeConfig(task.id, { resultExpression: 'outputs.result' });
  model.errors.forEach((entry) => graph.addBoundaryNode(task.id, 'bpmn_boundary_error', entry.errorId));
  for (const element of graph.nodesLayer.querySelectorAll('.fb-process-label')) {
    Object.defineProperties(element, { offsetWidth: { value: 130, configurable: true }, offsetHeight: { value: 36, configurable: true } });
  }
  for (const element of graph.nodesLayer.querySelectorAll('.fb-process-symbol > span')) Object.defineProperties(element, { scrollHeight: { value: 99, configurable: true }, clientHeight: { value: 10, configurable: true } });
  graph._layoutProcessLabels();
  const rects = []; const lines = [];
  for (const node of graph.nodes.filter((n) => n.type === 'bpmn_boundary_error')) {
    const el = graph.nodesLayer.querySelector(`[data-node-id="${node.id}"]`);
    const label = el.querySelector('.fb-process-label'); const leader = el.querySelector('.fb-process-label-leader');
    const left = node.x + parseFloat(label.style.left), top = node.y + parseFloat(label.style.top);
    rects.push({ id: node.id, left, top, right: left + 130, bottom: top + 36 });
    const sx = node.x + parseFloat(leader.style.left), sy = node.y + parseFloat(leader.style.top);
    const len = parseFloat(leader.style.width); const ang = parseFloat(/rotate\(([-\d.e]+)rad\)/.exec(leader.style.transform)?.[1] ?? 0);
    lines.push({ id: node.id, sx, sy, len, ang });
  }
  let crossings = 0;
  for (const line of lines) for (let step = 0; step <= 40; step += 1) {
    const x = line.sx + Math.cos(line.ang) * line.len * step / 40, y = line.sy + Math.sin(line.ang) * line.len * step / 40;
    for (const r of rects) if (r.id !== line.id && x > r.left && x < r.right && y > r.top && y < r.bottom) { crossings += 1; break; }
  }
  assert.equal(rects.length, 6);
  assert.equal(crossings, 0, 'no line crosses a name that is not its own');
  graph.destroy();
});

// ---------------------------------------------------------------------------
// Round 4 of the designer review: names instead of ids, numbered repeats,
// declarations and time zones told before the server refuses them.
// ---------------------------------------------------------------------------

const userTemplate = () => processTemplates().find((row) => row.node_type === 'bpmn_user_task');
const ID_SHAPE = /\b(?:n|e)_[0-9a-z]{6,}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|Process_[0-9a-f_]{12,}/i;

function twoStartCatalog(model) {
  return ['Start', 'Start_second'].map((id) => ({ processId: model.processId, processName: model.processId,
    startNodeId: id, startNodeName: '', version: 3, trigger: { Start: { canStart: true } } }));
}

test('start entries are named by body and start element, numbered when they repeat, and never by the id the server falls back to', () => {
  const model = emptyProcessModel();
  model.nodes.push({ id: 'Start_second', name: '', kind: 'Start' });
  const labels = [...processStartEntryLabels(twoStartCatalog(model), model).values()];
  assert.deepEqual(labels, ['Process 1 · Start (1)', 'Process 1 · Start (2)']);
  model.processName = 'Billing';
  assert.deepEqual([...processStartEntryLabels(twoStartCatalog(model).slice(0, 1), model).values()], ['Billing · Start']);
});

test('the Run and Simulate windows name their start entries without ids', async () => {
  const model = emptyProcessModel();
  model.nodes.push({ id: 'Start_second', name: 'Manual intake', kind: 'Start' });
  const published = definition('named-run', { publishedVersion: 3, model });
  const catalog = twoStartCatalog(model);
  catalog[1].startNodeName = 'Manual intake';
  const run = openProcessRun(published, [{ version: 3 }], catalog, catalog[0]);
  const shown = (form, selector) => [...form.querySelectorAll(`${selector} option`)].map((option) => option.textContent);
  assert.deepEqual(shown(run, '[data-start]'), [I18n.t('bpmn.start_entry'), 'Process 1 · Start', 'Process 1 · Manual intake']);
  run.remove();
  const simulation = openProcessSimulation(published, { version: 3 }, catalog, catalog[0]);
  assert.deepEqual(shown(simulation, '[data-simulation-start]'), [I18n.t('bpmn.start_entry'), 'Process 1 · Start', 'Process 1 · Manual intake']);
  assert.doesNotMatch(document.body.textContent, ID_SHAPE);
  simulation.remove();
});

test('the Simulate window says up front that a repeated step cannot be simulated, names it and offers no start', async () => {
  const model = emptyProcessModel();
  model.nodes.push({ id: 'n_mvcvd0bg_3zhn', name: 'Review step', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } },
    repeat: { StructuredLoop: { outputCollectionVariable: 'out' } } });
  const published = definition('repeat-simulation', { publishedVersion: 3, model });
  const catalog = manualStartCatalog(model, 3);
  const simulation = openProcessSimulation(published, { version: 3 }, catalog, catalog[0]);
  await flush(2);
  const note = simulation.querySelector('.tf-act__note').getAttribute('message');
  assert.equal(note, I18n.t('bpmn.simulation_repeat_unsupported', { names: 'Review step' }));
  assert.doesNotMatch(note, /n_mvcvd0bg_3zhn/);
  assert.equal(simulation.querySelector('[data-act="submit"]').hasAttribute('disabled'), true);
  simulation.remove();
  const plain = openProcessSimulation(definition('plain-simulation', { publishedVersion: 3, model: emptyProcessModel() }), { version: 3 }, catalog, catalog[0]);
  await flush(2);
  assert.equal(plain.querySelector('.tf-act__note').getAttribute('message'), I18n.t('bpmn.simulation_hint'));
  assert.equal(plain.querySelector('[data-act="submit"]').hasAttribute('disabled'), false);
  plain.remove();
});

test('every list of the inspector numbers elements that share a name, and a palette of two flows with one name does too', async () => {
  const graph = canvas();
  const config = inspector(graph);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_exclusive_gateway'), 300, 300);
  const gateway = graph.nodes.at(-1);
  const first = graph.addNodeFromTemplate(userTemplate(), 600, 200);
  const second = graph.addNodeFromTemplate(userTemplate(), 600, 400);
  graph.connectNodes(gateway.id, first.id); graph.connectNodes(gateway.id, second.id);
  config.show(gateway, graph.templates.get(gateway.type));
  await flush(2);
  const taskName = graph.templates.get('bpmn_user_task').label;
  const texts = (selector) => [...config.root.querySelectorAll(`${selector} option`)].filter((option) => option.value).map((option) => option.textContent);
  assert.deepEqual(texts('[data-process="defaultFlowId"]'), [`${taskName} (1)`, `${taskName} (2)`], 'Default path');
  assert.deepEqual(texts('[data-process="editSequence"]'), [`${taskName} (1)`, `${taskName} (2)`], 'Edit path');
  assert.ok(texts('[data-connect]').includes(`${taskName} (1)`) && texts('[data-connect]').includes(`${taskName} (2)`), 'Connect to');
  const labels = processNodeLabels(graph.nodes, graph.templates);
  assert.equal(labels.get(first.id), `${taskName} (1)`);
  const crowded = processTemplates({ serviceFlows: [{ flowId: 'a', name: 'Contract checker' }, { flowId: 'b', name: 'Contract checker' }, { flowId: 'c', name: 'Archive' }] });
  assert.deepEqual(crowded.filter((row) => row.subgroup === 'flows').map((row) => row.label), ['Contract checker (1)', 'Contract checker (2)', 'Archive']);
  const flowGraph = canvas(); const flowConfig = new FlowConfig(document.body.appendChild(document.createElement('aside')), { mode: 'bpmn',
    processOptions: { ...options, serviceBlocks, serviceFlows: [{ flowId: 'a', name: 'Contract checker' }, { flowId: 'b', name: 'Contract checker' }] },
    getCanvas: () => flowGraph, onConfigChange: () => {}, onEdgeChange: () => {}, onLabelChange: () => {} });
  const service = flowGraph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  flowConfig.show(service, flowGraph.templates.get(service.type));
  await flush(2);
  assert.deepEqual([...flowConfig.root.querySelectorAll('[data-process="flowId"] option')].filter((option) => option.value).map((option) => option.textContent),
    ['Contract checker (1)', 'Contract checker (2)'], 'the Service task flow list');
  flowGraph.destroy(); graph.destroy();
});

test('the breadcrumb numbers sub-processes that share a name and keeps Leave after the path', async () => {
  const state = await mount(definition('nested-crumb'));
  const sub = (x) => state.canvas.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_sub_process'), x, 300);
  const outer = sub(300);
  state.canvas.navigateProcessBody([outer.id]);
  const inner = sub(300);
  state.canvas.navigateProcessBody([outer.id, inner.id]);
  builder._syncProcessPath();
  const path = state.root.querySelector('[data-role="scope-path"]');
  const kind = I18n.t('bpmn.node_sub_process');
  assert.equal(path.textContent, `${state.root.querySelector('[data-role="name"]').value} › ${kind} (1) › ${kind} (2)`);
  const up = state.root.querySelector('[data-role="scope-up"]');
  assert.equal(up.hidden, false);
  assert.ok(up.compareDocumentPosition(state.root.querySelector('[data-role="crumb-name"]')) & window.Node.DOCUMENT_POSITION_PRECEDING,
    'the Leave button follows the whole path');
  assert.ok(up.compareDocumentPosition(path) & window.Node.DOCUMENT_POSITION_PRECEDING);
});

test('Sub-process and Human task inspectors head each mapping, so the two Add buttons are told apart', async () => {
  const graph = canvas();
  const config = inspector(graph);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_sub_process'), 300, 300);
  const sub = graph.nodes.at(-1);
  config.show(sub, graph.templates.get(sub.type));
  await flush(2);
  const headings = (root) => [...root.querySelectorAll('tf-keyvalue-editor .tf-kve-label')].map((label) => label.textContent);
  assert.deepEqual(headings(config.root), [I18n.t('bpmn.input_mapping'), I18n.t('bpmn.output_mapping')]);
  const user = userNode(graph);
  config.show(user, graph.templates.get(user.type));
  await flush(2);
  assert.deepEqual(headings(config.root), [I18n.t('bpmn.output_mapping')]);
  graph.destroy();
});

test('no inspector of a typical process shows an element id, a UUID or a definition id outside its technical details', async () => {
  const model = emptyProcessModel();
  model.errors = [{ errorId: 'Err_x', name: 'Payment failed', errorCode: 'PAY' }];
  const graph = canvas(model); const config = inspector(graph);
  const service = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 300);
  const gateway = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_exclusive_gateway'), 600, 300);
  graph.connectNodes(service.id, gateway.id);
  graph.addBoundaryNode(service.id, 'bpmn_boundary_error', 'Err_x');
  const boundary = graph.nodes.at(-1);
  const call = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_call_activity'), 300, 600);
  fixtures({ processDefinitionListRequest: { definitions: [], hasMore: false } });
  for (const node of [service, gateway, boundary, call, graph.nodes[0]]) {
    config.show(node, graph.templates.get(node.type));
    await flush(2);
    const clone = config.root.cloneNode(true);
    clone.querySelectorAll('[data-technical], [data-activity-field="id"], [data-process="linkDefinitionId"]').forEach((technical) => technical.remove());
    const visible = [...clone.querySelectorAll('option, .fb-config-title, .fb-config-subtitle, .fb-field-hint, tf-alert')]
      .map((element) => element.textContent + (element.getAttribute?.('message') ?? '')).join('\n');
    assert.doesNotMatch(visible, ID_SHAPE, node.type);
  }
  const edge = graph.edges[0];
  config.showEdge(edge);
  assert.ok(config.root.querySelector('tf-disclosure[data-technical] [data-process="elementId"]'), 'an element id sits behind the technical details');
  assert.equal(config.root.querySelector('tf-disclosure[data-technical]').hasAttribute('open'), false, 'and starts closed');
  graph.destroy();
});

test('a saved Send target is named, not shown as an id pair, and a failed lookup is told by name', async () => {
  const model = emptyProcessModel();
  model.nodes.splice(1, 0, { id: 'Send_1', name: 'Notify billing', kind: { SendTask: { messageRef: null, correlationExpression: null,
    target: { Start: { definitionId: 'dddddddd-0000-4000-8000-00000000000d', processId: 'Process_target_0001', startNodeId: 'n_start_0001' } },
    payloadExpression: null, ttlSeconds: null } } });
  model.sequenceFlows = [{ id: 'Flow_1', sourceId: 'Start', targetId: 'Send_1', condition: null }, { id: 'Flow_2', sourceId: 'Send_1', targetId: 'End', condition: null }];
  const published = emptyProcessModel(); published.processId = 'Process_target_0001'; published.processName = 'Billing intake';
  published.messages = [{ messageId: 'Message_1', name: 'invoice.received' }];
  const graph = canvas(model); const config = inspector(graph);
  fixtures({
    processDefinitionGetRequest: { definition: { definitionId: 'dddddddd-0000-4000-8000-00000000000d', name: 'Billing', archived: false, publishedVersion: 2 } },
    processVersionGetRequest: { version: { model: published }, startCatalog: [{ processId: 'Process_target_0001', processName: 'Process_target_0001', startNodeId: 'n_start_0001', startNodeName: 'Invoice arrives', version: 2, trigger: { MessageStart: { messageName: 'invoice.received', canSend: true } } }] },
  });
  const send = graph.nodes.find((node) => node.id === 'Send_1');
  config.show(send, graph.templates.get(send.type));
  await flush(4);
  assert.equal(config.root.querySelector('[data-process="targetDefinitionChoice"] .tf-select-selected').textContent, 'Billing');
  assert.equal(config.root.querySelector('[data-process="targetStartChoice"] .tf-select-selected').textContent, 'Billing intake · Invoice arrives');
  assert.doesNotMatch(config.root.textContent.replace(/[^\n]*\u0000/g, ''), /Process_target_0001|n_start_0001/);
  graph.destroy();
});

test('a Send task lists the owned definitions without a click and keeps the target id and the called process id behind technical details', async () => {
  const definitionId = 'eeeeeeee-0000-4000-8000-00000000000e';
  const graph = canvas(emptyProcessModel()); const config = inspector(graph);
  const send = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_send_task'), 300, 300);
  fixtures({ processDefinitionListRequest: { definitions: [{ definitionId, name: 'Billing', publishedVersion: 1, archived: false }], hasMore: false } });
  config.show(send, graph.templates.get(send.type));
  await flush(3);
  assert.ok(calls.some((call) => call.kind === 'processDefinitionListRequest' && call.payload.offset === 0), 'the first page is requested on its own');
  const choice = config.root.querySelector('[data-process="targetDefinitionChoice"]');
  assert.ok([...choice.querySelectorAll('option')].some((option) => option.textContent === 'Billing'), 'and offered by name');
  assert.ok(config.root.querySelector('tf-disclosure[data-technical] [data-process="targetDefinitionId"]'), 'the target id is a technical field');
  assert.equal(config.root.querySelector('tf-disclosure[data-technical]').hasAttribute('open'), false);
  assert.equal(config.root.querySelector('[data-load-targets]').hidden, true, 'nothing more to load');
  const call = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_call_activity'), 300, 600);
  fixtures({ processDefinitionListRequest: { definitions: [], hasMore: false } });
  config.show(call, graph.templates.get(call.type));
  assert.ok(config.root.querySelector('tf-disclosure[data-technical] [data-process="calledElement"]'), 'the called namespace and id are technical details');
  graph.destroy();
});

test('a Call inspector answer for the stored definition is dropped once another definition was chosen', async () => {
  const storedId = 'aaaaaaaa-0000-4000-8000-00000000000a', otherId = 'bbbbbbbb-0000-4000-8000-00000000000b';
  const called = emptyProcessModel(); called.processId = 'Called_Process';
  const graph = canvas(callerModel({ calledDefinitionId: storedId, calledVersion: 2, calledElement: { namespaceUri: 'urn:x', processId: 'Called_Process' } }));
  const config = inspector(graph);
  const lookup = deferred();
  fixtures({
    processDefinitionListRequest: { definitions: [{ definitionId: otherId, name: 'Other', publishedVersion: 1, archived: false }], hasMore: false },
    processVersionListRequest: { versions: [{ version: 1 }], hasMore: false },
    processDefinitionGetRequest: () => lookup.promise,
    processVersionGetRequest: () => ({ version: { model: called }, startCatalog: [] }),
  });
  const call = graph.nodes.find((node) => node.id === 'Call_1');
  config.show(call, graph.templates.get(call.type));
  await flush(3);
  change(config.root.querySelector('[data-process="calledDefinitionChoice"]'), otherId);
  lookup.resolve({ definition: { definitionId: storedId, name: 'Old approvals', archived: true, publishedVersion: 2 } });
  await flush(4);
  assert.equal(config.root.querySelector('[data-call-target-warning]').hidden, true, 'the stale answer raised no warning for the new choice');
  assert.equal(graph.nodes.find((node) => node.id === 'Call_1').config.calledDefinitionId, otherId);
  graph.destroy();
});

test('a repeated element without an output collection variable says so in its inspector', async () => {
  const graph = canvas(); const config = inspector(graph);
  const task = userNode(graph);
  graph.updateNodeLabel(task.id, 'Critic flow');
  graph.updateNodeConfig(task.id, {});
  task.repeat = { MultiInstance: { mode: 'Sequential', input: { Cardinality: { count: 2 } }, outputCollectionVariable: '' } };
  config.show(task, graph.templates.get(task.type));
  await flush(2);
  const warning = config.root.querySelector('[data-repeat-warning]');
  assert.ok(warning, 'a warning stands under the output variable');
  assert.equal(warning.getAttribute('message'), I18n.t('bpmn.repeat_output_declare', { node: 'Critic flow' }));
  assert.ok(config.root.querySelector('[data-repeat-declare-output]'), 'the section offers to declare one right there');
  task.repeat.MultiInstance.outputCollectionVariable = 'results';
  config.show(task, graph.templates.get(task.type));
  await flush(2);
  assert.equal(config.root.querySelector('[data-repeat-warning]'), null);
  graph.destroy();
});

test('server refusals about declarations, repeats and time zones are told in the author\'s words, naming the element', () => {
  const model = emptyProcessModel();
  model.errors = [{ errorId: 'Err_pay', name: 'Payment failed', errorCode: 'PAY' }, { errorId: 'Err_used', name: 'Used one', errorCode: 'USED' }];
  model.nodes.splice(1, 0, { id: 'Critic_1', name: 'Critic flow', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } },
    repeat: { MultiInstance: { mode: 'Sequential', input: { Cardinality: { count: 2 } }, outputCollectionVariable: '' } } },
  { id: 'Fail_end', name: 'Failed', kind: { ErrorEnd: { errorRef: 'Err_used' } } });
  const unreferenced = processExplainError('protocol error BadRequest: unreferenced error declaration', model);
  assert.match(unreferenced, /Payment failed/);
  assert.doesNotMatch(unreferenced, /protocol error|BadRequest|Used one/);
  assert.equal(processExplainError('repeat Critic_1 has invalid output collection variable', model), I18n.t('bpmn.repeat_output_missing', { node: 'Critic flow' }));
  assert.equal(processExplainError('timerless process cannot declare a timer timezone', model), I18n.t('bpmn.timer_timezone_unused'));
  assert.deepEqual(processPublishProblems(model), [I18n.t('bpmn.unused_error', { name: 'Payment failed' }), I18n.t('bpmn.repeat_output_missing', { node: 'Critic flow' })]);
});

test('publishing is refused before any request when a declaration is unused, naming it in the banner', async () => {
  const model = emptyProcessModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  const current = definition('unused-declaration', { model });
  const state = await mount(current, { processDefinitionSaveRequest: { definition: { ...current, draftRevision: 5 } }, processDefinitionPublishRequest: () => { throw new Error('the server must not be asked'); } });
  await builder._publish();
  const banner = state.root.querySelector('[data-role="error"]');
  assert.equal(banner.hidden, false);
  assert.match(banner.getAttribute('message'), /order\.received/);
  assert.equal(calls.some((call) => call.kind === 'processDefinitionPublishRequest'), false);
});

test('a time zone is trimmed and checked, and goes with the last timer', async () => {
  assert.equal(processTimezoneProblem('Europe/Warsaw'), null);
  assert.equal(processTimezoneProblem(' UTC '), null);
  assert.equal(processTimezoneProblem(''), null);
  assert.equal(processTimezoneProblem('Mars/Phobos'), 'invalid');
  assert.equal(processTimezoneProblem('not a zone'), 'invalid');
  const realSupportedValuesOf = Intl.supportedValuesOf;
  Intl.supportedValuesOf = () => ['Europe/Warsaw', 'America/Winnipeg'];
  try {
    for (const [typed, spelt] of [['utc', 'UTC'], ['Utc', 'UTC'], ['gmt', 'GMT'], ['est', 'EST'], ['europe/warsaw', 'Europe/Warsaw']]) {
      assert.equal(processTimezoneProblem(typed), null, `${typed} is accepted`);
      assert.equal(processTimezoneCanonical(typed), spelt, `${typed} is repaired to the engine spelling`);
    }
    assert.equal(processTimezoneProblem('us/pacific'), 'invalid', 'a link in lower case would pass the browser and be refused by the engine');
    assert.equal(processTimezoneCanonical('us/pacific'), null);
  } finally {
    Intl.supportedValuesOf = realSupportedValuesOf;
  }
  assert.equal(processTimezoneProblem('US/Pacific'), null);
  const graph = canvas(timedModel('TimerCatch'));
  graph.updateProcessTimezone('  Europe/Berlin ');
  assert.equal(graph.getData().timerTimezone, 'Europe/Berlin');
  graph.updateProcessTimezone('Mars/Phobos');
  assert.equal(graph.getData().timerTimezone, 'Europe/Berlin', 'an invalid zone is not stored');
  graph.removeNodes(['Wait']);
  assert.equal(graph.getData().timerTimezone, undefined, 'the zone goes with the last timer, so the server never refuses a timerless process for it');
  graph.destroy();
  const calendarModel = timedModel('TimerCatch'); calendarModel.workCalendar = { name: 'Office', workingHours: [], holidays: [] };
  const calendared = canvas(calendarModel);
  calendared.removeNodes(['Wait']);
  assert.equal(calendared.getData().timerTimezone, 'Europe/Warsaw', 'a work calendar keeps its zone');
  calendared.destroy();
  const invalid = canvas(timedModel('TimerCatch'));
  invalid.processModel.timerTimezone = 'Mars/Phobos';
  assert.ok(invalid.validate().some((message) => message === I18n.t('bpmn.timer_timezone_invalid', { zone: 'Mars/Phobos' })));
  invalid.destroy();
});

test('the process time zone field refuses a bad zone with a message and a save only walks the author to it for that reason', async () => {
  const current = definition('zone-field', { model: timedModel('TimerCatch') });
  const state = await mount(current);
  const field = state.root.querySelector('[data-role="timer-timezone"]');
  field.value = 'Mars/Phobos';
  field.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: field.value } }));
  assert.equal(field.getAttribute('error'), I18n.t('bpmn.timer_timezone_invalid', { zone: 'Mars/Phobos' }));
  assert.equal(state.canvas.getData().timerTimezone, 'Europe/Warsaw');
  field.value = ' Europe/Paris ';
  field.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: field.value } }));
  assert.equal(field.hasAttribute('error'), false);
  assert.equal(state.canvas.getData().timerTimezone, 'Europe/Paris');
  let revealed = 0;
  state.showSettings = () => { revealed += 1; };
  state.canvas.processModel.timerTimezone = '';
  builder._revealMissingTimezone(state, ['Some other refusal']);
  assert.equal(revealed, 0, 'another refusal leaves the settings alone');
  builder._revealMissingTimezone(state, [processTimezoneText(processTimezoneGaps(state.canvas.getData())[0])]);
  assert.equal(revealed, 1);
});

test('undoing a change of a process setting says so instead of claiming another part of the process changed', async () => {
  const graph = canvas(timedModel('TimerCatch'));
  // Toasts with the same words merge into one that is already on screen.
  for (const key of ['bpmn.undo_settings', 'bpmn.undo_elsewhere']) dismissToast(I18n.t(key), 'info');
  const shown = watchToasts();
  graph.updateProcessTimezone('Europe/Berlin');
  graph.undo();
  assert.ok(shown.texts().includes(I18n.t('bpmn.undo_settings')));
  assert.equal(shown.texts().includes(I18n.t('bpmn.undo_elsewhere')), false);
  shown.stop(); graph.destroy();
});

test('redoing a change of a process setting does not say it was undone', async () => {
  const graph = canvas(timedModel('TimerCatch'));
  for (const key of ['bpmn.undo_settings', 'bpmn.redo_settings']) dismissToast(I18n.t(key), 'info');
  graph.updateProcessTimezone('Europe/Berlin');
  graph.undo();
  for (const key of ['bpmn.undo_settings', 'bpmn.redo_settings']) dismissToast(I18n.t(key), 'info');
  const shown = watchToasts();
  graph.redo();
  assert.ok(shown.texts().includes(I18n.t('bpmn.redo_settings')));
  assert.equal(shown.texts().includes(I18n.t('bpmn.undo_settings')), false);
  assert.doesNotMatch(I18n.t('bpmn.redo_settings'), /^Undone|^Cofni/);
  shown.stop(); graph.destroy();
});

test('Run is described by its reason, takes the only start entry by itself and keeps the reason on a wrapper a disabled button cannot hide', async () => {
  const model = emptyProcessModel();
  const unpublished = await mount(definition('run-reason', { model }));
  const wrap = () => builder._state.root.querySelector('[data-role="run-wrap"]');
  assert.match(wrap().getAttribute('title'), new RegExp(I18n.t('bpmn.run_reason_unpublished').slice(0, 20)));
  assert.equal(unpublished.root.querySelector('[data-role="run-reason"]').textContent, wrap().getAttribute('title'));
  assert.equal(unpublished.root.querySelector('[data-role="run"]').getAttribute('data-aria-describedby'), 'fb-run-reason');
  const published = definition('run-single', { model, publishedVersion: 2 });
  const state = await mount(published, { processDefinitionGetRequest: { definition: published, startCatalog: manualStartCatalog(model, 2) } });
  assert.equal(state.root.querySelector('[data-role="start-entry"]').value, '0', 'the only entry is chosen');
  assert.equal(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'), false);
  model.nodes.push({ id: 'Start_second', name: 'Second', kind: 'Start' });
  const many = definition('run-many', { model, publishedVersion: 2 });
  const choose = await mount(many, { processDefinitionGetRequest: { definition: many, startCatalog: twoStartCatalog(model) } });
  assert.equal(choose.root.querySelector('[data-role="start-entry"]').value, '');
  assert.equal(choose.root.querySelector('[data-role="run"]').hasAttribute('disabled'), true);
  assert.match(choose.root.querySelector('[data-role="run-wrap"]').getAttribute('title'), new RegExp(I18n.t('bpmn.run_reason_choose_entry').slice(0, 20)));
});

test('a press that opens the inspector on a phone is not closed by the click that follows, and the tab bar follows the panel', async () => {
  const state = await mount(definition('phone-tabs'));
  const tabs = state.root.querySelector('.fb-mobile-tabs');
  Object.defineProperty(tabs, 'offsetParent', { configurable: true, get: () => document.body });
  const body = state.root.querySelector('[data-role="body"]');
  const config = state.root.querySelector('[data-role="config"]');
  Object.defineProperty(window, 'innerWidth', { value: 390, configurable: true });
  const press = (target) => target.dispatchEvent(new window.Event('pointerdown', { bubbles: true }));
  press(body.querySelector('[data-role="canvas"]') ?? body);
  state.showMobileTab('config');
  assert.equal(config.classList.contains('open'), true);
  body.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.equal(config.classList.contains('open'), true, 'the click that ends the opening press leaves the panel open');
  press(body);
  body.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.equal(config.classList.contains('open'), false, 'a press on the backdrop closes it');
  assert.equal(tabs.getAttribute('value'), 'canvas', 'and the tab bar names the canvas again');
  state.showMobileTab('config');
  assert.equal(config.classList.contains('open'), true, 'so the next element opens the inspector again');
  Object.defineProperty(window, 'innerWidth', { value: 1024, configurable: true });
});

test('a boundary event not on an activity looks unattached, not attached', () => {
  const graph = canvas();
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer'), 400, 300);
  const element = graph.nodesLayer.querySelector('.fb-process-node.bpmn_boundary_timer');
  assert.equal(element.classList.contains('fb-unattached'), true);
  assert.equal(element.dataset.unattachedLabel, I18n.t('bpmn.boundary_not_attached'));
  graph.destroy();
});

test('the label of an unattached event keeps clear of its Not attached tag when the room below is taken', () => {
  const graph = canvas();
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer'), 400, 300);
  const event = graph.nodes.at(-1);
  const task = processTemplates().find((row) => row.node_type === 'bpmn_user_task');
  for (const [x, y] of [[event.x - 120, event.y + 90], [event.x + 120, event.y + 90], [event.x - 330, event.y - 20], [event.x + 330, event.y - 20]]) graph.addNodeFromTemplate(task, x, y);
  graph.nodes.filter((node) => node.type === 'bpmn_user_task').forEach((node, index) => { node.x = [event.x - 120, event.x + 120, event.x - 330, event.x + 330][index]; node.y = [event.y + 90, event.y + 90, event.y - 20, event.y - 20][index]; });
  for (const element of graph.nodesLayer.querySelectorAll('.fb-process-label')) {
    Object.defineProperties(element, { offsetWidth: { value: 100, configurable: true }, offsetHeight: { value: 30, configurable: true } });
  }
  graph._layoutProcessLabels();
  const element = graph.nodesLayer.querySelector(`[data-node-id="${event.id}"]`);
  const label = element.querySelector('.fb-process-label');
  if (!element.classList.contains('fb-label-deferred')) {
    const left = event.x + parseFloat(label.style.left), top = event.y + parseFloat(label.style.top);
    const tag = { left: event.x + event.width / 2 - UNATTACHED_TAG.width / 2, right: event.x + event.width / 2 + UNATTACHED_TAG.width / 2,
      top: event.y - UNATTACHED_TAG.offset - UNATTACHED_TAG.height, bottom: event.y - UNATTACHED_TAG.offset };
    assert.equal(left < tag.right && tag.left < left + 100 && top < tag.bottom && tag.top < top + 30, false, 'the label does not cover the tag');
  }
  graph.destroy();
});

test('leader lines of six handlers on one task never run through the task, and a hidden canvas keeps its labels', () => {
  const model = emptyProcessModel();
  model.errors = ['A', 'B', 'C', 'D', 'E', 'F'].map((code) => ({ errorId: `Err_${code}`, name: `Failure ${code}`, errorCode: `CODE_${code}` }));
  const graph = canvas(model);
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 600, 300);
  const task = graph.nodes.at(-1);
  model.errors.forEach((entry) => graph.addBoundaryNode(task.id, 'bpmn_boundary_error', entry.errorId));
  for (const element of graph.nodesLayer.querySelectorAll('.fb-process-label')) {
    Object.defineProperties(element, { offsetWidth: { value: 130, configurable: true }, offsetHeight: { value: 36, configurable: true } });
  }
  for (const element of graph.nodesLayer.querySelectorAll('.fb-process-symbol > span')) Object.defineProperties(element, { scrollHeight: { value: 99, configurable: true }, clientHeight: { value: 10, configurable: true } });
  graph._layoutProcessLabels();
  const inside = { left: task.x + 3, top: task.y + 3, right: task.x + task.width - 3, bottom: task.y + task.height - 3 };
  let through = 0;
  for (const node of graph.nodes.filter((candidate) => candidate.type === 'bpmn_boundary_error')) {
    const leader = graph.nodesLayer.querySelector(`[data-node-id="${node.id}"] .fb-process-label-leader`);
    const sx = node.x + parseFloat(leader.style.left), sy = node.y + parseFloat(leader.style.top);
    const length = parseFloat(leader.style.width), angle = parseFloat(/rotate\(([-\d.e]+)rad\)/.exec(leader.style.transform)?.[1] ?? 0);
    for (let step = 0; step <= 40; step += 1) {
      const x = sx + Math.cos(angle) * length * step / 40, y = sy + Math.sin(angle) * length * step / 40;
      if (x > inside.left && x < inside.right && y > inside.top && y < inside.bottom) { through += 1; break; }
    }
  }
  assert.equal(through, 0, 'no leader line crosses the task it hangs from');
  const label = graph.nodesLayer.querySelector('.fb-process-node.bpmn_boundary_error .fb-process-label');
  label.style.left = '123px';
  graph.root.checkVisibility = () => false;
  graph._layoutProcessLabels();
  assert.equal(label.style.left, '123px', 'a canvas with no layout is left as it was placed');
  assert.equal(label.hidden, false);
  // A browser without checkVisibility answers the same question from the client rects.
  graph.root.checkVisibility = undefined;
  graph.root.getClientRects = () => [];
  graph._layoutProcessLabels();
  assert.equal(label.style.left, '123px', 'without checkVisibility a hidden canvas still keeps its labels');
  assert.equal(label.hidden, false);
  graph.destroy();
});

test('an outside label with nowhere to go: a title goes back inside the box, an event shows its name on hover and in its tooltip', () => {
  const model = emptyProcessModel();
  model.nodes.splice(1, 0, { id: 'Review', name: 'A name too long for the box', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } });
  model.sequenceFlows = [{ id: 'Flow_1', sourceId: 'Start', targetId: 'Review', condition: null }, { id: 'Flow_2', sourceId: 'Review', targetId: 'End', condition: null }];
  model.diagram.shapes.push({ elementId: 'Review', x: 200, y: 160, width: 240, height: 96 });
  model.nodes.push({ id: 'Giant', name: '', kind: 'End' });
  model.diagram.shapes.push({ elementId: 'Giant', x: -8000, y: -8000, width: 16000, height: 16000 });
  const graph = canvas(model);
  const review = graph.nodesLayer.querySelector('[data-node-id="Review"]');
  const start = graph.nodesLayer.querySelector('[data-node-id="Start"]');
  Object.defineProperties(review.querySelector('.fb-process-symbol > span'), { scrollHeight: { value: 99, configurable: true }, clientHeight: { value: 10, configurable: true }, scrollWidth: { value: 5, configurable: true }, clientWidth: { value: 5, configurable: true } });
  for (const element of [review, start]) Object.defineProperties(element.querySelector('.fb-process-label'), { offsetWidth: { value: 100, configurable: true }, offsetHeight: { value: 30, configurable: true } });
  const startLabel = start.querySelector('.fb-process-label');
  startLabel.style.left = '5px';
  graph._layoutProcessLabels();
  assert.equal(review.querySelector('.fb-process-label').hidden, true);
  assert.equal(review.querySelector('.fb-process-label-leader').hidden, true);
  assert.equal(review.classList.contains('fb-external-label'), false, 'the title is inside the box again');
  assert.equal(startLabel.hidden, false, 'an event has no title inside, so its label stays available');
  assert.equal(startLabel.style.left, '', 'but not at a spot that covers or crosses something');
  assert.equal(start.classList.contains('fb-label-deferred'), true, 'it shows on hover and selection');
  assert.equal(start.getAttribute('title'), startLabel.textContent, 'and always in the tooltip');
  assert.equal(start.querySelector('.fb-process-label-leader').hidden, true);
  graph.destroy();
});

test('a deferred label takes no part in bounds or footprints and no non-finite number reaches the view or the model', () => {
  const graph = canvas(timedModel());
  graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600 });
  const element = graph.nodesLayer.querySelector('[data-node-id="Start"]');
  const label = element.querySelector('.fb-process-label');
  Object.defineProperties(label, { offsetWidth: { value: 200, configurable: true }, offsetHeight: { value: 40, configurable: true } });
  label.hidden = false;
  label.style.left = '';
  label.style.top = '';
  element.classList.add('fb-label-deferred');
  const bounds = graph._contentBounds();
  for (const value of Object.values(bounds)) assert.ok(Number.isFinite(value), 'a deferred label has no offsets to measure');
  const start = graph.nodes.find((node) => node.id === 'Start');
  const footprint = graph._footprint(start, false);
  assert.deepEqual([footprint.w, footprint.h, footprint.ox + 0, footprint.oy + 0], [start.width, start.height, 0, 0]);
  graph.fitToContent();
  for (const value of [graph.view.x, graph.view.y, graph.view.zoom]) assert.ok(Number.isFinite(value));
  assert.match(graph.world.style.transform, /^translate\(-?[\d.]+px, -?[\d.]+px\) scale\([\d.]+\)$/);

  element.classList.remove('fb-label-deferred');
  label.style.left = '';
  assert.equal(graph._placedLabelBox(start), null, 'an unplaced label without offsets is not a box either');

  graph.view.x = Number.NaN;
  graph.view.y = Number.POSITIVE_INFINITY;
  graph.setZoom(1.5, Number.NaN, Number.NaN);
  assert.ok(Number.isFinite(graph.view.x) && Number.isFinite(graph.view.y));

  graph.nodes[0].x = Number.NaN;
  graph.nodes[0].y = Number.NaN;
  const saved = graph.getData();
  for (const shape of saved.diagram.shapes) for (const value of [shape.x, shape.y, shape.width, shape.height]) assert.ok(Number.isFinite(value));
  assert.doesNotMatch(JSON.stringify(saved), /"(x|y|width|height)":null/);

  const dropped = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_user_task'), Number.NaN, Number.NaN);
  assert.ok(Number.isFinite(dropped.x) && Number.isFinite(dropped.y));
  graph.destroy();
});

test('an idle toolbar does not measure itself again', async () => {
  const frames = [];
  const original = globalThis.requestAnimationFrame;
  globalThis.requestAnimationFrame = (callback) => { frames.push(callback); return original(callback); };
  try {
    const state = await mount(definition('toolbar-idle'));
    await flush(10);
    state.fitTopbar();
    await flush(10);
    frames.length = 0;
    await flush(20);
    assert.equal(frames.length, 0, 'nothing changed, so nothing is measured');
  } finally {
    globalThis.requestAnimationFrame = original;
  }
});

test('a drop under the minimap moves aside and a drop at the canvas edge is brought into view', () => {
  const root = document.createElement('div'); document.body.append(root);
  root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600 });
  const minimap = { left: 660, top: 500, width: 120, height: 80 };
  const graph = new FlowCanvas(root, { mode: 'bpmn', processOptions: { ...options, serviceBlocks }, obstacles: () => [minimap] });
  graph.setTemplates(processTemplates()); graph.setData(emptyProcessModel());
  graph.view = { x: 0, y: 0, zoom: 1 };
  const task = processTemplates().find((row) => row.node_type === 'bpmn_user_task');
  const under = graph.addNodeFromTemplate(task, 720, 540);
  const hidden = under.x < minimap.left + minimap.width && minimap.left < under.x + under.width
    && under.y < minimap.top + minimap.height && minimap.top < under.y + under.height;
  assert.equal(hidden, false, 'the node is not under the minimap');
  graph.view = { x: 0, y: 0, zoom: 1 };
  const edge = graph.addNodeFromTemplate(task, 799, 20);
  const left = edge.x * graph.view.zoom + graph.view.x;
  const right = (edge.x + edge.width) * graph.view.zoom + graph.view.x;
  assert.ok(left >= 0 && right <= 800, `the node (${left}..${right}) lies inside the canvas after the pan`);
  graph.destroy();
});

test('the minimap pans the canvas on a press and folds to its title bar, remembered across screens', async () => {
  localStorage.removeItem('tf_fb_minimap_collapsed');
  const model = emptyProcessModel();
  model.diagram.shapes.find((shape) => shape.elementId === 'End').x = 4000;
  const state = await mount(definition('minimap-pan', { model }));
  const mini = state.root.querySelector('[data-role="minimap"]');
  mini.getBoundingClientRect = () => ({ left: 100, top: 100, width: 120, height: 80, right: 220, bottom: 180 });
  Object.defineProperties(mini, { clientWidth: { value: 120, configurable: true }, clientHeight: { value: 80, configurable: true } });
  state.canvas.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600 });
  builder._renderMinimap();
  const map = state.minimapMap;
  assert.ok(map, 'the minimap knows where the drawing sits');
  const before = state.canvas.view.x;
  mini.dispatchEvent(new window.MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 215, clientY: 140 }));
  assert.ok(state.canvas.view.x < before, 'a press on the right of the map moves the view toward the right of the drawing');
  const toggle = mini.querySelector('[data-role="minimap-toggle"]');
  assert.equal(toggle.dataset.expanded, 'true');
  click(toggle);
  assert.equal(mini.classList.contains('collapsed'), true);
  assert.equal(toggle.dataset.expanded, 'false');
  assert.equal(toggle.getAttribute('data-aria-label'), I18n.t('flows_builder.minimap_expand'));
  assert.equal(localStorage.getItem('tf_fb_minimap_collapsed'), '1');
  const pressed = state.canvas.view.x;
  mini.dispatchEvent(new window.MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 120, clientY: 140 }));
  assert.equal(state.canvas.view.x, pressed, 'a folded minimap does not pan');
  const again = await mount(definition('minimap-pan-again', { model }));
  assert.equal(again.root.querySelector('[data-role="minimap"]').classList.contains('collapsed'), true, 'the choice is remembered');
  localStorage.removeItem('tf_fb_minimap_collapsed');
});

test('a boundary event refused on a repeating task names the real cause in the drop toast, the check before saving and the server message', async () => {
  const repeat = { MultiInstance: { mode: 'Sequential', input: { Cardinality: { count: 2 } }, outputCollectionVariable: 'parts' } };
  const graph = canvas();
  const script = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_script_task'), 700, 300);
  graph.updateNodeLabel(script.id, 'Compute each part');
  graph.updateNodeConfig(script.id, { repeat });
  const manual = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_user_task'), 1100, 300);
  graph.updateNodeLabel(manual.id, 'Review each part');
  graph.updateNodeConfig(manual.id, { repeat });
  const toasts = watchToasts();
  try {
    const point = (node) => [node.x + node.width / 2, node.y + node.height];
    const error = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_error'), ...point(script));
    assert.equal(error.config.attachedToId ?? '', '', 'a Script task takes no error event');
    assert.ok(toasts.texts().some((text) => text.includes('Compute each part') && /error event fits only a Service task/.test(text) && !/repeat/.test(text)),
      'the toast names the task and the kind of task as the reason, not the repetition');
    const escalation = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_escalation'), ...point(manual));
    assert.equal(escalation.config.attachedToId ?? '', '');
    assert.ok(toasts.texts().some((text) => text.includes('Review each part') && /escalation event fits only a Service task/.test(text) && !/repeat/.test(text)),
      'an escalation dropped on a looping User task is explained too');
    const service = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 300, 600);
    graph.updateNodeLabel(service.id, 'Call each part');
    graph.updateNodeConfig(service.id, { repeat });
    graph.updateNodeConfig(service.id, { resultExpression: '{}' });
    const timer = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_timer'), ...point(manual));
    assert.equal(timer.config.attachedToId, manual.id, 'a timer is welcome on a repeating task');
    const refusals = processBoundaryRefusals(graph, 'BoundaryError');
    assert.equal(refusals.get(service.id), null, 'an error event fits a repeating Service task');
    assert.equal(refusals.get(manual.id), 'error_type', 'a User task takes no error event, repeating or not');
    assert.equal(processBoundaryRefusals(graph, 'BoundaryEscalation').get(manual.id), 'escalation_type');
    assert.equal(processBoundaryRefusals(graph, 'BoundaryTimer').get(manual.id), null);
  } finally { toasts.stop(); }

  const config = inspector(graph);
  graph.updateNodeConfig(manual.id, { resultExpression: undefined });
  const tail = graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_message'), 500, 700);
  graph.updateNodeConfig(tail.id, { attachedToId: manual.id });
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_boundary_escalation'), 800, 800);
  const escalation = graph.nodes.at(-1);
  config.show(escalation, graph.templates.get(escalation.type));
  await flush(2);
  assert.equal(config.root.querySelector('[data-boundary-excluded]'), null, 'no event is refused by repetition alone, so no list blames it');

  graph.updateNodeConfig(escalation.id, { attachedToId: manual.id });
  assert.ok(graph.validate().some((message) => message.includes('Review each part') && /escalation event fits only a Service task/.test(message) && !/repeats/.test(message)),
    'the check before saving names the task and the real cause');

  const model = graph.getData();
  const hostId = graph.nodes.find((node) => node.label === 'Compute each part').id;
  const explained = processExplainError(`protocol error BadRequest: repeat ${hostId} has an unsupported boundary event`, model);
  assert.match(explained, /Compute each part/);
  assert.match(explained, /Remove the repetition/);
  assert.doesNotMatch(explained, /^repeat |protocol error/);
  graph.destroy();
});

test('known refusals from the engine are told in plain words and a repeating Script task with data inputs is flagged', () => {
  assert.equal(processPlainError('activity IO input capture lacks one later resource or accepted body fact'), null, 'the engine no longer refuses that start');
  assert.match(processPlainError('process draft revision conflict or archived definition'), /changed somewhere else/);
  assert.match(processPlainError('publication needs a Current verified calendar pin'), /working calendar/);
  assert.equal(processPlainError('something else'), null);
  const script = { type: 'bpmn_script_task', config: {}, repeat: { StructuredLoop: {} }, activityIo: { dataInputs: [{ id: 'in' }], inputAssociations: [{}], dataOutputs: [{ id: 'out' }], outputAssociations: [] } };
  assert.ok(processNodeWarnings(script).includes('repeated_script_io'));
  assert.equal(processNodeWarnings({ ...script, repeat: undefined }).includes('repeated_script_io'), false);
  assert.equal(processNodeWarnings({ ...script, activityIo: { dataInputs: [{ id: 'in' }], inputAssociations: [{}], dataOutputs: [], outputAssociations: [] } }).includes('repeated_script_io'), false, 'input data is supported');
});

test('in a repeated group that holds an incident the failed occurrence reads as failed and the later ones as not started', async () => {
  const group = { groupId: 'g1', nodeId: 'Script_1', nodeName: 'Compute each part', scopeId: 's', parentTokenId: 'p', mode: 'multi_instance_sequential',
    status: 'incident', total: 3, createdCount: 1, completed: 0, maxIterations: null, revision: 1, createdAtMs: 1, updatedAtMs: 1 };
  const row = (ordinal, status) => ({ occurrenceId: `o${ordinal}`, groupId: 'g1', ordinal, status, tokenId: `t${ordinal}`, userTaskId: null, jobId: null,
    verificationUserTaskId: null, acceptedSourceEventId: null, approvalEventId: null, createdAtMs: 1, updatedAtMs: 1 });
  const incident = { incidentId: 'i1', nodeId: 'Script_1', nodeName: 'Compute each part', jobId: null, code: 'SCRIPT_EVALUATION_FAILED', message: 'x', atMs: 1, canRetry: false, scopeId: 's' };
  const win = await monitor(instance('held-group', { repetitionGroups: [group], incidents: [incident],
    repetitionOccurrences: [row(0, 'active'), row(1, 'pending'), row(2, 'pending')] }));
  const rows = [...win.querySelectorAll('[data-repetition-occurrence-id]')].map((node) => node.textContent);
  assert.match(rows[0], new RegExp(`Occurrence 1 of 3[\\s\\S]*${I18n.t('bpmn.repeat_occurrence_failed')}`));
  assert.doesNotMatch(rows[0], new RegExp(I18n.t('bpmn.repeat_occurrence_status_active')));
  assert.match(rows[1], new RegExp(I18n.t('bpmn.repeat_occurrence_not_started')));
  win.remove();
});

test('a waiting person is never told as failed: the incident belongs to the first active occurrence without a job or a task', async () => {
  const group = { groupId: 'g1', nodeId: 'Script_1', nodeName: 'Compute each part', scopeId: 's', parentTokenId: 'p', mode: 'multi_instance_parallel',
    status: 'incident', total: 4, createdCount: 4, completed: 0, maxIterations: null, revision: 1, createdAtMs: 1, updatedAtMs: 1 };
  const row = (ordinal, status, patch = {}) => ({ occurrenceId: `o${ordinal}`, groupId: 'g1', ordinal, status, tokenId: `t${ordinal}`, userTaskId: null, jobId: null,
    verificationUserTaskId: null, acceptedSourceEventId: null, approvalEventId: null, createdAtMs: 1, updatedAtMs: 1, ...patch });
  const incident = { incidentId: 'i1', nodeId: 'Script_1', nodeName: 'Compute each part', jobId: null, code: 'SCRIPT_EVALUATION_FAILED', message: 'x', atMs: 1, canRetry: false, scopeId: 's' };
  const other = { ...incident, incidentId: 'i2', nodeId: 'Other_1', scopeId: 's' };
  const win = await monitor(instance('held-waiting', { repetitionGroups: [group], incidents: [incident, other],
    repetitionOccurrences: [row(0, 'active', { userTaskId: 'task-1' }), row(1, 'active', { jobId: 'job-1' }), row(2, 'active'), row(3, 'active')] }));
  const rows = [...win.querySelectorAll('[data-repetition-occurrence-id]')].map((node) => node.textContent);
  const failed = new RegExp(I18n.t('bpmn.repeat_occurrence_failed'));
  assert.doesNotMatch(rows[0], failed, 'a person is still working on it');
  assert.doesNotMatch(rows[1], failed, 'a job is running');
  assert.match(rows[2], failed);
  assert.doesNotMatch(rows[3], failed, 'one incident explains one occurrence');
  assert.match(rows[3], new RegExp(I18n.t('bpmn.repeat_occurrence_status_active')));
  win.remove();
});

test('a repeating activity refuses a boundary event exactly as the engine does', () => {
  const kinds = { bpmn_user_task: 'User', bpmn_service_task: 'Service', bpmn_sub_process: 'SubProcess', bpmn_call_activity: 'Call',
    bpmn_script_task: 'Script', bpmn_send_task: 'Send', bpmn_manual_task: 'Manual', bpmn_receive_task: 'Receive' };
  const accepted = { BoundaryTimer: Object.keys(kinds), BoundaryMessage: Object.keys(kinds),
    BoundaryError: ['bpmn_service_task', 'bpmn_sub_process', 'bpmn_call_activity'], BoundaryEscalation: ['bpmn_service_task'] };
  for (const [kind, types] of Object.entries(accepted)) {
    for (const repeat of [undefined, { StructuredLoop: {} }]) {
      const nodes = Object.keys(kinds).map((type) => ({ id: type, type, repeat, config: { resultExpression: '{}' } }));
      const offered = processBoundaryHosts({ nodes, edges: [] }, kind).map((node) => node.id);
      assert.deepEqual(offered.sort(), [...types].sort(), `${kind} hosts, repeat=${!!repeat}`);
    }
  }
});

test('a repeating Service task with no declared variable can declare its output collection in place', async () => {
  const graph = canvas();
  graph.addNodeFromTemplate(processTemplates().find((row) => row.node_type === 'bpmn_service_task'), 250, 300);
  const node = graph.nodes.at(-1);
  const config = inspector(graph);
  config.show(node, graph.templates.get(node.type));
  await flush(2);
  change(config.root.querySelector('[data-process="repeatMode"]'), 'mi_sequential');
  await flush(2);
  const select = () => config.root.querySelector('[data-process="repeatOutput"]');
  assert.equal(select().querySelectorAll('option').length, 1, 'only the placeholder: nothing is declared yet');
  assert.match(config.root.querySelector('[data-repeat-warning]').getAttribute('message'), /declares none yet/);
  const field = config.root.querySelector('[data-repeat-new-output]');
  assert.equal(field.getAttribute('value'), 'results', 'a free name is suggested');

  field.value = '1 bad';
  click(config.root.querySelector('[data-repeat-declare-output]'));
  assert.ok(field.getAttribute('error'), 'a name the engine would refuse is refused here');
  assert.deepEqual(graph.processModel.variables, {});

  node.config.outputMapping = { results: 'outputs.result' };
  field.value = 'results';
  click(config.root.querySelector('[data-repeat-declare-output]'));
  assert.match(field.getAttribute('error'), /output mapping/, 'a name the task itself writes is refused');
  assert.equal(node.repeat.MultiInstance.outputCollectionVariable, '');

  field.value = ' collected ';
  click(config.root.querySelector('[data-repeat-declare-output]'));
  await flush(2);
  assert.deepEqual(graph.processModel.variables, { collected: [] });
  assert.equal(node.repeat.MultiInstance.outputCollectionVariable, 'collected');
  assert.equal(select().value, 'collected');
  assert.equal(config.root.querySelector('[data-repeat-warning]'), null);
  assert.equal(config.root.querySelector('[data-repeat-new-output]'), null);
  assert.deepEqual(processRepeatOutputGaps(graph.getData()), []);
  graph.destroy();
});
