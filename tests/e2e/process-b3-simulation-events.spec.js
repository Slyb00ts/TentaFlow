// =============================================================================
// File: tests/e2e/process-b3-simulation-events.spec.js
// Description: Messages and event races in a simulated run (BPMN B3.7b) in the
//              designer against a real node. A process that waits for a message
//              is answered from the pick list of open waits; the message is
//              queued, the next Advance delivers it and the trace says so. A
//              message with the wrong correlation key stays queued and the wait
//              stays open. Invalid payload JSON is told in the form and sends
//              nothing. A race between a message and a timer is won by the
//              timer when the virtual clock moves on.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18495;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b3-simulation-events');

let PORT;
let proc;
let messageId;
let raceId;
let timerId;

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
  const ids = await page.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const catchNode = { id: 'Catch_1', name: 'Wait for payment', kind: { MessageCatch: {
      messageRef: 'Message_1', correlationExpression: "'PO-7'", outputMapping: {} } } };
    const build = (nodes, flows, timed = false, messaged = true) => {
      const model = emptyProcessModel();
      if (timed) model.timerTimezone = 'UTC';
      if (messaged) model.messages = [{ messageId: 'Message_1', name: 'order_paid' }];
      model.nodes = [model.nodes[0], ...nodes, model.nodes[1]];
      model.sequenceFlows = flows.map(([id, from, to]) => ({ id, sourceId: from, targetId: to, condition: null }));
      const x = new Map(model.nodes.map((node, index) => [node.id, 40 + index * 150]));
      model.diagram = {
        shapes: model.nodes.map((node) => ({ elementId: node.id, x: x.get(node.id), y: node.id === 'Timer_1' ? 300 : 160, width: node.id === 'Start' || node.id === 'End' ? 56 : 120, height: node.id === 'Start' || node.id === 'End' ? 56 : 72 })),
        edges: model.sequenceFlows.map((flow) => ({ sequenceFlowId: flow.id,
          waypoints: [{ x: x.get(flow.sourceId) + 60, y: 190 }, { x: x.get(flow.targetId), y: 190 }] })),
      };
      return model;
    };
    const create = async (name, model) => {
      const saved = await ApiBinary.one('processDefinitionSaveRequest', {
        commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0, name, description: '', model,
      });
      const id = saved.definition.definitionId;
      await ApiBinary.one('processDefinitionPublishRequest', {
        commandId: crypto.randomUUID(), definitionId: id, expectedRevision: saved.definition.draftRevision,
      });
      return id;
    };
    const message = await create('B3 simulation message', build([catchNode], [['F_1', 'Start', 'Catch_1'], ['F_2', 'Catch_1', 'End']]));
    const race = await create('B3 simulation race', build([
      { id: 'Race_1', name: 'First event', kind: 'EventBasedGateway' }, catchNode,
      { id: 'Timer_1', name: 'Deadline', kind: { TimerCatch: { timer: { Duration: { seconds: 5 } } } } },
    ], [['F_1', 'Start', 'Race_1'], ['F_2', 'Race_1', 'Catch_1'], ['F_3', 'Race_1', 'Timer_1'], ['F_4', 'Catch_1', 'End'], ['F_5', 'Timer_1', 'End']], true));
    const timer = await create('B3 simulation timer', build([
      { id: 'Timer_1', name: 'Two minutes', kind: { TimerCatch: { timer: { Duration: { seconds: 120 } } } } },
    ], [['F_1', 'Start', 'Timer_1'], ['F_2', 'Timer_1', 'End']], true, false));
    return { message, race, timer };
  });
  messageId = ids.message;
  raceId = ids.race;
  timerId = ids.timer;
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

test.use({ viewport: { width: 1600, height: 1100 } });

const simulation = (page) => page.locator('tf-window.fb-process-window').last();

async function startSimulation(page, definitionId) {
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
  await form.locator('[data-act="submit"]').click();
  await expect(simulation(page).locator('[data-simulation-content]')).toBeVisible({ timeout: 15000 });
  return simulation(page);
}

async function advanceUntil(page, ready) {
  const win = simulation(page);
  for (let attempt = 0; attempt < 12; attempt += 1) {
    if (await ready(win)) return;
    const button = win.locator('[data-simulation-advance]:not([disabled])');
    if (!(await button.count())) break;
    await button.click();
    await page.waitForTimeout(400);
  }
  expect(await ready(win)).toBe(true);
}

const setEditor = (locator, value) => locator.evaluate((editor, text) => {
  editor.value = text;
  editor.dispatchEvent(new Event('input', { bubbles: true }));
}, value);

