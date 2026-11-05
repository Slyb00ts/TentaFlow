// =============================================================================
// File: tests/e2e/process-b3-simulation-structure.spec.js
// Description: Sub-processes, repetitions and process calls in a simulated run
//              (BPMN B3.7c) in the designer against a real node. A Service
//              step inside a sub-process waits for the operator and the scope
//              closes after it; a parallel multi-instance task shows its
//              occurrences and counts them off; a process that calls another
//              published process shows the child in the run structure, the
//              child's task is completed there and the parent continues; a
//              call to a process archived before Start is refused in words.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18499;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b3-simulation-structure');
const FLOW_GRAPH = JSON.stringify({
  nodes: [{ id: 'trigger', type: 'trigger', config: {} }, { id: 'output', type: 'output', config: {} }],
  edges: [{ from: 'trigger', to: 'output', from_port: 'text', to_port: 'text' }],
});

let PORT;
let proc;
let ids;

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
  ids = await page.evaluate(async (graph) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const created = await ApiBinary.one('flowCreateRequest', { name: 'B3 structure flow', description: '', graphJson: graph });
    const flowId = String(created.id ?? created.flowId);
    await ApiBinary.action('flowUpdateRequest', { flowId, status: 'active' });

    // Start, the given nodes in a row, End; every node gets a shape so the designer can draw it.
    const build = (nodes, variables = {}) => {
      const model = emptyProcessModel();
      model.variables = variables;
      model.targetNamespace = 'urn:b3:structure';
      model.nodes = [model.nodes[0], ...nodes, model.nodes[1]];
      model.sequenceFlows = model.nodes.slice(1).map((node, index) => ({
        id: `F_${index + 1}`, sourceId: model.nodes[index].id, targetId: node.id, condition: null }));
      const x = new Map(model.nodes.map((node, index) => [node.id, 40 + index * 160]));
      const round = (node) => node.id === 'Start' || node.id === 'End';
      model.diagram = {
        shapes: model.nodes.map((node) => ({ elementId: node.id, x: x.get(node.id), y: 160, width: round(node) ? 56 : 120, height: round(node) ? 56 : 72 })),
        edges: model.sequenceFlows.map((flow) => ({ sequenceFlowId: flow.id, waypoints: [{ x: x.get(flow.sourceId) + 60, y: 190 }, { x: x.get(flow.targetId), y: 190 }] })),
      };
      return model;
    };
    const create = async (name, model, { publish = true } = {}) => {
      const saved = await ApiBinary.one('processDefinitionSaveRequest', {
        commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0, name, description: '', model });
      const id = saved.definition.definitionId;
      let revision = saved.definition.draftRevision;
      if (publish) {
        const published = await ApiBinary.one('processDefinitionPublishRequest', { commandId: crypto.randomUUID(), definitionId: id, expectedRevision: revision });
        revision = published.definition?.draftRevision ?? revision;
      }
      return { id, model };
    };
    const service = (id, name) => ({ id, name, kind: { ServiceTask: { flowId, inputMapping: {}, outputMapping: {}, verification: 'Human', timeoutSeconds: 60 } } });
    const user = (id, name, extra = {}) => ({ id, name, kind: { UserTask: { assigneeUserId: null, outputMapping: {} } }, ...extra });
    const call = (id, name, child) => ({ id, name, kind: { CallActivity: { calledDefinitionId: child.id, calledVersion: 1,
      calledElement: { namespaceUri: child.model.targetNamespace, processId: child.model.processId }, inputMapping: {}, outputMapping: {} } } });

    const sub = {
      id: 'Sub_1', name: 'Check order',
      kind: { SubProcess: { body: {
        nodes: [{ id: 'InStart', name: '', kind: 'Start' }, service('InSvc', 'Inner check'), { id: 'InEnd', name: '', kind: 'End' }],
        sequenceFlows: [{ id: 'InF_1', sourceId: 'InStart', targetId: 'InSvc', condition: null }, { id: 'InF_2', sourceId: 'InSvc', targetId: 'InEnd', condition: null }],
        variables: {}, diagram: { shapes: [], edges: [] } }, inputMapping: {}, outputMapping: {} } },
    };
    const scope = await create('B3 structure scope', build([sub]));
    const repeated = await create('B3 structure repeat', build([user('Review', 'Review item', {
      repeat: { MultiInstance: { mode: 'Parallel', input: { CollectionExpression: { expression: 'vars.items' } }, outputCollectionVariable: 'results' } } })],
    { items: ['a', 'b', 'c'], results: [] }));
    const child = await create('B3 structure child', build([user('Approve', 'Approve payment')]));
    const parent = await create('B3 structure parent', build([call('Call_1', 'Take payment', child), user('After', 'Confirm order')]));
    const lost = await create('B3 structure callee gone', build([user('Approve', 'Approve payment')]));
    const caller = await create('B3 structure caller of a gone callee', build([call('Call_1', 'Take payment', lost)]));
    await ApiBinary.one('processDefinitionArchiveRequest', { commandId: crypto.randomUUID(), definitionId: lost.id, expectedRevision: 0, archived: true })
      .catch(async () => {
        const current = await ApiBinary.one('processDefinitionGetRequest', { definitionId: lost.id });
        await ApiBinary.one('processDefinitionArchiveRequest', { commandId: crypto.randomUUID(), definitionId: lost.id,
          expectedRevision: current.definition.draftRevision, archived: true });
      });
    return { scope: scope.id, repeated: repeated.id, parent: parent.id, caller: caller.id };
  }, FLOW_GRAPH);
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

test.use({ viewport: { width: 1600, height: 1200 } });

