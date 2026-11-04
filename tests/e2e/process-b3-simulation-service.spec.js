// =============================================================================
// File: tests/e2e/process-b3-simulation-service.spec.js
// Description: Simulated Service steps (BPMN B3.7a) in the designer against a
//              real node. A published process runs script -> Service -> user
//              task: the Simulate window lists the Service step as waiting with
//              its input, the operator sends a result, the human verification is
//              approved and the run completes. A second run takes a prepared
//              result chosen in the start window and never waits; invalid JSON
//              in an output is told in the form, in the wait panel and in the
//              fixture, and sends nothing. Running a step for real is listed
//              but cannot be chosen.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18491;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b3-simulation-service');
const FLOW_NAME = 'B3 simulation flow';
const PROCESS_NAME = 'B3 simulation process';
const FLOW_GRAPH = JSON.stringify({
  nodes: [
    { id: 'trigger', type: 'trigger', config: {} },
    { id: 'output', type: 'output', config: {} },
  ],
  edges: [{ from: 'trigger', to: 'output', from_port: 'text', to_port: 'text' }],
});

let PORT;
let proc;
let definitionId;

test.beforeAll(async ({ browser }, testInfo) => {
  test.skip(!binaryExists(), 'tentaflow binary not built (set TENTAFLOW_E2E_BINARY)');
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  fs.mkdirSync(RUNTIME, { recursive: true });
  const config = renderConfig(path.join(RUNTIME, `config-${PORT}.toml`), PORT);
  proc = startBinary({
    port: PORT, configFile: config, db: path.join(RUNTIME, `node-${PORT}.db`),
    home: path.join(RUNTIME, `home-${PORT}`),
  });
  await waitForServer(PORT);
  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  definitionId = await page.evaluate(async ({ flowName, graph, processName }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const created = await ApiBinary.one('flowCreateRequest', { name: flowName, description: '', graphJson: graph });
    const flowId = String(created.id ?? created.flowId);
    await ApiBinary.action('flowUpdateRequest', { flowId, status: 'active' });

    // Start -> Script -> Service -> User -> End, each node with its shape so the designer draws it.
    const model = emptyProcessModel();
    model.variables = { value: 1 };
    const place = (id, x) => ({ elementId: id, x, y: 160, width: 120, height: 72 });
    model.nodes = [
      model.nodes[0],
      { id: 'Script_1', name: 'Add one', kind: { ScriptTask: { script: 'vars.value + 1', outputMapping: { answer: 'outputs' } } } },
      { id: 'Service_1', name: 'Check contract', kind: { ServiceTask: { flowId, inputMapping: { seed: 'vars.answer' },
        outputMapping: { service_answer: 'outputs.answer' }, verification: 'Human', timeoutSeconds: 60 } } },
      { id: 'User_1', name: 'Sign off', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } },
      model.nodes[1],
    ];
    model.sequenceFlows = [
      { id: 'F_1', sourceId: 'Start', targetId: 'Script_1', condition: null },
      { id: 'F_2', sourceId: 'Script_1', targetId: 'Service_1', condition: null },
      { id: 'F_3', sourceId: 'Service_1', targetId: 'User_1', condition: null },
      { id: 'F_4', sourceId: 'User_1', targetId: 'End', condition: null },
    ];
    model.diagram = {
      shapes: [{ elementId: 'Start', x: 40, y: 172, width: 56, height: 56 }, place('Script_1', 140), place('Service_1', 300),
        place('User_1', 460), { elementId: 'End', x: 620, y: 172, width: 56, height: 56 }],
      edges: [['F_1', 96, 140], ['F_2', 260, 300], ['F_3', 420, 460], ['F_4', 580, 620]].map(([id, from, to]) =>
        ({ sequenceFlowId: id, waypoints: [{ x: from, y: 200 }, { x: to, y: 200 }] })),
    };
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0,
      name: processName, description: '', model,
    });
    const id = saved.definition.definitionId;
    await ApiBinary.one('processDefinitionPublishRequest', {
      commandId: crypto.randomUUID(), definitionId: id, expectedRevision: saved.definition.draftRevision,
    });
    return id;
  }, { flowName: FLOW_NAME, graph: FLOW_GRAPH, processName: PROCESS_NAME });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

test.use({ viewport: { width: 1600, height: 1000 } });

async function openSimulationForm(page) {
  await loginAsAdmin(page, { port: PORT });
  await page.evaluate(async (target) => {
    const { openFlowBuilder } = await import('/js/modules/flows-builder.js');
    openFlowBuilder(target, { mode: 'bpmn' });
  }, definitionId);
  await expect(page.locator('.fb-canvas')).toBeVisible({ timeout: 15000 });
  await page.locator('[data-role="more"]').click();
  await expect(page.locator('tf-menu[data-role="more-menu"] .tf-menu.open')).toHaveCount(1);
  await page.locator('tf-menu[data-role="more-menu"] [data-role="simulate"]').click();
  const form = page.locator('tf-window.tf-act-window').last();
  await expect(form.locator('[data-fixture-step="Service_1"]')).toBeVisible({ timeout: 15000 });
  return form;
}

const simulation = (page) => page.locator('tf-window.fb-process-window').last();

// Advances the virtual clock until the predicate holds, one press at a time.
async function advanceUntil(page, ready) {
  const win = simulation(page);
  await expect(win.locator('[data-simulation-advance]')).toBeVisible({ timeout: 15000 });
  for (let attempt = 0; attempt < 12; attempt += 1) {
    if (await ready(win)) return;
    await win.locator('[data-simulation-advance]:not([disabled])').click();
    await page.waitForTimeout(400);
  }
  expect(await ready(win)).toBe(true);
}

