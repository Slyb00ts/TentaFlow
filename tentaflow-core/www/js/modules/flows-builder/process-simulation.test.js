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
const { simulationWindow, openProcessSimulation } = await import('./process-simulation.js');
const { SERVICE_OUTCOMES, readServiceAnswer, readServiceFixtures, simulationServiceOutcomes, simulationServiceSteps, simulationWaitSteps } = await import('./simulation-service.js');
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();

const calls = [];
let responder = () => { throw new Error('no responder'); };
// The language switch and the release of a closed window are not under test.
const SILENT = ['mePreferencesUpdateRequest', 'processSimulationReleaseRequest'];
ApiBinary.one = async (kind, payload) => {
  if (SILENT.includes(kind)) return {};
  calls.push({ kind, payload: structuredClone(payload) });
  return responder(kind, payload);
};
ApiBinary.action = ApiBinary.one;
const flush = async (count = 5) => { for (let i = 0; i < count; i += 1) await new Promise((resolve) => setTimeout(resolve, 0)); };
const click = (element) => { assert.ok(element, 'the actual control exists'); element.dispatchEvent(new MouseEvent('click', { bubbles: true })); };
const change = (control, value) => { control.value = value; control.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value } })); };

afterEach(async () => {
  document.body.replaceChildren();
  await flush(2);
  calls.length = 0;
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
    wait({ jobId: 'job-3', nodeId: 'Expr', mode: 'Fixture' }),
  ] }), MODEL);
  await flush(2);
  const articles = [...win.querySelectorAll('[data-simulation-wait]')];
  assert.equal(win.querySelector('[data-simulation-waits] h3').textContent, 'Waiting for a result: 3 steps');
  assert.deepEqual(articles.map((article) => article.querySelector('strong').textContent), ['Plain step', 'Checked step', 'Expression step']);
  assert.deepEqual(JSON.parse(articles[0].querySelector('[data-simulation-wait-input]').value), { seed: 7 });
  const outcomes = (article) => [...article.querySelectorAll('[data-answer-field="outcome"] option')].map((option) => option.value);
  assert.deepEqual(outcomes(articles[0]), ['Completed', 'Error']);
  assert.deepEqual(outcomes(articles[2]), ['Completed', 'Error', 'NeedsHuman', 'Cancelled']);
  assert.equal(articles[0].querySelector('[data-answer-field="verdict"]'), null);
  assert.ok(articles[1].querySelector('[data-answer-field="verdict"]'));
  assert.match(articles[2].querySelector('tf-chip').textContent, /prepared result applies/);
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
  assert.equal(win.querySelector('[data-error]').getAttribute('message'), 'The result for “Plain step” is not valid. Check the code, the summary and the outputs.');
  const again = win.querySelector('[data-simulation-wait]');
  assert.equal(again.querySelector('[data-answer-field="summary"]').value, 'kept text');
  assert.equal(again.querySelector('[data-answer-field="outputs"]').value, '{"keep": true}');
});

test('a verification task is decided with approved, a work task never sends it', async () => {
  responder = () => ({ view: baseView() });
  const tasks = [
    { userTaskId: 'ut-v', nodeId: 'Plain', name: 'Check it', kind: 'Verification', instructions: '', canComplete: true },
    { userTaskId: 'ut-w', nodeId: 'Plain', name: 'Do it', kind: 'Work', instructions: '', canComplete: true },
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
