// ============ File: flows-builder/process-simulation.test.js — simulated Service steps: waits, fixtures, verdicts, refusals ============

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import test, { after, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

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
const { emptyProcessModel, processExplainError } = await import('./bpmn.js');
const { simulationWindow, openProcessSimulation, resumeProcessSimulation, simulationErrorText } = await import('./process-simulation.js');
const { TfWindow } = await import('../../components/tf-window.js');
const { TfToast } = await import('../../components/tf-toast.js');
const { OUTPUTS_MAX_KIB, SIMULATION_EVENT_KINDS, SERVICE_OUTCOMES, simulationStructure, simulationTasksByOccurrence, simulationUnsupportedSteps, readMessageDraft, readOutputs, readServiceAnswer, readServiceFixtures, simulationServiceOutcomes, simulationServiceSteps, simulationWaitSteps } = await import('./simulation-service.js');
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();

const calls = [];
let responder = () => { throw new Error('no responder'); };
// The language switch and the release of a closed window are not under test.
const releases = [];
ApiBinary.one = async (kind, payload) => {
  if (kind === 'mePreferencesUpdateRequest') return {};
  if (kind === 'processSimulationReleaseRequest') { releases.push(payload.simulationId); return {}; }
  calls.push({ kind, payload: structuredClone(payload) });
  return responder(kind, payload);
};
ApiBinary.action = ApiBinary.one;
const deferred = () => { let resolve; const promise = new Promise((yes) => { resolve = yes; }); return { promise, resolve }; };
const flush = async (count = 5) => { for (let i = 0; i < count; i += 1) await new Promise((resolve) => setTimeout(resolve, 0)); };
const click = (element) => { assert.ok(element, 'the actual control exists'); element.dispatchEvent(new MouseEvent('click', { bubbles: true })); };
const change = (control, value) => { control.value = value; control.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value } })); };

afterEach(async () => {
  document.body.replaceChildren();
  await flush(2);
  calls.length = 0;
  releases.length = 0;
  await I18n.setLanguage('en');
});
after(async () => { await window.happyDOM.close(); });

const serviceTask = (overrides = {}) => ({ ServiceTask: { flowId: 'flow-one', inputMapping: {}, outputMapping: {}, verification: 'Human', timeoutSeconds: 60, ...overrides } });

function serviceModel(tasks) {
  const model = emptyProcessModel();
  model.nodes.push(...tasks.map(([id, name, kind, extra = {}]) => ({ id, name, kind, ...extra })));
  return model;
}

const MODEL = serviceModel([
  ['Plain', 'Plain step', serviceTask()],
  ['Expr', 'Expression step', serviceTask({ resultExpression: "{'outcome':'Completed','code':null,'summary':'','outputs':{},'evidence':[]}" })],
  ['Checked', 'Checked step', serviceTask({ verification: { Test: { flowId: 'test-flow', onFailure: 'Error' } } })],
  ['Agent', 'Agent step', serviceTask({ implementation: { Agent: { agentId: 'a-1' } } })],
  ['Unbound', 'Unbound step', serviceTask({ implementation: { Unbound: { origin: 'x' } } })],
  ['WithIo', 'Io step', serviceTask(), { activityIo: { dataOutputs: [{ id: 'o' }] } }],
  ['Script', 'Script step', { ScriptTask: { script: '1', outputMapping: {} } }],
]);

// The same steps without the ones a simulation refuses, so a run can start.
const RUNNABLE = { ...MODEL, nodes: MODEL.nodes.filter((node) => ['Plain', 'Expr', 'Checked'].includes(node.id) || !node.kind?.ServiceTask) };
const baseView = (overrides = {}) => ({
  simulationId: 'sim-1', clock: { nowMs: 5, horizonMs: 100, stepIndex: 1 }, instance: null,
  userTasks: [], timers: [], events: [], serviceWaits: [], eventsOmitted: 0, traceStepsOmitted: 0, ...overrides,
});
const wait = (overrides = {}) => ({ jobId: 'job-1', nodeId: 'Plain', scopeId: 's', tokenId: 't', input: { seed: 7 },
  mode: 'Result', verdictApplicable: false, ...overrides });

test('simulation steps skip what the engine refuses and offer NeedsHuman and Cancelled only with a result expression', () => {
  const steps = simulationServiceSteps(MODEL);
  assert.deepEqual(steps.map((step) => step.nodeId), ['Plain', 'Expr', 'Checked']);
  assert.deepEqual(steps.map((step) => step.hasResultExpression), [false, true, false]);
  assert.deepEqual(steps.map((step) => step.machineVerdict), [false, false, true]);
  assert.deepEqual(simulationServiceOutcomes(steps[0]), ['Completed', 'Error']);
  assert.deepEqual(simulationServiceOutcomes(steps[1]), SERVICE_OUTCOMES);
  const [{ step }] = simulationWaitSteps([wait({ nodeId: 'Expr', verdictApplicable: true })], MODEL);
  assert.deepEqual(step, { nodeId: 'Expr', hasResultExpression: true, machineVerdict: true });
});

test('an answer is read into the wire result, and every wrong field is named', () => {
  const step = { hasResultExpression: false, machineVerdict: false };
  const ok = readServiceAnswer(step, { outcome: 'Error', code: ' E_1 ', summary: 'boom', outputs: '{"a": {"b_c": 1}}', verdict: '' });
  assert.deepEqual(ok, { result: { outcome: 'Error', code: 'E_1', summary: 'boom', outputs: { a: { b_c: 1 } }, evidence: [] }, verdict: null });
  assert.equal(readServiceAnswer(step, { outcome: 'Completed', code: '', summary: '', outputs: '{}', verdict: '' }).result.code, null);

  const bad = readServiceAnswer(step, { outcome: 'NeedsHuman', code: 'no spaces', summary: 'x'.repeat(33 * 1024), outputs: '{nope', verdict: '' });
  assert.deepEqual(bad.errors, { outcome: 'simulation_outcome_unavailable', code: 'simulation_code_invalid',
    summary: 'simulation_summary_too_long', outputs: 'json_invalid' });
  for (const outputs of ['[]', '3', 'null']) {
    assert.equal(readServiceAnswer(step, { outcome: 'Completed', code: '', summary: '', outputs, verdict: '' }).errors.outputs, 'object_required');
  }
  assert.ok(readServiceAnswer({ hasResultExpression: true }, { outcome: 'Cancelled', code: '', summary: '', outputs: '{}', verdict: '' }).result);
});

test('a verdict is demanded for a completed result of a checked step only, and sent only where the engine reads it', () => {
  const checked = { hasResultExpression: false, machineVerdict: true };
  const draft = { code: '', summary: '', outputs: '{}' };
  assert.equal(readServiceAnswer(checked, { ...draft, outcome: 'Completed', verdict: '' }).errors.verdict, 'simulation_verdict_required');
  assert.equal(readServiceAnswer(checked, { ...draft, outcome: 'Completed', verdict: 'Approved' }).verdict, 'Approved');
  assert.equal(readServiceAnswer(checked, { ...draft, outcome: 'Error', verdict: 'Approved' }).verdict, null, 'a failed result never reaches the check');
  assert.equal(readServiceAnswer(checked, { ...draft, outcome: 'Error', verdict: '' }).errors, undefined);
  // With a result expression the outcome is decided server-side, so a verdict is optional and sent when chosen.
  const expression = { hasResultExpression: true, machineVerdict: true };
  assert.equal(readServiceAnswer(expression, { ...draft, outcome: 'Completed', verdict: '' }).verdict, null);
  assert.equal(readServiceAnswer(expression, { ...draft, outcome: 'Error', verdict: 'Rejected' }).verdict, 'Rejected');
  const plain = { hasResultExpression: false, machineVerdict: false };
  assert.equal(readServiceAnswer(plain, { ...draft, outcome: 'Completed', verdict: 'Approved' }).verdict, null);
});

test('only steps set to a prepared result produce fixtures, and a bad one is reported per step', () => {
  const steps = simulationServiceSteps(MODEL);
  const good = { outcome: 'Completed', code: '', summary: 's', outputs: '{"k": 1}', verdict: '' };
  const choices = new Map([
    ['Plain', { mode: 'Fixture', draft: good }],
    ['Expr', { mode: 'Result', draft: { ...good, outputs: '{bad' } }],
    ['Checked', { mode: 'Fixture', draft: { ...good, verdict: 'Approved' } }],
  ]);
  const { fixtures, errors } = readServiceFixtures(steps, choices);
  assert.deepEqual(fixtures.map((fixture) => [fixture.nodeId, fixture.mode, fixture.verdict ?? null]), [['Plain', 'Fixture', null], ['Checked', 'Fixture', 'Approved']]);
  assert.equal(errors.size, 0);
  choices.set('Checked', { mode: 'Fixture', draft: { ...good, outputs: '{bad' } });
  const refused = readServiceFixtures(steps, choices);
  assert.deepEqual([...refused.errors.keys()], ['Checked']);
  assert.equal(refused.fixtures.length, 1);
});

test('a waiting step shows its name and input, offers the outcomes its model allows and a verdict only when it applies', async () => {
  const win = simulationWindow(baseView({ serviceWaits: [
    wait(),
    wait({ jobId: 'job-2', nodeId: 'Checked', verdictApplicable: true, input: { other: true } }),
    wait({ jobId: 'job-3', nodeId: 'Expr' }),
    wait({ jobId: 'job-4', nodeId: 'Plain', mode: 'Fixture' }),
  ] }), MODEL, { fixtures: new Map([['Plain', { nodeId: 'Plain', mode: 'Fixture',
    result: { outcome: 'Completed', code: null, summary: 'prepared summary', outputs: { ready: 1 }, evidence: [] } }]]) });
  await flush(2);
  const articles = [...win.querySelectorAll('[data-simulation-wait]')];
  assert.equal(win.querySelector('[data-simulation-waits] h3').textContent, 'Waiting for a result: 4 steps');
  assert.deepEqual(articles.map((article) => article.querySelector('strong').textContent), ['Plain step', 'Checked step', 'Expression step', 'Plain step']);
  assert.deepEqual(JSON.parse(articles[0].querySelector('[data-simulation-wait-input]').value), { seed: 7 });
  const outcomes = (article) => [...article.querySelectorAll('[data-answer-field="outcome"] option')].map((option) => option.value);
  assert.deepEqual(outcomes(articles[0]), ['Completed', 'Error']);
  assert.deepEqual(outcomes(articles[2]), ['Completed', 'Error', 'NeedsHuman', 'Cancelled']);
  assert.equal(articles[0].querySelector('[data-answer-field="verdict"]'), null);
  assert.ok(articles[1].querySelector('[data-answer-field="verdict"]'));
  assert.equal(articles[2].querySelector('[data-answer-field="outcome"]').getAttribute('hint'), I18n.t('bpmn.simulation_outcome_expression_hint'));
  assert.equal(articles[0].querySelector('[data-answer-field="outcome"]').hasAttribute('hint'), false);
  // A prepared step shows its values read-only: no form, no button to send anything.
  assert.match(articles[3].querySelector('tf-chip').textContent, /prepared result applies/);
  assert.equal(articles[3].querySelector('[data-simulation-resolve]'), null);
  assert.equal(articles[3].querySelector('[data-answer-field]'), null);
  assert.match(articles[3].querySelector('[data-simulation-prepared]').textContent, /prepared summary/);
  assert.deepEqual(JSON.parse(articles[3].querySelector('[data-simulation-prepared-outputs]').value), { ready: 1 });
  assert.doesNotMatch(win.textContent, /job-\d/, 'jobs are never cited by id');
  const none = simulationWindow(baseView(), MODEL);
  assert.equal(none.querySelector('[data-simulation-waits]'), null);
});

