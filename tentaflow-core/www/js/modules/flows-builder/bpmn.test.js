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
const { openProcessInstance, openProcessInstances, openProcessRun, processEventText } = await import('./process-monitor.js');
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

test('palette offers only the six supported elements and cancels drag/filter work when disposed', async () => {
  const root = document.createElement('aside'); document.body.append(root); let added = 0;
  const palette = new FlowPalette(root, { mode: 'bpmn', onAdd: () => { added += 1; } }); await palette.init();
  assert.equal(root.querySelectorAll('[data-node-type]').length, 6);
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
    assert.equal(processTemplates().length, 6);
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