const setEditor = (locator, value) => locator.evaluate((editor, text) => {
  editor.value = text;
  editor.dispatchEvent(new Event('input', { bubbles: true }));
}, value);

test('the operator answers a waiting Service step, approves its verification and finishes the run', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const form = await openSimulationForm(page);
  // Real execution is listed and cannot be chosen.
  const real = form.locator('[data-fixture-step="Service_1"] [data-fixture-mode] option[value="RealExecution"]');
  await expect(real).toBeDisabled();
  await expect(form.locator('[data-fixture-step="Service_1"] [data-fixture-mode]')).toHaveAttribute('hint', /not available yet/);
  await form.locator('[data-act="submit"]').click();
  await expect(simulation(page).locator('[data-simulation-content]')).toBeVisible({ timeout: 15000 });

  const win = simulation(page);
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-wait]').count()) === 1);
  const wait = win.locator('[data-simulation-wait]');
  await expect(wait.locator('strong').first()).toHaveText('Check contract');
  await expect(wait.locator('tf-chip')).toContainText('Waiting for your result');
  // The input is what the step would have been given: the mapped variables, where the script added one.
  const input = JSON.parse(await wait.locator('[data-simulation-wait-input]').evaluate((editor) => editor.value));
  expect(input.variables).toEqual({ seed: 2 });
  expect(input.payload).toEqual({ answer: 2, value: 1 });
  // A step with no result expression can only complete or fail.
  const outcomes = await wait.locator('[data-answer-field="outcome"] option').evaluateAll((list) => list.map((option) => option.value));
  expect(outcomes).toEqual(['Completed', 'Error']);
  expect(await win.locator('[data-simulation-wait] [data-answer-field="verdict"]').count()).toBe(0);
  const eventsBefore = await win.locator('.fb-simulation-events li').count();

  // Invalid JSON is told in the form and nothing is sent: the step still waits.
  await setEditor(wait.locator('[data-answer-field="outputs"]'), '{"answer":');
  await wait.locator('[data-simulation-resolve]').click();
  await expect(wait.locator('[data-answer-error="outputs"]')).toBeVisible();
  await expect(wait.locator('[data-answer-error="outputs"]')).toHaveAttribute('message', 'Enter valid JSON.');
  await expect(win.locator('[data-simulation-wait]')).toHaveCount(1);
  expect(await win.locator('.fb-simulation-events li').count()).toBe(eventsBefore);

  // A valid result resolves the step and the trace advances.
  await wait.locator('[data-answer-field="summary"] input').fill('Contract looks fine');
  await setEditor(wait.locator('[data-answer-field="outputs"]'), '{"answer": 42}');
  await wait.locator('[data-simulation-resolve]').click();
  await expect(win.locator('[data-simulation-wait]')).toHaveCount(0, { timeout: 15000 });
  await expect.poll(() => win.locator('.fb-simulation-events li').count()).toBeGreaterThan(eventsBefore);
  await expect(win.locator('.fb-simulation-events')).toContainText('Check contract');

  // Human verification opens a decision, not a form of outputs.
  const decision = win.locator('.fb-simulation-task', { hasText: 'Verification' });
  await expect(decision).toBeVisible({ timeout: 15000 });
  await expect(decision.locator('tf-code-editor')).toHaveCount(0);
  await expect(decision.locator('tf-button')).toHaveText(['Approve', 'Reject']);
  await decision.locator('[data-simulation-decide="true"]').click();

  const work = win.locator('.fb-simulation-task', { hasText: 'Sign off' });
  await expect(work).toBeVisible({ timeout: 15000 });
  await expect(work.locator('[data-simulation-decide]')).toHaveCount(0);
  await work.locator('[data-simulation-task]').click();
  await expect(win.locator('[data-simulation-content]')).toContainText('Completed', { timeout: 15000 });
  await expect(win.locator('[data-error]')).toBeHidden();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a prepared result is chosen at start, applies on advance and never waits for the operator', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const form = await openSimulationForm(page);
  const row = form.locator('[data-fixture-step="Service_1"]');
  await expect(row.locator('[data-fixture-form]')).toBeHidden();
  await row.locator('[data-fixture-mode] select').selectOption('Fixture');
  await expect(row.locator('[data-fixture-form]')).toBeVisible();

  // Invalid JSON in the prepared result keeps the window open and marks the field.
  await setEditor(row.locator('[data-answer-field="outputs"]'), '{"answer":');
  await form.locator('[data-act="submit"]').click();
  await expect(row.locator('[data-answer-error="outputs"]')).toBeVisible();
  await expect(simulation(page)).toHaveCount(0);

  await row.locator('[data-answer-field="summary"] input').fill('Prepared answer');
  await setEditor(row.locator('[data-answer-field="outputs"]'), '{"answer": 9}');
  await form.locator('[data-act="submit"]').click();
  const win = simulation(page);
  await expect(win.locator('[data-simulation-content]')).toBeVisible({ timeout: 15000 });
  // The step may be listed as waiting until the clock moves, but it is the prepared result that applies.
  await advanceUntil(page, async (current) => (await current.locator('.fb-simulation-task', { hasText: 'Verification' }).count()) === 1);
  await expect(win.locator('[data-simulation-wait]')).toHaveCount(0);
  await expect(win.locator('.fb-simulation-events')).toContainText('Check contract');
  await win.locator('[data-simulation-decide="false"]').click();
  // A rejected verification is an incident of the run, not a crash of the window.
  await expect(win.locator('[data-error]')).toBeHidden();
  await expect(win.locator('[data-simulation-content]')).toBeVisible();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