test('resolving a wait sends the typed result with its job and the trace advances from the answer', async () => {
  const advanced = baseView({ clock: { nowMs: 6, horizonMs: 100, stepIndex: 2 }, serviceWaits: [],
    events: [{ atMs: 6, kind: 'service_result_accepted', nodeId: 'Checked' }] });
  responder = (kind) => {
    if (kind === 'processSimulationServiceResolveRequest') return { view: advanced };
    throw new Error(`unexpected ${kind}`);
  };
  const win = simulationWindow(baseView({ serviceWaits: [wait({ jobId: 'job-9', nodeId: 'Checked', verdictApplicable: true })] }), MODEL);
  await flush(2);
  const article = win.querySelector('[data-simulation-wait="job-9"]');
  change(article.querySelector('[data-answer-field="summary"]'), 'looks fine');
  article.querySelector('[data-answer-field="outputs"]').value = '{"answer": 42}';
  click(article.querySelector('[data-simulation-resolve]'));
  await flush(2);
  assert.equal(calls.length, 0, 'a completed result of a checked step is not sent without a verdict');
  assert.ok(article.querySelector('[data-answer-error="verdict"]:not([hidden])'));
  change(article.querySelector('[data-answer-field="verdict"]'), 'Approved');
  click(article.querySelector('[data-simulation-resolve]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].kind, 'processSimulationServiceResolveRequest');
  assert.deepEqual(calls[0].payload, { simulationId: 'sim-1', nodeId: 'Checked', jobId: 'job-9', verdict: 'Approved',
    result: { outcome: 'Completed', code: null, summary: 'looks fine', outputs: { answer: 42 }, evidence: [] } });
  assert.equal(win.querySelector('[data-simulation-wait]'), null, 'the answered wait is gone');
  assert.match(win.querySelector('.fb-simulation-events').textContent, /service_result_accepted · Checked step/);
});

test('an output that is not a JSON object stays in the form with an inline error and sends nothing', async () => {
  const win = simulationWindow(baseView({ serviceWaits: [wait()] }), MODEL);
  await flush(2);
  const article = win.querySelector('[data-simulation-wait]');
  article.querySelector('[data-answer-field="outputs"]').value = '{"half":';
  click(article.querySelector('[data-simulation-resolve]'));
  await flush(3);
  assert.equal(calls.length, 0);
  const error = article.querySelector('[data-answer-error="outputs"]');
  assert.equal(error.hidden, false);
  assert.equal(error.getAttribute('message'), I18n.t('bpmn.json_invalid'));
  article.querySelector('[data-answer-field="outputs"]').value = '[1]';
  click(article.querySelector('[data-simulation-resolve]'));
  assert.equal(error.getAttribute('message'), I18n.t('bpmn.object_required'));
  assert.equal(calls.length, 0);
});

test('a refusal from the engine is told in the author\'s words and the typed answer survives the redraw', async () => {
  responder = () => { throw new Error('SIMULATION_ANSWER_INVALID: the answer for Plain is invalid: service business error code is invalid'); };
  const win = simulationWindow(baseView({ serviceWaits: [wait()] }), MODEL);
  await flush(2);
  const article = win.querySelector('[data-simulation-wait]');
  change(article.querySelector('[data-answer-field="summary"]'), 'kept text');
  article.querySelector('[data-answer-field="outputs"]').value = '{"keep": true}';
  click(article.querySelector('[data-simulation-resolve]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.equal(win.querySelector('[data-error]').getAttribute('message'), I18n.t('bpmn.simulation_error_answer_invalid', { node: 'Plain step' }));
  const again = win.querySelector('[data-simulation-wait]');
  assert.equal(again.querySelector('[data-answer-field="summary"]').value, 'kept text');
  assert.equal(again.querySelector('[data-answer-field="outputs"]').value, '{"keep": true}');
});

test('a verification task is decided with approved, a work task never sends it', async () => {
  responder = () => ({ view: baseView() });
  const tasks = [
    { userTaskId: 'ut-v', nodeId: 'Plain', name: 'Check it', kind: 'Verification', status: 'Open', instructions: '', canComplete: true },
    { userTaskId: 'ut-w', nodeId: 'Plain', name: 'Do it', kind: 'Work', status: 'Open', instructions: '', canComplete: true },
  ];
  const win = simulationWindow(baseView({ userTasks: tasks }), MODEL);
  await flush(2);
  const verification = win.querySelector('[data-simulation-task="ut-v"]').closest('article');
  assert.equal(verification.querySelector('tf-code-editor'), null, 'a decision has no outputs to type');
  assert.deepEqual([...verification.querySelectorAll('tf-button')].map((button) => button.textContent.trim()), ['Approve', 'Reject']);
  click(verification.querySelector('[data-simulation-decide="false"]'));
  await flush(5);
  assert.deepEqual(calls[0].payload, { simulationId: 'sim-1', userTaskId: 'ut-v', outputs: {}, approved: false });
  calls.length = 0;
  win.remove();

  const second = simulationWindow(baseView({ userTasks: tasks }), MODEL);
  await flush(2);
  click(second.querySelector('[data-simulation-decide="true"]'));
  await flush(5);
  assert.equal(calls[0].payload.approved, true);
  calls.length = 0;
  const third = simulationWindow(baseView({ userTasks: [tasks[1]] }), MODEL);
  await flush(2);
  click(third.querySelector('[data-simulation-task="ut-w"]'));
  await flush(5);
  assert.equal(calls[0].kind, 'processSimulationUserTaskCompleteRequest');
  assert.equal(Object.hasOwn(calls[0].payload, 'approved'), false);
});

const catalogFor = (model) => [{ processId: model.processId, processName: null, startNodeId: model.nodes.find((node) => node.kind === 'Start').id,
  startNodeName: null, version: 3, trigger: { Start: { canStart: true } } }];
const definitionFor = (model) => ({ definitionId: 'def-1', name: 'Sim process', description: '', model, publishedVersion: 3, archived: false });

async function startWindow(model) {
  const catalog = catalogFor(model);
  const form = openProcessSimulation(definitionFor(model), { version: 3 }, catalog, catalog[0]);
  await flush(3);
  return form;
}

test('the start window offers each runnable Service step a result source and a disabled real execution', async () => {
  const form = await startWindow(MODEL);
  const rows = [...form.querySelectorAll('[data-fixture-step]')];
  assert.deepEqual(rows.map((row) => row.dataset.fixtureStep), ['Plain', 'Expr', 'Checked']);
  assert.deepEqual(rows.map((row) => row.querySelector('strong').textContent), ['Plain step', 'Expression step', 'Checked step']);
  const options = [...rows[0].querySelectorAll('[data-fixture-mode] option')];
  assert.deepEqual(options.map((option) => [option.value, option.hasAttribute('disabled')]), [['Result', false], ['Fixture', false], ['RealExecution', true]]);
  assert.match(rows[0].querySelector('[data-fixture-mode]').getAttribute('hint'), /not available yet/);
  assert.equal(rows[0].querySelector('[data-fixture-form]').hidden, true);
  change(rows[0].querySelector('[data-fixture-mode]'), 'Fixture');
  assert.equal(rows[0].querySelector('[data-fixture-form]').hidden, false);
  const noServices = await startWindow(emptyProcessModel());
  assert.equal(noServices.querySelector('.fb-simulation-fixtures'), null);
});

test('start sends no fixture for a step left to the operator and the prepared result for a step set to one', async () => {
  responder = () => ({ view: baseView() });
  const form = await startWindow(RUNNABLE);
  click(form.querySelector('[data-act="submit"]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].payload.serviceFixtures, []);
  form.remove();
  calls.length = 0;

  const prepared = await startWindow(RUNNABLE);
  const checked = prepared.querySelector('[data-fixture-step="Checked"]');
  change(checked.querySelector('[data-fixture-mode]'), 'Fixture');
  change(checked.querySelector('[data-answer-field="summary"]'), 'prepared');
  checked.querySelector('[data-answer-field="outputs"]').value = '{"ok": true}';
  change(checked.querySelector('[data-answer-field="verdict"]'), 'Approved');
  click(prepared.querySelector('[data-act="submit"]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].payload.serviceFixtures, [{ nodeId: 'Checked', mode: 'Fixture', verdict: 'Approved',
    result: { outcome: 'Completed', code: null, summary: 'prepared', outputs: { ok: true }, evidence: [] } }]);
});

test('a prepared result with invalid JSON blocks the start and marks the field', async () => {
  const form = await startWindow(RUNNABLE);
  const plain = form.querySelector('[data-fixture-step="Plain"]');
  change(plain.querySelector('[data-fixture-mode]'), 'Fixture');
  plain.querySelector('[data-answer-field="outputs"]').value = '{"broken":';
  click(form.querySelector('[data-act="submit"]'));
  await flush(5);
  assert.equal(calls.length, 0);
  const error = plain.querySelector('[data-answer-error="outputs"]');
  assert.equal(error.hidden, false);
  assert.equal(error.getAttribute('message'), I18n.t('bpmn.json_invalid'));
});

test('every refusal of a simulated Service step is told with the step\'s name, in all five languages', async () => {
  const model = serviceModel([['Svc_1', 'Charge card', serviceTask()]]);
  const codes = {
    SIMULATION_REAL_EXECUTION_UNSUPPORTED: 'Svc_1 cannot run for real: a simulation keeps every write in its private database',
    SIMULATION_SIDE_EFFECTS_NOT_CONFIRMED: 'running Svc_1 for real has side effects and costs and needs an explicit confirmation',
    SIMULATION_FIXTURE_INVALID: 'Svc_1 is a fixture without a result',
    SIMULATION_SERVICE_NOT_WAITING: 'Svc_1 is not waiting for a result',
    SIMULATION_SERVICE_AMBIGUOUS: '2 jobs wait on Svc_1; name one by job_id',
    SIMULATION_VERDICT_REQUIRED: 'Svc_1 has a machine verification and needs a verdict for a completed result',
    SIMULATION_ANSWER_INVALID: 'the answer for Svc_1 is invalid: boom',
    SIMULATION_OUTCOME_NOT_PRODUCIBLE: 'Svc_1 has no result expression, so it can only complete or fail',
  };
  const spoken = () => Object.entries(codes).map(([code, detail]) => processExplainError(`protocol error ProcessBody: ${code}: ${detail}`, model));
  const english = spoken();
  assert.ok(english.every((line) => line.includes('“Charge card”') && !line.includes('SIMULATION_') && !line.includes('Svc_1')), english.join('\n'));
  assert.equal(new Set(english).size, 8);
  const unnamed = processExplainError('SIMULATION_FIXTURE_INVALID: a fixture names a node id that is not a valid id', model);
  assert.match(unnamed, /this step/);
  for (const language of ['pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    const lines = spoken();
    assert.ok(lines.every((line) => line.includes('Charge card') && !line.includes('bpmn.')), `${language}: ${lines.join('\n')}`);
    assert.equal(new Set(lines).size, 8, language);
  }
  await I18n.setLanguage('en');
});

test('agent steps and service steps with activity IO are refused in the author\'s words', () => {
  const model = serviceModel([['Svc_1', 'Charge card', serviceTask()], ['Svc_2', 'Ask agent', serviceTask()]]);
  assert.equal(processExplainError('a Service task with activity IO is unsupported in simulation (Svc_1): its result continues through a deferred external step', model),
    '“Charge card” has data inputs or outputs, which a simulation cannot run yet.');
  assert.equal(processExplainError('agent steps need the report sink; not simulated yet (Svc_2)', model),
    '“Ask agent” is run by an agent, which a simulation cannot run yet.');
  assert.match(processExplainError('UNBOUND_IMPLEMENTATION: service task Svc_1 has no implementation yet (imported from Camunda)', model), /Charge card/);
});

const gone = (win) => !win.isConnected || win._closing === true;
const taskOf = (overrides = {}) => ({ userTaskId: 'ut-1', nodeId: 'Plain', name: 'Check it', kind: 'Verification', status: 'Open', instructions: '', canComplete: true, ...overrides });
const event = (kind, nodeId = null, data = null, atMs = 1000) => ({ atMs, kind, nodeId, data });

test('only an open task has buttons; a finished one is read-only with the decision the events carry', async () => {
  const win = simulationWindow(baseView({
    userTasks: [taskOf({ userTaskId: 'ut-done', status: 'Completed', name: 'Decided' }), taskOf({ userTaskId: 'ut-no', status: 'Completed', name: 'Refused' }),
      taskOf({ userTaskId: 'ut-open', name: 'Pending' }), taskOf({ userTaskId: 'ut-cancel', status: 'Cancelled', kind: 'Work', name: 'Dropped' })],
    events: [event('verification_approved', 'Plain', { user_task_id: 'ut-done' }), event('verification_rejected', 'Plain', { user_task_id: 'ut-no' })],
  }), MODEL);
  await flush(2);
  assert.deepEqual([...win.querySelectorAll('[data-simulation-open-task]')].map((row) => row.dataset.simulationOpenTask), ['ut-open']);
  assert.equal(win.querySelectorAll('[data-simulation-task]').length, 2, 'only the open verification has its two buttons');
  const done = (id) => win.querySelector(`[data-simulation-done-task="${id}"]`);
  assert.equal(done('ut-done').querySelector('tf-button'), null);
  assert.equal(done('ut-done').querySelector('[data-simulation-decision]').dataset.simulationDecision, 'approved');
  assert.equal(done('ut-no').querySelector('[data-simulation-decision]').dataset.simulationDecision, 'rejected');
  assert.equal(done('ut-cancel').querySelector('[data-simulation-decision]'), null);
  assert.match(done('ut-done').textContent, /Completed/);
  assert.match(done('ut-cancel').textContent, /Cancelled/);
  assert.equal(win.querySelectorAll('tf-button[data-simulation-decide]').length, 2);
});

test('a decision cannot be taken twice: the buttons go busy, and a task that finished meanwhile sends nothing', async () => {
  const gate = deferred();
  responder = () => gate.promise;
  const win = simulationWindow(baseView({ userTasks: [taskOf()] }), MODEL);
  await flush(2);
  const approve = win.querySelector('[data-simulation-decide="true"]');
  click(approve);
  click(win.querySelector('[data-simulation-decide="true"]'));
  click(win.querySelector('[data-simulation-decide="false"]'));
  await flush(2);
  assert.equal(calls.length, 1, 'the second and third press are dropped while the first is in flight');
  gate.resolve({ view: baseView({ userTasks: [taskOf({ status: 'Completed' })], events: [event('verification_approved', 'Plain', { user_task_id: 'ut-1' })] }) });
  await flush(5);
  assert.equal(win.querySelector('[data-simulation-decide]'), null);
  assert.equal(win.querySelector('[data-simulation-decision]').dataset.simulationDecision, 'approved');
});

test('incidents are shown in words with their cause, and a finished run cannot be advanced', async () => {
  const incident = (code, message = '') => ({ incidentId: code, nodeId: 'Plain', nodeName: '', jobId: null, code, message, atMs: 1, canRetry: false, scopeId: 's' });
  const view = (incidents, status = 'Incident') => baseView({ instance: { status }, incidents });
  const text = async (incidents) => {
    const win = simulationWindow(view(incidents), MODEL);
    await flush(2);
    const lines = [...win.querySelectorAll('[data-simulation-incident]')].map((alert) => alert.getAttribute('message'));
    assert.equal(win.querySelector('[data-simulation-advance]').hasAttribute('disabled'), true, 'an incident ends the run');
    win.remove();
    return lines;
  };
  assert.deepEqual(await text([incident('HUMAN_REJECTED', 'the activity result was rejected by its verifier')]), ['Plain step: The verifier rejected the result. (HUMAN_REJECTED)']);
  assert.deepEqual(await text([incident('UMOWA_ZLA')]), ['Plain step: The step reported the error code UMOWA_ZLA. (UMOWA_ZLA)']);
  assert.deepEqual(await text([incident('SERVICE_ERROR')]), ['Plain step: The service step failed. (SERVICE_ERROR)']);
  const [mapping] = await text([incident('TRANSITION_ERROR')]);
  assert.match(mapping, /output mapping needs a value the result did not provide/);
  assert.match((await text([incident('RESULT_EXPRESSION_ERROR')]))[0], /result expression of the step could not be evaluated/);
  const win = simulationWindow(view([incident('VERIFICATION_FAILED', 'The simulation operator chose this verdict')]), MODEL);
  await flush(2);
  assert.doesNotMatch(win.querySelector('[data-simulation-incident]').getAttribute('message'), /simulation operator chose/, 'the headline is the platform\'s words, not the engine\'s');
  assert.match(win.querySelector('[data-simulation-incidents] tf-disclosure').textContent, /simulation operator chose/, 'the engine\'s text stays on a secondary line for support');
  const unknown = simulationWindow(view([incident('SOMETHING_NEW', 'subscription activation is closed')]), MODEL);
  await flush(2);
  assert.equal(unknown.querySelector('[data-simulation-incident]').getAttribute('message'), 'Plain step: The step reported the error code SOMETHING_NEW. (SOMETHING_NEW)');
  assert.doesNotMatch(unknown.querySelector('[data-simulation-incident]').getAttribute('message'), /activation is closed/);
  assert.equal(win.querySelector('[data-simulation-status]').getAttribute('status'), 'err');
  const running = simulationWindow(baseView({ instance: { status: 'Running' } }), MODEL);
  assert.equal(running.querySelector('[data-simulation-incidents]'), null);
  assert.equal(running.querySelector('[data-simulation-advance]').hasAttribute('disabled'), false);
  for (const status of ['Completed', 'Cancelled', 'Error']) {
    const done = simulationWindow(baseView({ instance: { status } }), MODEL);
    assert.equal(done.querySelector('[data-simulation-advance]').hasAttribute('disabled'), true, status);
  }
});

test('outputs are limited to what the engine keeps, in the service form, the fixture and the work task', async () => {
  const limit = OUTPUTS_MAX_KIB * 1024;
  const padded = (bytes) => JSON.stringify({ blob: 'x'.repeat(bytes - 11) });
  assert.equal(new TextEncoder().encode(padded(limit)).length, limit);
  assert.deepEqual(Object.keys(readOutputs(padded(limit))), ['value']);
  assert.deepEqual(readOutputs(padded(limit + 1)).error, { key: 'simulation_outputs_too_large', values: { max: 240 } });
  assert.deepEqual(readOutputs('x'.repeat(5 * 1024 * 1024)).error.key, 'simulation_outputs_too_large');
  const manyKeys = JSON.stringify(Object.fromEntries(Array.from({ length: 129 }, (_, index) => [`k${index}`, 1])));
  assert.equal(readOutputs(manyKeys).error.key, 'simulation_outputs_too_many_keys');
  assert.equal(readServiceAnswer({ hasResultExpression: false }, { outcome: 'Completed', code: '', summary: '', outputs: padded(limit + 1), verdict: '' }).errors.outputs.key, 'simulation_outputs_too_large');

  responder = () => ({ view: baseView() });
  const win = simulationWindow(baseView({ serviceWaits: [wait()], userTasks: [taskOf({ kind: 'Work' })] }), MODEL);
  await flush(2);
  const article = win.querySelector('[data-simulation-wait]');
  article.querySelector('[data-answer-field="outputs"]').value = padded(limit + 1);
  click(article.querySelector('[data-simulation-resolve]'));
  await flush(2);
  assert.equal(calls.length, 0, 'nothing above the limit is sent');
  assert.equal(article.querySelector('[data-answer-error="outputs"]').getAttribute('message'), 'The outputs are too large: at most 240 KiB.');

  const work = win.querySelector('[data-simulation-open-task]');
  work.querySelector('[data-simulation-output]').value = '[1]';
  click(work.querySelector('[data-simulation-task]'));
  await flush(2);
  assert.equal(calls.length, 0);
  assert.equal(work.querySelector('[data-task-error]').getAttribute('message'), I18n.t('bpmn.object_required'));
  work.querySelector('[data-simulation-output]').value = '{bad';
  click(work.querySelector('[data-simulation-task]'));
  assert.equal(work.querySelector('[data-task-error]').getAttribute('message'), I18n.t('bpmn.json_invalid'));
  work.querySelector('[data-simulation-output]').value = padded(limit + 1);
  click(work.querySelector('[data-simulation-task]'));
  assert.match(work.querySelector('[data-task-error]').getAttribute('message'), /at most 240 KiB/);
  assert.equal(calls.length, 0);
  assert.equal(win.querySelector('[data-error]').hidden, true, 'the problem is at the field, not in the window alert');
  work.querySelector('[data-simulation-output]').value = '{"ok": 1}';
  click(work.querySelector('[data-simulation-task]'));
  await flush(3);
  assert.deepEqual(calls[0].payload.outputs, { ok: 1 });

  const form = await startWindow(RUNNABLE);
  const plain = form.querySelector('[data-fixture-step="Plain"]');
  change(plain.querySelector('[data-fixture-mode]'), 'Fixture');
  plain.querySelector('[data-answer-field="outputs"]').value = padded(limit + 1);
  calls.length = 0;
  click(form.querySelector('[data-act="submit"]'));
  await flush(3);
  assert.equal(calls.length, 0);
  assert.match(plain.querySelector('[data-answer-error="outputs"]').getAttribute('message'), /at most 240 KiB/);
});

test('a message too big, a closed run and a vanished simulation are told in words', () => {
  assert.equal(simulationErrorText('transport closed: message too big', MODEL), I18n.t('bpmn.simulation_error_too_big', { max: OUTPUTS_MAX_KIB }));
  assert.equal(simulationErrorText('process instance revision conflict or closed instance', MODEL), I18n.t('bpmn.simulation_error_closed'));
  assert.equal(simulationErrorText('simulation task is assigned to another actor', MODEL), I18n.t('bpmn.simulation_error_task_unavailable'));
  assert.equal(simulationErrorText('protocol error NotFound: process resource not found', MODEL), I18n.t('bpmn.simulation_error_gone'));
});

test('Escape in the JSON editor stays in the editor, and Escape or Close on an open run asks before dropping it', async () => {
  const seen = [];
  const listener = (keyEvent) => seen.push(keyEvent.key);
  document.addEventListener('keydown', listener);
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, serviceWaits: [wait()] }), MODEL);
  await flush(2);
  const editor = win.querySelector('[data-simulation-wait] [data-answer-field="outputs"]');
  editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', bubbles: true }));
  document.removeEventListener('keydown', listener);
  assert.deepEqual(seen, ['a'], 'Escape from the editor never reaches the window');
  assert.match(win.textContent, /Press Esc, then Tab, to leave the editor/);

  const asked = [];
  const confirmOriginal = TfWindow.confirm;
  let answer = false;
  TfWindow.confirm = async (options) => { asked.push(options); return answer; };
  try {
    win.close();
    await flush(3);
    assert.equal(asked.length, 1);
    assert.equal(asked[0].cancelLabel, 'Keep it');
    assert.equal(asked[0].confirmLabel, 'End simulation');
    assert.equal(gone(win), false, 'declining keeps the run');
    assert.equal(releases.length, 0);
    answer = true;
    win.close();
    await flush(3);
    assert.equal(gone(win), true);
    const finished = simulationWindow(baseView({ instance: { status: 'Completed' } }), MODEL);
    await flush(2);
    finished.close();
    await flush(3);
    assert.equal(asked.length, 2, 'a finished run closes without a question');
    assert.equal(gone(finished), true);
    const explicit = simulationWindow(baseView({ instance: { status: 'Waiting' } }), MODEL);
    await flush(2);
    click(explicit.querySelector('[data-simulation-end]'));
    await flush(3);
    assert.equal(asked.length, 2, 'the End button is the explicit way out');
    assert.equal(gone(explicit), true);
  } finally {
    TfWindow.confirm = confirmOriginal;
  }
});

test('a language switch carries the run and the unsent drafts to the remounted screen, for the same process only', async () => {
  const live = baseView({ instance: { status: 'Waiting' }, serviceWaits: [wait()] });
  responder = (kind) => {
    if (kind === 'processSimulationViewRequest') return { view: live };
    throw new Error(`unexpected ${kind}`);
  };
  const open = async () => {
    const win = simulationWindow(live, MODEL, { flowId: 'flow-1' });
    await flush(2);
    change(win.querySelector('[data-answer-field="summary"]'), 'half typed');
    win.querySelector('[data-answer-field="outputs"]').value = '{"keep": 1}';
    return win;
  };
  const first = await open();
  await I18n.setLanguage('pl');
  first.remove();
  await flush(3);
  const resumed = await resumeProcessSimulation('flow-1');
  await flush(3);
  assert.ok(resumed?.isConnected, 'the run comes back');
  assert.equal(resumed.querySelector('[data-answer-field="summary"]').value, 'half typed');
  assert.equal(resumed.querySelector('[data-answer-field="outputs"]').value, '{"keep": 1}');
  assert.equal(resumed.querySelector('[data-simulation-resolve]').textContent.trim(), 'Wyślij wynik');
  assert.equal(calls.filter((call) => call.kind === 'processSimulationViewRequest').length, 1);
  assert.equal(releases.length, 0, 'a language switch releases nothing');
  assert.equal(await resumeProcessSimulation('flow-1'), null, 'it is carried once');
  resumed.remove();
  await flush(3);
  await I18n.setLanguage('en');

  // The window may still be up when the new screen asks (the router closes it with an animation).
  releases.length = 0;
  const lingering = await open();
  await I18n.setLanguage('pl');
  const early = await resumeProcessSimulation('flow-1');
  await flush(3);
  assert.equal(early.querySelector('[data-answer-field="summary"]').value, 'half typed');
  lingering.remove();
  await flush(3);
  assert.equal(releases.length, 0, 'the old window closing late releases nothing');
  early.remove();
  await flush(3);
  await I18n.setLanguage('en');

  // Another process mounts: the orphan is released, not shown.
  releases.length = 0;
  const second = simulationWindow(live, MODEL, { flowId: 'flow-1' });
  await flush(2);
  await I18n.setLanguage('pl');
  second.remove();
  await flush(3);
  assert.equal(await resumeProcessSimulation('flow-2'), null);
  await flush(2);
  assert.equal(releases.length, 1);
  await I18n.setLanguage('en');
  // A plain close in the same language still releases the run.
  const plain = simulationWindow(live, MODEL, { flowId: 'flow-1' });
  await flush(2);
  const before = releases.length;
  plain.remove();
  await flush(3);
  assert.equal(releases.length, before + 1);
});

test('a result applied from a fixture is said to be prepared in the trace', async () => {
  const fixtures = new Map([['Plain', { nodeId: 'Plain', mode: 'Fixture', result: { outcome: 'Completed', code: null, summary: '', outputs: {}, evidence: [] } }]]);
  const view = baseView({ events: [event('service_result', 'Plain'), event('service_result', 'Expr')] });
  const win = simulationWindow(view, MODEL, { fixtures });
  await flush(2);
  const lines = [...win.querySelectorAll('.fb-simulation-events li')].map((row) => row.textContent.replace(/\s+/g, ' ').trim());
  assert.match(lines[0], /Prepared result applied · Plain step$/);
  assert.match(lines[1], /Result from the operator · Expression step$/);
});

test('run state, task kind, event kinds and the clock are told in the interface language, unknown kinds by name', async () => {
  const view = baseView({ instance: { status: 'Waiting' }, clock: { nowMs: Date.UTC(2026, 10, 4, 10, 0, 0), horizonMs: Date.UTC(2026, 10, 4, 11, 0, 0), stepIndex: 3 },
    userTasks: [taskOf({ kind: 'Work' })], events: [event('instance_started'), event('service_queued', 'Plain'), event('verification_rejected', 'Plain'), event('incident', 'Plain'), event('brand_new_kind', 'Plain')] });
  const win = simulationWindow(view, MODEL);
  await flush(2);
  const toolbar = win.querySelector('.fb-simulation-toolbar').textContent;
  assert.match(toolbar, /Waiting/);
  assert.doesNotMatch(toolbar, /\d{13}/, 'the clock is a date and time, not an epoch');
  assert.match(toolbar, /2026/);
  assert.match(toolbar, /step 3/);
  assert.match(win.querySelector('.fb-simulation-task').textContent, /Work/);
  const lines = [...win.querySelectorAll('.fb-simulation-events li')].map((row) => row.textContent.replace(/\s+/g, ' ').trim());
  assert.ok(lines[0].endsWith('Run started'));
  assert.ok(lines[1].endsWith('Service step queued · Plain step'));
  assert.ok(lines[2].endsWith('Verification rejected · Plain step'));
  assert.ok(lines[3].endsWith('Incident raised · Plain step'));
  assert.ok(lines[4].endsWith('brand_new_kind · Plain step'));
  await I18n.setLanguage('pl');
  try {
    const polish = simulationWindow(view, MODEL);
    await flush(2);
    const text = polish.textContent;
    assert.match(text, /Czeka/);
    assert.match(text, /Praca/);
    assert.match(text, /Uruchomiono przebieg/);
    assert.match(text, /Krok usługi w kolejce/);
    assert.match(text, /Weryfikacja odrzucona/);
    assert.doesNotMatch(text, /service_queued|instance_started|Waiting|Running/);
  } finally {
    await I18n.setLanguage('en');
  }
});

test('a verdict is asked for only where it can matter, and a verdict the engine still wants is shown at its field', async () => {
  const model = serviceModel([
    ['Checked', 'Checked step', serviceTask({ verification: { Critic: { agentId: 'a', criteria: 'x', onFailure: 'Error' } }, resultExpression: "{'outcome':'Completed','code':null,'summary':'','outputs':{},'evidence':[]}" })],
    ['Plain', 'Plain step', serviceTask({ verification: { Test: { flowId: 't', onFailure: 'Error' } } })],
  ]);
  responder = () => { throw new Error('SIMULATION_VERDICT_REQUIRED: Checked has a machine verification and needs a verdict for a completed result'); };
  const win = simulationWindow(baseView({ serviceWaits: [
    wait({ jobId: 'job-a', nodeId: 'Plain', verdictApplicable: true }), wait({ jobId: 'job-b', nodeId: 'Checked', verdictApplicable: true })] }), model);
  await flush(2);
  const [plain, expression] = [...win.querySelectorAll('[data-simulation-wait]')];
  const shown = (article) => !article.querySelector('[data-answer-verdict]').hidden;
  assert.equal(shown(plain), true);
  change(plain.querySelector('[data-answer-field="outcome"]'), 'Error');
  assert.equal(shown(plain), false, 'a failed result never reaches the check');
  change(plain.querySelector('[data-answer-field="outcome"]'), 'Completed');
  assert.equal(shown(plain), true);
  change(expression.querySelector('[data-answer-field="outcome"]'), 'Error');
  assert.equal(shown(expression), true, 'with an expression the engine decides the outcome');
  click(expression.querySelector('[data-simulation-resolve]'));
  await flush(5);
  const after = win.querySelector('[data-simulation-wait="job-b"]');
  const alert = after.querySelector('[data-answer-error="verdict"]');
  assert.equal(alert.hidden, false);
  assert.equal(alert.getAttribute('message'), I18n.t('bpmn.simulation_verdict_required'));
});

test('the start window lists the steps it cannot simulate with the reason and holds the start back, naming them', async () => {
  const model = serviceModel([
    ['Plain', 'Plain step', serviceTask()],
    ['Agent', 'Agent step', serviceTask({ implementation: { Agent: { agentId: 'a-1' } } })],
    ['WithIo', 'Io step', serviceTask(), { activityIo: { dataOutputs: [{ id: 'o' }] } }],
    ['Unbound', 'Unbound step', serviceTask({ implementation: { Unbound: { origin: 'x' } } })],
  ]);
  const form = await startWindow(model);
  const notes = [...form.querySelectorAll('[data-fixture-unsupported]')];
  assert.deepEqual(notes.map((note) => note.dataset.fixtureUnsupported), ['Agent', 'WithIo', 'Unbound']);
  assert.match(notes[0].textContent, /run by an agent/);
  assert.match(notes[1].textContent, /data inputs or outputs/);
  assert.match(notes[2].textContent, /no implementation yet/);
  assert.deepEqual([...form.querySelectorAll('[data-fixture-step]')].map((row) => row.dataset.fixtureStep), ['Plain']);
  assert.equal(form.querySelector('[data-act="submit"]').hasAttribute('disabled'), true, 'the engine would refuse this run');
  assert.equal(form.querySelector('.tf-act__note').getAttribute('message'), I18n.t('bpmn.simulation_blocked_by_steps', { names: 'Agent step, Io step, Unbound step' }));
  click(form.querySelector('[data-act="submit"]'));
  await flush(3);
  assert.equal(calls.length, 0);
  const free = await startWindow(serviceModel([['Plain', 'Plain step', serviceTask()]]));
  assert.equal(free.querySelector('[data-act="submit"]').hasAttribute('disabled'), false);
  assert.equal(free.querySelector('.tf-act__note').getAttribute('message'), I18n.t('bpmn.simulation_hint'));
});

test('a bad prepared field is scrolled to, the start button says it is starting and no toast is left behind', async () => {
  const gate = deferred();
  responder = () => gate.promise;
  const toasts = [];
  const showOriginal = TfToast.show;
  TfToast.show = (options) => { toasts.push(options); };
  const form = await startWindow(serviceModel([['Plain', 'Plain step', serviceTask()]]));
  const scrolled = [];
  const original = window.HTMLElement.prototype.scrollIntoView;
  window.HTMLElement.prototype.scrollIntoView = function scroll() { scrolled.push(this.dataset.answerError); };
  try {
    const plain = form.querySelector('[data-fixture-step="Plain"]');
    change(plain.querySelector('[data-fixture-mode]'), 'Fixture');
    plain.querySelector('[data-answer-field="outputs"]').value = '{bad';
    click(form.querySelector('[data-act="submit"]'));
    await flush(3);
    assert.deepEqual(scrolled, ['outputs'], 'the first bad field is brought into view');
    plain.querySelector('[data-answer-field="outputs"]').value = '{}';
    click(form.querySelector('[data-act="submit"]'));
    await flush(3);
    assert.equal(form.querySelector('[data-act="submit"]').getAttribute('label'), 'Starting…');
  } finally {
    window.HTMLElement.prototype.scrollIntoView = original;
    gate.resolve({ view: baseView() });
    await flush(3);
  }
  TfToast.show = showOriginal;
  assert.deepEqual(toasts, [], 'starting leaves no toast behind to go stale in another language');
});

const sub = (overrides = {}) => ({ subscriptionId: 'sub-1', kind: 'MessageCatch', nodeId: 'Plain', nodeName: '', scopeId: 's', tokenId: 't',
  messageName: 'order_paid', correlationKey: 'PO-7', signalNamespaceUri: null, signalDeclarationId: null, raceId: null, dueAtMs: null, ...overrides });
const message = (overrides = {}) => ({ messageId: 'm-1', messageName: 'order_paid', correlationKey: 'PO-7', status: 'Pending',
  receivedAtMs: 1, expiresAtMs: Date.UTC(2026, 10, 4, 12, 0, 0), deliveredAtMs: null, lastReason: null, ...overrides });
const T0 = Date.UTC(2026, 10, 4, 10, 0, 0);
const draftOf = (overrides = {}) => ({ pick: '', name: 'order_paid', key: 'PO-7', payload: '{}', ttl: '3600', exact: false, ...overrides });

test('a message form is read into the request, and every wrong field is named', () => {
  const waits = [sub(), sub({ subscriptionId: 'sub-sig', kind: 'SignalCatch', messageName: null, correlationKey: null })];
  assert.deepEqual(readMessageDraft(draftOf({ payload: '{"a": {"b_c": 1}}', ttl: ' 90 ' }), waits).request,
    { messageName: 'order_paid', correlationKey: 'PO-7', payload: { a: { b_c: 1 } }, ttlSeconds: 90, subscriptionId: null });
  assert.equal(readMessageDraft(draftOf({ pick: 'sub-1', exact: true }), waits).request.subscriptionId, 'sub-1');
  assert.equal(readMessageDraft(draftOf({ pick: 'sub-1', exact: false }), waits).request.subscriptionId, null);
  assert.equal(readMessageDraft(draftOf({ pick: '', exact: true }), waits).request.subscriptionId, null, 'a free entry has no wait to address');
  assert.equal(readMessageDraft(draftOf({ name: '' }), waits).errors.name, 'simulation_message_name_invalid');
  assert.equal(readMessageDraft(draftOf({ key: 'a\u0007b' }), waits).errors.key, 'simulation_message_key_invalid');
  assert.equal(readMessageDraft(draftOf({ name: 'x'.repeat(257) }), waits).errors.name, 'simulation_message_name_invalid');
  assert.equal(readMessageDraft(draftOf({ name: 'x'.repeat(256) }), waits).errors, undefined);
  for (const ttl of ['0', '604801', 'abc', '1.5', '', '-3']) assert.equal(readMessageDraft(draftOf({ ttl }), waits).errors.ttl, 'simulation_message_ttl_invalid', ttl);
  assert.equal(readMessageDraft(draftOf({ ttl: '604800' }), waits).errors, undefined);
  assert.equal(readMessageDraft(draftOf({ payload: '{bad' }), waits).errors.payload, 'json_invalid');
  assert.equal(readMessageDraft(draftOf({ payload: '[1]' }), waits).errors.payload, 'object_required');
  assert.equal(readMessageDraft(draftOf({ payload: JSON.stringify({ blob: 'x'.repeat(OUTPUTS_MAX_KIB * 1024) }) }), waits).errors.payload.key, 'simulation_payload_too_large');
  assert.equal(readMessageDraft(draftOf({ pick: 'gone', exact: true }), waits).errors.pick, 'simulation_message_pick_gone');
  assert.equal(readMessageDraft(draftOf({ pick: 'sub-sig', exact: true }), waits).errors.pick, 'simulation_message_pick_gone', 'a signal wait cannot be addressed');
  assert.equal(readMessageDraft(draftOf({ pick: 'sub-1', exact: true, key: 'PO-8' }), waits).errors.pick, 'simulation_message_pick_mismatch');
});

test('open waits are listed with names, keys, race groups and due times; a signal wait is read-only with its reason', async () => {
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, clock: { nowMs: T0, horizonMs: T0 + 3600000, stepIndex: 1 },
    subscriptions: [sub({ nodeName: 'Wait for payment' }),
      sub({ subscriptionId: 'sub-2', kind: 'ReceiveTask', nodeId: 'Expr', nodeName: '', messageName: 'ship', correlationKey: 'S-1', raceId: 'race-1', dueAtMs: T0 + 60000 }),
      sub({ subscriptionId: 'sub-3', kind: 'SignalCatch', nodeId: 'Checked', messageName: null, correlationKey: null, signalDeclarationId: 'Signal_stop', raceId: 'race-1', dueAtMs: T0 + 60000 })] }), MODEL);
  await flush(2);
  const panel = win.querySelector('[data-simulation-awaited]');
  assert.ok(panel);
  const rows = (root) => [...root.querySelectorAll('[data-simulation-subscription]')].map((row) => row.dataset.simulationSubscription);
  assert.deepEqual(rows(panel), ['sub-1', 'sub-2', 'sub-3']);
  assert.match(panel.querySelector('[data-simulation-subscription="sub-1"]').textContent, /Wait for payment.*Message catch/s);
  assert.match(panel.querySelector('[data-simulation-subscription="sub-1"]').textContent, /Message: order_paid · Key: PO-7/);
  assert.deepEqual(rows(panel.querySelector('[data-simulation-race="race-1"]')), ['sub-2', 'sub-3'], 'a race groups its waits');
  assert.match(panel.querySelector('[data-simulation-race="race-1"]').textContent, /first event to happen wins/);
  const due = panel.querySelector('[data-simulation-subscription="sub-2"]').textContent;
  assert.match(due, /fires at .*2026/);
  assert.doesNotMatch(due, /\d{13}/);
  assert.match(panel.querySelector('[data-simulation-subscription="sub-3"]').textContent, /Signal: Signal_stop/);
  assert.match(panel.querySelector('[data-simulation-signal-note]').textContent, /A simulation cannot send a signal/);
  const picks = [...win.querySelectorAll('[data-message-field="pick"] option')].map((option) => option.value);
  assert.deepEqual(picks, ['', 'sub-1', 'sub-2'], 'only message waits can be answered');
  assert.match(win.querySelector('[data-simulation-advance-hint]').textContent, /next due item/);
  const bare = simulationWindow(baseView({ instance: { status: 'Waiting' } }), MODEL);
  assert.equal(bare.querySelector('[data-simulation-awaited]'), null);
});

