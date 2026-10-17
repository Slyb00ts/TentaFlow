// =============================================================================
// File: tests/e2e/process-b3-inspector.spec.js
// Description: The BPMN designer's Service task inspector against a real node:
//              a flow is dropped from the palette, the inspector tabs are
//              switched, an error boundary event is added from the "Result and
//              errors" tab, the process is saved and reopened, and the routing
//              table is derived again from the stored model. A second test
//              measures the layout at laptop, small-laptop and phone sizes: every
//              tab reachable, a usable inspector height, handlers on the task's
//              bottom edge clear of its icon.
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
  await page.evaluate(async ({ flowName, graph }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const created = await ApiBinary.one('flowCreateRequest', { name: flowName, description: '', graphJson: graph });
    const flowId = created.id ?? created.flowId;
    await ApiBinary.action('flowUpdateRequest', { flowId: String(flowId), status: 'active' });
  }, { flowName: FLOW_NAME, graph: FLOW_GRAPH });
  definitionId = await createDefinition(page, PROCESS_NAME);
  await page.close();
});

// A process whose model declares the error and escalation codes the layout test
// adds one handler per code for, besides the catch-all.
function createDefinition(page, name) {
  return page.evaluate(async (processName) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const model = emptyProcessModel();
    model.errors = ['INVALID_INPUT', 'NOT_FOUND', 'DENIED'].map((code) => ({ errorId: `Err_${code}`, name: code, errorCode: code }));
    model.escalations = [{ escalationId: 'Esc_REVIEW', name: 'REVIEW', escalationCode: 'REVIEW' }];
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0,
      name: processName, description: '', model,
    });
    return saved.definition.definitionId;
  }, name);
}

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

async function openDesigner(page, { login = true, id = definitionId } = {}) {
  if (login) await loginAsAdmin(page, { port: PORT });
  await page.evaluate(async (target) => {
    const { openFlowBuilder } = await import('/js/modules/flows-builder.js');
    openFlowBuilder(target, { mode: 'bpmn' });
  }, id);
  await expect(page.locator('.fb-canvas')).toBeVisible({ timeout: 15000 });
  // Below laptop width the palette is a drawer, so only its presence is meaningful here.
  await expect(page.locator('.fb-palette-category[data-cat="tentaflow"]')).toBeAttached({ timeout: 15000 });
}

async function dragFlowToCanvas(page, offsetX, offsetY) {
  // Below laptop width the palette is a drawer, opened from the canvas.
  const drawer = page.locator('[data-role="palette-toggle"]');
  if (await drawer.isVisible()) {
    await drawer.click();
    await expect(page.locator('.fb-palette')).toHaveClass(/open/);
  }
  const item = page.locator('.fb-palette-category[data-cat="tentaflow"] .fb-node-item', { hasText: FLOW_NAME }).first();
  await item.scrollIntoViewIfNeeded();
  // The drawer slides in; a drag started on a moving row misses it.
  let from = await item.boundingBox();
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await page.waitForTimeout(100);
    const next = await item.boundingBox();
    const settled = next.x === from.x && next.y === from.y;
    from = next;
    if (settled) break;
  }
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

  await page.getByRole('tab', { name: 'Data' }).click();
  await expect(panel('io')).toBeVisible();
  await expect(panel('impl')).toBeHidden();

  await page.getByRole('tab', { name: 'Result' }).click();
  await expect(panel('result')).toBeVisible();
  await expect.poll(async () => (await routingRows(page)).length).toBe(7);
  expect((await routingRows(page))[2].route).toMatch(/result rule/);
  await expect(page.locator('[data-routing-add="escalation"]')).toHaveAttribute('aria-disabled', 'true');

  await page.locator('[data-process="resultExpression"] textarea').fill(RESULT_EXPRESSION);
  await page.locator('[data-process="resultExpression"] textarea').blur();
  await expect(page.locator('[data-routing-add="escalation"]')).not.toHaveAttribute('aria-disabled', 'true');
  const before = await routingRows(page);
  expect(before.find((row) => row.outcome === 'Any other error').route).toMatch(/Incident/);

  await page.locator('[data-routing-add="error"]').click();
  await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(1);
  await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toContainText('Error: any');
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
  await page.getByRole('tab', { name: 'Result' }).click();
  await expect.poll(async () => (await routingRows(page)).find((row) => row.outcome === 'Any other error')?.route).toMatch(/Boundary event/);
  await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(1);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

