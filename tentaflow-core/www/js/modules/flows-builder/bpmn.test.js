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
const { TfWindow } = await import('../../components/tf-window.js');
const { emptyProcessModel, processToCanvas, canvasToProcess, processBoundaryKind, processCommand, processJson, processStatusLabel, checkProcessDocument, PROCESS_DOCUMENT_BYTES } = await import('./bpmn.js');
const { FlowCanvas } = await import('./canvas.js');
const { FlowConfig } = await import('./config.js');
const { FlowPalette } = await import('./palette.js');
const { processTemplates } = await import('./bpmn.js');
const { openProcessCalendar, openProcessInstance, openProcessInstances, openProcessMessageDetail, openProcessMessageSend, openProcessRun, openProcessSchedule, processEventText, processLifecycleReasonText, processTimerText } = await import('./process-monitor.js');
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
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
const options = { assignees: [{ userId: 'anna', displayName: 'Anna Kowalska' }, { userId: 'lee', displayName: 'Lee Chen' }], serviceFlows: [{ flowId: 'flow-one', name: 'Contract checker' }] };
function definition(id = 'definition-one', overrides = {}) {
  return { definitionId: id, name: `Process ${id}`, description: '', ownerUserId: 'owner', draftRevision: 4, model: emptyProcessModel(), publishedVersion: null, archived: false, ...overrides };
}
function instance(id = 'instance-one', overrides = {}) {
  const value = { instanceId: id, definitionId: 'definition-one', definitionName: 'Document approval', initiatorUserId: 'owner', version: 2, revision: 11,
    status: 'Waiting', variables: { Purchase_ID: 'PO-7' }, activeNodeIds: ['Review'], userTasks: [], incidents: [], timers: [],
    subscriptions: [], eventRaces: [], outgoingMessages: [], messageNames: [], scopes: [], calls: [], canSendMessage: false,
    createdAtMs: 1000, updatedAtMs: 2000, canCancel: false, canRetry: false, ...overrides };
  value.pages ??= Object.fromEntries(['userTasks', 'incidents', 'timers', 'subscriptions', 'eventRaces', 'outgoingMessages', 'scopes', 'calls'].map((name) =>
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
  fixtures({ processDefinitionGetRequest: { definition: current }, processOptionsRequest: options, ...additions });
  document.body.innerHTML = `<main>${builder.render({ mode: 'bpmn' })}</main>`;
  await builder.mount({ flowId: current.definitionId, mode: 'bpmn' });
  return builder._state;
}
function canvas(model = emptyProcessModel()) {
  const root = document.createElement('div'); document.body.append(root);
  const graph = new FlowCanvas(root, { mode: 'bpmn' });
  graph.setTemplates(processTemplates()); graph.setData(model);
  return graph;
}
function inspector(graph, readOnly = false) {
  const root = document.createElement('aside'); document.body.append(root);
  return new FlowConfig(root, { mode: 'bpmn', readOnly, processOptions: options, getCanvas: () => graph,
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

test('undo and redo clear selections and restore the active scope inspector and breadcrumb', async () => {
  const current = definition('undo-root-definition', { model: embeddedModel() });
  const state = await mount(current);
  state.canvas.selectNode('Scope_Review');
  click(state.config.root.querySelector('[data-process-enter]'));
  assert.deepEqual(state.canvas.processPath, ['Scope_Review']);
  const template = processTemplates().find((item) => item.node_type === 'bpmn_inclusive_gateway');
  const added = state.canvas.addNodeFromTemplate(template, 300, 200);
  assert.deepEqual([...state.canvas.selectedIds], [added.id]);
  assert.equal(state.config.node?.id, added.id);
  assert.ok(state.config.root.querySelector('[data-process="name"]'));

  state.canvas.undo();
  assert.deepEqual(state.canvas.processPath, []);
  assert.deepEqual(state.canvas.getData(), current.model);
  assert.deepEqual([...state.canvas.selectedIds], []);
  assert.equal(state.canvas.selectedEdgeId, null);
  assert.ok(state.config.root.querySelector('.fb-config-empty'));
  assert.equal(state.config.node, null);
  assert.equal(state.config.root.querySelector('[data-process="name"]'), null);
  assert.equal(state.root.querySelector('[data-role="scope-path"]').hidden, true);
  assert.equal(state.root.querySelector('[data-role="crumb-name"]').textContent, current.name);
  assert.deepEqual(state.canvas.nodes.map((node) => node.id), ['Start', 'Scope_Review', 'End']);

  state.canvas.selectNode('Scope_Review');
  assert.deepEqual([...state.canvas.selectedIds], ['Scope_Review']);
  assert.equal(state.config.node?.id, 'Scope_Review');
  state.canvas.redo();
  assert.deepEqual(state.canvas.processPath, ['Scope_Review']);
  assert.ok(state.canvas.nodes.some((node) => node.id === added.id));
  assert.equal(state.canvas.nodes.some((node) => node.id === 'Scope_Review'), false);
  assert.deepEqual([...state.canvas.selectedIds], []);
  assert.equal(state.canvas.selectedEdgeId, null);
  assert.equal(state.config.node, null);
  assert.ok(state.config.root.querySelector('.fb-config-empty'));
  assert.equal(state.config.root.querySelector('[data-process="name"]'), null);
  assert.equal(state.root.querySelector('[data-role="scope-path"]').hidden, false);
  assert.match(state.root.querySelector('[data-role="scope-path"]').textContent, /Review department/);
  assert.equal(state.root.querySelector('[data-role="crumb-name"]').textContent, current.name);
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
  assert.equal(graph.nodesLayer.querySelector('[data-node-id="Timer_A"] .fb-port-in'), null);
  assert.equal(graph.connectNodes('Start', 'Timer_A'), false);
  assert.deepEqual(graph.getData(), model);
  graph.destroy();
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
    `${model.messages[0].name} · Message_1`);
  assert.ok(config.root.querySelector('[data-process="attachedToId"]').hasAttribute('wrap-selected'));
  assert.ok(config.root.querySelector('[data-process="outputMapping"]').hasAttribute('multiline'));
  const errorNode = graph.nodes.find((node) => node.id === 'BoundaryError_1');
  config.show(errorNode, graph.templates.get(errorNode.type));
  assert.ok(config.root.querySelector('[data-process="errorRef"]').hasAttribute('wrap-selected'));
  const throwNode = graph.nodes.find((node) => node.id === 'Throw_1');
  config.show(throwNode, graph.templates.get(throwNode.type));
  fixtures({
    processDefinitionListRequest: { definitions: [{ definitionId: 'target-definition', name: 'Target' }], hasMore: false },
    processDefinitionGetRequest: { definition: definition('target-definition', { publishedVersion: 1 }) },
    processVersionGetRequest: { version: { model } },
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
  const called = emptyProcessModel();
  called.processId = 'Called_Process';
  called.targetNamespace = 'urn:example:called';
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
  const config = inspector(graph);
  const call = graph.nodes.find((node) => node.id === 'Call_1');
  config.show(call, graph.templates.get(call.type));
  const before = structuredClone(graph.getData());
  const definitionId = '91764f75-dadb-41aa-a252-a8a911fe7a94';
  const secondDefinitionId = 'f020336c-5ae2-4b7d-b7ef-c71497b192db';
  fixtures({
    processDefinitionListRequest: { definitions: [{ definitionId, name: 'Published review' },
      { definitionId: secondDefinitionId, name: 'Second published process' }], hasMore: false },
    processVersionListRequest: ({ definitionId: selected }) => ({ versions: [{ definitionId: selected,
      version: selected === definitionId ? 7 : 3 }], hasMore: false }),
    processVersionGetRequest: ({ definitionId: selected, version }) => ({ version: { definitionId: selected,
      version, model: selected === definitionId ? called : second } }),
  });
  click(config.root.querySelector('[data-load-calls]'));
  await flush(2);
  change(config.root.querySelector('[data-process="calledDefinitionChoice"]'), definitionId);
  assert.match(config.root.querySelector('[data-process="calledDefinitionChoice"] .tf-select-selected').textContent,
    /Published review/, 'the selected call uses the published name rather than only its UUID');
  click(config.root.querySelector('[data-load-call-versions]'));
  await flush(2);
  change(config.root.querySelector('[data-process="calledVersionChoice"]'), '7');
  await flush(2);
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
  assert.deepEqual(graph.getData().nodes[0], before.nodes[0]);
  assert.equal(graph.getData().nodes.find((node) => node.id === 'Call_1').kind.CallActivity.calledVersion, 7);
  change(config.root.querySelector('[data-process="calledVersionChoice"]'), '');
  assert.equal(call.config.calledVersion, 0, 'clearing the selected version clears the actual draft binding');
  assert.deepEqual(call.config.calledElement, { namespaceUri: '', processId: '' });
  assert.equal(config.root.querySelector('[data-preview-called]').hasAttribute('disabled'), true);
  const loader = config.root.querySelector('[data-load-call-versions]');
  assert.equal(loader.hidden, true, 'one exhausted page hides the first target loader');
  change(config.root.querySelector('[data-process="calledDefinitionChoice"]'), secondDefinitionId);
  assert.equal(loader.hidden, false, 'switching targets permits an exact version load in the same inspector');
  click(loader);
  await flush(2);
  change(config.root.querySelector('[data-process="calledVersionChoice"]'), '3');
  await flush(2);
  assert.equal(call.config.calledDefinitionId, secondDefinitionId);
  assert.equal(call.config.calledVersion, 3);
  assert.deepEqual(call.config.calledElement, { namespaceUri: 'urn:example:second', processId: 'Second_Process' });
  assert.deepEqual(graph.getData().nodes[0], before.nodes[0], 'switching exact targets retains the unsaved caller');
  config.destroy(); graph.destroy();
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

test('palette offers the supported elements and cancels drag/filter work when disposed', async () => {
  const root = document.createElement('aside'); document.body.append(root); let added = 0;
  const palette = new FlowPalette(root, { mode: 'bpmn', onAdd: () => { added += 1; } }); await palette.init();
  assert.equal(root.querySelectorAll('[data-node-type]').length, 20);
  assert.ok(root.querySelector('[data-node-type="bpmn_boundary_escalation"]'));
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
  assert.equal(await builder._save(), true);
  const saved = calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model;
  assert.equal(saved.targetNamespace, 'urn:orders:Łódź');
  assert.deepEqual(saved.messages, [{ messageId: 'Message_Order', name }]);
  assert.deepEqual(saved.escalations, [{ escalationId: 'Escalation_Order',
    name: `${'Review'.repeat(40)}<&>`, escalationCode: 'NEEDS.HUMAN' }]);
  assert.deepEqual(saved.variables, {});
  assert.equal(state.definition.publishedVersion, 1, 'draft edits do not mutate the published version');
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
  fixtures({ processDefinitionGetRequest: ({ definitionId }) => definitionId === 'A' ? load.promise : { definition: definition('B') }, processOptionsRequest: () => optionRead.promise });
  document.body.innerHTML = builder.render({ mode: 'bpmn' }); const first = builder.mount({ flowId: 'A', mode: 'bpmn' });
  await builder.unmount(); document.body.innerHTML = builder.render({ mode: 'bpmn' });
  optionRead.resolve(options); const second = builder.mount({ flowId: 'B', mode: 'bpmn' }); await second;
  load.resolve({ definition: definition('A') }); await first;
  assert.equal(builder._state.flowId, 'B'); assert.equal(document.querySelector('[data-role="name"]').value, 'Process B');
  const delayedSave = deferred(); responder = (kind) => kind === 'processDefinitionSaveRequest' ? delayedSave.promise : kind === 'processDefinitionGetRequest' ? { definition: definition('C') } : options;
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
  await builder._archiveProcess();
  assert.equal(calls.find((row) => row.kind === 'processDefinitionArchiveRequest').payload.archived, false);
  assert.equal(state.canvas.readOnly, false); assert.equal(state.root.querySelector('[data-role="save"]').hasAttribute('disabled'), false);
});

test('published version paging and competing previews use the selected current response', async () => {
  const old = deferred(), recent = deferred();
  const state = await mount(definition(), { processVersionListRequest: { versions: [{ version: 2, publishedAtMs: 1000 }, { version: 1, publishedAtMs: 100 }], total: 2, hasMore: false }, processVersionGetRequest: ({ version }) => version === 1 ? old.promise : recent.promise });
  await builder._openProcessVersions(); const table = document.querySelector('tf-window tf-table');
  click(table.shadowRoot.querySelectorAll('tbody tf-button')[1]); click(table.shadowRoot.querySelectorAll('tbody tf-button')[0]);
  recent.resolve({ version: { version: 2, model: emptyProcessModel() } }); await flush();
  old.resolve({ version: { version: 1, model: { ...emptyProcessModel(), variables: { wrong: true } } } }); await flush();
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
    processDefinitionGetRequest: { definition: current },
    processVersionListRequest: { versions: [{ version: 1, publishedAtMs: 1000 }], total: 1, hasMore: false },
    processVersionGetRequest: { version: { version: 1, model: published } },
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

test('empty required message collections render each page for an active instance', async () => {
  const current = instance('empty-message-pages', { canSendMessage: true, messageNames: [],
    subscriptions: [], eventRaces: [], outgoingMessages: [] });
  const win = await monitor(current);
  for (const name of ['subscriptions', 'eventRaces', 'outgoingMessages']) {
    assert.ok(win.querySelector(`[data-page-controls="${name}"]`), `${name} has a real empty page`);
  }
  assert.equal(win.querySelector('[data-subscription-rows]').children.length, 0);
  assert.equal(win.querySelector('[data-race-rows]').children.length, 0);
  assert.equal(win.querySelector('[data-outgoing-rows]').children.length, 0);
  assert.equal(win.querySelector('[data-summary]').textContent.includes('undefined'), false);
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

test('instance summaries paginate without fetching full opaque business values', async () => {
  fixtures({ processInstanceListRequest: ({ offset }) => ({ instances: [instance(`page-${offset}`)], total: 60, hasMore: true }) });
  const win = await openProcessInstances(); const table = win.querySelector('tf-table'); table.dispatchEvent(new CustomEvent('page-change', { detail: { page: 3 }, bubbles: true })); await flush();
  assert.equal(calls.at(-1).payload.offset, 50); assert.equal(table.rows[0].instanceId, 'page-50'); assert.equal(calls.some((row) => row.kind === 'processInstanceGetRequest'), false);
});

test('run uses immutable selected version variables and blocks oversized input before any request', async () => {
  fixtures({ processInstanceStartRequest: () => { throw new Error('Start should remain blocked'); } });
  const form = openProcessRun(definition('published', { publishedVersion: 3, model: { ...emptyProcessModel(), variables: { Version_ID: 'pinned' } } }), [{ version: 3 }]);
  assert.match(form.querySelector('tf-code-editor').value, /Version_ID/); const editor = form.querySelector('tf-code-editor'); editor.value = JSON.stringify({ Long_Value: 'a'.repeat(256 * 1024) });
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.match(form.querySelector('[data-json-error]').getAttribute('message'), /256/); assert.equal(calls.some((row) => row.kind === 'processInstanceStartRequest'), false);
});

test('all five locales translate supported elements, current statuses and every finite event without exposing internal IDs', async () => {
  const events = ['instance_started', 'node_completed', 'end_reached', 'instance_completed', 'user_task_opened', 'exclusive_selected', 'parallel_split', 'parallel_joined', 'inclusive_split', 'inclusive_joined', 'service_queued', 'service_claimed', 'service_result', 'verification_passed', 'user_task_completed', 'verification_approved', 'verification_rejected', 'incident', 'cancelled', 'job_retried', 'job_interrupted', 'job_denied', 'job_failed'];
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    assert.equal(processTemplates().length, 20);
    for (const template of processTemplates()) assert.doesNotMatch(template.label, /^bpmn\./);
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
  await mount(draft, { processVersionGetRequest: (payload) => { assert.equal(payload.version, 4); return { version: { version: 4, model: published } }; } });
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
  assert.deepEqual(state.canvas.validate(), [I18n.t('bpmn.timer_timezone_required')]);
  assert.equal(await builder._save(), false);
  await builder._publish();
  assert.equal(calls.some((call) => ['processDefinitionSaveRequest', 'processDefinitionPublishRequest'].includes(call.kind)), false);
  state.canvas.updateProcessTimezone('   ');
  assert.deepEqual(state.canvas.validate(), [I18n.t('bpmn.timer_timezone_required')]);
  assert.equal(await builder._save(), false);
  state.canvas.updateProcessTimezone('Europe/Warsaw');
  assert.deepEqual(state.canvas.validate(), []);
  assert.equal(await builder._save(), true);
  assert.equal(calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model.timerTimezone, 'Europe/Warsaw');
  const plain = canvas();
  assert.deepEqual(plain.validate(), [], 'a process without a calendar or timer needs no timezone');
  plain.destroy();
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
      processDefinitionGetRequest: () => ({ definition: { ...current, publishedVersion: published ? 1 : null }, timerStart: published ? savedTimer() : null }),
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
    processVersionGetRequest: { version: { version: 1, model: current.model } },
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
  const shortTask = graph.nodesLayer.querySelector('[data-node-id="Review"]');
  assert.equal(shortTask.classList.contains('fb-external-label'), false);
  assert.equal(shortTask.querySelector('.fb-process-label').hidden, true);
  assert.equal(shortTask.querySelector('.fb-process-symbol > span').textContent.trim(), 'Review contract');
  graph.destroy();
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
  const state = await mount(current, { processDefinitionGetRequest: { definition: current, timerStart: savedTimer() }, processVersionGetRequest: { version: { version: 2, model: current.model } } });
  assert.ok(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'));
  await builder._runProcess();
  assert.equal(document.querySelector('.tf-act-window'), null);
  assert.match(document.querySelector('[data-timers]').textContent, /Daily approval/);
  const form = openProcessRun(current, [{ version: 2, model: current.model }]);
  assert.ok(form.querySelector('[data-act="submit"]').hasAttribute('disabled'));
  click(form.querySelector('[data-act="submit"]')); await flush();
  assert.equal(calls.some((call) => call.kind === 'processInstanceStartRequest'), false);
});

test('published message start offers the current authorized send and never invokes manual start', async () => {
  const model = emptyProcessModel();
  model.messages = [{ messageId: 'Message_1', name: 'order.received' }];
  model.nodes[0].kind = { MessageStart: { messageRef: 'Message_1', outputMapping: {} } };
  const current = definition('message-start', { model, publishedVersion: 2 });
  const messageStart = { nodeId: 'Start', nodeName: 'Start', messageName: 'order.received', version: 2, canSend: true };
  const state = await mount(current, { processDefinitionGetRequest: { definition: current, messageStart },
    processVersionGetRequest: { version: { version: 2, model } } });
  assert.equal(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'), true);
  assert.equal(state.root.querySelector('[data-role="send-start"]').hidden, false);
  click(state.root.querySelector('[data-role="send-start"]'));
  assert.ok(document.querySelector('.tf-act-window [data-message-name]'));
  await builder._runProcess();
  assert.equal(calls.some((call) => call.kind === 'processInstanceStartRequest'), false);
  assert.equal(document.querySelectorAll('.tf-act-window [data-message-name]').length, 2);
});

test('schedule reads persisted Pending, Blocked and Missed states and scopes delayed reads to its window', async () => {
  let actual = savedTimer();
  fixtures({ processDefinitionGetRequest: () => ({ definition: definition('schedule', { publishedVersion: 2 }), timerStart: actual }) });
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
  fixtures({ processDefinitionGetRequest: { definition: definition('other'), timerStart: savedTimer({ nodeName: 'Current other' }) } });
  const other = await openProcessSchedule('other');
  pending.resolve({ definition: definition('late'), timerStart: savedTimer({ nodeName: 'Late private data' }) }); await loading;
  assert.match(other.querySelector('[data-timers]').textContent, /Current other/);
  assert.doesNotMatch(other.textContent, /Late private data/);
});

test('schedule renders a long unbroken definition name and untrusted markup as text', async () => {
  const name = `${'Approval'.repeat(24)}X<img src=x onerror=alert(1)>`;
  assert.equal(name.length, 221);
  fixtures({ processDefinitionGetRequest: { definition: definition('long-schedule', { name, publishedVersion: 1 }), timerStart: savedTimer() } });
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
      const state = await mount(current, { processDefinitionGetRequest: () => ({ definition: current, timerStart: timer }) });
      const win = await openProcessSchedule(current.definitionId);
      for (const [reason, key] of reasons) {
        timer.lastReason = reason;
        state.timerStart = timer;
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
      state.timerStart = timer;
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
  const current = instance('boundary-empty-name', { timers: [{ ...savedTimer({ kind: 'Boundary', nodeName: '' }), attachedToId: 'Review' }] });
  const win = await monitor(current);
  assert.ok(win.querySelector('[data-timers]').textContent.includes(I18n.t('bpmn.node_boundary_timer')));
  assert.ok(win.querySelector('[data-timers]').textContent.includes('Review'));
  assert.equal(win.querySelector('[data-timers] script'), null);
});

test('boundary timer shows its actual escaped element ID and ordinal occurrence in five locales', async () => {
  const captions = {
    en: ['Attached activity', 'Attached activity (element ID)'],
    pl: ['Przypięta czynność', 'Przypięta czynność (ID elementu)'],
    de: ['Angehängte Aktivität', 'Angehängte Aktivität (Element-ID)'],
    es: ['Actividad adjunta', 'Actividad adjunta (ID del elemento)'],
    fr: ['Activité attachée', 'Activité attachée (ID de l’élément)'],
  };
  try {
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await I18n.setLanguage(language);
      const graph = canvas(boundaryModel());
      const boundary = graph.nodes.find((node) => node.id === 'Timer_A');
      const panel = inspector(graph);
      panel.show(boundary, graph.templates.get(boundary.type));
      assert.equal(panel.root.querySelector('[data-process="attachedToId"]').getAttribute('label'), captions[language][0]);
      const timer = savedTimer({ kind: 'Boundary', nodeName: 'Deadline', attachedToId: 'Review_1' });
      const win = await monitor(instance(`boundary-id-${language}`, { timers: [timer] }));
      const terms = [...win.querySelectorAll('[data-timers] dt')].map((term) => term.textContent);
      const values = [...win.querySelectorAll('[data-timers] dd')].map((value) => value.textContent);
      assert.deepEqual(terms, [I18n.t('bpmn.timer_occurrence'), captions[language][1]]);
      assert.deepEqual(values, [I18n.t('bpmn.timer_slot', { occurrence: 3, total: 7 }), 'Review_1']);
      assert.doesNotMatch(terms.join(' '), /bpmn\.|\{occurrence\}|\{total\}/);
      assert.notEqual(terms[0], terms[1]);
      win.remove();
      graph.destroy();
    }
    await I18n.setLanguage('en');
    const untrusted = '<img src=x onerror=alert(1)>';
    const timer = savedTimer({ kind: 'Boundary', attachedToId: untrusted, totalFirings: null });
    const win = await monitor(instance('boundary-escaped-id', { timers: [timer] }));
    const values = [...win.querySelectorAll('[data-timers] dd')].map((value) => value.textContent);
    assert.deepEqual(values, ['3', untrusted]);
    assert.equal(win.querySelector('[data-timers] img'), null);
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
  const state = await mount(actual, { processDefinitionGetRequest: { definition: actual, timerStart: { ...timer, kind: 'Start' } } });
  assert.ok(state.root.querySelector('[data-role="timer-summary"]').textContent.includes(reason));
  assert.match(state.root.querySelector('[data-role="timer-summary"]').textContent, /Current start.*4.*Timer error/);
  assert.ok(state.root.querySelector('[data-role="run"]').hasAttribute('disabled'));
});

test('current schedule access denial clears persisted private details and disables its run-list action', async () => {
  let revoked = false;
  fixtures({ processDefinitionGetRequest: () => { if (revoked) throw new Error('Current access was revoked'); return { definition: definition('private-schedule'), timerStart: savedTimer() }; } });
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