test('choosing a wait fills the form and addresses it exactly; a free entry addresses by name and key only', async () => {
  responder = () => ({ view: baseView({ instance: { status: 'Waiting' } }) });
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, subscriptions: [sub({ nodeName: 'Wait for payment' })] }), MODEL);
  await flush(2);
  const field = (name) => win.querySelector(`[data-message-field="${name}"]`);
  change(field('pick'), 'sub-1');
  assert.equal(field('name').value, 'order_paid');
  assert.equal(field('key').value, 'PO-7');
  assert.equal(field('exact').checked, true);
  field('payload').value = '{"paid": true}';
  change(field('ttl'), '120');
  click(win.querySelector('[data-simulation-message-send]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].kind, 'processSimulationMessageSendRequest');
  assert.deepEqual(calls[0].payload, { simulationId: 'sim-1', messageName: 'order_paid', correlationKey: 'PO-7', payload: { paid: true }, ttlSeconds: 120, subscriptionId: 'sub-1' });
  calls.length = 0;
  change(field('pick'), '');
  assert.equal(field('exact').checked, false);
  change(field('name'), 'nobody_waits');
  change(field('key'), 'K');
  click(win.querySelector('[data-simulation-message-send]'));
  await flush(5);
  assert.equal(calls[0].payload.subscriptionId, null);
  assert.equal(calls[0].payload.messageName, 'nobody_waits');
});

