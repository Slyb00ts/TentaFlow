// =============================================================================
// File: tests/e2e/process-b4-output-retry.spec.js
// Description: Apply an accepted service result again against a real node (BPMN B4.5).
//              The step's work succeeds and its result is accepted, but mapping the
//              result into a variable divides by zero, so the run stops at one
//              incident. "Apply result again" asks first and applies the pinned
//              result to the CURRENT variables without running the flow again: a
//              second failure keeps the same incident, and once the variable is
//              fixed the run completes with one service result in its history.
//              A person who may only watch the run is told so and gets no button.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { execFileSync } = require('child_process');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18521;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b4-output-retry');
const FLOW_GRAPH = JSON.stringify({
  nodes: [
    { id: 'trigger', type: 'trigger', config: {} },
    { id: 'output', type: 'output', config: {} },
  ],
  edges: [{ from: 'trigger', to: 'output', from_port: 'text', to_port: 'text' }],
});

let PORT;
let DB;
let proc;
let ids;

test.beforeAll(async ({ browser }, testInfo) => {
  test.skip(!binaryExists(), 'tentaflow binary not built (set TENTAFLOW_E2E_BINARY)');
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = path.join(RUNTIME, `node-${PORT}.db`);
  fs.mkdirSync(RUNTIME, { recursive: true });
  const config = renderConfig(path.join(RUNTIME, `config-${PORT}.toml`), PORT);
  proc = startBinary({ port: PORT, configFile: config, db: DB, home: path.join(RUNTIME, `home-${PORT}`) });
  await waitForServer(PORT);
  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  ids = await page.evaluate(async (graph) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    await ApiBinary.action('iamCreateUserRequest', { username: 'b45onlooker', password: 'Onlooker-pass-w0rd-1', displayName: 'b45onlooker', email: '', role: 'admin', groupIds: [] });
    const created = await ApiBinary.one('flowCreateRequest', { name: 'B45 flow', description: '', graphJson: graph });
    const flowId = String(created.id ?? created.flowId);
    await ApiBinary.action('flowUpdateRequest', { flowId, status: 'active' });
    const model = emptyProcessModel();
    model.targetNamespace = 'urn:b45:output';
    model.variables = { divisor: 0, noise: 0, ratio: null };
    model.nodes = [
      model.nodes[0],
      { id: 'Service_1', name: 'Check contract', kind: { ServiceTask: { flowId, inputMapping: {}, outputMapping: { ratio: '10 / vars.divisor' },
        verification: { Condition: { expression: 'true' } }, timeoutSeconds: 60 } } },
      model.nodes[1],
    ];
    model.sequenceFlows = [
      { id: 'F_0', sourceId: 'Start', targetId: 'Service_1', condition: null },
      { id: 'F_1', sourceId: 'Service_1', targetId: 'End', condition: null },
    ];
    model.diagram = {
      shapes: [{ elementId: 'Start', x: 40, y: 172, width: 56, height: 56 }, { elementId: 'Service_1', x: 160, y: 160, width: 120, height: 72 },
        { elementId: 'End', x: 340, y: 172, width: 56, height: 56 }],
      edges: [['F_0', 96, 160], ['F_1', 280, 340]].map(([id, from, to]) => ({ sequenceFlowId: id, waypoints: [{ x: from, y: 200 }, { x: to, y: 200 }] })),
    };
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0, name: 'B45 output retry process', description: '', model });
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

function sqlite(sql) {
  return execFileSync('/usr/bin/sqlite3', ['-cmd', '.timeout 5000', DB, sql], { encoding: 'utf8' }).trim();
}

async function readRun(page, instanceId) {
  return page.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { instance } = await ApiBinary.one('processInstanceGetRequest', { instanceId: id });
    const { events } = await ApiBinary.one('processHistoryRequest', { instanceId: id, afterSeq: 0, limit: 200 });
    return { status: instance.status, incidents: instance.incidents, variables: instance.variables, kinds: events.map((event) => event.kind) };
  }, instanceId);
}