const simulation = (page) => page.locator('tf-window.fb-process-window').last();

async function openForm(page, definitionId) {
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
  await expect(form.locator('[data-simulation-start]')).toBeVisible({ timeout: 15000 });
  return form;
}

async function startSimulation(page, definitionId) {
  const form = await openForm(page, definitionId);
  await form.locator('[data-act="submit"]').click();
  await expect(simulation(page).locator('[data-simulation-content]')).toBeVisible({ timeout: 15000 });
  return simulation(page);
}

async function advanceUntil(page, ready) {
  const win = simulation(page);
  for (let attempt = 0; attempt < 14; attempt += 1) {
    if (await ready(win)) return;
    const button = win.locator('[data-simulation-advance]:not([disabled])');
    if (!(await button.count())) break;
    await button.click();
    await page.waitForTimeout(400);
  }
  expect(await ready(win)).toBe(true);
}

const count = (selector) => async (win) => win.locator(selector).count();
const setEditor = (locator, value) => locator.evaluate((editor, text) => {
  editor.value = text;
  editor.dispatchEvent(new Event('input', { bubbles: true }));
}, value);

test('a Service step inside a sub-process waits for the operator and the scope closes after it', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const win = await startSimulation(page, ids.scope);
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-wait]').count()) === 1);
  const scope = win.locator('[data-simulation-scope]');
  await expect(scope).toHaveCount(1);
  await expect(scope).toContainText('Check order');
  await expect(scope).not.toContainText('Completed');
  const wait = win.locator('[data-simulation-wait]');
  await expect(wait.locator('strong').first()).toHaveText('Inner check');
  await setEditor(wait.locator('[data-answer-field="outputs"]'), '{"ok": 1}');
  await wait.locator('[data-simulation-resolve]').click();
  const decision = win.locator('[data-simulation-open-task] [data-simulation-decide="true"]');
  await expect(decision).toBeVisible({ timeout: 15000 });
  await decision.click();
  await expect(decision).toHaveCount(0, { timeout: 15000 });
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-status="Completed"]').count()) === 1);
  await expect(win.locator('[data-simulation-scope]')).toContainText('Completed');
  await expect(win.locator('.fb-simulation-events')).toContainText('Sub-process completed');
  await expect(win.locator('[data-error]')).toBeHidden();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a parallel multi-instance task shows its occurrences and counts them off', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const win = await startSimulation(page, ids.repeated);
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-open-task]').count()) >= 1);
  const group = win.locator('[data-simulation-group]');
  await expect(group).toHaveCount(1);
  await expect(group).toContainText('Review item');
  await expect(group).toContainText('Multiple instances, in parallel');
  await expect(group.locator('[data-group-progress]')).toContainText('0 of 3 occurrences done');
  await expect(group.locator('[data-simulation-occurrence]')).toHaveCount(3);
  await expect(win.locator('[data-simulation-open-task]')).toHaveCount(3);
  await win.locator('[data-simulation-open-task]').first().locator('[data-simulation-task]').click();
  await expect(group.locator('[data-group-progress]')).toContainText('1 of 3 occurrences done', { timeout: 15000 });
  for (let remaining = 2; remaining > 0; remaining -= 1) {
    await win.locator('[data-simulation-open-task]').first().locator('[data-simulation-task]').click();
    await expect(win.locator('[data-simulation-open-task]')).toHaveCount(remaining - 1, { timeout: 15000 });
  }
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-status="Completed"]').count()) === 1);
  await expect(group.locator('[data-group-progress]')).toContainText('3 of 3 occurrences done');
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a process call runs the child in the same window, its task is completed there and the parent continues', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const win = await startSimulation(page, ids.parent);
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-instance]').count()) === 2);
  const [root, child] = await win.locator('[data-simulation-instance]').all();
  await expect(root).toHaveAttribute('data-depth', '0');
  await expect(child).toHaveAttribute('data-depth', '1');
  await expect(child).toContainText('B3 structure child v1');
  await expect(child.locator('[data-simulation-call]')).toContainText('Called by “Take payment”');
  await expect(child.locator('[data-simulation-call]')).toContainText('Waiting');
  // The child's task names its process, because the run now has two.
  const task = win.locator('[data-simulation-open-task]', { hasText: 'Approve payment' });
  await expect(task.locator('[data-instance-label]')).toHaveText('B3 structure child v1');
  await task.locator('[data-simulation-task]').click();
  await expect(task).toHaveCount(0, { timeout: 15000 });
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-open-task]', { hasText: 'Confirm order' }).count()) === 1);
  await expect(win.locator('[data-simulation-instance]').nth(1).locator('[data-simulation-call]')).toContainText('Returned');
  await expect(win.locator('.fb-simulation-events')).toContainText('Called process returned');
  const confirm = win.locator('[data-simulation-open-task]', { hasText: 'Confirm order' });
  await confirm.locator('[data-simulation-task]').click();
  await expect(confirm).toHaveCount(0, { timeout: 15000 });
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-status="Completed"]').count()) === 1);
  await expect(win.locator('[data-error]')).toBeHidden();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a call to a process archived before Start is refused in words and nothing starts', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  const form = await openForm(page, ids.caller);
  await form.locator('[data-act="submit"]').click();
  await expect(form.locator('.tf-act__error')).toContainText('cannot be fixed into the simulation', { timeout: 15000 });
  await expect(form.locator('.tf-act__error')).not.toContainText('SIMULATION_');
  await expect(page.locator('tf-window.fb-process-window')).toHaveCount(0);
  expect(errors.filter((line) => !/CALL_PIN|BadRequest|Failed to load resource/.test(line)), diagnostics(errors, proc)).toEqual([]);
});
