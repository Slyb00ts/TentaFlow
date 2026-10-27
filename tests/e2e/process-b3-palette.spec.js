// =============================================================================
// File: tests/e2e/process-b3-palette.spec.js
// Description: The BPMN designer palette group "TentaFlow blocks" against a real
//              node: a flow, a Flow Builder block and an addon tool are dragged
//              onto the canvas as ready-made Service tasks, the process is saved
//              and reopened, and the stored model carries each implementation.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { installAddonInstance, collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18461;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b3-palette');
const FLOW_NAME = 'B3 palette flow';
const PROCESS_NAME = 'B3 palette process';
const NOTES_PERMISSIONS = ['ui', 'sql.read', 'sql.write', 'state.read', 'state.write', 'directory.read'];
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
let panelPage;

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
  const installer = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(installer, { port: PORT });
  const addonId = await installAddonInstance(installer, { packageId: 'notes', displayName: 'Notes B3', permissions: NOTES_PERMISSIONS });
  await installer.close();

  // The process path never starts an addon, so a tool is offered only while its
  // addon runs; opening the addon's panel is how an operator starts it. The
  // navigation lists the instance only for a page loaded after the install.
  const panel = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(panel, { port: PORT });
  const navItem = panel.locator(`.addon-app-nav-item[data-addon-id="${addonId}"]`);
  await expect(navItem).toBeVisible({ timeout: 15000 });
  await navItem.click();
  await expect(panel.locator(`.addon-app-shell[data-addon="${addonId}"]`)).toBeVisible({ timeout: 15000 });
  panelPage = panel;

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  // Starting the WASM instance takes seconds after the panel opens, and the
  // palette is built from one read of the options when the designer opens.
  await expect.poll(() => page.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return (await ApiBinary.one('processOptionsRequest', {})).serviceTools.length;
  }), { timeout: 60000 }).toBeGreaterThan(0);
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
  await panelPage?.close();
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

async function dragToCanvas(page, entry, offsetX, offsetY) {
  const item = page.locator('.fb-palette-category[data-cat="tentaflow"] .fb-node-item', { hasText: entry }).first();
  await item.scrollIntoViewIfNeeded();
  const from = await item.boundingBox();
  const canvas = await page.locator('.fb-canvas').boundingBox();
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(canvas.x + offsetX, canvas.y + offsetY, { steps: 8 });
  await page.mouse.up();
}

// The designer packs palette, canvas and inspector side by side; the default
// 720 px viewport leaves the canvas too small to drop three tasks apart.
test.use({ viewport: { width: 1800, height: 1000 } });

test('a flow, a block and an addon tool dropped from the palette survive save and reopen', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await openDesigner(page);

  const group = page.locator('.fb-palette-category[data-cat="tentaflow"]');
  await expect(group.locator('[data-subgroup]')).toHaveCount(4);
  const order = await group.locator('[data-subgroup]').evaluateAll((all) => all.map((node) => node.dataset.subgroup));
  expect(order).toEqual(['flows', 'blocks', 'agents', 'tools']);
  // The node ships system agents, so the Agents subgroup lists rows and shows no empty hint.
  await expect(group.locator('[data-subgroup="agents"] + .fb-node-item')).toHaveCount(1);
  await expect(group.locator('.fb-palette-note')).toHaveCount(0);
  await expect(group.locator('.fb-node-item', { hasText: FLOW_NAME })).toHaveCount(1);
  await expect(group.locator('.fb-node-item', { hasText: 'Language model' })).toHaveCount(1);
  await expect(group.locator('.fb-node-item', { hasText: 'search_notes' })).toHaveCount(1);
  await expect(group.locator('.fb-node-item', { hasText: 'search_notes' }).locator('.fb-node-desc')).toHaveText('Notes B3');

  await dragToCanvas(page, FLOW_NAME, 120, 150);
  await dragToCanvas(page, 'Language model', 360, 150);
  await dragToCanvas(page, 'search_notes', 600, 150);

  await page.locator('[data-role="save"]').click();

  const stored = async () => page.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const response = await ApiBinary.one('processDefinitionGetRequest', { definitionId: id });
    return response.definition.model.nodes
      .filter((node) => node.kind.ServiceTask)
      .map((node) => ({ name: node.name, task: node.kind.ServiceTask }));
  }, definitionId);

  await expect.poll(async () => (await stored()).map((entry) => entry.name), { timeout: 10000 }).toHaveLength(3);
  const tasks = await stored();
  const byName = Object.fromEntries(tasks.map((entry) => [entry.name, entry.task]));
  expect(byName[FLOW_NAME].flowId).toBeTruthy();
  expect(byName['Language model (LLM)'].implementation.Block.nodeType).toBe('llm');
  const tool = byName['Notes B3 › search_notes'].implementation.AddonTool;
  expect(tool.toolName).toBe('search_notes');
  expect(tool.addonId).toMatch(/^notes-/);

  await page.reload();
  await expect(page.locator('#sidebar, .sidebar, nav').first()).toBeVisible({ timeout: 15000 });
  await openDesigner(page, { login: false });
  for (const label of [FLOW_NAME, 'Language model (LLM)', 'Notes B3 › search_notes']) {
    await expect(page.locator('.fb-canvas').getByText(label, { exact: true }).first()).toBeVisible({ timeout: 10000 });
  }
  expect(await stored()).toEqual(tasks);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('palette keyboard adds cascade, keep focus on the row and the group comes first', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await openDesigner(page);

  await expect(page.locator('.fb-palette-category').first()).toHaveAttribute('data-cat', 'tentaflow');
  await expect(page.locator('.fb-palette-subgroup').first()).toHaveAttribute('role', 'heading');

  const rows = page.locator('.fb-palette-category[data-cat="tentaflow"] .fb-node-item', { hasText: 'Language model' });
  const nodes = page.locator('.fb-canvas .fb-node');
  // The definition already holds the nodes the previous test saved.
  const before = await nodes.count();
  await rows.first().locator('button').focus();
  await page.keyboard.press('Enter');
  await expect(nodes).toHaveCount(before + 1);
  await expect(rows.first().locator('button')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(nodes).toHaveCount(before + 2);
  await expect(rows.first().locator('button')).toBeFocused();

  const spots = await nodes.evaluateAll((all) => all.slice(-2).map((node) => `${node.style.left},${node.style.top}`));
  expect(new Set(spots).size).toBe(2);

  const items = page.locator('.fb-palette .fb-node-item');
  const all = await items.count();
  await page.locator('.fb-palette tf-searchbox input').fill(' ');
  await page.waitForTimeout(400);
  expect(await items.count()).toBe(all);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