// The work succeeds, its result is accepted, and mapping it divides by the
// variable's starting value 0: the run stops at one incident whose retry is
// the output mapping one.
async function blockedRun(page) {
  const instanceId = await page.evaluate(async ({ definitionId, processId }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const response = await ApiBinary.one('processInstanceStartRequest', {
      commandId: crypto.randomUUID(), definitionId, version: 1, processId, startNodeId: 'Start', variables: {} });
    return response.instance.instanceId;
  }, ids);
  await expect.poll(async () => (await readRun(page, instanceId)).incidents.length, { timeout: 60000 }).toBe(1);
  const run = await readRun(page, instanceId);
  expect(run.incidents[0].retry.kind).toBe('OutputMapping');
  expect(run.incidents[0].canRetry).toBe(false);
  expect(run.incidents[0].retry.deniedReason).toBe('VariablesUnchanged');
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

async function confirm(page) {
  const dialog = page.locator('tf-window', { hasText: 'Only copying the result into the run failed' }).last();
  await dialog.locator('tf-button[data-action="confirm"]').click({ timeout: 15000 });
}

test('Apply result again is offered only once something changed, asks first and says when it failed again', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await blockedRun(page);
  const incidentId = (await readRun(page, instanceId)).incidents[0].incidentId;
  const win = await openRun(page, instanceId);

  // Nothing changed since the failure: the sentence is about saved work, and there is nothing to press.
  await expect(win.locator('[data-incidents]')).toContainText('The work finished and its result is saved; copying it into the variables failed.', { timeout: 15000 });
  await expect(win.locator('[data-incidents]')).not.toContainText('CEL');
  await expect(win.locator('[data-incidents] [data-retry-note]')).toContainText('Nothing it depends on has changed since', { timeout: 15000 });
  await expect(win.locator('[data-incidents] [data-retry]')).toHaveCount(0);
  const refusedEarly = await page.evaluate(async ({ id, jobId }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { instance } = await ApiBinary.one('processInstanceGetRequest', { instanceId: id });
    try {
      await ApiBinary.one('processJobRetryRequest', { commandId: crypto.randomUUID(), instanceId: id, jobId, expectedRevision: instance.revision });
      return 'accepted';
    } catch (error) { return error.code ?? String(error.message); }
  }, { id: instanceId, jobId: (await readRun(page, instanceId)).incidents[0].jobId });
  expect(refusedEarly).toMatch(/PolicyDenied/);
  expect((await readRun(page, instanceId)).kinds).not.toContain('output_retry_failed');

  // A variable the mapping does not use moved: the press is offered, asks first, and fails again honestly.
  sqlite(`UPDATE bpmn_instances SET variables_json=json_set(variables_json,'$.noise',1) WHERE instance_id='${instanceId}'`);
  const button = win.locator('[data-incidents] [data-retry] button');
  await expect(button).toHaveText('Apply result again', { timeout: 15000 });
  await button.click();
  await confirm(page);
  await expect(page.locator('.toast', { hasText: 'It failed again' }).first()).toBeVisible({ timeout: 15000 });
  await expect(page.locator('.toast', { hasText: 'The result was applied again.' })).toHaveCount(0);

  // The same incident stays, nothing new is raised, the tool did not run again, and the press is held back again.
  await expect.poll(async () => (await readRun(page, instanceId)).kinds.includes('output_retry_failed'), { timeout: 30000 }).toBe(true);
  const failed = await readRun(page, instanceId);
  expect(failed.status).toBe('Incident');
  expect(failed.incidents.map((incident) => incident.incidentId)).toEqual([incidentId]);
  expect(failed.kinds.filter((kind) => kind === 'service_result')).toHaveLength(1);
  expect(sqlite(`SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id='${instanceId}'`)).toBe('1');
  await expect(win.locator('[data-incidents] [data-retry]')).toHaveCount(0, { timeout: 15000 });
  await expect(win.locator('[data-incidents] [data-retry-note]')).toContainText('Nothing it depends on has changed since');

  // The token of the blocked step is not "ready to continue".
  await win.locator('[data-view] button[data-value="diagram"]').click();
  await expect(win.locator('.fb-canvas.fb-runtime')).toHaveCount(1, { timeout: 15000 });
  await win.locator('.fb-canvas .fb-node[data-node-id="Service_1"]').click();
  await expect(win.locator('.fb-diagram-panel')).not.toContainText('Ready to continue');
  await win.locator('[data-view] button[data-value="list"]').click();

  // Fixing the variable is what lets the pinned result map.
  sqlite(`UPDATE bpmn_instances SET variables_json=json_set(variables_json,'$.divisor',5) WHERE instance_id='${instanceId}'`);
  await expect(win.locator('[data-incidents] [data-retry] button')).toBeVisible({ timeout: 15000 });
  await win.locator('[data-incidents] [data-retry] button').click();
  await confirm(page);
  await expect(page.locator('.toast', { hasText: 'The result was applied again.' }).first()).toBeVisible({ timeout: 15000 });
  await expect.poll(async () => (await readRun(page, instanceId)).status, { timeout: 60000 }).toBe('Completed');
  const done = await readRun(page, instanceId);
  expect(done.variables.ratio).toBe(2);
  expect(done.kinds).toContain('output_retried');
  expect(done.kinds.filter((kind) => kind === 'service_result')).toHaveLength(1);
  expect(done.incidents.every((incident) => incident.canRetry === false)).toBe(true);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a person who may only watch the run is told so in words and the server refuses the same call', async ({ page, browser }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await blockedRun(page);
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1400, height: 1000 } });
  const onlooker = await context.newPage();
  await loginAsAdmin(onlooker, { port: PORT, username: 'b45onlooker', password: 'Onlooker-pass-w0rd-1' });
  const win = await openRun(onlooker, instanceId);
  await expect(win.locator('[data-incidents] [data-retry-note]')).toHaveText(/only the person who started the run can do that/i, { timeout: 15000 });
  await expect(win.locator('[data-incidents] [data-retry]')).toHaveCount(0);

  const refused = await onlooker.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { instance } = await ApiBinary.one('processInstanceGetRequest', { instanceId: id });
    try {
      await ApiBinary.one('processJobRetryRequest', { commandId: crypto.randomUUID(), instanceId: id, jobId: instance.incidents[0].jobId, expectedRevision: instance.revision });
      return 'accepted';
    } catch (error) { return error.code ?? String(error.message); }
  }, instanceId);
  expect(refused).toMatch(/PolicyDenied/);
  expect((await readRun(page, instanceId)).kinds).not.toContain('output_retried');
  await context.close();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
