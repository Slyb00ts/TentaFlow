// =============================================================================
// File: tests/e2e/process-b4-retry.spec.js
// Description: Retry of a failed service task against a real node (BPMN B4.4).
//              The flow behind the task is switched off when the run starts, so
//              the worker fails the task before any call (an incident whose
//              task is proven to have done nothing). While the flow is off the
//              incident says why there is no retry; once it is back, Retry is
//              offered in the incident list and in the diagram panel, asks first,
//              starts a new attempt in the same run, and a person who may only
//              watch the run is told that in words and gets no button.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18511;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b4-retry');
const FLOW_GRAPH = JSON.stringify({
  nodes: [
    { id: 'trigger', type: 'trigger', config: {} },
    { id: 'output', type: 'output', config: {} },
  ],
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
    await ApiBinary.action('iamCreateUserRequest', { username: 'b4onlooker', password: 'Onlooker-pass-w0rd-1', displayName: 'b4onlooker', email: '', role: 'admin', groupIds: [] });
    const created = await ApiBinary.one('flowCreateRequest', { name: 'B4 retry flow', description: '', graphJson: graph });
    const flowId = String(created.id ?? created.flowId);
    await ApiBinary.action('flowUpdateRequest', { flowId, status: 'active' });
    const model = emptyProcessModel();
    model.targetNamespace = 'urn:b4:retry';
    model.nodes = [
      model.nodes[0],
      { id: 'User_1', name: 'Prepare', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } },
      { id: 'Service_1', name: 'Check contract', kind: { ServiceTask: { flowId, inputMapping: {}, outputMapping: {},
        verification: { Condition: { expression: 'true' } }, timeoutSeconds: 60 } } },
      model.nodes[1],
    ];
    model.sequenceFlows = [
      { id: 'F_0', sourceId: 'Start', targetId: 'User_1', condition: null },
      { id: 'F_1', sourceId: 'User_1', targetId: 'Service_1', condition: null },
      { id: 'F_2', sourceId: 'Service_1', targetId: 'End', condition: null },
    ];
    model.diagram = {
      shapes: [{ elementId: 'Start', x: 40, y: 172, width: 56, height: 56 }, { elementId: 'User_1', x: 140, y: 160, width: 120, height: 72 },
        { elementId: 'Service_1', x: 320, y: 160, width: 120, height: 72 }, { elementId: 'End', x: 500, y: 172, width: 56, height: 56 }],
      edges: [['F_0', 96, 140], ['F_1', 260, 320], ['F_2', 440, 500]].map(([id, from, to]) => ({ sequenceFlowId: id, waypoints: [{ x: from, y: 200 }, { x: to, y: 200 }] })),
    };
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0, name: 'B4 retry process', description: '', model });
    await ApiBinary.one('processDefinitionPublishRequest', {
      commandId: crypto.randomUUID(), definitionId: saved.definition.definitionId, expectedRevision: saved.definition.draftRevision });
    return { flowId, definitionId: saved.definition.definitionId, processId: model.processId };
  }, FLOW_GRAPH);
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

test.use({ viewport: { width: 1400, height: 1000 } });

const instanceWindow = (page) => page.locator('tf-window.fb-process-window').last();

async function setFlow(page, status) {
  await page.evaluate(async ({ flowId, value }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    await ApiBinary.action('flowUpdateRequest', { flowId, status: value });
  }, { flowId: ids.flowId, value: status });
}

async function readRun(page, instanceId) {
  return page.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { instance } = await ApiBinary.one('processInstanceGetRequest', { instanceId: id });
    const { events } = await ApiBinary.one('processHistoryRequest', { instanceId: id, afterSeq: 0, limit: 200 });
    return { status: instance.status, incidents: instance.incidents, kinds: events.map((event) => event.kind) };
  }, instanceId);
}

// The flow behind a service task is checked when the step is admitted and again
// when the worker claims the job, and nothing else lets the two differ. So the
// flow is switched off in the same burst that finishes the person's task before
// it: when the switch wins the race against the worker's claim, the worker finds
// the flow gone before it makes any call. A run that got through is left behind
// and another one tries, so the helper ends only with a run that failed so.
async function runFailedBeforeAnyCall(page) {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    await setFlow(page, 'active');
    const instanceId = await page.evaluate(async ({ definitionId, processId, flowId }) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const response = await ApiBinary.one('processInstanceStartRequest', {
        commandId: crypto.randomUUID(), definitionId, version: 1, processId, startNodeId: 'Start', variables: {} });
      const id = response.instance.instanceId;
      const { instance } = await ApiBinary.one('processInstanceGetRequest', { instanceId: id });
      const task = instance.userTasks.find((candidate) => candidate.status === 'Open');
      await Promise.allSettled([
        ApiBinary.one('processUserTaskCompleteRequest', { commandId: crypto.randomUUID(), instanceId: id,
          userTaskId: task.userTaskId, expectedRevision: instance.revision, outputs: {}, approved: null }),
        ApiBinary.action('flowUpdateRequest', { flowId, status: 'draft' }),
      ]);
      return id;
    }, ids);
    for (let wait = 0; wait < 20; wait += 1) {
      const run = await readRun(page, instanceId);
      if (run.incidents.length === 1) return instanceId;
      if (run.status === 'Completed') break;
      await page.waitForTimeout(250);
    }
  }
  throw new Error('no run failed before its first call in 40 attempts');
}