test('a message from the pick list is queued, delivered by the next Advance and finishes the run', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const win = await startSimulation(page, messageId);
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-subscription]').count()) === 1);
  const row = win.locator('[data-simulation-subscription]');
  await expect(row).toContainText('Wait for payment');
  await expect(row).toContainText('Message: order_paid · Key: PO-7');
  await expect(win.locator('[data-simulation-advance-hint]')).toContainText('next due item');

  // The wait is on the pick list; choosing it fills the name and key and addresses that wait.
  await win.locator('[data-message-field="pick"] select').selectOption({ index: 1 });
  await expect(win.locator('[data-message-field="name"] input')).toHaveValue('order_paid');
  await expect(win.locator('[data-message-field="key"] input')).toHaveValue('PO-7');
  await expect(win.locator('[data-message-field="exact"]')).toHaveAttribute('checked', '');

  // Invalid payload JSON is told in the form and nothing is sent.
  await setEditor(win.locator('[data-message-field="payload"]'), '{"half":');
  await win.locator('[data-simulation-message-send]').click();
  await expect(win.locator('[data-message-error="payload"]')).toHaveAttribute('message', 'Enter valid JSON.');
  await expect(win.locator('[data-simulation-messages]')).toHaveCount(0);

  await setEditor(win.locator('[data-message-field="payload"]'), '{"paid": true}');
  await win.locator('[data-simulation-message-send]').click();
  const queued = win.locator('[data-simulation-message]');
  await expect(queued).toHaveCount(1, { timeout: 15000 });
  await expect(queued).toHaveAttribute('data-message-status', 'Pending');
  await expect(queued).toContainText('Expires');
  await expect(win.locator('.fb-simulation-events')).toContainText('Message sent');

  await advanceUntil(page, async (current) => (await current.locator('[data-message-status="Delivered"]').count()) === 1);
  await expect(win.locator('.fb-simulation-events')).toContainText('Message delivered');
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-status="Completed"]').count()) === 1);
  await expect(win.locator('[data-simulation-awaited]')).toHaveCount(0);
  // A finished run takes no more messages.
  await expect(win.locator('[data-message-finished]')).toBeVisible();
  await expect(win.locator('[data-simulation-message-send]')).toHaveAttribute('disabled', '');
  await expect(win.locator('[data-error]')).toBeHidden();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a message with the wrong correlation key stays queued and the wait stays open', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const win = await startSimulation(page, messageId);
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-subscription]').count()) === 1);
  // A name and key typed by hand (no wait chosen): nobody waits for this key.
  await win.locator('[data-message-field="name"] input').fill('order_paid');
  await win.locator('[data-message-field="key"] input').fill('PO-WRONG');
  await win.locator('[data-simulation-message-send]').click();
  await expect(win.locator('[data-simulation-message]')).toHaveCount(1, { timeout: 15000 });
  await expect(win.locator('[data-simulation-message]')).toHaveAttribute('data-message-status', 'Pending');
  // Nothing waits for this key: the message says so instead of promising a delivery.
  await expect(win.locator('[data-message-pending-hint="none"]')).toContainText('No open wait matches this name and key');
  await expect(win.locator('[data-message-pending-hint="none"]')).not.toContainText('delivers it');
  await expect(win.locator('[data-simulation-subscription]')).toHaveCount(1);
  // The message's lifetime ends with the simulation, so one Advance jumps the clock to the end: Advance is
  // off with its reason, the message can no longer arrive, and the wait is still open.
  await win.locator('[data-simulation-advance]:not([disabled])').click();
  await expect(win.locator('[data-simulation-horizon]')).toBeVisible({ timeout: 15000 });
  await expect(win.locator('[data-simulation-advance]')).toHaveAttribute('disabled', '');
  await expect(win.locator('[data-message-pending-hint="finished"]')).toContainText('will never be delivered');
  await expect(win.locator('[data-simulation-subscription]')).toHaveCount(1);
  await expect(win.locator('[data-simulation-status]')).not.toHaveText('Completed');
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('in a race between a message and a timer the timer wins when the virtual clock moves on', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const win = await startSimulation(page, raceId);
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-race]').count()) === 1);
  const race = win.locator('[data-simulation-race]');
  await expect(race).toContainText('first event to happen wins');
  await expect(race.locator('[data-simulation-subscription]')).toContainText('Message: order_paid · Key: PO-7');
  await expect(race.locator('[data-simulation-subscription]')).toContainText('fires at');
  await expect(win.locator('.fb-simulation-timer')).toContainText('Deadline');
  await advanceUntil(page, async (current) => (await current.locator('[data-simulation-status="Completed"]').count()) === 1);
  await expect(win.locator('.fb-simulation-events')).toContainText('Timer fired');
  await expect(win.locator('.fb-simulation-events')).toContainText('Event race won');
  await expect(win.locator('[data-simulation-race]')).toHaveCount(0);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('one Advance jumps the virtual clock over a two-minute timer', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  const win = await startSimulation(page, timerId);
  const pending = win.locator('.fb-simulation-timer tf-chip', { hasText: 'Pending' });
  await advanceUntil(page, async () => (await pending.count()) === 1);
  const before = await win.locator('.fb-simulation-toolbar').innerText();
  await expect(win.locator('[data-simulation-status="Completed"]')).toHaveCount(0);
  // One press: the clock goes straight to the timer, however far that is.
  await win.locator('[data-simulation-advance]:not([disabled])').click();
  await expect(win.locator('[data-simulation-status="Completed"]')).toHaveCount(1, { timeout: 15000 });
  await expect(win.locator('.fb-simulation-events')).toContainText('Timer fired');
  const after = await win.locator('.fb-simulation-toolbar').innerText();
  expect(after).not.toBe(before);
  const timeOf = (label) => win.locator('.fb-simulation-events li', { hasText: label }).first().locator('span').textContent();
  expect(await timeOf('Timer fired')).not.toBe(await timeOf('Timer set'));
  // The run is over: nothing left to advance, nothing left to send.
  await expect(win.locator('[data-simulation-advance]')).toHaveAttribute('disabled', '');
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
