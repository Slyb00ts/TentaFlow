// =============================================================================
// File: tests/e2e/process-b3-inspector.spec.js
// Description: The BPMN designer's Service task inspector against a real node:
//              a flow is dropped from the palette, the inspector tabs are
//              switched, an error boundary event is added from the "Result and
//              errors" tab, the process is saved and reopened, and the routing
//              table is derived again from the stored model.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18471;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b3-inspector');
const FLOW_NAME = 'B3 inspector flow';
const PROCESS_NAME = 'B3 inspector process';
const FLOW_GRAPH = JSON.stringify({
  nodes: [
    { id: 'trigger', type: 'trigger', config: {} },
    { id: 'output', type: 'output', config: {} },
  ],
  edges: [{ from: 'trigger', to: 'output', from_port: 'text', to_port: 'text' }],
});
const RESULT_EXPRESSION = '{"outcome": "Completed", "code": null, "summary": "ok", "outputs": {}, "evidence": []}';

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
  definitionId = await page.evaluate(async ({ flowName, processName, graph }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const created = await ApiBinary.one('flowCreateRequest', { name: flowName, description: '', graphJson: graph });
    const flowId = created.id ?? created.flowId;
    await ApiBinary.action('flowUpdateRequest', { flowId: String(flowId), status: 'active' });
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0,
      name: processName, description: '', model: emptyProcessModel(),
    });
    return saved.definition.definitionId;
  }, { flowName: FLOW_NAME, processName: PROCESS_NAME, graph: FLOW_GRAPH });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

async function openDesigner(page, { login = true } = {}) {
  if (login) await loginAsAdmin(page, { port: PORT });
  await page.evaluate(async (id) => {
    const { openFlowBuilder } = await import('/js/modules/flows-builder.js');
    openFlowBuilder(id, { mode: 'bpmn' });
  }, definitionId);
  await expect(page.locator('.fb-canvas')).toBeVisible({ timeout: 15000 });
  await expect(page.locator('.fb-palette-category[data-cat="tentaflow"]')).toBeVisible({ timeout: 15000 });
}

async function dragFlowToCanvas(page, offsetX, offsetY) {
  const item = page.locator('.fb-palette-category[data-cat="tentaflow"] .fb-node-item', { hasText: FLOW_NAME }).first();
  await item.scrollIntoViewIfNeeded();
  const from = await item.boundingBox();
  const canvas = await page.locator('.fb-canvas').boundingBox();
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(canvas.x + offsetX, canvas.y + offsetY, { steps: 8 });
  await page.mouse.up();
}

const routingRows = (page) => page.evaluate(() => document.querySelector('tf-table[data-routing]').rows);
const storedModel = (page) => page.evaluate(async (id) => {
  const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
  return (await ApiBinary.one('processDefinitionGetRequest', { definitionId: id })).definition.model;
}, definitionId);

test.use({ viewport: { width: 1800, height: 1000 } });

test('tabs, the routing table and an added error boundary event survive save and reopen', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await openDesigner(page);

  await dragFlowToCanvas(page, 300, 200);
  const node = page.locator('.fb-canvas .fb-node.bpmn_service_task');
  await expect(node).toHaveCount(1);
  await expect(node.locator('.fb-process-kind[data-kind="flow"]')).toHaveCount(1);

  // The dropped node is selected: its inspector starts on the implementation tab.
  const panel = (id) => page.locator(`[data-service-panel="${id}"]`);
  await expect(page.locator('[data-service-tabs] [role="tab"]')).toHaveCount(5);
  await expect(panel('impl')).toBeVisible();
  await expect(panel('result')).toBeHidden();

  await page.getByRole('tab', { name: 'Inputs and outputs' }).click();
  await expect(panel('io')).toBeVisible();
  await expect(panel('impl')).toBeHidden();

  await page.getByRole('tab', { name: 'Result and errors' }).click();
  await expect(panel('result')).toBeVisible();
  await expect.poll(async () => (await routingRows(page)).length).toBe(6);
  expect((await routingRows(page))[1].route).toMatch(/result source/);
  await expect(page.locator('[data-routing-add="escalation"]')).toHaveAttribute('aria-disabled', 'true');

  await page.locator('[data-process="resultExpression"] textarea').fill(RESULT_EXPRESSION);
  await page.locator('[data-process="resultExpression"] textarea').blur();
  await expect(page.locator('[data-routing-add="escalation"]')).not.toHaveAttribute('aria-disabled', 'true');
  const before = await routingRows(page);
  expect(before.find((row) => row.outcome === 'Any other error').route).toMatch(/Incident/);

  await page.locator('[data-routing-add="error"]').click();
  await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(1);
  const after = await routingRows(page);
  expect(after.find((row) => row.outcome === 'Any other error').route).toMatch(/Boundary event/);
  await expect(page.locator('[data-routing-warning="boundary_no_flow"]')).toHaveCount(1);

  await page.locator('[data-role="save"]').click();
  await expect.poll(async () => (await storedModel(page)).nodes.some((row) => row.kind.BoundaryError), { timeout: 10000 }).toBe(true);
  const model = await storedModel(page);
  const task = model.nodes.find((row) => row.kind.ServiceTask);
  expect(task.kind.ServiceTask.resultExpression).toBe(RESULT_EXPRESSION);
  const boundary = model.nodes.find((row) => row.kind.BoundaryError);
  expect(boundary.kind.BoundaryError.attachedToId).toBe(task.id);

  await page.reload();
  await expect(page.locator('#sidebar, .sidebar, nav').first()).toBeVisible({ timeout: 15000 });
  await openDesigner(page, { login: false });
  await page.locator(`.fb-canvas .fb-node[data-node-id="${task.id}"]`).click();
  await page.getByRole('tab', { name: 'Result and errors' }).click();
  await expect.poll(async () => (await routingRows(page)).find((row) => row.outcome === 'Any other error')?.route).toMatch(/Boundary event/);
  await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(1);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