// The same run with the flow switched on again.
async function failedRun(page) {
  const instanceId = await runFailedBeforeAnyCall(page);
  await setFlow(page, 'active');
  return instanceId;
}

async function openRun(page, instanceId) {
  await page.evaluate(async (id) => {
    const { openProcessInstance } = await import('/js/modules/flows-builder/process-monitor.js');
    await openProcessInstance(id);
  }, instanceId);
  const win = instanceWindow(page);
  await expect(win.locator('[data-incidents]')).toBeVisible({ timeout: 15000 });
  return win;
}

async function confirmRetry(page) {
  const dialog = page.locator('tf-window', { hasText: 'did nothing outside' }).last();
  await dialog.locator('tf-button[data-action="confirm"]').click({ timeout: 15000 });
}

async function expectRetried(page, instanceId) {
  await expect(page.locator('.toast', { hasText: 'Retry started' }).first()).toBeVisible({ timeout: 15000 });
  await expect.poll(async () => (await readRun(page, instanceId)).kinds.includes('job_retried'), { timeout: 30000 }).toBe(true);
  // The new attempt runs the flow now that it is on again, and the run goes on.
  await expect.poll(async () => (await readRun(page, instanceId)).status, { timeout: 60000 }).toBe('Completed');
  const run = await readRun(page, instanceId);
  expect(run.incidents).toEqual([]);
}

test('the incident list says why there is no retry, then offers it, asks first and starts a new attempt', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await runFailedBeforeAnyCall(page);
  const win = await openRun(page, instanceId);

  // The flow is off: no button, and the reason is a sentence, not a code.
  const note = win.locator('[data-incidents] [data-retry-note]');
  await expect(note).toHaveText(/can no longer use the flow, block, tool or agent/, { timeout: 15000 });
  await expect(note).not.toContainText(/SourceUnavailable|SOURCE_ACCESS/);
  await expect(win.locator('[data-incidents] [data-retry]')).toHaveCount(0);

  // The flow is back: the next read offers the retry.
  await setFlow(page, 'active');
  const retry = win.locator('[data-incidents] [data-retry] button');
  await expect(retry).toBeVisible({ timeout: 15000 });
  await expect(win.locator('[data-incidents] [data-retry-note]')).toHaveCount(0);
  await retry.focus();
  await retry.click();
  await confirmRetry(page);
  await expectRetried(page, instanceId);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('the diagram panel offers the same retry on the failed element', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await failedRun(page);
  const win = await openRun(page, instanceId);
  await win.locator('[data-view] button[data-value="diagram"]').click();
  await expect(win.locator('.fb-canvas.fb-runtime')).toHaveCount(1, { timeout: 15000 });
  await win.locator('.fb-canvas .fb-node[data-node-id="Service_1"]').click();
  const panel = win.locator('.fb-diagram-panel');
  const retry = panel.locator('[data-diagram-retry] button');
  await expect(retry).toBeVisible({ timeout: 15000 });
  await expect(panel.locator('tf-button button', { hasText: 'Inspect incident' })).toBeVisible();
  await retry.click();
  await confirmRetry(page);
  await expectRetried(page, instanceId);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a person who may only watch the run is told so in words and gets no retry', async ({ page, browser }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await failedRun(page);
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1400, height: 1000 } });
  const onlooker = await context.newPage();
  await loginAsAdmin(onlooker, { port: PORT, username: 'b4onlooker', password: 'Onlooker-pass-w0rd-1' });
  const win = await openRun(onlooker, instanceId);
  await expect(win.locator('[data-incidents] [data-retry-note]')).toHaveText(/only the person who started it or the process owner can/i, { timeout: 15000 });
  await expect(win.locator('[data-incidents] [data-retry]')).toHaveCount(0);

  // The server agrees with what the window shows.
  const refused = await onlooker.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { instance } = await ApiBinary.one('processInstanceGetRequest', { instanceId: id });
    try {
      await ApiBinary.one('processJobRetryRequest', { commandId: crypto.randomUUID(), instanceId: id, jobId: instance.incidents[0].jobId, expectedRevision: instance.revision });
      return 'accepted';
    } catch (error) { return error.code ?? String(error.message); }
  }, instanceId);
  expect(refused).toMatch(/PolicyDenied/);
  expect((await readRun(page, instanceId)).kinds).not.toContain('job_retried');
  await context.close();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