test('a bad message stays in the form with its errors at the fields and sends nothing; the draft survives a redraw', async () => {
  responder = () => { throw new Error('SIMULATION_MESSAGE_INVALID: message TTL must be 1..604800 seconds'); };
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' } }), MODEL);
  await flush(2);
  const field = (name) => win.querySelector(`[data-message-field="${name}"]`);
  const error = (name) => win.querySelector(`[data-message-error="${name}"]`);
  click(win.querySelector('[data-simulation-message-send]'));
  assert.equal(error('name').hidden, false);
  assert.equal(error('key').hidden, false);
  assert.equal(error('name').getAttribute('message'), I18n.t('bpmn.simulation_message_name_invalid'));
  change(field('name'), 'order_paid');
  change(field('key'), 'PO-7');
  field('payload').value = '{"half":';
  click(win.querySelector('[data-simulation-message-send]'));
  assert.equal(error('name').hidden, true);
  assert.equal(error('payload').getAttribute('message'), I18n.t('bpmn.json_invalid'));
  field('payload').value = JSON.stringify({ blob: 'x'.repeat(OUTPUTS_MAX_KIB * 1024) });
  click(win.querySelector('[data-simulation-message-send]'));
  assert.match(error('payload').getAttribute('message'), /at most 240 KiB/);
  change(field('ttl'), '0');
  click(win.querySelector('[data-simulation-message-send]'));
  assert.equal(error('ttl').getAttribute('message'), I18n.t('bpmn.simulation_message_ttl_invalid'));
  assert.equal(calls.length, 0, 'nothing is sent while a field is wrong');
  assert.match(win.querySelector('[data-simulation-message-form]').textContent, /Press Esc, then Tab/);
  // The engine's own refusal is told in words, and what was typed is still there.
  field('payload').value = '{"keep": 1}';
  change(field('ttl'), '5');
  click(win.querySelector('[data-simulation-message-send]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.match(win.querySelector('[data-error]').getAttribute('message'), /The message is not valid/);
  assert.equal(win.querySelector('[data-message-field="name"]').value, 'order_paid');
  assert.equal(win.querySelector('[data-message-field="payload"]').value, '{"keep": 1}');
  assert.equal(win.querySelector('[data-message-field="ttl"]').value, '5');
});

test('messages are listed with a localized status and their expiry on the virtual clock; an ambiguous one says how to address it', async () => {
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, subscriptions: [sub(), sub({ subscriptionId: 'sub-2', nodeId: 'Expr' })],
    messages: [message(), message({ messageId: 'm-2', status: 'Ambiguous' }), message({ messageId: 'm-3', status: 'Delivered', deliveredAtMs: T0 }),
      message({ messageId: 'm-4', status: 'Expired' }), message({ messageId: 'm-5', status: 'Error', lastReason: 'mapping failed' }), message({ messageId: 'm-6', status: 'Cancelled' })] }), MODEL);
  await flush(2);
  const item = (id) => win.querySelector(`[data-simulation-message="${id}"]`);
  assert.equal(win.querySelector('[data-simulation-messages] h3').textContent, '6 messages');
  assert.match(item('m-1').textContent, /Pending/);
  assert.match(item('m-1').textContent, /Expires .*2026/);
  assert.match(item('m-1').textContent, /next Advance delivers it/);
  assert.match(item('m-3').textContent, /Delivered .*2026/);
  assert.match(item('m-4').textContent, /Expired .*2026/);
  assert.match(item('m-5').textContent, /Failed/);
  assert.match(item('m-6').textContent, /Cancelled/);
  assert.equal(item('m-1').querySelector('[data-message-ambiguous-hint]'), null);
  assert.match(item('m-2').querySelector('[data-message-ambiguous-hint]').textContent, /not delivered.*exactly one wait/);
  click(item('m-2').querySelector('[data-message-address]'));
  assert.equal(win.querySelector('[data-message-field="name"]').value, 'order_paid');
  assert.equal(win.querySelector('[data-message-field="key"]').value, 'PO-7');
  assert.equal(win.querySelector('[data-message-field="pick"]').value, 'sub-1', 'one of the candidate waits is chosen');
  assert.equal(win.querySelector('[data-message-field="exact"]').checked, true);
});

