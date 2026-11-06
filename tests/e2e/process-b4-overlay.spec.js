// =============================================================================
// File: tests/e2e/process-b4-overlay.spec.js
// Description: Token overlay on the diagram of a real process run (BPMN B4.2)
//              against a real node. The instance window gets a Diagram view
//              next to the lists: a person's task shows its token, a parallel
//              fork shows two tokens and the join waits for the second, a
//              sub-process token opens the sub-process diagram and the
//              breadcrumb leads back, a gateway with no matching path shows an
//              incident on the element. The polling stops once the run is over.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18505;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b4-overlay');

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

    // Start, the given nodes in a row, End; every node gets a shape. `flows` replaces the default chain.
    const build = (nodes, { flows = null, variables = {}, namespace = 'urn:b4:overlay' } = {}) => {
      const model = emptyProcessModel();
      model.variables = variables;
      model.targetNamespace = namespace;
      model.nodes = [model.nodes[0], ...nodes, model.nodes[1]];
      model.sequenceFlows = (flows ?? model.nodes.slice(1).map((node, index) => [model.nodes[index].id, node.id]))
        .map(([sourceId, targetId, condition = null], index) => ({ id: `F_${index + 1}`, sourceId, targetId, condition }));
      const round = (node) => ['Start', 'End', 'Fork', 'Join', 'Gate'].includes(node.id);
      const place = (layout) => new Map(model.nodes.map((node, index) => [node.id, layout.get(node.id) ?? { x: 40 + index * 160, y: 160 }]));
      return { model, place, round };
    };
    const finish = ({ model, place, round }, layout = new Map()) => {
      const spots = place(layout);
      const size = (id) => (round(model.nodes.find((node) => node.id === id)) ? { width: 56, height: 56 } : { width: 120, height: 72 });
      model.diagram = {
        shapes: model.nodes.map((node) => ({ elementId: node.id, ...spots.get(node.id), ...size(node.id) })),
        edges: model.sequenceFlows.map((flow) => {
          const from = spots.get(flow.sourceId);
          const to = spots.get(flow.targetId);
          return { sequenceFlowId: flow.id, waypoints: [
            { x: from.x + size(flow.sourceId).width, y: from.y + size(flow.sourceId).height / 2 },
            { x: to.x, y: to.y + size(flow.targetId).height / 2 }] };
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
    const user = (id, name, assignee = null, outputMapping = {}) => ({ id, name, kind: { UserTask: { assigneeUserId: assignee, outputMapping } } });
    const account = async (username, password, role) => {
      const created = await ApiBinary.action('iamCreateUserRequest', { username, password, displayName: username, email: '', role, groupIds: [] });
      return created.userId;
    };
    const monitorId = await account('b4monitor', 'Monitor-pass-w0rd-1', 'admin');
    const workerId = await account('b4worker', 'Worker-pass-w0rd-1', 'user');
    await account('b4stranger', 'Stranger-pass-w0rd-1', 'user');

    const single = await create('B4 overlay user task', finish(build([user('Review', 'Review order')])));

    const fork = build([
      { id: 'Fork', name: 'Split work', kind: 'ParallelGateway' }, user('TaskA', 'Prepare goods'), user('TaskB', 'Prepare invoice'),
      { id: 'Join', name: 'Merge work', kind: 'ParallelGateway' }],
    { flows: [['Start', 'Fork'], ['Fork', 'TaskA'], ['Fork', 'TaskB'], ['TaskA', 'Join'], ['TaskB', 'Join'], ['Join', 'End']] });
    const parallel = await create('B4 overlay parallel', finish(fork, new Map([
      ['Start', { x: 20, y: 200 }], ['Fork', { x: 120, y: 200 }], ['TaskA', { x: 240, y: 90 }], ['TaskB', { x: 240, y: 310 }],
      ['Join', { x: 440, y: 200 }], ['End', { x: 540, y: 200 }]])));

    const inner = {
      nodes: [{ id: 'InStart', name: '', kind: 'Start' }, user('InTask', 'Inner review'), { id: 'InEnd', name: '', kind: 'End' }],
      sequenceFlows: [{ id: 'InF_1', sourceId: 'InStart', targetId: 'InTask', condition: null }, { id: 'InF_2', sourceId: 'InTask', targetId: 'InEnd', condition: null }],
      variables: {},
      diagram: {
        shapes: [{ elementId: 'InStart', x: 30, y: 120, width: 56, height: 56 }, { elementId: 'InTask', x: 150, y: 112, width: 120, height: 72 }, { elementId: 'InEnd', x: 340, y: 120, width: 56, height: 56 }],
        edges: [{ sequenceFlowId: 'InF_1', waypoints: [{ x: 86, y: 148 }, { x: 150, y: 148 }] }, { sequenceFlowId: 'InF_2', waypoints: [{ x: 270, y: 148 }, { x: 340, y: 148 }] }],
      },
    };
    const nested = await create('B4 overlay sub-process', finish(build([
      { id: 'Sub_1', name: 'Check order', kind: { SubProcess: { body: inner, inputMapping: {}, outputMapping: {} } } }])));

    const blocked = await create('B4 overlay incident', finish(build([
      { id: 'Gate', name: 'Big order?', kind: { ExclusiveGateway: { defaultFlowId: null } } }],
    { flows: [['Start', 'Gate'], ['Gate', 'End', 'vars.big == true']], variables: { big: false } })));
    const assigned = await create('B4 overlay assigned', finish(build([
      user('Assigned', 'Check the contract', workerId, { verdict: 'outputs.secret_verdict' })], { variables: { customer: 'private-customer' } })));
    return { single, parallel, nested, blocked, assigned, monitorId, workerId };
  });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

test.use({ viewport: { width: 1400, height: 1000 } });

const instanceWindow = (page) => page.locator('tf-window.fb-process-window').last();

async function startRun(page, definition, variables = {}) {
  return page.evaluate(async ({ definitionId, processId, values }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const response = await ApiBinary.one('processInstanceStartRequest', {
      commandId: crypto.randomUUID(), definitionId, version: 1, processId, startNodeId: 'Start', variables: values });
    return response.instance.instanceId;
  }, { definitionId: definition.id, processId: definition.processId, values: variables });
}

async function openDiagram(page, instanceId) {
  await page.evaluate(async (id) => {
    const { openProcessInstance } = await import('/js/modules/flows-builder/process-monitor.js');
    await openProcessInstance(id);
  }, instanceId);
  const win = instanceWindow(page);
  await expect(win.locator('[data-view]')).toBeVisible({ timeout: 15000 });
  await win.locator('[data-view] button[data-value="diagram"]').click();
  await expect(win.locator('.fb-process-diagram')).toBeVisible();
  await expect(win.locator('.fb-canvas.fb-runtime')).toHaveCount(1, { timeout: 15000 });
  return win;
}

const node = (win, id) => win.locator(`.fb-canvas .fb-node[data-node-id="${id}"]`);
const badge = (win, id) => node(win, id).locator('.fb-runtime-badge');

async function completeFromPanel(page, win, id) {
  await node(win, id).click();
  const action = win.locator('.fb-diagram-panel tf-button button', { hasText: 'Complete work' });
  await expect(action).toBeVisible({ timeout: 15000 });
  await action.click();
  const form = page.locator('tf-window.tf-act-window').last();
  await expect(form.locator('[data-act="submit"]')).toBeVisible({ timeout: 15000 });
  await form.locator('[data-act="submit"]').click();
  await expect(form).toHaveCount(0, { timeout: 15000 });
}

test('a person\'s task shows its token, the path taken, and the polling ends with the run', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await startRun(page, ids.single);
  const win = await openDiagram(page, instanceId);

  await expect(node(win, 'Review')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
  await expect(badge(win, 'Review').locator('b')).toHaveText('1');
  await expect(badge(win, 'Review')).toHaveAttribute('aria-label', /Waiting for a person/);
  await expect(node(win, 'Start')).toHaveClass(/fb-node--visited/);
  await expect(win.locator('.fb-canvas .fb-edge-path.fb-edge--taken')).toHaveCount(1);

  await node(win, 'Review').click();
  const panel = win.locator('.fb-diagram-panel');
  await expect(panel).toContainText('Review order');
  await expect(panel).toContainText('Waiting for a person');
  await expect(panel).not.toContainText(/[A-Z]+_[A-Z_]+/);

  await completeFromPanel(page, win, 'Review');
  await expect(win.locator('tf-chip', { hasText: 'Completed' }).first()).toBeVisible({ timeout: 15000 });
  await expect(win.locator('.fb-canvas .fb-runtime-badge')).toHaveCount(0, { timeout: 15000 });
  await expect(node(win, 'End')).toHaveClass(/fb-node--visited/);
  await expect(win.locator('.fb-canvas .fb-edge-path.fb-edge--taken')).toHaveCount(2);

  // Once the run is over no further token read is made.
  await page.evaluate(async () => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    window.__tokenReads = 0;
    const one = ApiBinary.one.bind(ApiBinary);
    ApiBinary.one = (kind, ...rest) => { if (kind === 'processInstanceTokensGetRequest') window.__tokenReads += 1; return one(kind, ...rest); };
  });
  await page.waitForTimeout(7500);
  expect(await page.evaluate(() => window.__tokenReads)).toBe(0);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a parallel fork shows two tokens and the join waits for the second', async ({ page }) => {
  test.setTimeout(150000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await startRun(page, ids.parallel);
  const win = await openDiagram(page, instanceId);

  await expect(node(win, 'TaskA')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
  await expect(node(win, 'TaskB')).toHaveClass(/fb-node--token-waiting/);
  await expect(badge(win, 'TaskA').locator('b')).toHaveText('1');
  await expect(badge(win, 'TaskB').locator('b')).toHaveText('1');
  await expect(node(win, 'Join')).not.toHaveClass(/fb-node--token/);
  await expect(win.locator('.fb-canvas .fb-runtime-badge')).toHaveCount(2);

  // The run changes under a focused control of the panel; the refresh must not take the focus away.
  await node(win, 'TaskB').click();
  const hold = win.locator('.fb-diagram-panel tf-button button', { hasText: 'Complete work' });
  await expect(hold).toBeVisible();
  await hold.focus();
  await expect(hold).toBeFocused();
  await page.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { instance } = (await ApiBinary.one('processInstanceGetRequest', { instanceId: id }));
    const task = instance.userTasks.find((candidate) => candidate.name === 'Prepare goods' && candidate.status === 'Open');
    await ApiBinary.one('processUserTaskCompleteRequest', { commandId: crypto.randomUUID(), instanceId: id,
      userTaskId: task.userTaskId, expectedRevision: instance.revision, outputs: {}, approved: null });
  }, instanceId);
  await expect(badge(win, 'TaskA')).toHaveCount(0, { timeout: 15000 });
  await expect(node(win, 'Join')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
  await expect(badge(win, 'Join')).toHaveAttribute('aria-label', /Held at a gateway/);
  await expect(badge(win, 'TaskB')).toHaveCount(1);
  await expect(hold).toBeFocused();

  await completeFromPanel(page, win, 'TaskB');
  await expect(win.locator('tf-chip', { hasText: 'Completed' }).first()).toBeVisible({ timeout: 15000 });
  await expect(win.locator('.fb-canvas .fb-runtime-badge')).toHaveCount(0, { timeout: 15000 });
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a sub-process token opens the sub-process diagram and the breadcrumb leads back', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const win = await openDiagram(page, await startRun(page, ids.nested));

  await expect(node(win, 'Sub_1')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
  await expect(badge(win, 'Sub_1').locator('b')).toHaveText('1');
  await expect(badge(win, 'Sub_1')).toHaveAttribute('aria-label', /Inside a subprocess/);
  await expect(win.locator('[data-trail]')).toBeHidden();

  await node(win, 'Sub_1').click();
  const open = win.locator('.fb-diagram-panel tf-button button', { hasText: 'Open subprocess' });
  await expect(open).toBeVisible();
  await expect(win.locator('.fb-diagram-panel')).toContainText('1 token inside');
  await open.click();

  await expect(node(win, 'InTask')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
  await expect(node(win, 'Sub_1')).toHaveCount(0);
  await expect(win.locator('[data-trail]')).toContainText('Main process');
  await expect(win.locator('[data-trail]')).toContainText('Check order');
  await expect(win.locator('.fb-canvas .fb-edge-path.fb-edge--taken')).toHaveCount(1);
  await node(win, 'InTask').click();
  await expect(win.locator('.fb-diagram-panel')).toContainText('Inner review');

  await win.locator('[data-trail] a.tf-breadcrumb-item', { hasText: 'Main process' }).click();
  await expect(node(win, 'Sub_1')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
  await expect(node(win, 'InTask')).toHaveCount(0);
  await expect(win.locator('[data-trail]')).toBeHidden();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a gateway with no matching path shows its incident on the element and in words', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const win = await openDiagram(page, await startRun(page, ids.blocked));

  await expect(node(win, 'Gate')).toHaveClass(/fb-node--token-incident/, { timeout: 15000 });
  await expect(badge(win, 'Gate')).toHaveAttribute('data-tone', 'incident');
  // Motion is allowed here: the incident ring pulses (the narrow test below asks for none).
  expect(await node(win, 'Gate').evaluate((element) => getComputedStyle(element).animationName)).toBe('fb-incident-pulse');
  await expect(badge(win, 'Gate')).toHaveAttribute('aria-label', /1 incident/);
  await node(win, 'Gate').click();
  const panel = win.locator('.fb-diagram-panel');
  await expect(panel).toContainText('No path matches and no default path is configured.');
  await expect(panel).not.toContainText('NO_MATCHING_FLOW');

  // The incident can be inspected: the lists open on it.
  await panel.locator('tf-button button', { hasText: 'Inspect incident' }).click();
  await expect(win.locator('.fb-process-lists')).toBeVisible();
  await expect(win.locator('[data-selected-incident]')).toHaveCount(1, { timeout: 15000 });
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

async function otherReader(browser, username, password) {
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1400, height: 1000 } });
  const page = await context.newPage();
  await loginAsAdmin(page, { port: PORT, username, password });
  return { page, context };
}

test('a reader who does not own the definition still gets the diagram with its tokens, and only its structure', async ({ page, browser }) => {
  test.setTimeout(240000);
  const errors = collectConsoleErrors(page);
  await loginAsAdmin(page, { port: PORT });
  const instanceId = await startRun(page, ids.assigned, { customer: 'private-customer' });
  const modelOf = (reader) => reader.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    try {
      const response = await ApiBinary.one('processInstanceModelGetRequest', { instanceId: id });
      return { structureOnly: response.structureOnly, text: JSON.stringify(response.model) };
    } catch (error) { return { refused: String(error.message) }; }
  }, instanceId);

  // The owner reads the model as published.
  const whole = await modelOf(page);
  expect(whole.structureOnly).toBe(false);
  expect(whole.text).toContain('outputs.secret_verdict');

  for (const [username, password, canComplete] of [['b4monitor', 'Monitor-pass-w0rd-1', false], ['b4worker', 'Worker-pass-w0rd-1', true]]) {
    const reader = await otherReader(browser, username, password);
    try {
      const readerErrors = collectConsoleErrors(reader.page);
      const projected = await modelOf(reader.page);
      expect(projected.structureOnly, username).toBe(true);
      expect(projected.text, username).toContain('Check the contract');
      expect(projected.text, username).not.toContain('secret_verdict');
      expect(projected.text, username).not.toContain(ids.workerId);

      const win = await openDiagram(reader.page, instanceId);
      await expect(node(win, 'Assigned')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
      await expect(badge(win, 'Assigned').locator('b')).toHaveText('1');
      await expect(win.locator('.fb-process-diagram tf-alert[tone="danger"]')).toHaveCount(0);
      await node(win, 'Assigned').click();
      const panel = win.locator('.fb-diagram-panel');
      await expect(panel).toContainText('Check the contract');
      await expect(panel.locator('tf-button button', { hasText: 'Complete work' })).toHaveCount(canComplete ? 1 : 0, { timeout: 15000 });
      if (username === 'b4monitor') await expect(win.locator('.fb-process-diagram')).toContainText('not the values it works with');
      expect(readerErrors, diagnostics(readerErrors, proc)).toEqual([]);
    } finally { await reader.context.close(); }
  }

  // A member who is neither initiator, owner, assignee nor monitor learns nothing about the instance.
  const stranger = await otherReader(browser, 'b4stranger', 'Stranger-pass-w0rd-1');
  try {
    const refused = await modelOf(stranger.page);
    expect(refused.refused).toBeTruthy();
    expect(refused.text).toBeUndefined();
  } finally { await stranger.context.close(); }
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test.describe('narrow screen, keyboard and reduced motion', () => {
  test.use({ viewport: { width: 390, height: 800 } });

  test('the diagram fits a phone, opens from the keyboard and holds still when asked', async ({ page }) => {
    test.setTimeout(120000);
    await page.emulateMedia({ reducedMotion: 'reduce' });
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    const win = await openDiagram(page, await startRun(page, ids.blocked));
    await expect(node(win, 'Gate')).toHaveClass(/fb-node--token-incident/, { timeout: 15000 });

    const overflow = await page.evaluate(() => {
      const content = document.querySelector('tf-window.fb-process-window:last-of-type .fb-process-content');
      return { page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        content: content.scrollWidth - content.clientWidth };
    });
    expect(overflow.page).toBeLessThanOrEqual(0);
    expect(overflow.content).toBeLessThanOrEqual(0);

    // Reduced motion: the incident ring does not pulse.
    expect(await node(win, 'Gate').evaluate((element) => getComputedStyle(element).animationName)).toBe('none');

    // A keyboard user reaches the element, opens it with Enter and the panel answers.
    await node(win, 'Gate').focus();
    await page.keyboard.press('Enter');
    await expect(win.locator('.fb-diagram-panel')).toContainText('Held at a gateway');
    await expect(node(win, 'Gate')).toBeFocused();
    expect(errors, diagnostics(errors, proc)).toEqual([]);
  });

  test('a fork and join opens whole on a phone, with Join and End inside the stage', async ({ page }) => {
    test.setTimeout(120000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    const win = await openDiagram(page, await startRun(page, ids.parallel));
    await expect(node(win, 'TaskA')).toHaveClass(/fb-node--token-waiting/, { timeout: 15000 });
    const inside = await page.evaluate(() => {
      const window = document.querySelector('tf-window.fb-process-window:last-of-type');
      const stage = window.querySelector('.fb-diagram-stage').getBoundingClientRect();
      return ['Start', 'Fork', 'TaskA', 'TaskB', 'Join', 'End'].map((id) => {
        const box = window.querySelector(`.fb-canvas .fb-node[data-node-id="${id}"]`).getBoundingClientRect();
        return { id, left: box.left >= stage.left - 1, right: box.right <= stage.right + 1, top: box.top >= stage.top - 1, bottom: box.bottom <= stage.bottom + 1 };
      });
    });
    for (const box of inside) expect(box, JSON.stringify(box)).toEqual({ id: box.id, left: true, right: true, top: true, bottom: true });
    expect(errors, diagnostics(errors, proc)).toEqual([]);
  });
});