// What the critic could not use: a 340 px inspector whose tabs scrolled out of
// sight, a body squeezed to 80 px by the process header, handlers piled on the
// task's icon. The numbers below are measured in a real browser, not derived.
const rect = (locator) => locator.evaluate((element) => {
  const box = element.getBoundingClientRect();
  return { left: box.left, top: box.top, right: box.right, bottom: box.bottom, width: box.width, height: box.height };
});
const intersects = (a, b) => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;

for (const [width, height] of [[1280, 720], [1024, 700], [390, 844]]) {
  test(`the inspector stays usable at ${width}x${height}`, async ({ page }) => {
    test.setTimeout(180000);
    await page.setViewportSize({ width, height });
    await loginAsAdmin(page, { port: PORT });
    // Its own process: the first test saves a task into the shared one.
    const own = await createDefinition(page, `Layout ${width}x${height}`);
    await openDesigner(page, { login: false, id: own });
    const phone = width < 768;
    if (phone) await page.locator('.fb-mobile-tabs tf-tab[id="palette"] button').click();
    await dragFlowToCanvas(page, 120, 120);
    if (phone) await page.locator('.fb-mobile-tabs tf-tab[id="config"] button').click();
    const node = page.locator('.fb-canvas .fb-node.bpmn_service_task');
    await expect(node).toHaveCount(1);

    // Every tab is inside the inspector at once: no chevron-scrolled strip.
    const config = await rect(page.locator('.fb-config'));
    const tabs = page.locator('[data-service-tabs] [role="tab"]');
    await expect(tabs).toHaveCount(5);
    for (let index = 0; index < 5; index += 1) {
      const box = await rect(tabs.nth(index));
      expect(box.left, `tab ${index} starts inside the inspector`).toBeGreaterThanOrEqual(config.left - 1);
      expect(box.right, `tab ${index} ends inside the inspector`).toBeLessThanOrEqual(config.right + 1);
      expect(box.width).toBeGreaterThan(30);
    }
    await expect(page.locator('[data-service-tabs] .tf-tabs-chev.visible')).toHaveCount(0);

    // The inspector body keeps a height a form can be used at; the page never scrolls sideways.
    if (!phone) {
      expect((await rect(page.locator('.fb-config-body'))).height).toBeGreaterThanOrEqual(300);
      expect((await rect(page.locator('.fb-canvas'))).width).toBeGreaterThanOrEqual(400);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

    // Handlers added from the buttons sit on the bottom edge, clear of the icon.
    await page.getByRole('tab', { name: 'Result' }).click();
    await page.locator('[data-process="resultExpression"] textarea').fill(RESULT_EXPRESSION);
    await page.locator('[data-process="resultExpression"] textarea').blur();
    for (let index = 0; index < 4; index += 1) {
      await page.locator('[data-routing-add="error"]').scrollIntoViewIfNeeded();
      await page.locator('[data-routing-add="error"]').click();
      await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(index + 1);
    }
    await page.locator('[data-routing-add="escalation"]').click();
    await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_escalation')).toHaveCount(1);
    const task = await rect(node);
    const icon = await rect(node.locator('.fb-process-symbol > svg').first());
    const handlers = page.locator('.fb-canvas .fb-node.bpmn_boundary_error, .fb-canvas .fb-node.bpmn_boundary_escalation');
    const boxes = [];
    for (let index = 0; index < 5; index += 1) boxes.push(await rect(handlers.nth(index)));
    boxes.forEach((box, index) => {
      expect(intersects(box, icon), `handler ${index} stays off the task icon`).toBe(false);
      const centre = (box.top + box.bottom) / 2;
      expect(Math.min(Math.abs(centre - task.bottom), Math.abs(centre - task.top)), `handler ${index} sits on the top or bottom border`).toBeLessThan(2);
      boxes.slice(index + 1).forEach((other, offset) => expect(intersects(box, other), `handlers ${index} and ${index + 1 + offset} do not overlap`).toBe(false));
    });
    expect(boxes.slice(0, 4).every((box) => Math.abs((box.top + box.bottom) / 2 - task.bottom) < 2), 'the first four lie along the bottom edge').toBe(true);
    const left = boxes.slice(0, 4).map((box) => box.left);
    expect(left, 'in left-to-right order').toEqual([...left].sort((a, b) => a - b));
    await expect(page.locator('[data-routing-add-hint]')).toBeVisible();
  });
}
