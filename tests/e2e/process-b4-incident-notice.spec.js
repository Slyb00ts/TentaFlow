// =============================================================================
// File: tests/e2e/process-b4-incident-notice.spec.js
// Description: Incident notice of a real process run (BPMN B4.3) against a real
//              node. A run whose gateway has no matching path raises an
//              incident; the person who started it gets a live toast built from
//              the incident code (no worker text), its action opens that run,
//              another signed-in user (a monitor) gets nothing, and the run list
//              filter "Needs attention" lists the run while the plain list also
//              holds the healthy one.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18509;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b4-incident-notice');

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
  ids = await page.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const build = (nodes, flows, variables = {}) => {
      const model = emptyProcessModel();
      model.variables = variables;
      model.targetNamespace = 'urn:b4:notice';
      model.nodes = [model.nodes[0], ...nodes, model.nodes[1]];
      model.sequenceFlows = flows.map(([sourceId, targetId, condition = null], index) => ({ id: `F_${index + 1}`, sourceId, targetId, condition }));
      const round = (node) => ['Start', 'End', 'Gate'].includes(node.id);
      const spots = new Map(model.nodes.map((node, index) => [node.id, { x: 40 + index * 160, y: 160, ...(round(node) ? { width: 56, height: 56 } : { width: 120, height: 72 }) }]));
      model.diagram = {
        shapes: model.nodes.map((node) => ({ elementId: node.id, ...spots.get(node.id) })),
        edges: model.sequenceFlows.map((flow) => {
          const from = spots.get(flow.sourceId);
          const to = spots.get(flow.targetId);
          return { sequenceFlowId: flow.id, waypoints: [{ x: from.x + from.width, y: from.y + from.height / 2 }, { x: to.x, y: to.y + to.height / 2 }] };
        }),
      };
      return model;
    };
    const create = async (name, model) => {
      const saved = await ApiBinary.one('processDefinitionSaveRequest', {
        commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0, name, description: '', model });
      await ApiBinary.one('processDefinitionPublishRequest', {
        commandId: crypto.randomUUID(), definitionId: saved.definition.definitionId, expectedRevision: saved.definition.draftRevision });
      return { id: saved.definition.definitionId, processId: model.processId };
    };
    await ApiBinary.action('iamCreateUserRequest', { username: 'b4watcher', password: 'Watcher-pass-w0rd-1', displayName: 'b4watcher', email: '', role: 'admin', groupIds: [] });
    const blocked = await create('B4 notice incident', build(
      [{ id: 'Gate', name: 'Big order?', kind: { ExclusiveGateway: { defaultFlowId: null } } }],
      [['Start', 'Gate'], ['Gate', 'End', 'vars.big == true']], { big: false }));
    const healthy = await create('B4 notice healthy', build(
      [{ id: 'Review', name: 'Review order', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } }],
      [['Start', 'Review'], ['Review', 'End']]));
    return { blocked, healthy };
  });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

test.use({ viewport: { width: 1400, height: 1000 } });

async function startRun(page, definition) {
  return page.evaluate(async ({ definitionId, processId }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const response = await ApiBinary.one('processInstanceStartRequest', {
      commandId: crypto.randomUUID(), definitionId, version: 1, processId, startNodeId: 'Start', variables: {} });
    return response.instance.instanceId;
  }, { definitionId: definition.id, processId: definition.processId });
}

const noticeToast = (page) => page.locator('tf-toast', { hasText: 'needs attention: B4 notice incident' });

test('the person who started a blocked run gets one toast that opens the run, a monitor gets none', async ({ page, browser }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const watcher = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1400, height: 1000 } });
  const watcherPage = await watcher.newPage();
  await loginAsAdmin(watcherPage, { port: PORT, username: 'b4watcher', password: 'Watcher-pass-w0rd-1' });
  // The watcher's socket is the one that would receive a personal event if the
  // server addressed it wrongly; record what actually arrives there.
  await watcherPage.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    window.__events = [];
    (await ApiBinary.client()).addUnsolicitedListener(({ body }) => window.__events.push(body?.kind ?? body?._variant));
  });

  const instanceId = await startRun(page, ids.blocked);
  const toast = noticeToast(page);
  await expect(toast).toHaveCount(1, { timeout: 15000 });
  await expect(toast.locator('.tf-toast-message')).toContainText('No path matches');
  await expect(toast).not.toContainText('NO_MATCHING_FLOW');
  await expect(toast.locator('tf-button')).toHaveAttribute('label', 'Open run');

  await toast.locator('tf-button button').click();
  const win = page.locator('tf-window.fb-process-window').last();
  await expect(win).toBeVisible({ timeout: 15000 });
  await expect(win.locator('[data-incidents]')).toBeVisible({ timeout: 15000 });
  await expect(win.locator('[data-incidents]')).toContainText('No path matches');
  await expect(noticeToast(page)).toHaveCount(0, { timeout: 5000 });

  await page.waitForTimeout(1500);
  await expect(noticeToast(watcherPage)).toHaveCount(0);
  expect(await watcherPage.evaluate(() => window.__events)).not.toContain('process_incident');
  expect(await page.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return (await ApiBinary.one('processInstanceGetRequest', { instanceId: id })).instance.incidents.length;
  }, instanceId)).toBe(1);
  await watcher.close();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('the run list filter Needs attention keeps the blocked run and drops the healthy one', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  await startRun(page, ids.blocked);
  await startRun(page, ids.healthy);
  await page.evaluate(async () => {
    const { openProcessInstances } = await import('/js/modules/flows-builder/process-monitor.js');
    await openProcessInstances();
  });
  const win = page.locator('tf-window.fb-process-window').last();
  const rows = win.locator('tf-table tbody tr');
  await expect(rows.filter({ hasText: 'B4 notice healthy' }).first()).toBeVisible({ timeout: 15000 });
  await expect(rows.filter({ hasText: 'B4 notice incident' }).first()).toBeVisible();

  await win.locator('[data-filter] button[data-value="attention"]').click();
  await expect(win.locator('[data-attention-count]')).toContainText(/runs? needs? attention/, { timeout: 15000 });
  await expect(rows.filter({ hasText: 'B4 notice incident' }).first()).toBeVisible();
  await expect(rows.filter({ hasText: 'B4 notice healthy' })).toHaveCount(0);

  await win.locator('[data-filter] button[data-value="all"]').click();
  await expect(rows.filter({ hasText: 'B4 notice healthy' }).first()).toBeVisible({ timeout: 15000 });
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