test('a finished run takes no more messages', async () => {
  const win = simulationWindow(baseView({ instance: { status: 'Completed' }, messages: [message({ status: 'Ambiguous' })] }), MODEL);
  await flush(2);
  assert.ok(win.querySelector('[data-message-finished]'));
  assert.equal(win.querySelector('[data-simulation-message-send]').hasAttribute('disabled'), true);
  assert.equal(win.querySelector('[data-message-field="name"]').hasAttribute('disabled'), true);
  assert.equal(win.querySelector('[data-message-field="payload"]').hasAttribute('readonly'), true);
  assert.equal(win.querySelector('[data-message-address]').hasAttribute('disabled'), true);
  win.querySelector('[data-message-field="name"]').value = 'order_paid';
  win.querySelector('[data-message-field="key"]').value = 'PO-7';
  win.querySelector('[data-simulation-message-send]').dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await flush(2);
  assert.equal(calls.length, 0, 'a complete form still sends nothing into a finished run');
  const open = simulationWindow(baseView({ instance: { status: 'Waiting' } }), MODEL);
  assert.equal(open.querySelector('[data-message-finished]'), null);
  assert.equal(open.querySelector('[data-simulation-message-send]').hasAttribute('disabled'), false);
});

test('message refusals, new trace events, incident codes and timer attachments are told in words, unknown kinds by name', async () => {
  const model = serviceModel([['Svc_1', 'Await payment', serviceTask()]]);
  const refusals = {
    SIMULATION_MESSAGE_INVALID: 'message TTL must be 1..604800 seconds',
    SIMULATION_MESSAGE_SUBSCRIPTION_UNKNOWN: 'no open message wait of this run has that id, name and correlation key',
    SIMULATION_MESSAGE_TARGET_UNSUPPORTED: 'a simulation runs one process, so a message must address a catch of this same definition (Svc_1)',
    SIMULATION_EVENT_DELIVERY_UNSUPPORTED: 'MessageStart is not simulated: a message start would create another instance (Svc_1)',
  };
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    const lines = Object.entries(refusals).map(([code, detail]) => simulationErrorText(`protocol error ProcessBody: ${code}: ${detail}`, model));
    assert.equal(new Set(lines).size, 4, language);
    assert.ok(lines.every((line) => !line.includes('SIMULATION_') && !line.includes('bpmn.')), `${language}: ${lines.join(' | ')}`);
    assert.ok(lines[2].includes('Await payment') && lines[3].includes('Await payment'), language);
    assert.notEqual(simulationErrorText('target instance is closed', model), 'target instance is closed', language);
  }
  await I18n.setLanguage('en');
  assert.equal(simulationErrorText('target instance is closed', model), I18n.t('bpmn.simulation_error_target_closed'));

  const kinds = ['simulation_message_sent', 'message_queued', 'message_delivered', 'message_ambiguous', 'message_expired', 'signal_received', 'event_race_won', 'event_race_lost',
    'receive_task_opened', 'timer_fired', 'subscription_cancelled', 'brand_new_kind'];
  const view = baseView({ instance: { status: 'Incident' }, events: kinds.map((kind) => event(kind, 'Plain')),
    incidents: [{ incidentId: 'i', nodeId: 'Plain', nodeName: '', jobId: null, code: 'MESSAGE_MAPPING_ERROR', message: 'x', atMs: 1, canRetry: false, scopeId: 's' }],
    timers: [{ timerId: 't', nodeId: 'Expr', nodeName: '', kind: 'Boundary', status: 'Pending', dueAtMs: T0, timezone: 'UTC', occurrence: 0, attachedToId: 'Plain' }] });
  const english = simulationWindow(view, MODEL);
  await flush(2);
  const lines = [...english.querySelectorAll('.fb-simulation-events li')].map((row) => row.textContent.replace(/\s+/g, ' ').trim().replace(/^\d+:\d+:\d+( [AP]M)? /, ''));
  assert.deepEqual(lines.map((line) => line.replace(/ · Plain step$/, '')), ['Message sent', 'Message queued', 'Message delivered', 'Message is ambiguous', 'Message expired', 'Signal received',
    'Event race won', 'Event race lost', 'Receive task opened', 'Timer fired', 'Wait cancelled', 'brand_new_kind']);
  assert.match(english.querySelector('[data-simulation-incident]').getAttribute('message'), /output mapping needs a value the message did not carry/);
  assert.match(english.querySelector('.fb-simulation-timer').textContent, /Boundary · attached to Plain step/);
  await I18n.setLanguage('pl');
  const polish = simulationWindow(view, MODEL);
  await flush(2);
  assert.doesNotMatch(polish.querySelector('.fb-simulation-events').textContent, /message_queued|event_race_won|signal_received/);
  assert.match(polish.querySelector('.fb-simulation-events').textContent, /Wyścig zdarzeń wygrany/);
  assert.doesNotMatch(polish.textContent, /węzeł/);
});

test('the start window names the steps whose events a simulation cannot run', async () => {
  const model = serviceModel([
    ['Msg_start', 'Order arrives', { MessageStart: { messageRef: 'm', outputMapping: {} } }],
    ['Catch_io', 'Wait with data', { MessageCatch: { messageRef: 'm', correlationExpression: '', outputMapping: {} } }, { activityIo: { dataOutputs: [{ id: 'o' }] } }],
    ['Catch_ok', 'Plain wait', { MessageCatch: { messageRef: 'm', correlationExpression: '', outputMapping: {} } }],
  ]);
  const form = await startWindow(model);
  const notes = [...form.querySelectorAll('[data-fixture-unsupported]')];
  assert.deepEqual(notes.map((note) => note.dataset.fixtureUnsupported), ['Msg_start', 'Catch_io']);
  assert.match(notes[0].textContent, /Order arrives/);
  assert.match(notes[0].textContent, /message start would create another run/);
  assert.match(notes[1].textContent, /data inputs or outputs/);
  assert.match(form.querySelector('.fb-simulation-fixtures h3').textContent, /Steps that are not simulated/);
  assert.equal(form.querySelectorAll('[data-fixture-step]').length, 0);
});

test('what was typed into the message form survives a redraw caused by something else', async () => {
  responder = () => ({ view: baseView({ instance: { status: 'Waiting' }, clock: { nowMs: 2, horizonMs: 100, stepIndex: 2 } }) });
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' } }), MODEL);
  await flush(2);
  change(win.querySelector('[data-message-field="name"]'), 'typed_name');
  change(win.querySelector('[data-message-field="key"]'), 'typed_key');
  win.querySelector('[data-message-field="payload"]').value = '{"typed": 1}';
  change(win.querySelector('[data-message-field="ttl"]'), '77');
  click(win.querySelector('[data-simulation-advance]'));
  await flush(5);
  assert.equal(calls[0].kind, 'processSimulationAdvanceRequest');
  assert.equal(win.querySelector('[data-message-field="name"]').value, 'typed_name');
  assert.equal(win.querySelector('[data-message-field="key"]').value, 'typed_key');
  assert.equal(win.querySelector('[data-message-field="payload"]').value, '{"typed": 1}');
  assert.equal(win.querySelector('[data-message-field="ttl"]').value, '77');
});

const clockOf = (nowMs, horizonMs, stepIndex = 1, tickDurationMs = 1000) => ({ nowMs, horizonMs, stepIndex, tickDurationMs });

