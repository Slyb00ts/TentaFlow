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
const { emptyProcessModel, processToCanvas, canvasToProcess, processCommand, processJson, checkProcessDocument, PROCESS_DOCUMENT_BYTES } = await import('./bpmn.js');
const { FlowCanvas } = await import('./canvas.js');
const { FlowConfig } = await import('./config.js');
const { FlowPalette } = await import('./palette.js');
const { processTemplates } = await import('./bpmn.js');
const { openProcessInstance, openProcessInstances, openProcessRun, openProcessSchedule, processEventText, processTimerReasonText, processTimerText } = await import('./process-monitor.js');
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
  return { instanceId: id, definitionId: 'definition-one', definitionName: 'Document approval', initiatorUserId: 'owner', version: 2, revision: 11,
    status: 'Waiting', variables: { Purchase_ID: 'PO-7' }, activeNodeIds: ['Review'], userTasks: [], incidents: [], createdAtMs: 1000, updatedAtMs: 2000, canCancel: false, canRetry: false, ...overrides };
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
  change(config.root.querySelector('[data-process="flowId"]'), 'flow-one');
  change(config.root.querySelector('[data-process="inputMapping"]'), { Request_ID: 'vars.Source_ID' });
  change(config.root.querySelector('[data-process="verification"]'), 'Condition');
  change(config.root.querySelector('[data-process="expression"]'), 'outputs.Result_OK == true');
  change(config.root.querySelector('[data-process="timeoutSeconds"]'), '80');
  assert.deepEqual(node.config, { flowId: 'flow-one', inputMapping: { Request_ID: 'vars.Source_ID' }, outputMapping: {}, verification: { Condition: { expression: 'outputs.Result_OK == true' } }, timeoutSeconds: 80 });
  const readonly = inspector(graph, true); readonly.show(node, graph.templates.get(node.type));
  assert.ok(readonly.root.querySelector('[data-process="flowId"]').hasAttribute('disabled'));
  assert.equal(readonly.root.querySelector('[data-process="delete"]'), null);
  graph.destroy(); config.destroy(); readonly.destroy();
});

test('palette offers only the eight supported elements and cancels drag/filter work when disposed', async () => {
  const root = document.createElement('aside'); document.body.append(root); let added = 0;
  const palette = new FlowPalette(root, { mode: 'bpmn', onAdd: () => { added += 1; } }); await palette.init();
  assert.equal(root.querySelectorAll('[data-node-type]').length, 8);
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
  const events = ['instance_started', 'node_completed', 'end_reached', 'instance_completed', 'user_task_opened', 'exclusive_selected', 'parallel_split', 'parallel_joined', 'service_queued', 'service_claimed', 'service_result', 'verification_passed', 'user_task_completed', 'verification_approved', 'verification_rejected', 'incident', 'cancelled', 'job_retried', 'job_interrupted', 'job_denied', 'job_failed'];
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    assert.equal(processTemplates().length, 8);
    for (const template of processTemplates()) assert.doesNotMatch(template.label, /^bpmn\./);
    for (const kind of events) {
      const output = processEventText({ kind, nodeName: '<Contract>', data: { summary: 'Actual result', code: 'SOURCE_ACCESS_REVOKED', message: 'Access revoked', job_id: 'raw-job-uuid', user_task_id: 'raw-task-uuid' } });
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
  assert.deepEqual([...catchConfig.root.querySelectorAll('[data-process="timerType"] option')].map((option) => option.value), ['Date', 'Duration']);
  assert.equal(catchConfig.root.querySelector('[data-process="timerTotal"]'), null);
  graph.destroy(); config.destroy(); readonly.destroy(); catchGraph.destroy(); catchConfig.destroy();
});

test('process timezone requires an explicit value and actual undo/redo preserves it through save', async () => {
  const model = timedModel(); delete model.timerTimezone;
  const state = await mount(definition('timezone', { model }), { processDefinitionSaveRequest: (payload) => ({ definition: definition('timezone', { model: payload.model, draftRevision: 5 }) }) });
  const field = state.root.querySelector('[data-role="timer-timezone"]');
  assert.equal(field.hidden, false); assert.equal(field.value, '');
  assert.equal(Object.hasOwn(state.canvas.getData(), 'timerTimezone'), false, 'no UTC default is fabricated');
  change(field, 'Europe/Warsaw');
  assert.equal(state.canvas.getData().timerTimezone, 'Europe/Warsaw');
  state.canvas.processModel.variables.Current_Key = 'preserved';
  state.canvas.undo(); assert.equal(field.value, '');
  assert.equal(state.canvas.getData().variables.Current_Key, 'preserved', 'timezone undo does not revert independently edited variables');
  state.canvas.redo(); assert.equal(field.value, 'Europe/Warsaw');
  await builder._save();
  assert.equal(calls.find((call) => call.kind === 'processDefinitionSaveRequest').payload.model.timerTimezone, 'Europe/Warsaw');
  state.canvas.updateProcessTimezone('');
  assert.equal(Object.hasOwn(state.canvas.getData(), 'timerTimezone'), false);
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
    offsetLeft: { value: node.width / 2 }, offsetTop: { value: node.height + 10 },
    offsetWidth: { value: 200 }, offsetHeight: { value: 400 },
  });
  graph.root.getBoundingClientRect = () => ({ left: 0, top: 0, width: 500, height: 400 });
  const original = graph.getData();
  const bounds = graph._contentBounds();
  assert.equal(bounds.minX, node.x + node.width / 2 - 100);
  assert.equal(bounds.maxY, node.y + node.height + 410);
  graph.fitToContent();
  assert.ok(graph.view.y + bounds.maxY * graph.view.zoom <= 400);
  assert.ok(graph.view.y + bounds.minY * graph.view.zoom >= 0);
  assert.deepEqual(graph.getData(), original);
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
        assert.equal(processTimerReasonText(reason), translated);
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
      assert.equal(processTimerReasonText(arbitrary), arbitrary);
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
