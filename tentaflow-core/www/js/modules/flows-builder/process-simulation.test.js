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
const { OUTPUTS_MAX_KIB, SERVICE_OUTCOMES, readOutputs, readServiceAnswer, readServiceFixtures, simulationServiceOutcomes, simulationServiceSteps, simulationWaitSteps } = await import('./simulation-service.js');
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
  const form = await startWindow(MODEL);
  click(form.querySelector('[data-act="submit"]'));
  await flush(5);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].payload.serviceFixtures, []);
  form.remove();
  calls.length = 0;

  const prepared = await startWindow(MODEL);
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
  const form = await startWindow(MODEL);
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
  assert.doesNotMatch(win.querySelector('[data-simulation-incidents]').textContent, /simulation operator chose/, 'a known code is told in the platform\'s words, not the engine\'s');
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

  const form = await startWindow(MODEL);
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

test('the start window lists the steps it cannot simulate with the reason, scrolls to a bad field and names its busy state', async () => {
  const model = serviceModel([
    ['Plain', 'Plain step', serviceTask()],
    ['Agent', 'Agent step', serviceTask({ implementation: { Agent: { agentId: 'a-1' } } })],
    ['WithIo', 'Io step', serviceTask(), { activityIo: { dataOutputs: [{ id: 'o' }] } }],
    ['Unbound', 'Unbound step', serviceTask({ implementation: { Unbound: { origin: 'x' } } })],
  ]);
  const gate = deferred();
  responder = () => gate.promise;
  const toasts = [];
  const showOriginal = TfToast.show;
  TfToast.show = (options) => { toasts.push(options); };
  const form = await startWindow(model);
  const notes = [...form.querySelectorAll('[data-fixture-unsupported]')];
  assert.deepEqual(notes.map((note) => note.dataset.fixtureUnsupported), ['Agent', 'WithIo', 'Unbound']);
  assert.match(notes[0].textContent, /run by an agent/);
  assert.match(notes[1].textContent, /data inputs or outputs/);
  assert.match(notes[2].textContent, /no implementation yet/);
  assert.deepEqual([...form.querySelectorAll('[data-fixture-step]')].map((row) => row.dataset.fixtureStep), ['Plain']);
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