test('a clock on the horizon switches Advance off and says so, and the refusal names the horizon and refreshes the clock', async () => {
  const horizon = Date.UTC(2026, 10, 4, 11, 0, 0);
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, clock: clockOf(horizon, horizon, 7) }), MODEL);
  await flush(2);
  assert.equal(win.querySelector('[data-simulation-advance]').hasAttribute('disabled'), true);
  assert.match(win.querySelector('[data-simulation-horizon]').getAttribute('message'), /reached the end of the simulation \(.*2026.*\)/);
  const away = simulationWindow(baseView({ instance: { status: 'Waiting' }, clock: clockOf(horizon - 5000, horizon) }), MODEL);
  assert.equal(away.querySelector('[data-simulation-advance]').hasAttribute('disabled'), false);
  assert.equal(away.querySelector('[data-simulation-horizon]'), null);
  // The hint tells what Advance does and where the clock stands.
  const hint = away.querySelector('[data-simulation-advance-hint]').textContent;
  assert.match(hint, /jumps the virtual clock to the next due item/);
  assert.match(hint, /one tick \(1 s\)/);
  assert.match(hint, /Now: .*2026.*the simulation ends: .*2026/);

  // A refusal for the horizon is told with its date, and the window then shows the run's own clock.
  const stale = baseView({ instance: { status: 'Waiting' }, clock: clockOf(horizon - 1000, horizon, 2) });
  const fresh = baseView({ instance: { status: 'Waiting' }, clock: clockOf(horizon, horizon, 3) });
  responder = (kind) => {
    if (kind === 'processSimulationAdvanceRequest') throw new Error('SIMULATION_HORIZON_REACHED: the clock is at 2026-11-04T11:00:00Z, the horizon is 2026-11-04T11:00:00Z');
    if (kind === 'processSimulationViewRequest') return { view: fresh };
    throw new Error(`unexpected ${kind}`);
  };
  const run = simulationWindow(stale, MODEL);
  await flush(2);
  assert.match(run.querySelector('.fb-simulation-toolbar').textContent, /step 2/);
  click(run.querySelector('[data-simulation-advance]'));
  await flush(5);
  assert.match(run.querySelector('[data-error]').getAttribute('message'), /end of the simulation \(.*2026.*\): there is nothing left to advance to/);
  assert.match(run.querySelector('.fb-simulation-toolbar').textContent, /step 3/, 'the clock shown is the run\'s clock after the refusal');
  assert.equal(run.querySelector('[data-simulation-advance]').hasAttribute('disabled'), true);
});

test('a pending message says whether a wait matches it, and that it will never arrive once the run is over', async () => {
  const view = (status, subscriptions) => baseView({ instance: { status }, subscriptions,
    messages: [message({ messageId: 'm-match', correlationKey: 'PO-7' }), message({ messageId: 'm-none', correlationKey: 'PO-9' })] });
  const open = simulationWindow(view('Waiting', [sub()]), MODEL);
  await flush(2);
  const hint = (win, id) => win.querySelector(`[data-simulation-message="${id}"] [data-message-pending-hint]`);
  assert.equal(hint(open, 'm-match').dataset.messagePendingHint, 'match');
  assert.match(hint(open, 'm-match').textContent, /next Advance delivers it/);
  assert.equal(hint(open, 'm-none').dataset.messagePendingHint, 'none');
  assert.match(hint(open, 'm-none').textContent, /No open wait matches this name and key/);
  assert.doesNotMatch(hint(open, 'm-none').textContent, /delivers it/);
  const over = simulationWindow(view('Completed', []), MODEL);
  await flush(2);
  assert.equal(hint(over, 'm-match').dataset.messagePendingHint, 'finished');
  assert.match(hint(over, 'm-match').textContent, /will never be delivered/);
  const horizon = Date.UTC(2026, 10, 4, 11, 0, 0);
  const atEnd = simulationWindow(baseView({ instance: { status: 'Waiting' }, clock: clockOf(horizon, horizon), subscriptions: [sub()], messages: [message({ messageId: 'm-match' })] }), MODEL);
  await flush(2);
  assert.equal(hint(atEnd, 'm-match').dataset.messagePendingHint, 'finished', 'a clock on the horizon delivers nothing more');
  assert.match(hint(atEnd, 'm-match').textContent, /will never be delivered/);
});

test('an ambiguous message is re-prepared for one of its candidate waits, chosen from a small list when there are several', async () => {
  const two = simulationWindow(baseView({ instance: { status: 'Waiting' },
    subscriptions: [sub({ subscriptionId: 'sub-a', nodeId: 'Plain' }), sub({ subscriptionId: 'sub-b', nodeId: 'Expr' }), sub({ subscriptionId: 'sub-c', nodeId: 'Checked', correlationKey: 'OTHER' })],
    messages: [message({ status: 'Ambiguous' })] }), MODEL);
  await flush(2);
  const candidates = two.querySelector('[data-message-candidate]');
  assert.deepEqual([...candidates.querySelectorAll('option')].map((option) => option.textContent), ['Plain step', 'Expression step'], 'only the waits it matches');
  change(candidates, 'sub-b');
  click(two.querySelector('[data-message-address]'));
  const field = (name) => two.querySelector(`[data-message-field="${name}"]`);
  assert.equal(field('pick').value, 'sub-b');
  assert.equal(field('name').value, 'order_paid');
  assert.equal(field('key').value, 'PO-7');
  assert.equal(field('exact').checked, true, 'the new message addresses exactly that wait');
  const one = simulationWindow(baseView({ instance: { status: 'Waiting' }, subscriptions: [sub({ subscriptionId: 'sub-a' })], messages: [message({ status: 'Ambiguous' })] }), MODEL);
  await flush(2);
  assert.equal(one.querySelector('[data-message-candidate]'), null);
  click(one.querySelector('[data-message-address]'));
  assert.equal(one.querySelector('[data-message-field="pick"]').value, 'sub-a');
  assert.equal(one.querySelector('[data-message-field="exact"]').checked, true);
});

test('a failed or cancelled message is told in the interface language and claims a mapping problem only when the run holds that incident', async () => {
  const incident = (code) => ({ incidentId: code, nodeId: 'Plain', nodeName: '', jobId: null, code, message: 'engine text', atMs: 1, canRetry: false, scopeId: 's' });
  const failing = (extra) => baseView({ instance: { status: 'Waiting' }, ...extra,
    messages: [message({ messageId: 'm-map', status: 'Error', lastReason: 'message mapping failed: missing outputs.amount' }),
      message({ messageId: 'm-closed', status: 'Cancelled', lastReason: 'subscription activation is closed' }),
      message({ messageId: 'm-odd', status: 'Error', lastReason: 'some unforeseen engine words' })] });
  const failure = (win, id) => win.querySelector(`[data-simulation-message="${id}"] [data-message-failure]`).textContent;
  const bare = simulationWindow(failing({}), MODEL);
  await flush(2);
  assert.doesNotMatch(failure(bare, 'm-map'), /output mapping/, 'no mapping incident in the run: no mapping claim');
  assert.equal(failure(bare, 'm-map'), I18n.t('bpmn.simulation_reason_other'));
  assert.equal(failure(bare, 'm-closed'), I18n.t('bpmn.simulation_reason_closed'));
  assert.equal(failure(bare, 'm-odd'), I18n.t('bpmn.simulation_reason_other'));
  const withIncident = simulationWindow(failing({ incidents: [incident('MESSAGE_MAPPING_ERROR')] }), MODEL);
  await flush(2);
  assert.equal(failure(withIncident, 'm-map'), I18n.t('bpmn.simulation_reason_mapping'));
  // The engine's words stay on a secondary line for support, never as the headline.
  const raw = withIncident.querySelector('[data-simulation-message="m-odd"] tf-disclosure');
  assert.equal(raw.getAttribute('label'), 'Details for support');
  assert.match(raw.textContent, /some unforeseen engine words/);
  assert.doesNotMatch(failure(withIncident, 'm-odd'), /unforeseen/);
  await I18n.setLanguage('pl');
  const polish = simulationWindow(failing({}), MODEL);
  await flush(2);
  assert.equal(failure(polish, 'm-closed'), 'Oczekiwanie, do którego wiadomość była adresowana, jest już zamknięte.');
  assert.doesNotMatch(polish.querySelector('[data-message-failure]').textContent, /[a-z]+ [a-z]+ [a-z]+ (is|was|closed|failed)/);
});

test('signal waits show the signal name from the model, a boundary says which step it hangs on, and a delivered message names its wait', async () => {
  const model = { ...MODEL, signals: [{ signalId: 'Signal_stop', namespaceUri: 'urn:x', name: 'Stop the line' }] };
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' },
    subscriptions: [sub({ subscriptionId: 's1', kind: 'SignalCatch', messageName: null, correlationKey: null, signalDeclarationId: 'Signal_stop' }),
      sub({ subscriptionId: 's2', kind: 'SignalCatch', messageName: null, correlationKey: null, signalDeclarationId: 'Signal_unknown' }),
      sub({ subscriptionId: 's3', kind: 'BoundaryMessage', nodeId: 'Expr' })],
    events: [event('message_armed', 'Expr', { subscription_id: 's3', attached_to_id: 'Plain' }), event('message_armed', 'Checked', { subscription_id: 'gone' })],
    messages: [message({ status: 'Delivered', deliveredAtMs: T0, matchedSubscriptionId: 'gone' })] }), model);
  await flush(2);
  const row = (id) => win.querySelector(`[data-simulation-subscription="${id}"]`).textContent;
  assert.match(row('s1'), /Signal: Stop the line/);
  assert.match(row('s2'), /Signal: Signal_unknown/, 'an undeclared id falls back to itself');
  assert.match(row('s3'), /attached to Plain step/);
  assert.match(win.querySelector('[data-message-reached]').textContent, /Delivered to: Checked step/);
});

test('an interrupted service step and an interrupted task are named in the trace, and the service step is kept as cancelled', async () => {
  const view = baseView({ instance: { status: 'Waiting' }, events: [event('simulation_service_cancelled', 'Plain', { job_id: 'j', node_id: 'Plain' }), event('simulation_user_task_cancelled', 'Expr', { user_task_id: 'u', node_id: 'Expr' }), event('message_armed', 'Expr')],
    userTasks: [taskOf({ status: 'Cancelled', name: 'Interrupted task' })] });
  const win = simulationWindow(view, MODEL);
  await flush(2);
  const lines = [...win.querySelectorAll('.fb-simulation-events li')].map((row) => row.textContent.replace(/\s+/g, ' ').trim().replace(/^\d+:\d+:\d+( [AP]M)? /, ''));
  assert.deepEqual(lines, ['Service step interrupted · Plain step', 'Task interrupted · Expression step', 'Message wait opened · Expression step']);
  const cancelled = win.querySelector('[data-simulation-cancelled-services]');
  assert.match(cancelled.textContent, /Plain step/);
  assert.match(cancelled.textContent, /Cancelled/);
  assert.match(win.querySelector('[data-simulation-done-task]').textContent, /Cancelled/);
});

test('after Send the message list is shown, the wait is released and a confirmation stands until the form is touched', async () => {
  const sent = baseView({ instance: { status: 'Waiting' }, subscriptions: [sub()], messages: [message()] });
  responder = () => ({ view: sent });
  const scrolled = [];
  const original = window.HTMLElement.prototype.scrollIntoView;
  window.HTMLElement.prototype.scrollIntoView = function scroll() { scrolled.push(this.dataset.simulationMessages !== undefined ? 'messages' : this.tagName); };
  try {
    const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, subscriptions: [sub()] }), MODEL);
    await flush(2);
    const field = (name) => win.querySelector(`[data-message-field="${name}"]`);
    change(field('pick'), 'sub-1');
    assert.equal(field('exact').checked, true);
    assert.equal(win.querySelector('[data-message-confirmation]'), null);
    click(win.querySelector('[data-simulation-message-send]'));
    await flush(5);
    assert.equal(calls[0].payload.subscriptionId, 'sub-1');
    assert.match(win.querySelector('[data-message-confirmation]').textContent, /Message queued; the next time step delivers it\./);
    assert.equal(field('pick').value, '', 'the chosen wait is released');
    assert.equal(field('exact').checked, false);
    assert.equal(field('name').value, 'order_paid');
    assert.equal(field('key').value, 'PO-7');
    assert.ok(scrolled.includes('messages'), 'the list of messages is brought into view');
    // A second press now sends a free message, never silently the same addressed one again.
    click(win.querySelector('[data-simulation-message-send]'));
    await flush(5);
    assert.equal(calls[1].payload.subscriptionId, null);
    change(field('key'), 'PO-8');
    assert.equal(win.querySelector('[data-message-confirmation]'), null, 'touching the form clears the confirmation');
  } finally {
    window.HTMLElement.prototype.scrollIntoView = original;
  }
});

test('a race group lists its timer, and a won race names the winner', async () => {
  const view = baseView({ instance: { status: 'Waiting' }, clock: clockOf(T0, T0 + 3600000),
    subscriptions: [sub({ raceId: 'race-1', scopeId: 'scope-1', dueAtMs: T0 + 5000 })],
    timers: [{ timerId: 't1', nodeId: 'Expr', nodeName: 'Deadline', kind: 'Catch', status: 'Pending', dueAtMs: T0 + 5000, timezone: 'UTC', occurrence: 0, scopeId: 'scope-1' },
      { timerId: 't2', nodeId: 'Checked', nodeName: 'Elsewhere', kind: 'Catch', status: 'Pending', dueAtMs: T0 + 9000, timezone: 'UTC', occurrence: 0, scopeId: 'scope-2' }],
    events: [event('event_race_won', 'Plain', { race_id: 'race-1', winner_node_id: 'Expr' }), event('event_race_won', 'Plain', {})] });
  const win = simulationWindow(view, MODEL);
  await flush(2);
  const race = win.querySelector('[data-simulation-race="race-1"]');
  assert.deepEqual([...race.querySelectorAll('[data-simulation-race-timer]')].map((row) => row.dataset.simulationRaceTimer), ['t1'], 'only the timer of this race');
  assert.match(race.textContent, /Deadline.*fires at/s);
  const lines = [...win.querySelectorAll('.fb-simulation-events li')].map((row) => row.textContent.replace(/\s+/g, ' ').trim());
  assert.match(lines[0], /Event race won by Expression step/);
  assert.match(lines[1], /Event race won · Plain step/, 'without the winner the plain sentence stays');
});

test('a message start and an event step with data are refused with different sentences, and a payload that is too large is called a payload', () => {
  const model = serviceModel([['Svc_1', 'Order arrives', serviceTask()]]);
  const start = simulationErrorText('SIMULATION_EVENT_DELIVERY_UNSUPPORTED: MessageStart is not simulated: a message start would create another instance (Svc_1)', model);
  const io = simulationErrorText('SIMULATION_EVENT_DELIVERY_UNSUPPORTED: MessageCatch with activity IO is not simulated: a message start would create another instance and an event step with activity IO continues (Svc_1)', model);
  assert.notEqual(start, io);
  assert.match(start, /starts the process with a message/);
  assert.match(io, /data inputs or outputs/);
  assert.equal(readMessageDraft({ pick: '', name: 'n', key: 'k', payload: JSON.stringify({ b: 'x'.repeat(OUTPUTS_MAX_KIB * 1024) }), ttl: '60', exact: false }, []).errors.payload.key, 'simulation_payload_too_large');
  assert.equal(I18n.t('bpmn.simulation_payload_too_large', { max: 240 }), 'The payload is too large: at most 240 KiB.');
});

const inst = (overrides = {}) => ({ instanceId: 'inst-1', definitionId: 'def-1', definitionName: 'Order', version: 3, processId: 'P1', status: 'Waiting', parentInstanceId: null, depth: 0, ...overrides });
const treeView = (overrides = {}) => baseView({
  instance: { status: 'Waiting' },
  instances: [inst(), inst({ instanceId: 'inst-2', definitionId: 'def-2', definitionName: 'Payment', version: 1, parentInstanceId: 'inst-1', depth: 1 })],
  calls: [{ callId: 'c1', parentInstanceId: 'inst-1', parentScopeId: 'sc-0', callNodeId: 'Plain', callNodeName: 'Take payment', childInstanceId: 'inst-2',
    calledDefinitionId: 'def-2', calledDefinitionName: 'Payment', calledVersion: 1, calledProcessId: 'P2', local: false, status: 'Waiting' }],
  scopes: [{ scopeId: 'sc-1', parentScopeId: 'sc-0', subprocessNodeId: 'Expr', subprocessNodeName: '', parentTokenId: 't', revision: 1, status: 'Running', depth: 1 }],
  repetitionGroups: [{ groupId: 'g-1', nodeId: 'Checked', nodeName: 'Review each', scopeId: 'sc-2', parentTokenId: 't', mode: 'multi_instance_parallel', status: 'open',
    total: 3, createdCount: 3, completed: 1, maxIterations: null }],
  repetitionOccurrences: [{ occurrenceId: 'o-1', groupId: 'g-1', ordinal: 0, status: 'completed' }, { occurrenceId: 'o-2', groupId: 'g-1', ordinal: 1, status: 'active' }],
  events: [event('call_entered', 'Plain', null).constructor === Object ? { ...event('call_entered', 'Plain'), scopeId: 'sc-2', instanceId: 'inst-2' } : null,
    { ...event('scope_entered', 'Expr'), scopeId: 'sc-1', instanceId: 'inst-1' }],
  ...overrides });

test('the run structure lists the call tree with the call that made each child, its scopes and its repetition groups', async () => {
  const view = treeView();
  const rows = simulationStructure(view);
  assert.deepEqual(rows.map((row) => [row.instance.instanceId, row.depth, row.call?.callNodeName ?? null]), [['inst-1', 0, null], ['inst-2', 1, 'Take payment']]);
  assert.deepEqual(rows[0].scopes.map((scope) => scope.scopeId), ['sc-1'], 'a scope the events place in the root');
  assert.deepEqual(rows[1].groups.map(({ group }) => group.groupId), ['g-1'], 'a group the events place in the child');
  const win = simulationWindow(view, MODEL);
  await flush(2);
  const panel = win.querySelector('[data-simulation-structure]');
  assert.ok(panel);
  const items = [...panel.querySelectorAll('[data-simulation-instance]')];
  assert.deepEqual(items.map((item) => item.dataset.depth), ['0', '1']);
  assert.match(items[1].textContent, /Payment v1/);
  assert.match(items[1].querySelector('[data-simulation-call]').textContent, /Called by “Take payment”/);
  assert.match(items[0].querySelector('[data-simulation-scope]').textContent, /Expression step.*Sub-process.*Running/s);
  const group = items[1].querySelector('[data-simulation-group]');
  assert.match(group.textContent, /Review each.*Multiple instances, in parallel.*Open/s);
  assert.match(group.querySelector('[data-group-progress]').textContent, /1 of 3 occurrences done · now running: occurrence 2/);
  assert.equal(group.querySelectorAll('[data-simulation-occurrence]').length, 2);
  const loop = simulationWindow(treeView({ instances: [inst()], calls: [], repetitionGroups: [{ groupId: 'g-1', nodeId: 'Checked', nodeName: 'Retry', scopeId: 'sc-1', mode: 'structured_loop', status: 'open', total: null, createdCount: 2, completed: 1, maxIterations: 5 }], repetitionOccurrences: [] }), MODEL);
  await flush(2);
  assert.match(loop.querySelector('[data-group-progress]').textContent, /1 done; the loop ends by its condition \(at most 5 rounds\)/);
  // A plain run has no structure to show.
  const plain = simulationWindow(baseView({ instance: { status: 'Waiting' }, instances: [inst()] }), MODEL);
  await flush(2);
  assert.ok(plain.querySelector('[data-simulation-structure]') === null);
});

test('with more than one instance each item names its process, with one the run looks as before', async () => {
  const many = treeView({
    serviceWaits: [wait({ jobId: 'job-a', nodeId: 'Plain', instanceId: 'inst-1' }), wait({ jobId: 'job-b', nodeId: 'Plain', instanceId: 'inst-2' })],
    userTasks: [taskOf({ userTaskId: 'ut-c', scopeId: 'sc-2', kind: 'Work' })],
    timers: [{ timerId: 't', nodeId: 'Expr', nodeName: 'Deadline', kind: 'Catch', status: 'Pending', dueAtMs: T0, timezone: 'UTC', occurrence: 0, scopeId: 'sc-2' }],
    subscriptions: [sub({ scopeId: 'sc-1' })],
    incidents: [{ incidentId: 'i', nodeId: 'Plain', nodeName: '', code: 'CALL_CHILD_ERROR', message: '', atMs: 1, canRetry: false, scopeId: 'sc-2' }] });
  const win = simulationWindow(many, MODEL);
  await flush(2);
  const labels = (root) => [...root.querySelectorAll('[data-instance-label]')].map((chip) => chip.textContent);
  assert.deepEqual(labels(win.querySelector('[data-simulation-wait="job-a"]')), ['Order v3']);
  assert.deepEqual(labels(win.querySelector('[data-simulation-wait="job-b"]')), ['Payment v1'], 'two waits on one node are told apart by their process');
  assert.deepEqual(labels(win.querySelector('[data-simulation-open-task]')), ['Payment v1']);
  assert.deepEqual(labels(win.querySelector('.fb-simulation-timer')), ['Payment v1']);
  assert.deepEqual(labels(win.querySelector('[data-simulation-subscription]')), ['Order v3']);
  assert.match(win.querySelector('[data-simulation-incident]').getAttribute('message'), /\(Payment v1\).*The called process failed\./);
  assert.match(win.querySelector('.fb-simulation-events li [data-event-instance]').textContent, /Payment v1/);
  const single = simulationWindow(baseView({ instance: { status: 'Waiting' }, instances: [inst()], serviceWaits: [wait({ instanceId: 'inst-1' })], userTasks: [taskOf()] }), MODEL);
  await flush(2);
  assert.ok(single.querySelector('[data-instance-label]') === null);
  assert.ok(single.querySelector('[data-event-instance]') === null);
});

test('each wait is resolved by its own job, also when two waits stand on the same node of different processes', async () => {
  responder = () => ({ view: baseView({ instance: { status: 'Waiting' }, instances: [inst()] }) });
  const win = simulationWindow(treeView({ serviceWaits: [wait({ jobId: 'job-a', nodeId: 'Plain', instanceId: 'inst-1' }), wait({ jobId: 'job-b', nodeId: 'Plain', instanceId: 'inst-2' })] }), MODEL);
  await flush(2);
  click(win.querySelector('[data-simulation-wait="job-b"] [data-simulation-resolve]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].payload.jobId, 'job-b');
  assert.equal(calls[0].payload.nodeId, 'Plain');
});

test('a task assigned to someone else is shown read-only with the reason instead of live buttons', async () => {
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, userTasks: [taskOf({ userTaskId: 'ut-mine' }), taskOf({ userTaskId: 'ut-theirs', name: 'Their task', kind: 'Work', canComplete: false })] }), MODEL);
  await flush(2);
  const theirs = win.querySelector('[data-simulation-open-task="ut-theirs"]');
  assert.ok(theirs.querySelector('[data-simulation-task]') === null);
  assert.ok(theirs.querySelector('tf-code-editor') === null);
  assert.match(theirs.querySelector('[data-task-foreign]').textContent, /assigned to someone else/);
  assert.ok(win.querySelector('[data-simulation-open-task="ut-mine"] [data-simulation-task]'));
});

test('every event kind a simulated run writes for scopes, calls and repetitions has a sentence in all five languages', async () => {
  const kinds = ['scope_entered', 'scope_completed', 'scope_cancelled', 'scope_entry_failed', 'scope_error_propagated', 'scope_result_accepted', 'call_requested', 'call_entered',
    'call_returned', 'call_cancelled', 'call_error_propagated', 'call_result_accepted', 'call_request_cancelled', 'repetition_group_started', 'repetition_occurrence_started',
    'repetition_occurrence_completed', 'repetition_completed', 'repetition_condition_checked', 'repetition_entry_failed', 'repetition_group_blocked', 'instance_cancelled'];
  for (const kind of kinds) assert.ok(SIMULATION_EVENT_KINDS.includes(kind), kind);
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, events: [...kinds, 'brand_new_kind'].map((kind) => event(kind)) }), MODEL);
    await flush(2);
    const lines = [...win.querySelectorAll('.fb-simulation-events li')].map((row) => row.textContent.replace(/\s+/g, ' ').trim().replace(/^\S+( [AP]M)? /, ''));
    assert.equal(lines.at(-1), 'brand_new_kind');
    assert.equal(new Set(lines.slice(0, -1)).size, kinds.length, `${language}: one sentence per kind`);
    assert.ok(lines.slice(0, -1).every((line) => !/^[a-z]+_[a-z_]+$/.test(line) && !line.includes('bpmn.')), `${language}: ${lines.join(' | ')}`);
    win.remove();
  }
});

test('the limits, an unpinnable call and an unsupported call are told with their numbers and the run is refreshed', async () => {
  const model = serviceModel([['Call_1', 'Take payment', serviceTask()]]);
  const refusals = {
    SIMULATION_SCOPE_LIMIT: 'the transition would open scope 33, and a simulation opens at most 32',
    SIMULATION_REPETITION_LIMIT: 'the transition would reach 33 repetition groups and 4 occurrences, and a simulation holds at most 32 groups and 128 occurrences',
    SIMULATION_CALL_LIMIT: 'the transition would start instance 9, and a simulation starts at most 8',
    SIMULATION_CALL_PIN_UNAVAILABLE: 'a called process version cannot be pinned into the simulation',
    SIMULATION_CALL_UNSUPPORTED: 'a call with activity IO is not simulated: its result continues through a deferred external step (Call_1)',
  };
  const body = 'SIMULATION_CALL_UNSUPPORTED: MessageCatch inside a called process is not simulated: a simulation delivers messages and signals to the one instance it started (Msg_in_child)';
  const numbers = { SIMULATION_SCOPE_LIMIT: /32/, SIMULATION_REPETITION_LIMIT: /32.*128/, SIMULATION_CALL_LIMIT: /8/ };
  for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
    await I18n.setLanguage(language);
    const lines = Object.entries(refusals).map(([code, detail]) => simulationErrorText(`protocol error ProcessBody: ${code}: ${detail}`, model));
    assert.equal(new Set(lines).size, 5, language);
    Object.keys(numbers).forEach((code, index) => assert.match(lines[index], numbers[code], `${language} ${code}`));
    assert.ok(lines.every((line) => !line.includes('SIMULATION_') && !line.includes('bpmn.')), `${language}: ${lines.join(' | ')}`);
    assert.ok(lines[4].includes('Take payment'), language);
    assert.notEqual(simulationErrorText(body, model), lines[4], 'a body refusal is another sentence than an activity IO one');
    if (language === 'en') {
      assert.match(lines[4], /data inputs or outputs/);
      assert.match(simulationErrorText(body, model), /message, signal, receive or send step or an event gateway/);
    }
  }
  await I18n.setLanguage('en');
  // The run stays usable: the window asks the server for the run as it is after the refusal.
  const fresh = baseView({ instance: { status: 'Waiting' }, clock: clockOf(5, 100, 9) });
  responder = (kind) => {
    if (kind === 'processSimulationAdvanceRequest') throw new Error('SIMULATION_CALL_LIMIT: the transition would start instance 9, and a simulation starts at most 8');
    if (kind === 'processSimulationViewRequest') return { view: fresh };
    throw new Error(`unexpected ${kind}`);
  };
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, clock: clockOf(5, 100, 2) }), MODEL);
  await flush(2);
  click(win.querySelector('[data-simulation-advance]'));
  await flush(5);
  assert.match(win.querySelector('[data-error]').getAttribute('message'), /at most 8 processes in one call tree/);
  assert.match(win.querySelector('.fb-simulation-toolbar').textContent, /step 9/);
  assert.equal(win.querySelector('[data-simulation-advance]').hasAttribute('disabled'), false, 'the run is still usable');
  assert.doesNotMatch(processExplainError('repetition is unsupported in simulation (X)', model), /bpmn\./);
});

test('steps inside a sub-process get prepared-result rows, a call with data is held back, and a call says its own steps wait for the operator', async () => {
  const inner = { id: 'Inner', name: 'Inner step', kind: serviceTask() };
  const model = serviceModel([
    ['Sub_1', 'Check', { SubProcess: { body: { nodes: [inner], sequenceFlows: [], variables: {} }, inputMapping: {}, outputMapping: {} } }],
    ['Call_1', 'Take payment', { CallActivity: { calledDefinitionId: 'd', calledVersion: 1, calledElement: { namespaceUri: '', processId: 'p' }, inputMapping: {}, outputMapping: {} } }],
  ]);
  assert.deepEqual(simulationServiceSteps(model).map((step) => step.nodeId), ['Inner']);
  const form = await startWindow(model);
  assert.deepEqual([...form.querySelectorAll('[data-fixture-step]')].map((row) => row.dataset.fixtureStep), ['Inner']);
  assert.match(form.querySelector('[data-fixtures-calls]').textContent, /Steps inside the processes it calls wait for your answer/);
  assert.equal(form.querySelector('[data-act="submit"]').hasAttribute('disabled'), false);
  const callKind = model.nodes.find((node) => node.id === 'Call_1').kind;
  const withIo = serviceModel([['Call_1', 'Take payment', callKind], ['Call_2', 'Charge', callKind, { activityIo: { dataOutputs: [{ id: 'o' }] } }]]);
  assert.deepEqual(simulationUnsupportedSteps(withIo), [{ nodeId: 'Call_2', reason: 'call_io' }]);
  const held = await startWindow(withIo);
  assert.match(held.querySelector('[data-fixture-unsupported="Call_2"]').textContent, /This call has data inputs or outputs/);
  assert.equal(held.querySelector('[data-act="submit"]').hasAttribute('disabled'), true);
});

test('a step of a called process is named and judged from the called process, not from the caller\'s step of the same id', async () => {
  const sameId = wait({ jobId: 'job-child', nodeId: 'Plain', nodeName: 'Child check', instanceId: 'inst-2', hasResultExpression: true, verdictApplicable: false });
  const [{ step }] = simulationWaitSteps([sameId], MODEL);
  assert.equal(step.hasResultExpression, true, 'the flag comes from the wait, not from the model\'s node of the same id');
  const [{ step: older }] = simulationWaitSteps([wait({ nodeId: 'Expr' })], MODEL);
  assert.equal(older.hasResultExpression, true, 'a wait without the flag falls back to the model');
  const view = treeView({
    serviceWaits: [sameId],
    events: [{ ...event('service_queued', 'Plain'), nodeName: 'Child check', scopeId: 'sc-2', instanceId: 'inst-2' }, { ...event('service_queued', 'Plain'), scopeId: 'sc-0', instanceId: 'inst-1' }],
  });
  const win = simulationWindow(view, MODEL);
  await flush(2);
  const card = win.querySelector('[data-simulation-wait="job-child"]');
  assert.match(card.querySelector('strong').textContent, /Child check/);
  assert.deepEqual([...card.querySelectorAll('[data-answer-field="outcome"] option')].map((option) => option.value), ['Completed', 'Error', 'NeedsHuman', 'Cancelled'],
    'the child step decides its outcome by an expression, which the caller\'s step of that id does not');
  const rows = [...win.querySelectorAll('.fb-simulation-events li')].map((row) => row.textContent);
  assert.ok(rows.some((row) => row.includes('Child check')), rows.join(' | '));
  assert.ok(rows.some((row) => !row.includes('Child check') && row.includes('·')), 'the event of the caller still reads the model');
});

test('tasks of a multi-instance group show their occurrence and stay in ordinal order, and a child of a repeated call says which occurrence started it', async () => {
  const occurrence = (ordinal, userTaskId, tokenId) => ({ occurrenceId: `o-${ordinal}`, groupId: 'g-1', ordinal, status: 'active', tokenId, userTaskId });
  const view = treeView({
    userTasks: [taskOf({ userTaskId: 'ut-c', kind: 'Work', scopeId: 'sc-2' }), taskOf({ userTaskId: 'ut-a', kind: 'Work', scopeId: 'sc-2' }), taskOf({ userTaskId: 'ut-b', kind: 'Work', scopeId: 'sc-2' })],
    repetitionOccurrences: [occurrence(2, 'ut-c', 'tok-2'), occurrence(0, 'ut-a', 'tok-0'), occurrence(1, 'ut-b', 'tok-1')],
    events: [{ ...event('call_entered', 'Plain', { child_instance_id: 'inst-2', parent_token_id: 'tok-1' }), scopeId: 'sc-0', instanceId: 'inst-1' }],
  });
  assert.deepEqual(simulationTasksByOccurrence(view.userTasks, view.repetitionOccurrences).map(({ task }) => task.userTaskId), ['ut-a', 'ut-b', 'ut-c']);
  const win = simulationWindow(view, MODEL);
  await flush(2);
  const cards = [...win.querySelectorAll('[data-simulation-open-task]')];
  assert.deepEqual(cards.map((card) => card.dataset.simulationOpenTask), ['ut-a', 'ut-b', 'ut-c']);
  assert.deepEqual(cards.map((card) => card.querySelector('[data-task-occurrence]').textContent.trim()), ['Occurrence 1', 'Occurrence 2', 'Occurrence 3']);
  assert.match(win.querySelector('[data-simulation-instance="inst-2"] [data-call-occurrence]').textContent, /Occurrence 2/);
  const plain = simulationWindow(baseView({ instance: { status: 'Waiting' }, userTasks: [taskOf({ kind: 'Work' })] }), MODEL);
  await flush(2);
  assert.ok(plain.querySelector('[data-task-occurrence]') === null, 'a task outside a group names no occurrence');
});

test('the control that had the keyboard keeps it after an action redraws the window', async () => {
  const fresh = baseView({ instance: { status: 'Waiting' }, clock: clockOf(6, 100, 2) });
  responder = () => ({ view: fresh });
  const win = simulationWindow(baseView({ instance: { status: 'Waiting' }, userTasks: [taskOf({ userTaskId: 'ut-1', kind: 'Work' })] }), MODEL);
  await flush(2);
  const advance = win.querySelector('[data-simulation-advance] button');
  advance.focus();
  assert.equal(document.activeElement, advance, 'the harness gives the button the focus');
  click(win.querySelector('[data-simulation-advance]'));
  await flush(6);
  const redrawn = win.querySelector('[data-simulation-advance] button');
  assert.notEqual(redrawn, advance, 'the redraw rebuilt the control');
  assert.equal(document.activeElement, redrawn, 'the keyboard stays on the control that was used');
});

test('a group that failed on entry, a parallel group and a finished group are told without an empty number, without a false "now" and collapsed', async () => {
  const group = (overrides) => ({ groupId: 'g-1', nodeId: 'Checked', nodeName: 'Review each', scopeId: 'sc-2', parentTokenId: 't', mode: 'multi_instance_sequential', status: 'open', total: 3, createdCount: 1, completed: 0, maxIterations: null, ...overrides });
  const occ = (ordinal, status) => ({ occurrenceId: `o-${ordinal}`, groupId: 'g-1', ordinal, status });
  const progress = async (overrides, occurrences) => {
    const win = simulationWindow(treeView({ repetitionGroups: [group(overrides)], repetitionOccurrences: occurrences }), MODEL);
    await flush(2);
    const row = win.querySelector('[data-simulation-instance="inst-2"] [data-simulation-group]');
    return { row, text: row.querySelector('[data-group-progress]').textContent.replace(/\s+/g, ' ').trim() };
  };
  const failed = await progress({ total: null, status: 'incident', createdCount: 0, completed: 0 }, []);
  assert.doesNotMatch(failed.text, /at most|rounds|\s{2}/);
  assert.match(failed.text, /number of occurrences is not known/);
  const unbounded = await progress({ mode: 'structured_loop', total: null, maxIterations: null }, [occ(0, 'active')]);
  assert.match(unbounded.text, /^0 done; the loop ends by its condition · now: occurrence 1$/);
  assert.doesNotMatch(unbounded.text, /at most|rounds/);
  const sequential = await progress({}, [occ(0, 'completed'), occ(1, 'active')]);
  assert.match(sequential.text, /now: occurrence 2/);
  const parallel = await progress({ mode: 'multi_instance_parallel', total: 3, createdCount: 3 }, [occ(0, 'active'), occ(1, 'completed'), occ(2, 'active')]);
  assert.match(parallel.text, /now running: occurrences 1, 3/);
  assert.doesNotMatch(parallel.text, /now: occurrence/);
  assert.ok(parallel.row.querySelector('tf-disclosure') === null, 'a running group lists its occurrences');
  const done = await progress({ status: 'completed', completed: 3, createdCount: 3 }, [occ(0, 'completed'), occ(1, 'completed'), occ(2, 'completed')]);
  const folded = done.row.querySelector('tf-disclosure[data-group-occurrences]');
  assert.ok(folded, 'a finished group collapses its occurrences');
  assert.equal(folded.querySelectorAll('[data-simulation-occurrence]').length, 3);
  assert.doesNotMatch(done.text, /now/);
});

test('a limit refusal at Start says nothing was started; mid-run it says the run cannot go on and points to End simulation; an unpinnable call says archived', async () => {
  const model = serviceModel([['Call_1', 'Take payment', serviceTask()]]);
  const raw = 'protocol error ProcessBody: SIMULATION_REPETITION_LIMIT: the transition would reach 33 repetition groups and 4 occurrences, and a simulation holds at most 32 groups and 128 occurrences';
  const pin = 'protocol error ProcessBody: SIMULATION_CALL_PIN_UNAVAILABLE: a called process version cannot be pinned into the simulation';
  const expected = {
    en: { start: /not started/, run: /cannot continue past this step\. Use “End simulation”/, pin: /archived or unpublished/ },
    pl: { start: /nie została uruchomiona/, run: /„Zakończ symulację”/, pin: /zarchiwizowany lub wycofany/ },
    de: { start: /nicht gestartet/, run: /„Simulation beenden“/, pin: /archiviert oder zurückgezogen/ },
    es: { start: /no se inició/, run: /«Terminar simulación»/, pin: /archivado o retirado/ },
    fr: { start: /n’a pas démarré/, run: /« Terminer la simulation »/, pin: /archivé ou dépublié/ },
  };
  for (const [language, want] of Object.entries(expected)) {
    await I18n.setLanguage(language);
    const atStart = simulationErrorText(raw, model, { atStart: true });
    const midRun = simulationErrorText(raw, model);
    assert.match(atStart, want.start, language);
    assert.match(midRun, want.run, language);
    assert.doesNotMatch(atStart, /unchanged|unverändert|bez zmian|inchangée|no cambia|Zakończ|End simulation/i, `${language}: no run exists at Start`);
    assert.match(simulationErrorText(pin, model, { atStart: true }), want.pin, language);
    assert.doesNotMatch(simulationErrorText(pin, model, { atStart: true }), /Publish the called process|Opublikuj wywoływany/, language);
  }
  await I18n.setLanguage('pl');
  assert.doesNotMatch(I18n.t('bpmn.simulation_error_scope_limit'), /został bez zmian/);
  await I18n.setLanguage('en');
});

test('the start window explains a limit refusal as a start that did not happen', async () => {
  responder = () => { throw new Error('SIMULATION_CALL_LIMIT: the transition would start instance 9, and a simulation starts at most 8'); };
  const form = await startWindow(RUNNABLE);
  click(form.querySelector('[data-act="submit"]'));
  await flush(5);
  const shown = [...form.querySelectorAll('tf-alert')].map((alert) => alert.getAttribute('message') ?? alert.textContent).join(' | ');
  assert.match(shown, /at most 8 processes in one call tree.*The simulation was not started\./);
  assert.doesNotMatch(shown, /unchanged/);
});
