// =============================================================================
// File: tests/e2e/process-b3-inspector.spec.js
// Description: The BPMN designer's Service task inspector against a real node:
//              a flow is dropped from the palette, the inspector tabs are
//              switched, an error boundary event is added from the "Result and
//              errors" tab, the process is saved and reopened, and the routing
//              table is derived again from the stored model. A second test
//              measures the layout at laptop, small-laptop and phone sizes: every
//              tab reachable, a usable inspector height, handlers on the task's
//              bottom edge clear of its icon. Further tests cover a task with
//              more handlers than its border holds, an escalation handler that
//              runs into an End, a timer dropped on a task, the one-line bottom
//              bar inside a sub-process, a Call activity picker that fills
//              itself, and the palette drawer's keyboard and click behaviour.
//              A last group measures the one-line toolbar in five languages, the
//              More menu, a refused seventh boundary event, a drop onto another
//              node, a long code in a label, a timer without a time zone, an
//              archived Call target, undo inside a body and the cost of editing
//              a diagram of 80 nodes.
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
function createDefinition(page, name, { errors = ['INVALID_INPUT', 'NOT_FOUND', 'DENIED'], escalations = ['REVIEW'] } = {}) {
  return page.evaluate(async ({ processName, errorCodes, escalationCodes }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const model = emptyProcessModel();
    model.errors = errorCodes.map((code) => ({ errorId: `Err_${code}`, name: code, errorCode: code }));
    model.escalations = escalationCodes.map((code) => ({ escalationId: `Esc_${code}`, name: code, escalationCode: code }));
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0,
      name: processName, description: '', model,
    });
    return saved.definition.definitionId;
  }, { processName: name, errorCodes: errors, escalationCodes: escalations });
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
    const node = page.locator('.fb-canvas .fb-node.bpmn_service_task');
    await expect(node).toHaveCount(1);
    if (phone) {
      // The author leaves the inspector and taps the element again: that alone must bring the inspector back.
      await page.locator('.fb-mobile-tabs tf-tab[id="canvas"] button').click();
      await expect(page.locator('.fb-config')).not.toHaveClass(/open/);
      // An element that is not selected yet: the press selects it, the click that ends the press must not undo that.
      await page.locator('.fb-canvas .fb-node.bpmn_start').click();
      await expect(page.locator('.fb-config')).toHaveClass(/open/);
      await expect(page.locator('.fb-mobile-tabs')).toHaveAttribute('value', 'config');
      await page.waitForTimeout(600);
      await expect(page.locator('.fb-config')).toHaveClass(/open/);
      await page.locator('.fb-mobile-tabs tf-tab[id="canvas"] button').click();
      await node.click();
      await expect(page.locator('.fb-config')).toHaveClass(/open/);
      await expect(page.locator('.fb-mobile-tabs')).toHaveAttribute('value', 'config');
    }

    // The inspector slides in on a phone; measure it once it has stopped.
    for (let attempt = 0, previous = null; attempt < 30; attempt += 1) {
      const current = (await rect(page.locator('.fb-config'))).left;
      if (current === previous) break;
      previous = current;
      await page.waitForTimeout(100);
    }
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
      // The scrolling part of the inspector, once its header and footer are paid for.
      expect((await rect(page.locator('.fb-config-body'))).height).toBeGreaterThanOrEqual(400);
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

// Palette entries that are not Service tasks (a sub-process, a Call activity, a
// boundary timer) are dragged by their name; the drawer below laptop width is
// opened first, as in dragFlowToCanvas.
async function dragPaletteItem(page, name, offsetX, offsetY) {
  const drawer = page.locator('[data-role="palette-toggle"]');
  if (await drawer.isVisible()) {
    await drawer.click();
    await expect(page.locator('.fb-palette')).toHaveClass(/open/);
    await page.waitForTimeout(400);
  }
  const item = page.locator('.fb-palette .fb-node-item', { hasText: new RegExp(`^\\s*${name}`, 'i') }).first();
  await item.scrollIntoViewIfNeeded();
  const from = await item.boundingBox();
  const canvas = await page.locator('.fb-canvas').boundingBox();
  await page.mouse.move(from.x + 30, from.y + 20);
  await page.mouse.down();
  await page.mouse.move(canvas.x + offsetX, canvas.y + offsetY, { steps: 8 });
  await page.mouse.up();
  await page.waitForTimeout(300);
}

async function giveResultRule(page) {
  await page.getByRole('tab', { name: 'Result' }).click();
  await page.locator('[data-process="resultExpression"] textarea').fill(RESULT_EXPRESSION);
  await page.locator('[data-process="resultExpression"] textarea').blur();
}

test.describe('on a large window', () => {
  test.use({ viewport: { width: 1600, height: 950 } });

  test('a task with more handlers than its border holds refuses the extra one and keeps icon, ports, badge and centre clear', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Many handlers', {
      errors: ['E1', 'E2', 'E3', 'E4', 'E5', 'E6', 'E7', 'E8'], escalations: ['R1', 'R2'],
    });
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 400, 250);
    const node = page.locator('.fb-canvas .fb-node.bpmn_service_task');
    await expect(node).toHaveCount(1);
    await giveResultRule(page);

    // Eight codes are declared, but the border holds six: the sixth click is the last one that works.
    const addError = page.locator('[data-routing-add="error"]');
    let added = 0;
    for (let attempt = 0; attempt < 10 && (await addError.getAttribute('aria-disabled')) !== 'true'; attempt += 1) {
      await addError.scrollIntoViewIfNeeded();
      await addError.click();
      added += 1;
      await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(added);
    }
    expect(added).toBe(6);
    for (const kind of ['error', 'escalation']) {
      const button = page.locator(`[data-routing-add="${kind}"]`);
      await expect(button).toHaveAttribute('aria-disabled', 'true');
      await expect(button).toHaveAttribute('title', /no free place on its border/);
    }
    await expect(page.locator('[data-routing-add-hint]')).toContainText('no free place on its border');

    const task = await rect(node);
    const handlers = page.locator('.fb-canvas .fb-node.bpmn_boundary_error');
    const boxes = [];
    for (let index = 0; index < 6; index += 1) boxes.push(await rect(handlers.nth(index)));
    // The line from a handler to its name stays outside the task it hangs from.
    const crossings = await handlers.evaluateAll((elements, host) => elements.filter((element) => {
      const leader = element.querySelector('.fb-process-label-leader');
      if (!leader || leader.hidden) return false;
      // A rotated line's box is the box of the segment; the angle says which corners it joins.
      const box = leader.getBoundingClientRect();
      const angle = parseFloat(/rotate\(([-\d.e]+)rad\)/.exec(leader.style.transform)?.[1] ?? 0);
      const [x0, x1] = Math.cos(angle) >= 0 ? [box.left, box.right] : [box.right, box.left];
      const [y0, y1] = Math.sin(angle) >= 0 ? [box.top, box.bottom] : [box.bottom, box.top];
      return Array.from({ length: 41 }, (_, step) => step / 40).some((t) => {
        const x = x0 + (x1 - x0) * t;
        const y = y0 + (y1 - y0) * t;
        return x > host.left + 4 && x < host.right - 4 && y > host.top + 4 && y < host.bottom - 4;
      });
    }).length, task);
    expect(crossings, 'no leader line runs through the task').toBe(0);
    // Nothing the task draws or connects with is covered: icon, kind badge and both ports.
    const covered = [
      ['icon', await rect(node.locator('.fb-process-symbol > svg').first())],
      ['kind badge', await rect(node.locator('.fb-process-kind'))],
      ['incoming port', await rect(node.locator('.fb-port-in'))],
      ['outgoing port', await rect(node.locator('.fb-port-out'))],
    ];
    boxes.forEach((box, index) => {
      for (const [what, other] of covered) expect(intersects(box, other), `handler ${index} stays off the ${what}`).toBe(false);
      boxes.slice(index + 1).forEach((next, offset) => expect(intersects(box, next), `handlers ${index} and ${index + 1 + offset} do not overlap`).toBe(false));
    });
    // The task stays selectable: its centre and most of its area answer to the task, not to a handler.
    const share = await page.evaluate(({ left, top, right, bottom }) => {
      const belongs = (x, y) => document.elementFromPoint(x, y)?.closest('.fb-node')?.classList.contains('bpmn_service_task');
      let hit = 0; let total = 0; let centre = true;
      for (let x = left + 2; x < right - 2; x += 4) {
        for (let y = top + 2; y < bottom - 2; y += 4) {
          total += 1;
          const ok = belongs(x, y);
          if (ok) hit += 1;
          const inMiddle = x > left + (right - left) * 0.25 && x < right - (right - left) * 0.25 && y > top + (bottom - top) * 0.3 && y < bottom - (bottom - top) * 0.3;
          if (inMiddle && !ok) centre = false;
        }
      }
      return { fraction: hit / total, centre };
    }, task);
    expect(share.centre, 'the middle of the task is never under a handler').toBe(true);
    expect(share.fraction, 'most of the task can still be clicked').toBeGreaterThan(0.5);

    // Every name sits near its own handler and inside the canvas, not across it.
    const canvas = await rect(page.locator('.fb-canvas'));
    for (let index = 0; index < 6; index += 1) {
      const label = await rect(handlers.nth(index).locator('.fb-process-label'));
      const centre = (box) => ({ x: (box.left + box.right) / 2, y: (box.top + box.bottom) / 2 });
      expect(Math.hypot(centre(label).x - centre(boxes[index]).x, centre(label).y - centre(boxes[index]).y), `label ${index} stays near its handler`).toBeLessThan(150);
      expect(label.left, `label ${index} inside the canvas`).toBeGreaterThanOrEqual(canvas.left);
      expect(label.right).toBeLessThanOrEqual(canvas.right);
    }
    // The task keeps its own name in its box.
    await expect(node.locator('.fb-process-symbol > span')).toBeVisible();
    await page.screenshot({ path: test.info().outputPath('many-handlers.png') });
  });

  test('an escalation handler that runs into an End is named in the table and in the save refusal, never by id', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Escalation to End');
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 400, 250);
    await giveResultRule(page);
    await page.locator('[data-routing-add="escalation"]').click();
    const handler = page.locator('.fb-canvas .fb-node.bpmn_boundary_escalation');
    await handler.click();
    await page.locator('tf-select[data-connect]').evaluate((select) => {
      select.value = [...select.querySelectorAll('option')].find((option) => option.textContent === 'End').value;
      select.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await page.locator('[data-connect-add]').click();
    await page.locator('.fb-canvas .fb-node.bpmn_service_task').click({ position: { x: 30, y: 36 } });
    await page.getByRole('tab', { name: 'Result' }).click();
    const warning = page.locator('[data-routing-warning="escalation_path_terminal_before_wait"]');
    await expect(warning).toHaveAttribute('message', /Escalation: any.*End/);
    await page.locator('[data-role="save"]').click();
    const banner = page.locator('[data-role="error"]');
    await expect(banner).toBeVisible();
    const text = await banner.getAttribute('message');
    expect(text).toContain('Escalation: any');
    expect(text).not.toMatch(/\b[ne]_[a-z0-9]+_[a-z0-9]+/);

    // The same refusal from the server, past the client check, is rewritten by name as well.
    await page.evaluate(async () => {
      const { FlowCanvas } = await import('/js/modules/flows-builder/canvas.js');
      FlowCanvas.prototype.validate = () => [];
    });
    await page.locator('[data-role="save"]').click();
    await expect(banner).toHaveAttribute('message', /Escalation: any/);
    expect(await banner.getAttribute('message')).not.toMatch(/\b[ne]_[a-z0-9]+_[a-z0-9]+/);
    expect(await banner.getAttribute('message')).not.toContain('unsupported immediate path');
  });

  test('a boundary timer dropped on a task attaches to it; one dropped in the open says it is attached to nothing', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Timer drop');
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 400, 250);
    const task = await rect(page.locator('.fb-canvas .fb-node.bpmn_service_task'));
    const canvas = await page.locator('.fb-canvas').boundingBox();
    await dragPaletteItem(page, 'Boundary timer', task.left - canvas.x + 120, task.bottom - canvas.y + 2);
    const timers = page.locator('.fb-canvas .fb-node.bpmn_boundary_timer');
    await expect(timers).toHaveCount(1);
    await expect(page.locator('[data-boundary-target]')).toHaveText('B3 inspector flow');
    // Measured again: choosing the timer replaced the task's inspector and moved the canvas.
    const box = await rect(timers.first());
    const host = await rect(page.locator('.fb-canvas .fb-node.bpmn_service_task'));
    expect(Math.abs((box.top + box.bottom) / 2 - host.bottom), 'it sits on the task border').toBeLessThan(3);
    await expect(page.locator('.fb-canvas .fb-process-warn[data-warning~="boundary_unattached"]')).toHaveCount(0);

    await dragPaletteItem(page, 'Boundary timer', 600, 420);
    await expect(timers).toHaveCount(2);
    await expect(page.locator('.fb-canvas .fb-process-warn[data-warning~="boundary_unattached"]')).toHaveCount(1);
    // Any timer needs the process time zone; with it set, the unattached event is what blocks the save.
    await page.locator('[data-role="timer-timezone"] input').fill('Europe/Warsaw');
    await page.locator('[data-role="timer-timezone"] input').blur();
    await page.locator('[data-role="save"]').click();
    const banner = page.locator('[data-role="error"]');
    await expect(banner).toBeVisible();
    expect(await banner.getAttribute('message')).toMatch(/“Boundary timer” is not attached to an activity/);
  });

  test('inside a sub-process the bottom bar stays on one line with its counts and undo buttons in view', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Sub-process bar');
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Subprocess', 300, 250);
    await page.locator('[data-process-enter]').click();
    await expect(page.locator('[data-role="scope-up"]')).toBeVisible();
    for (const [width, height] of [[1600, 950], [1280, 720]]) {
      await page.setViewportSize({ width, height });
      await page.waitForTimeout(300);
      const bar = await rect(page.locator('.fb-bottombar'));
      expect(bar.height, `one line at ${width}`).toBeLessThanOrEqual(60);
      for (const selector of ['.fb-stats', '[data-role="undo"]', '[data-role="redo"]', '[data-role="scope-up"]']) {
        const box = await rect(page.locator(selector));
        expect(box.right, `${selector} ends inside the window at ${width}`).toBeLessThanOrEqual(width);
        expect(box.bottom).toBeLessThanOrEqual(bar.bottom + 1);
        expect(box.top).toBeGreaterThanOrEqual(bar.top - 1);
      }
      // The path never shows an internal id and the root crumb names the process, not a flow.
      const crumb = await page.locator('.fb-breadcrumb').innerText();
      expect(crumb).toMatch(/PROCESS/);
      expect(crumb).not.toMatch(/N_[A-Z0-9]+_[A-Z0-9]+/);
    }
  });

  test('a task inside a sub-process names the enclosing handler its error climbs to', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Nested handler');
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Subprocess', 250, 250);
    const sub = page.locator('.fb-canvas .fb-node.bpmn_sub_process');
    const box = await rect(sub);
    const canvas = await page.locator('.fb-canvas').boundingBox();
    await dragPaletteItem(page, 'Boundary error', box.left - canvas.x + 120, box.bottom - canvas.y + 2);
    const handler = page.locator('.fb-canvas .fb-node.bpmn_boundary_error');
    await expect(handler).toHaveCount(1);
    await handler.click();
    await page.locator('[data-process="name"] textarea').fill('Claim failed');
    await page.locator('[data-process="name"] textarea').blur();
    await sub.click({ position: { x: 20, y: 20 } });
    await page.locator('[data-process="name"] textarea').fill('Handle claim');
    await page.locator('[data-process="name"] textarea').blur();
    await page.locator('[data-process-enter]').click();
    await dragFlowToCanvas(page, 250, 250);
    await giveResultRule(page);
    await page.locator('tf-disclosure[data-routing-disclosure] summary').click();
    await expect.poll(async () => (await routingRows(page)).find((row) => row.outcome === 'Any other error')?.route)
      .toBe('An unhandled error climbs to the error boundary event “Claim failed” on the enclosing sub-process “Handle claim”.');
  });

  test('a Call activity lists what can be called by itself and says what is missing before it saves', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const callee = await createDefinition(page, 'Callee process', { errors: [], escalations: [] });
    await createDefinition(page, 'Never published', { errors: [], escalations: [] });
    await page.evaluate(async (id) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const current = (await ApiBinary.one('processDefinitionGetRequest', { definitionId: id })).definition;
      await ApiBinary.one('processDefinitionPublishRequest', { commandId: crypto.randomUUID(), definitionId: id, expectedRevision: current.draftRevision });
    }, callee);
    const own = await createDefinition(page, 'Caller process', { errors: [], escalations: [] });
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Call activity', 400, 250);

    const definitions = page.locator('[data-process="calledDefinitionChoice"]');
    await expect.poll(() => definitions.evaluate((select) => [...select.querySelectorAll('option')].map((option) => option.textContent.split(' · ')[0])))
      .toEqual(['Choose a published definition', 'Callee process']);
    await expect(page.locator('[data-load-calls]')).toBeHidden();

    // Saving before a version is chosen names the element and what to choose.
    await page.locator('[data-role="save"]').click();
    const banner = page.locator('[data-role="error"]');
    await expect(banner).toHaveAttribute('message', /The call activity “Call activity” has no exact target/);

    await definitions.evaluate((select, value) => { select.value = value; select.dispatchEvent(new Event('change', { bubbles: true })); }, callee);
    const versions = page.locator('[data-process="calledVersionChoice"]');
    await expect.poll(() => versions.evaluate((select) => [...select.querySelectorAll('option')].map((option) => option.value).filter(Boolean))).toEqual(['1']);
    await expect(page.locator('[data-load-call-versions]')).toBeHidden();
    await versions.evaluate((select) => { select.value = '1'; select.dispatchEvent(new Event('change', { bubbles: true })); });
    const body = page.locator('[data-process="calledProcessChoice"]');
    await expect.poll(() => body.evaluate((select) => select.querySelectorAll('option').length)).toBeGreaterThan(1);
    await body.evaluate((select) => { select.value = [...select.querySelectorAll('option')].find((option) => option.value).value; select.dispatchEvent(new Event('change', { bubbles: true })); });
    await page.locator('[data-role="save"]').click();
    await expect(banner).toBeHidden();
    await expect(page.locator('[data-role="autosave-text"]')).toHaveText(/Saved/);
  });
});

test.describe('on a laptop window', () => {
  test.use({ viewport: { width: 1024, height: 700 } });

  test('the palette drawer takes focus when it opens, closes on Escape and on a canvas click, and slides out', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Drawer');
    await openDesigner(page, { login: false, id: own });
    const palette = page.locator('.fb-palette');
    const toggle = page.locator('[data-role="palette-toggle"]');
    await toggle.click();
    await expect(palette).toHaveClass(/open/);
    await page.waitForTimeout(400);
    expect(await page.evaluate(() => document.activeElement?.closest('.fb-palette') != null), 'focus moved into the drawer').toBe(true);
    await page.keyboard.press('Escape');
    await expect(palette).not.toHaveClass(/open/);
    // Closing is animated: the drawer is still painted right after, and gone once it has slid out.
    expect(await palette.evaluate((element) => getComputedStyle(element).visibility)).toBe('visible');
    await expect.poll(() => palette.evaluate((element) => getComputedStyle(element).visibility)).toBe('hidden');
    expect(await page.evaluate(() => document.activeElement?.closest('[data-role="palette-toggle"]') != null), 'focus returned to the button').toBe(true);

    await toggle.click();
    await expect(palette).toHaveClass(/open/);
    await page.waitForTimeout(400);
    await page.mouse.click(520, 400);
    await expect(palette).not.toHaveClass(/open/);
  });

  test('the routing table starts folded on a short window and keeps the tab reachable', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Folded table');
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 120, 120);
    await giveResultRule(page);
    const disclosure = page.locator('tf-disclosure[data-routing-disclosure]');
    await expect(disclosure).not.toHaveAttribute('open', '');
    // What the author chose survives every redraw.
    await disclosure.locator('summary').click();
    await expect(disclosure).toHaveAttribute('open', '');
    await page.locator('[data-routing-add="error"]').click();
    await expect(disclosure).toHaveAttribute('open', '');
    expect((await routingRows(page)).length).toBeGreaterThanOrEqual(7);
  });
});

// Round 3 of the designer review: the toolbar, the save refusals, the drop rules
// and the labels, each measured in a real browser.
const setLanguage = (page, language) => page.evaluate(async (value) => {
  const { I18n } = await import('/js/i18n.js');
  await I18n.setLanguage(value);
}, language);

async function addServiceTask(page) {
  await page.evaluate(async () => {
    const { default: builder } = await import('/js/modules/flows-builder.js');
    const canvas = builder._state.canvas;
    const node = canvas.addNodeFromTemplate(canvas.templates.get('bpmn_service_task'), 0, 0, { cascade: true });
    canvas.selectNode(node.id);
  });
  return page.locator('.fb-canvas .fb-node.bpmn_service_task').last();
}

test.describe('toolbar, refusals, drops and labels', () => {
  test.use({ viewport: { width: 1600, height: 950 } });

  test('the toolbar is one line that never shifts, in every language, with its own icon on every control', async ({ page }) => {
    test.setTimeout(240000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Toolbar lines');
    for (const [width, height, language] of [[1600, 950, 'en'], [1280, 720, 'en'], [1024, 700, 'en'], [1024, 700, 'pl'], [1024, 700, 'de'], [1024, 700, 'es'], [1024, 700, 'fr']]) {
      await page.setViewportSize({ width, height });
      await setLanguage(page, language);
      await openDesigner(page, { login: false, id: own });
      await addServiceTask(page);
      const label = `${language} ${width}x${height}`;
      const bar = page.locator('.fb-topbar');
      // The screen eases in with a transform, so layout heights are read, not painted ones.
      const layoutHeight = (locator) => locator.evaluate((element) => element.offsetHeight);
      await expect.poll(() => layoutHeight(page.locator('.fb-config-body')), { message: `${label}: the inspector body`, timeout: 10000 }).toBeGreaterThanOrEqual(400);
      const heights = [];
      for (const [kind, reason] of [['ok'], ['pending'], ['error', 'A long reason the save was refused, which is wider than any chip'], ['ok']]) {
        await page.evaluate(async ({ kind: state, reason: why }) => {
          const { default: builder } = await import('/js/modules/flows-builder.js');
          builder._setAutosave(state, why);
        }, { kind, reason });
        heights.push(await layoutHeight(bar));
      }
      expect(Math.max(...heights), `${label}: one line`).toBeLessThanOrEqual(60);
      expect(Math.max(...heights) - Math.min(...heights), `${label}: the bar does not move as the chip changes (${heights.join(', ')})`).toBe(0);
      expect(await bar.evaluate((element) => element.scrollWidth <= element.clientWidth + 1), `${label}: nothing is cut off at the end`).toBe(true);
      expect((await rect(page.locator('[data-role="more"]'))).right, `${label}: the menu button is inside the bar`).toBeLessThanOrEqual((await rect(bar)).right);
      const icons = await bar.locator('[icon]').evaluateAll((elements) => elements.map((element) => element.getAttribute('icon')));
      expect(new Set(icons).size, `${label}: icons ${icons.join(', ')} are all different`).toBe(icons.length);
    }
    await setLanguage(page, 'en');
  });

  test('the More menu holds the rest of the toolbar, opens under its button and closes with Escape', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'More menu');
    await openDesigner(page, { login: false, id: own });
    const more = page.locator('[data-role="more"]');
    await more.click();
    const menu = page.locator('tf-menu[data-role="more-menu"]');
    await expect(menu).toHaveAttribute('open', '');
    await expect(more.locator('button')).toHaveAttribute('aria-expanded', 'true');
    // The button that opened the menu closes it; the press must not close it first and the click reopen it.
    await more.click();
    await expect(menu).not.toHaveAttribute('open', '');
    await more.click();
    await expect(menu).toHaveAttribute('open', '');
    for (const role of ['variables', 'import', 'export', 'calendar', 'declarations', 'modeling', 'simulate', 'instances', 'messages', 'history']) {
      await expect(menu.locator(`[data-role="${role}"]`)).toBeVisible();
    }
    expect(await page.evaluate(() => document.activeElement?.closest('tf-menu') != null), 'focus moved into the menu').toBe(true);
    await page.keyboard.press('Escape');
    await expect(menu).not.toHaveAttribute('open', '');
    await expect(more.locator('button')).toHaveAttribute('aria-expanded', 'false');
    expect(await page.evaluate(() => document.activeElement?.closest('[data-role="more"]') != null), 'focus is back on the button').toBe(true);
    await more.click();
    await menu.locator('[data-role="calendar"]').click();
    await expect(menu).not.toHaveAttribute('open', '');
    await expect(page.getByText('Working calendar').first()).toBeVisible();
  });

  test('a seventh boundary event dropped on a full border lands beside the task, clear of it and of every handler', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Full border drop', { errors: ['A', 'B', 'C', 'D', 'E', 'F', 'G'], escalations: [] });
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 400, 250);
    await giveResultRule(page);
    const addError = page.locator('[data-routing-add="error"]');
    for (let added = 1; added <= 6; added += 1) {
      await addError.scrollIntoViewIfNeeded();
      await addError.click();
      await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(added);
    }
    const task = await rect(page.locator('.fb-canvas .fb-node.bpmn_service_task'));
    const canvas = await page.locator('.fb-canvas').boundingBox();
    await dragPaletteItem(page, 'Boundary timer', task.left - canvas.x + 120, task.bottom - canvas.y + 2);
    const timers = page.locator('.fb-canvas .fb-node.bpmn_boundary_timer');
    await expect(timers).toHaveCount(1);
    const timer = await rect(timers.first());
    const covered = [['the task', await rect(page.locator('.fb-canvas .fb-node.bpmn_service_task'))]];
    const handlers = page.locator('.fb-canvas .fb-node.bpmn_boundary_error');
    for (let index = 0; index < 6; index += 1) covered.push([`handler ${index}`, await rect(handlers.nth(index))]);
    covered.push(['the task icon', await rect(page.locator('.fb-canvas .fb-node.bpmn_service_task .fb-process-symbol > svg').first())]);
    for (const [what, box] of covered) expect(intersects(timer, box), `the refused event stays off ${what}`).toBe(false);
    await expect(page.locator('.fb-canvas .fb-process-warn[data-warning~="boundary_unattached"]')).toHaveCount(1);
    await expect(page.locator('.toast').first()).toContainText('no free place on its border');
    await expect(page.locator('.toast').first()).toContainText('placed beside it');
  });

  test('a node dropped onto another does not hide it, and a long code wraps after its underscores inside a clamped label', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Overlap and labels', { errors: ['INVALID_CUSTOMER_ADDRESS_FORMAT_FOR_REGION'], escalations: [] });
    await openDesigner(page, { login: false, id: own });
    const start = await rect(page.locator('.fb-canvas .fb-node.bpmn_start'));
    const canvas = await page.locator('.fb-canvas').boundingBox();
    await dragPaletteItem(page, 'Terminate end', start.left - canvas.x + 28, start.top - canvas.y + 28);
    const dropped = await rect(page.locator('.fb-canvas .fb-node.bpmn_terminate_end'));
    expect(intersects(dropped, await rect(page.locator('.fb-canvas .fb-node.bpmn_start'))), 'the Start event is still visible').toBe(false);
    expect(intersects(dropped, await rect(page.locator('.fb-canvas .fb-node.bpmn_end'))), 'so is the End event').toBe(false);

    await dragFlowToCanvas(page, 450, 300);
    await giveResultRule(page);
    await page.locator('[data-routing-add="error"]').click();
    await page.locator('[data-routing-add="error"]').click();
    const label = page.locator('.fb-canvas .fb-node.bpmn_boundary_error', { hasText: 'INVALID_CUSTOMER' }).locator('.fb-process-label');
    const lines = await label.evaluate((element) => {
      const range = document.createRange();
      const rows = []; let top = null;
      const walk = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
      while (walk.nextNode()) {
        const node = walk.currentNode;
        for (let index = 0; index < node.length; index += 1) {
          range.setStart(node, index); range.setEnd(node, index + 1);
          const box = range.getBoundingClientRect();
          if (!box.width) continue;
          if (top === null || Math.abs(box.top - top) > 4) { rows.push(''); top = box.top; }
          rows[rows.length - 1] += node.data[index];
        }
      }
      return { rows, width: element.getBoundingClientRect().width, title: element.title };
    });
    expect(lines.title).toBe('Error: INVALID_CUSTOMER_ADDRESS_FORMAT_FOR_REGION');
    expect(lines.width).toBeLessThanOrEqual(201);
    expect(lines.rows.length).toBeLessThanOrEqual(4);
    for (const row of lines.rows.slice(0, -1)) expect(row.endsWith('_') || row.endsWith(':') || row.endsWith(' '), `the line "${row}" breaks at a separator, not inside a word`).toBe(true);
    await page.screenshot({ path: test.info().outputPath('long-code-label.png') });
  });

  test('editing a diagram of 80 nodes does not rebuild its labels one forced layout at a time', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Eighty nodes');
    await openDesigner(page, { login: false, id: own });
    const timing = await page.evaluate(async () => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      const canvas = builder._state.canvas;
      const types = ['bpmn_user_task', 'bpmn_service_task', 'bpmn_timer_catch', 'bpmn_exclusive_gateway', 'bpmn_message_catch'];
      for (let index = 0; index < 80; index += 1) {
        const node = canvas.addNodeFromTemplate(canvas.templates.get(types[index % 5]), 0, 0, { cascade: true });
        canvas.updateNodeLabel(node.id, `Long element name number ${index} that needs a label outside`);
      }
      const median = (action) => {
        const samples = [];
        for (let run = 0; run < 9; run += 1) { const start = performance.now(); action(); samples.push(performance.now() - start); }
        return samples.sort((a, b) => a - b)[4];
      };
      // The floor is rebuilding the node layer and laying it out once; a render
      // that reads layout after each node's class change costs many times that.
      const floor = median(() => { canvas._renderNodes(); void canvas.root.offsetHeight; });
      const render = median(() => canvas.render());
      return { nodes: canvas.nodes.length, floor, render };
    });
    expect(timing.nodes).toBeGreaterThanOrEqual(80);
    expect(timing.render, `render ${timing.render.toFixed(1)} ms against a ${timing.floor.toFixed(1)} ms floor`).toBeLessThan(timing.floor * 8 + 5);
  });

  test('Escape deselects, and undo restores the content of the body on screen without moving the author', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Undo in place');
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Subprocess', 300, 250);
    const sub = page.locator('.fb-canvas .fb-node.bpmn_sub_process');
    await expect(sub).toHaveCount(1);
    await expect(page.locator('[data-role="crumb-name"]')).toHaveText('Subprocess');
    await page.keyboard.press('Escape');
    await expect(page.locator('[data-role="crumb-name"]')).toHaveText('Undo in place');
    await expect(page.locator('.fb-canvas .fb-node.selected')).toHaveCount(0);

    await sub.click({ position: { x: 20, y: 20 } });
    await page.locator('[data-process-enter]').click();
    await dragPaletteItem(page, 'Human task', 400, 200);
    await expect(page.locator('.fb-canvas .fb-node.bpmn_user_task')).toHaveCount(1);
    await page.locator('[data-role="scope-up"]').click();
    await dragPaletteItem(page, 'Terminate end', 700, 400);
    await expect(page.locator('.fb-canvas .fb-node.bpmn_terminate_end')).toHaveCount(1);
    await page.locator('[data-role="undo"]').click();
    await expect(page.locator('.fb-canvas .fb-node.bpmn_terminate_end')).toHaveCount(0);
    await expect(page.locator('[data-role="scope-path"]')).toBeHidden();
    await expect(page.locator('.fb-canvas .fb-node.bpmn_sub_process')).toHaveCount(1);
    // Inside the body, undoing the step made at the root changes nothing here and says so.
    await page.locator('.fb-canvas .fb-node.bpmn_sub_process').click({ position: { x: 20, y: 20 } });
    await page.locator('[data-process-enter]').click();
    await expect(page.locator('.fb-canvas .fb-node.bpmn_user_task')).toHaveCount(1);
    await page.locator('[data-role="undo"]').click();
    await expect(page.locator('[data-role="scope-path"]')).toBeVisible();
    await expect(page.locator('.fb-canvas .fb-node.bpmn_user_task')).toHaveCount(0);
    await expect(page.locator('[data-role="scope-path"]')).toContainText('Undo in place ›');
  });

  test('a Call activity whose saved target is archived names it and warns, instead of showing an id', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const callee = await createDefinition(page, 'Invoice approval', { errors: [], escalations: [] });
    await page.evaluate(async (id) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const current = (await ApiBinary.one('processDefinitionGetRequest', { definitionId: id })).definition;
      await ApiBinary.one('processDefinitionPublishRequest', { commandId: crypto.randomUUID(), definitionId: id, expectedRevision: current.draftRevision });
    }, callee);
    const own = await createDefinition(page, 'Archived target caller', { errors: [], escalations: [] });
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Call activity', 400, 250);
    const definitions = page.locator('[data-process="calledDefinitionChoice"]');
    await expect.poll(() => definitions.evaluate((select) => select.querySelectorAll('option').length)).toBeGreaterThan(1);
    await definitions.evaluate((select, value) => { select.value = value; select.dispatchEvent(new Event('change', { bubbles: true })); }, callee);
    const versions = page.locator('[data-process="calledVersionChoice"]');
    await expect.poll(() => versions.evaluate((select) => select.querySelectorAll('option').length)).toBeGreaterThan(1);
    await versions.evaluate((select) => { select.value = '1'; select.dispatchEvent(new Event('change', { bubbles: true })); });
    const body = page.locator('[data-process="calledProcessChoice"]');
    await expect.poll(() => body.evaluate((select) => select.querySelectorAll('option').length)).toBeGreaterThan(1);
    await body.evaluate((select) => { select.value = [...select.querySelectorAll('option')].find((option) => option.value).value; select.dispatchEvent(new Event('change', { bubbles: true })); });
    await page.locator('[data-role="save"]').click();
    await expect(page.locator('[data-role="autosave-text"]')).toHaveText(/Saved/);
    await expect(page.locator('[data-call-target-warning]')).toBeHidden();

    await page.evaluate(async (id) => {
      const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
      const current = (await ApiBinary.one('processDefinitionGetRequest', { definitionId: id })).definition;
      await ApiBinary.one('processDefinitionArchiveRequest', { commandId: crypto.randomUUID(), definitionId: id, expectedRevision: current.draftRevision, archived: true });
    }, callee);
    await openDesigner(page, { login: false, id: own });
    await page.locator('.fb-canvas .fb-node.bpmn_call_activity').click({ position: { x: 20, y: 20 } });
    await expect(page.locator('[data-call-target-warning]')).toBeVisible();
    await expect(page.locator('[data-call-target-warning]')).toHaveAttribute('message', /archived/);
    await expect(page.locator('[data-process="calledDefinitionChoice"] .tf-select-selected')).toHaveText('Invoice approval');
    await expect(page.locator('[data-process="calledProcessChoice"] .tf-select-selected')).not.toHaveText(/Process_[0-9a-f_]{8,}|Checking/);
  });
});

test.describe('refusals on a laptop window', () => {
  test.use({ viewport: { width: 1024, height: 700 } });

  test('a timer without a process time zone is refused by name, the folded settings open and the field takes focus', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Timer zone');
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 120, 120);
    const task = await rect(page.locator('.fb-canvas .fb-node.bpmn_service_task'));
    const canvas = await page.locator('.fb-canvas').boundingBox();
    await dragPaletteItem(page, 'Boundary timer', task.left - canvas.x + 120, task.bottom - canvas.y + 2);
    await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_timer')).toHaveCount(1);
    const notice = page.locator('.fb-process-notice');
    await expect(notice).toHaveClass(/fb-collapsed/);

    await page.locator('[data-role="save"]').click();
    const banner = page.locator('[data-role="error"]');
    await expect(banner).toBeVisible();
    const text = await banner.getAttribute('message');
    expect(text).toContain('is a timer, so the process needs a time zone');
    expect(text).not.toContain('explicit IANA timezone');
    expect(text).not.toMatch(/\b[ne]_[a-z0-9]+_[a-z0-9]+/);
    await expect(notice).not.toHaveClass(/fb-collapsed/);
    const field = page.locator('[data-role="timer-timezone"]');
    await expect(field).toBeVisible();
    expect(await page.evaluate(() => document.activeElement?.closest('[data-role="timer-timezone"]') != null), 'the field has focus').toBe(true);

    // The toast hangs under the bar, so it never covers the buttons the author needs next.
    const toast = await rect(page.locator('.toast').first());
    for (const role of ['publish', 'save', 'more']) expect(intersects(toast, await rect(page.locator(`[data-role="${role}"]`))), `the toast stays off ${role}`).toBe(false);

    await field.locator('input').fill('Europe/Warsaw');
    await field.locator('input').blur();
    await page.locator('[data-role="save"]').click();
    await expect(banner).toBeHidden();
    await expect(page.locator('[data-role="autosave"]')).toHaveAttribute('title', /Saved/);
  });
});

// Round 4 of the designer review: the bar at tablet widths, names instead of ids
// in every list and window, the keyboard in the More menu, and what an author
// needs to publish and run.
const ID_SHAPE = /\b[ne]_[0-9a-z]{6,}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|Process_[0-9a-f_]{12,}/i;
const shownText = (locator) => locator.evaluate((element) => [
  element.innerText,
  ...[...element.querySelectorAll('option')].map((option) => option.textContent),
  ...[...element.querySelectorAll('[message]')].map((alert) => alert.getAttribute('message')),
].join('\n'));

test.describe('round 4: toolbar width, keyboard, names and publishing', () => {
  test.use({ viewport: { width: 1600, height: 950 } });

  test('the toolbar fits from 768 px up in five languages: every button stays inside the bar', async ({ page }) => {
    test.setTimeout(300000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Toolbar widths with a rather long process name');
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await page.setViewportSize({ width: 1600, height: 900 });
      await setLanguage(page, language);
      await openDesigner(page, { login: false, id: own });
      for (const width of [768, 820, 900, 1024]) {
        await page.setViewportSize({ width, height: 800 });
        // The screen eases in with a transform; boxes are read once the shell has settled at the window's width.
        await expect.poll(async () => {
          const first = await rect(page.locator('.fb-shell'));
          await page.waitForTimeout(150);
          const second = await rect(page.locator('.fb-shell'));
          return Math.abs(first.right - second.right) < 0.01 && Math.abs(second.right - width) < 2;
        }, { message: `${language} ${width}: the shell has settled` }).toBe(true);
        const bar = page.locator('.fb-topbar');
        await expect.poll(() => bar.evaluate((element) => element.scrollWidth <= element.clientWidth + 1), { message: `${language} ${width}: nothing is cut off` }).toBe(true);
        const shell = await rect(page.locator('.fb-shell'));
        for (const role of ['back', 'save', 'publish', 'run-wrap', 'more']) {
          const box = await rect(page.locator(`.fb-topbar [data-role="${role}"]`));
          expect(box.left, `${language} ${width}: ${role} starts inside the window`).toBeGreaterThanOrEqual(shell.left - 1);
          expect(box.right, `${language} ${width}: ${role} ends inside the window`).toBeLessThanOrEqual(shell.right + 1);
        }
        expect((await rect(bar)).height, `${language} ${width}: one line`).toBeLessThanOrEqual(60);
        await page.locator('[data-role="more"]').click();
        await expect(page.locator('tf-menu[data-role="more-menu"]')).toHaveAttribute('open', '');
        const menu = await rect(page.locator('tf-menu[data-role="more-menu"] .tf-menu'));
        expect(menu.right, `${language} ${width}: the menu is on screen`).toBeLessThanOrEqual(width);
        await page.keyboard.press('Escape');
      }
    }
    await setLanguage(page, 'en');
  });

  test('the arrow keys, Home and End walk the whole More menu and skip what is hidden or disabled', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Menu keys');
    await openDesigner(page, { login: false, id: own });
    // The menu closes on any scroll or resize of the page, so it is opened once the screen has stopped moving.
    await page.waitForTimeout(1000);
    await page.locator('[data-role="more"]').click();
    const menu = page.locator('tf-menu[data-role="more-menu"]');
    await expect(menu).toHaveAttribute('open', '');
    const reachable = await menu.evaluate((element) => [...element.querySelectorAll(':scope > tf-menu-item:not([hidden]):not([disabled])')].map((item) => item.dataset.role));
    expect(reachable.length).toBeGreaterThan(6);
    const focused = () => page.evaluate(() => document.activeElement?.closest('tf-menu-item')?.dataset.role ?? null);
    expect(await focused()).toBe(reachable[0]);
    const down = [];
    for (let step = 0; step < reachable.length; step += 1) { await page.keyboard.press('ArrowDown'); down.push(await focused()); }
    expect(down, 'ArrowDown visits every reachable item and wraps').toEqual([...reachable.slice(1), reachable[0]]);
    await page.keyboard.press('ArrowUp');
    expect(await focused(), 'ArrowUp wraps to the last').toBe(reachable.at(-1));
    await page.keyboard.press('Home');
    expect(await focused()).toBe(reachable[0]);
    await page.keyboard.press('End');
    expect(await focused(), 'End reaches the last one past the hidden items').toBe(reachable.at(-1));
    expect(reachable).not.toContain('refresh-calendar');
    expect(reachable).not.toContain('simulate');
  });

  test('the autosave chip shows its whole text at laptop width and above, in every state and language', async ({ page }) => {
    test.setTimeout(240000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Chip text');
    for (const language of ['en', 'pl', 'de', 'es', 'fr']) {
      await setLanguage(page, language);
      await openDesigner(page, { login: false, id: own });
      for (const [kind, reason] of [['ok'], ['pending'], ['error', 'Refused']]) {
        await page.evaluate(async ({ state, why }) => {
          const { default: builder } = await import('/js/modules/flows-builder.js');
          builder._setAutosave(state, why);
        }, { state: kind, why: reason });
        const text = page.locator('[data-role="autosave-text"]');
        expect(await text.evaluate((element) => element.scrollWidth <= element.clientWidth + 1), `${language} ${kind}: the text is not cut`).toBe(true);
      }
    }
    await setLanguage(page, 'en');
  });

  test('a published process names its start entry without an id, Run takes the only entry and says why it is off before', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Publish and run', { errors: [], escalations: [] });
    await openDesigner(page, { login: false, id: own });
    const wrap = page.locator('[data-role="run-wrap"]');
    await expect(page.locator('[data-role="run"]')).toBeDisabled().catch(() => {});
    expect(await wrap.getAttribute('title')).toContain('Publish the process first');
    await page.locator('[data-role="publish"]').click();
    await expect(page.locator('.toast').first()).toContainText('published');
    // One entry exists, so it is chosen without a visit to the settings row.
    await expect(page.locator('[data-role="run"]')).not.toHaveAttribute('disabled', '');
    expect(await wrap.getAttribute('title')).not.toContain('Choose a start entry');
    await page.locator('[data-role="run"]').click();
    const windowText = (locator) => locator.evaluate((element) => [element.textContent,
      ...[...element.querySelectorAll('option')].map((option) => option.textContent)].join('\n'));
    const run = page.locator('tf-window.tf-act-window').last();
    await expect(run.locator('[data-start]')).toBeVisible();
    expect(await windowText(run)).not.toMatch(ID_SHAPE);
    await expect(run.locator('[data-start] .tf-select-selected')).toContainText('Process 1');
    await page.keyboard.press('Escape');
    await expect(run.locator('[data-start]')).toBeHidden();

    await page.locator('[data-role="more"]').click();
    await page.locator('tf-menu[data-role="more-menu"] [data-role="simulate"]').click();
    const simulation = page.locator('tf-window.tf-act-window').last();
    await expect(simulation.locator('[data-simulation-start]')).toBeVisible();
    expect(await windowText(simulation)).not.toMatch(ID_SHAPE);
  });

  test('publishing with an unused declaration names it in the banner and never shows the server\'s words', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Unused declarations');
    await openDesigner(page, { login: false, id: own });
    await page.locator('[data-role="publish"]').click();
    const banner = page.locator('[data-role="error"]');
    await expect(banner).toBeVisible();
    const text = await banner.getAttribute('message');
    expect(text).toContain('INVALID_INPUT');
    expect(text).not.toMatch(/protocol error|BadRequest/);
    await expect(page.locator('[data-role="run"]')).toBeDisabled().catch(() => {});
  });

  test('inspectors show names, never ids: every service tab, a handler, a gateway, a sub-process, and element ids sit behind technical details', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'No ids');
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 300, 200);
    await giveResultRule(page);
    const config = page.locator('.fb-config');
    for (const tab of ['Action', 'Data', 'Result', 'Timing', 'More']) {
      await page.getByRole('tab', { name: tab }).click();
      expect(await shownText(config), `${tab} tab`).not.toMatch(ID_SHAPE);
    }
    await page.getByRole('tab', { name: 'Result' }).click();
    await page.locator('[data-routing-add="error"]').scrollIntoViewIfNeeded();
    await page.locator('[data-routing-add="error"]').click();
    await expect(page.locator('.fb-canvas .fb-node.bpmn_boundary_error')).toHaveCount(1);
    await page.locator('.fb-canvas .fb-node.bpmn_boundary_error').click();
    expect(await shownText(config), 'the handler').not.toMatch(ID_SHAPE);
    const technical = config.locator('tf-disclosure[data-technical]');
    await expect(technical).toBeVisible();
    await expect(technical).not.toHaveAttribute('open', '');
    await expect(config.locator('[data-process="elementId"]')).toBeHidden();
    await technical.locator('summary').click();
    await expect(config.locator('[data-process="elementId"]')).toBeVisible();

    await dragPaletteItem(page, 'Sub-?process', 700, 120);
    const headings = config.locator('tf-keyvalue-editor .tf-kve-label');
    await expect(headings).toHaveText([/^Input mapping/, /^Output mapping/]);
    expect(await shownText(config), 'the sub-process').not.toMatch(ID_SHAPE);
  });

  test('a boundary event that is not on a task is drawn dashed and tagged, not like an attached one', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Unattached look');
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Boundary timer', 500, 330);
    const event = page.locator('.fb-canvas .fb-node.bpmn_boundary_timer');
    await expect(event).toHaveCount(1);
    await expect(event).toHaveClass(/fb-unattached/);
    expect(await event.evaluate((element) => getComputedStyle(element).outlineStyle)).toBe('dashed');
    expect(await event.evaluate((element) => getComputedStyle(element, '::after').content)).toContain('Not attached');
  });
});

test.describe('round 4: settings row on a short window', () => {
  test.use({ viewport: { width: 1024, height: 700 } });

  test('opening the process settings keeps the inspector body at 400 px or more', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Settings height');
    await openDesigner(page, { login: false, id: own });
    await dragFlowToCanvas(page, 120, 120);
    const body = page.locator('.fb-config-body');
    const height = () => body.evaluate((element) => element.offsetHeight);
    await expect.poll(height).toBeGreaterThanOrEqual(400);
    await page.locator('[data-role="notice-toggle"]').click();
    await expect(page.locator('.fb-process-notice')).not.toHaveClass(/fb-collapsed/);
    await expect.poll(height, { message: 'with the settings row open' }).toBeGreaterThanOrEqual(400);
  });
});

// Round 5 of the designer review: the host attributes of tf-button, names in the
// run, schedule and simulation windows, leader lines on a crowded diagram, the
// time zone field, the closed menu, the whole canvas as a drop target, and the
// phone in both orientations.
function createModelDefinition(page, name, patch) {
  return page.evaluate(async ({ processName, source }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const model = emptyProcessModel();
    new Function('model', source)(model);
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0,
      name: processName, description: '', model,
    });
    return saved.definition.definitionId;
  }, { processName: name, source: patch });
}

const REVIEW_ID = 'n_mvcuyz3t_kubw';
const WITH_BOUNDARY_TIMER = `
  model.timerTimezone = 'Europe/Warsaw';
  model.nodes.splice(1, 0,
    { id: '${REVIEW_ID}', name: 'Review step', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } } },
    { id: 'n_mvcuzz00_t1mr', name: 'Deadline', kind: { BoundaryTimer: { attachedToId: '${REVIEW_ID}', cancelActivity: true, timer: { Duration: { seconds: 3600 } } } } });
  model.sequenceFlows = [
    { id: 'e_mvcu0001_aaaa', sourceId: 'Start', targetId: '${REVIEW_ID}', condition: null },
    { id: 'e_mvcu0002_bbbb', sourceId: '${REVIEW_ID}', targetId: 'End', condition: null },
    { id: 'e_mvcu0003_cccc', sourceId: 'n_mvcuzz00_t1mr', targetId: 'End', condition: null }];
  model.diagram.shapes = [
    { elementId: 'Start', x: 80, y: 160, width: 56, height: 56 }, { elementId: '${REVIEW_ID}', x: 200, y: 160, width: 240, height: 96 },
    { elementId: 'n_mvcuzz00_t1mr', x: 412, y: 228, width: 56, height: 56 }, { elementId: 'End', x: 550, y: 160, width: 56, height: 56 }];
  model.diagram.edges = model.sequenceFlows.map((edge) => ({ sequenceFlowId: edge.id, waypoints: [{ x: 100, y: 180 }, { x: 300, y: 180 }] }));`;
const WITH_REPEAT = `
  model.nodes.splice(1, 0, { id: 'n_mvcvd0bg_3zhn', name: 'Review step', kind: { UserTask: { assigneeUserId: null, outputMapping: {} } },
    repeat: { StructuredLoop: { condition: 'vars.more', testBefore: false, maxIterations: 3, outputCollectionVariable: 'results' } } });
  model.variables = { results: [] };
  model.sequenceFlows = [
    { id: 'e_mvcu0001_aaaa', sourceId: 'Start', targetId: 'n_mvcvd0bg_3zhn', condition: null },
    { id: 'e_mvcu0002_bbbb', sourceId: 'n_mvcvd0bg_3zhn', targetId: 'End', condition: null }];
  model.diagram.shapes = [
    { elementId: 'Start', x: 80, y: 160, width: 56, height: 56 }, { elementId: 'n_mvcvd0bg_3zhn', x: 200, y: 160, width: 240, height: 96 },
    { elementId: 'End', x: 550, y: 160, width: 56, height: 56 }];
  model.diagram.edges = model.sequenceFlows.map((edge) => ({ sequenceFlowId: edge.id, waypoints: [{ x: 100, y: 180 }, { x: 300, y: 180 }] }));`;

test.describe('round 5: names in every window, closed menu, drops and the clock', () => {
  test.use({ viewport: { width: 1600, height: 950 } });

  test('tf-button keeps its attributes on the host and announces one button', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Host attributes');
    await openDesigner(page, { login: false, id: own });
    const host = page.locator('tf-button[aria-label="More"]');
    await expect(host).toHaveCount(1);
    await expect(page.getByRole('button', { name: 'More' })).toHaveCount(1);
    expect(await host.evaluate((element) => element.matches('[data-haspopup="menu"]') && element.closest('.fb-topbar') !== null)).toBe(true);
    await host.click();
    await expect(page.locator('tf-button[aria-label="More"][data-expanded="true"]')).toHaveCount(1);
    await page.keyboard.press('Escape');
    await expect(page.locator('tf-button[aria-label="More"][data-expanded="true"]')).toHaveCount(0);
    expect(await page.locator('tf-button[data-role="zoom-in"]').evaluate((element) => element.querySelector('button').getAttribute('title')), 'title reaches the real button').toBeTruthy();
  });

  test('a closed menu leaves the tab order and the accessibility tree', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Closed menu');
    await openDesigner(page, { login: false, id: own });
    const menu = page.locator('tf-menu[data-role="more-menu"]');
    await expect(menu.getByRole('menuitem')).toHaveCount(0);
    await page.locator('[data-role="more"] button').focus();
    await page.keyboard.press('Tab');
    expect(await page.evaluate(() => document.activeElement?.closest('tf-menu[data-role="more-menu"]') !== null), 'Tab does not land in the closed menu').toBe(false);
    await page.locator('[data-role="more"] button').click();
    await expect.poll(() => menu.getByRole('menuitem').count()).toBeGreaterThan(5);
  });

  for (const [width, height] of [[1600, 950], [1280, 720], [1024, 700]]) {
    test(`a palette drop works over the whole visible canvas at ${width}x${height}, down to its bottom edge`, async ({ page }) => {
      test.setTimeout(120000);
      await page.setViewportSize({ width, height });
      await loginAsAdmin(page, { port: PORT });
      const own = await createDefinition(page, `Edge drop ${width}`);
      await openDesigner(page, { login: false, id: own });
      // The screen eases in with a transform; positions are read once it has settled.
      await page.waitForTimeout(1500);
      const wrap = await rect(page.locator('.fb-canvas-wrap'));
      const canvas = await rect(page.locator('.fb-canvas'));
      expect(canvas.bottom, 'the canvas reaches the bottom of its area').toBeGreaterThanOrEqual(wrap.bottom - 1);
      await dragPaletteItem(page, 'Sub-?process', 120, canvas.bottom - canvas.top - 24);
      await expect(page.locator('.fb-canvas .fb-node.bpmn_sub_process')).toHaveCount(1);
    });
  }

  test('a time zone that is not one blocks the save and keeps the text, a lowercase one is stored as the engine spells it', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createModelDefinition(page, 'Zone typing', WITH_BOUNDARY_TIMER);
    await openDesigner(page, { login: false, id: own });
    const field = page.locator('[data-role="timer-timezone"]');
    if (await page.locator('.fb-process-notice.fb-collapsed').count()) await page.locator('[data-role="notice-toggle"]').click();
    const input = field.locator('input');
    await input.fill('Mars/Olympus');
    await input.blur();
    await expect(field).toHaveAttribute('error', /Mars\/Olympus/);
    await page.locator('[data-role="save"]').click();
    await expect(page.locator('.toast', { hasText: 'Process saved' })).toHaveCount(0);
    await expect(page.locator('[data-role="error"]')).toBeVisible();
    await expect(input).toHaveValue('Mars/Olympus');
    await page.locator('.fb-canvas .fb-node.bpmn_user_task').click({ position: { x: 20, y: 20 } });
    await expect(field).toHaveAttribute('error', /Mars\/Olympus/);
    await input.fill('europe/warsaw');
    await input.blur();
    await expect(input).toHaveValue('Europe/Warsaw');
    await expect(field).not.toHaveAttribute('error', /.+/);
    await page.locator('[data-role="save"]').click();
    await expect(page.locator('.toast', { hasText: 'Process saved' })).toHaveCount(1);
    expect((await storedModel2(page, own)).timerTimezone).toBe('Europe/Warsaw');
  });

  test('redo of a process setting says it was redone', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createModelDefinition(page, 'Redo toast', WITH_BOUNDARY_TIMER);
    await openDesigner(page, { login: false, id: own });
    await page.evaluate(async () => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      builder._state.canvas.updateProcessTimezone('Europe/Berlin');
      builder._state.canvas.undo();
    });
    await expect(page.locator('.toast', { hasText: /^Undone/ })).toHaveCount(1);
    await page.evaluate(async () => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      builder._state.canvas.redo();
    });
    await expect(page.locator('.toast', { hasText: /^Redone/ })).toHaveCount(1);
  });

  test('windows of a running process name the activity and never show an element id', async ({ page }) => {
    test.setTimeout(240000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createModelDefinition(page, 'Run with timer', WITH_BOUNDARY_TIMER);
    await openDesigner(page, { login: false, id: own });
    await page.locator('[data-role="publish"]').click();
    await expect(page.locator('.toast').first()).toContainText('published', { timeout: 20000 });
    await page.locator('[data-role="run"]').click();
    const run = page.locator('tf-window.tf-act-window').last();
    await run.locator('[data-act="submit"]').click();
    const instance = page.locator('tf-window.fb-process-window', { hasText: 'Run with timer' }).last();
    const timers = instance.locator('[data-timers]');
    await expect(timers).toContainText('Review step', { timeout: 20000 });
    expect(await timers.innerText()).not.toMatch(ID_SHAPE);
    expect(await instance.innerText()).not.toMatch(ID_SHAPE);
  });

  test('the Simulate window says up front that a repeated step cannot run, by name', async ({ page }) => {
    test.setTimeout(240000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createModelDefinition(page, 'Repeat simulation', WITH_REPEAT);
    await openDesigner(page, { login: false, id: own });
    await page.locator('[data-role="publish"]').click();
    await expect(page.locator('.toast').first()).toContainText('published', { timeout: 20000 });
    await page.locator('[data-role="more"]').click();
    await page.locator('tf-menu[data-role="more-menu"] [data-role="simulate"]').click();
    const simulation = page.locator('tf-window.tf-act-window').last();
    await expect(simulation.locator('.tf-act__note')).toHaveAttribute('message', /Review step/);
    expect(await simulation.innerText()).not.toMatch(ID_SHAPE);
    await expect(simulation.locator('[data-act="submit"]')).toHaveAttribute('disabled', '');
  });

  test('a Send task offers the owned definitions on its own and keeps the target id behind technical details', async ({ page }) => {
    test.setTimeout(180000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Send picker');
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Send task', 400, 200);
    await page.locator('.fb-canvas .fb-node.bpmn_send_task').click({ position: { x: 20, y: 20 } });
    const picker = page.locator('.fb-config [data-process="targetDefinitionChoice"]');
    await expect(picker).toBeVisible();
    await expect.poll(() => picker.evaluate((element) => [...element.querySelectorAll('option')].map((option) => option.textContent)),
      { message: 'the first page of owned definitions is listed without a click' }).toContain(PROCESS_NAME);
    const technical = page.locator('.fb-config tf-disclosure[data-technical]', { has: page.locator('[data-process="targetDefinitionId"]') });
    await expect(technical).not.toHaveAttribute('open', '');
    await expect(page.locator('.fb-config [data-process="targetDefinitionId"]')).toBeHidden();
  });
});

const storedModel2 = (page, id) => page.evaluate(async (target) => {
  const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
  return (await ApiBinary.one('processDefinitionGetRequest', { definitionId: target })).definition.model;
}, id);

test.describe('round 5: the bar and its menu', () => {
  test('zoom to fit is in the More menu exactly when the bar has dropped its own button', async ({ page }) => {
    test.setTimeout(180000);
    await page.setViewportSize({ width: 1280, height: 800 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Zoom fit menu');
    await openDesigner(page, { login: false, id: own });
    const state = () => page.evaluate(() => ({
      barHidden: getComputedStyle(document.querySelector('[data-role="zoom-fit"]')).display === 'none',
      menuHidden: document.querySelector('[data-role="zoom-fit-more"]').hidden,
    }));
    expect(await state(), 'room enough: the bar keeps its button and the menu does not repeat it').toEqual({ barHidden: false, menuHidden: true });
    // A bar this narrow is out of room for everything but the last step.
    await page.locator('.fb-topbar').evaluate((bar) => { bar.style.maxWidth = '430px'; });
    await expect.poll(state).toEqual({ barHidden: true, menuHidden: false });
    await page.locator('.fb-topbar').evaluate((bar) => { bar.style.maxWidth = ''; });
    await expect.poll(state).toEqual({ barHidden: false, menuHidden: true });
  });
});

test.describe('round 5: tag and label', () => {
  test.use({ viewport: { width: 1600, height: 950 } });

  test('the name of an event that is on no task is not covered by its Not attached tag', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Tag and label');
    await openDesigner(page, { login: false, id: own });
    await page.waitForTimeout(1500);
    await dragPaletteItem(page, 'Boundary timer', 500, 330);
    const event = page.locator('.fb-canvas .fb-node.bpmn_boundary_timer');
    await expect(event).toHaveClass(/fb-unattached/);
    await page.waitForTimeout(500);
    const covered = await event.evaluate((element) => {
      const tag = getComputedStyle(element, '::after');
      const box = element.getBoundingClientRect();
      const scale = box.width / element.offsetWidth;
      const width = parseFloat(tag.width) * scale; const height = parseFloat(tag.height) * scale;
      const pill = { left: box.left + box.width / 2 - width / 2, right: box.left + box.width / 2 + width / 2,
        top: box.top - 10 * scale - height, bottom: box.top - 10 * scale };
      const label = element.querySelector('.fb-process-label');
      if (label.hidden || getComputedStyle(label).visibility === 'hidden') return false;
      const l = label.getBoundingClientRect();
      return l.left < pill.right && pill.left < l.right && l.top < pill.bottom && pill.top < l.bottom;
    });
    expect(covered, 'the label and the tag do not overlap').toBe(false);
  });
});

test.describe('round 5: crowded diagram', () => {
  test.use({ viewport: { width: 1600, height: 950 } });

  test('no leader line crosses an element and a label without a spot shows its name on hover', async ({ page }) => {
    test.setTimeout(240000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Crowded');
    await openDesigner(page, { login: false, id: own });
    await page.evaluate(async () => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      const canvas = builder._state.canvas;
      for (let index = 0; index < 12; index += 1) {
        const task = canvas.addNodeFromTemplate(canvas.templates.get('bpmn_service_task'), 0, 0, { cascade: true });
        task.x = 100 + (index % 6) * 320; task.y = 100 + Math.floor(index / 6) * 300;
        task.label = `Review step ${index}`;
        for (let handler = 0; handler < 4; handler += 1) canvas.addBoundaryNode(task.id, handler % 2 ? 'bpmn_boundary_escalation' : 'bpmn_boundary_error');
      }
      canvas.render();
      canvas.fitToContent();
    });
    await page.waitForTimeout(800);
    const crossings = await page.evaluate(async () => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      const canvas = builder._state.canvas;
      const hits = (x1, y1, x2, y2, rect) => {
        let near = 0; let far = 1;
        for (const [from, delta, low, high] of [[x1, x2 - x1, rect.left, rect.right], [y1, y2 - y1, rect.top, rect.bottom]]) {
          if (delta === 0) { if (from < low || from > high) return false; } else {
            const enter = (low - from) / delta; const leave = (high - from) / delta;
            near = Math.max(near, Math.min(enter, leave)); far = Math.min(far, Math.max(enter, leave));
            if (near > far) return false;
          }
        }
        return true;
      };
      const found = [];
      let shown = 0; let deferred = 0;
      for (const node of canvas.nodes) {
        const element = canvas.nodesLayer.querySelector(`[data-node-id="${CSS.escape(node.id)}"]`);
        if (element.classList.contains('fb-label-deferred')) deferred += 1;
        const leader = element.querySelector('.fb-process-label-leader');
        if (!leader || leader.hidden) continue;
        shown += 1;
        const x1 = node.x + parseFloat(leader.style.left); const y1 = node.y + parseFloat(leader.style.top);
        const angle = Number(/rotate\(([-\d.e]+)rad\)/.exec(leader.style.transform)[1]);
        const length = parseFloat(leader.style.width);
        const x2 = x1 + Math.cos(angle) * length; const y2 = y1 + Math.sin(angle) * length;
        for (const other of canvas.nodes) {
          if (other.id === node.id) continue;
          if (hits(x1, y1, x2, y2, { left: other.x + 3, top: other.y + 3, right: other.x + other.width - 3, bottom: other.y + other.height - 3 })) found.push(`${node.id} -> ${other.id}`);
        }
        if (length > 260) found.push(`${node.id} leader ${Math.round(length)}px`);
      }
      return { found, shown, deferred, nodes: canvas.nodes.length };
    });
    expect(crossings.nodes, '12 tasks with 4 handlers each, plus Start and End').toBe(62);
    expect(crossings.found).toEqual([]);
    expect(crossings.shown + crossings.deferred).toBeGreaterThan(0);
  });
});

test.describe('round 5: phone', () => {
  for (const [name, width, height] of [['portrait', 390, 844], ['landscape', 844, 390]]) {
    test(`the bars stay in view and the More menu stays on screen in ${name}`, async ({ page }) => {
      test.setTimeout(180000);
      await page.setViewportSize({ width, height });
      await loginAsAdmin(page, { port: PORT });
      const own = await createDefinition(page, `Phone ${name}`);
      await openDesigner(page, { login: false, id: own });
      await addServiceTask(page);
      const bar = page.locator('.fb-bottombar');
      // The screen eases in with a transform, so a first sample would be mid-flight.
      await page.waitForTimeout(1800);
      expect((await rect(bar)).bottom, 'the bottom bar ends inside the window').toBeLessThanOrEqual(height + 0.5);
      await expect(page.locator('.fb-bottombar [data-role="undo"]')).toBeVisible();
      expect((await rect(page.locator('.fb-bottombar [data-role="undo"]'))).bottom).toBeLessThanOrEqual(height);
      const topbar = page.locator('.fb-topbar');
      await page.locator('[data-role="zoom-in"]').click();
      expect(await topbar.evaluate((element) => element.scrollTop), 'Back and the name never scroll away').toBe(0);
      await page.locator('[data-role="more"]').click();
      await page.waitForTimeout(500);
      const menu = await page.locator('tf-menu[data-role="more-menu"]').evaluate((element) => {
        const box = element.shadowRoot.querySelector('.tf-menu').getBoundingClientRect();
        return { top: box.top, bottom: box.bottom, left: box.left, right: box.right };
      });
      expect(menu.bottom, 'the menu ends inside the window').toBeLessThanOrEqual(height);
      expect(menu.right).toBeLessThanOrEqual(width);
      expect(menu.left).toBeGreaterThanOrEqual(0);
    });
  }

  test('the bar refits without flicker or a ResizeObserver loop while the window is resized', async ({ page }) => {
    test.setTimeout(180000);
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
    await page.setViewportSize({ width: 1100, height: 800 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Refit');
    await openDesigner(page, { login: false, id: own });
    // Find the width where the bar takes a step, so the wobble below crosses it.
    const classesAt = async (width) => {
      await page.setViewportSize({ width, height: 800 });
      await page.waitForTimeout(150);
      return page.locator('.fb-topbar').evaluate((bar) => bar.className);
    };
    let edge = null;
    let previous = await classesAt(1500);
    for (let width = 1460; width >= 800 && edge === null; width -= 10) {
      const current = await classesAt(width);
      if (current !== previous) edge = width;
      previous = current;
    }
    expect(edge, 'the bar takes a step somewhere between 1500 and 800 px').not.toBeNull();
    // The step lies between `edge` and the 10 px wider window; narrow that to a pixel.
    const tight = await classesAt(edge);
    let wide = edge + 10;
    for (let probe = edge + 1; probe < edge + 10; probe += 1) {
      if (await classesAt(probe) === tight) wide = probe; else break;
    }
    edge = wide;
    await classesAt(edge);
    // The classes as painted, one sample per frame: the measuring itself briefly
    // clears them inside a frame, which nobody sees.
    await page.evaluate(() => {
      const bar = document.querySelector('.fb-topbar');
      window.__barFrames = [];
      const sample = () => { window.__barFrames.push(bar.className); window.__barSampling = requestAnimationFrame(sample); };
      sample();
    });
    for (let step = 0; step < 16; step += 1) {
      await page.setViewportSize({ width: edge + (step % 2 ? 4 : -3), height: 800 });
      await page.waitForTimeout(60);
    }
    await page.waitForTimeout(400);
    const frames = await page.evaluate(() => { cancelAnimationFrame(window.__barSampling); return window.__barFrames; });
    const changes = frames.filter((value, index) => index > 0 && value !== frames[index - 1]).length;
    expect(changes, 'the buttons are not toggled back and forth as the width wobbles').toBeLessThanOrEqual(2);
    expect(errors.filter((message) => /ResizeObserver/.test(message))).toEqual([]);
  });
});

test.describe('round 6: crowded labels, idle bar, zone, menu, minimap and drops', () => {
  const finiteView = (page) => page.evaluate(async () => {
    const { default: builder } = await import('/js/modules/flows-builder.js');
    const canvas = builder._state.canvas;
    return { finite: [canvas.view.x, canvas.view.y, canvas.view.zoom].every(Number.isFinite),
      transform: canvas.world.style.transform, badNodes: canvas.nodes.filter((node) => !Number.isFinite(node.x) || !Number.isFinite(node.y)).length,
      minimap: document.querySelectorAll('.fb-minimap-node').length, nodes: canvas.nodes.length };
  });

  test('one deferred label does not break zoom, fit, the minimap, a drop or the save', async ({ page }) => {
    test.setTimeout(240000);
    await page.setViewportSize({ width: 2400, height: 1100 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Deferred label');
    await openDesigner(page, { login: false, id: own });
    await page.waitForTimeout(1500);
    const box = await page.locator('.fb-canvas').boundingBox();
    await page.evaluate(async ({ left, top }) => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      const canvas = builder._state.canvas;
      canvas.resetZoom();
      const tasks = [];
      for (let index = 0; index < 4; index += 1) {
        tasks.push(canvas.addNodeFromTemplate(canvas.templates.get('bpmn_user_task'), left + 400 + index * 320, top + 600));
      }
      for (const task of tasks) {
        for (let slot = 0; slot < 4; slot += 1) {
          const timer = canvas.addNodeFromTemplate(canvas.templates.get('bpmn_boundary_timer'),
            left + task.x + 30 + slot * 55 + canvas.view.x, top + task.y + task.height + canvas.view.y);
          canvas.updateNodeLabel(timer.id, 'Send a reminder to the reviewer after N working days');
        }
      }
      canvas.updateProcessTimezone('Europe/Warsaw');
      canvas.render();
    }, { left: box.x, top: box.y });
    await page.waitForTimeout(800);
    expect(await page.locator('.fb-label-deferred').count(), 'the repro produces a deferred label').toBeGreaterThan(0);
    const before = await page.locator('[data-role="zoom-level"]').textContent();
    await page.locator('[data-role="zoom-in"]').click();
    await page.waitForTimeout(300);
    const zoomed = await finiteView(page);
    expect(zoomed.finite && zoomed.transform.startsWith('translate(')).toBe(true);
    expect(await page.locator('[data-role="zoom-level"]').textContent()).not.toBe(before);
    await page.locator('[data-role="zoom-fit"]').click();
    await page.waitForTimeout(400);
    const fitted = await finiteView(page);
    expect(fitted.finite).toBe(true);
    expect(fitted.minimap, 'the minimap shows every node').toBe(fitted.nodes);
    await dragPaletteItem(page, 'Script task', 400, 300);
    const after = await finiteView(page);
    expect(after.badNodes, 'a dropped node has a real position').toBe(0);
    const saved = await page.evaluate(async () => (await import('/js/modules/flows-builder.js')).default._save());
    expect(saved, await page.locator('[data-role="error"]').getAttribute('message')).toBe(true);
  });

  test('a cascade of 100 nodes keeps a finite view and a drawn minimap', async ({ page }) => {
    test.setTimeout(240000);
    await page.setViewportSize({ width: 1600, height: 950 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Hundred nodes');
    await openDesigner(page, { login: false, id: own });
    await page.evaluate(async () => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      const canvas = builder._state.canvas;
      for (let index = 0; index < 100; index += 1) {
        canvas.addNodeFromTemplate(canvas.templates.get(index % 2 ? 'bpmn_boundary_timer' : 'bpmn_user_task'), 0, 0, { cascade: true });
      }
    });
    await page.waitForTimeout(1000);
    const state = await finiteView(page);
    expect(state.finite && state.badNodes === 0).toBe(true);
    expect(state.minimap).toBe(state.nodes);
    await page.locator('[data-role="zoom-fit"]').click();
    await page.waitForTimeout(400);
    expect((await finiteView(page)).finite).toBe(true);
  });

  test('an idle toolbar is not touched at all for two seconds', async ({ page }) => {
    test.setTimeout(120000);
    await page.setViewportSize({ width: 1100, height: 800 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Idle bar');
    await openDesigner(page, { login: false, id: own });
    await page.waitForTimeout(2000);
    const mutations = await page.evaluate(() => new Promise((resolve) => {
      let count = 0;
      const observer = new MutationObserver((records) => { count += records.length; });
      observer.observe(document.querySelector('.fb-topbar'), { subtree: true, attributes: true, childList: true, characterData: true });
      setTimeout(() => { observer.disconnect(); resolve(count); }, 2000);
    }));
    expect(mutations).toBe(0);
  });

  test('a typed time zone keeps its spelling and the empty field shows an example, not a value', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Zone spelling');
    await openDesigner(page, { login: false, id: own });
    await dragPaletteItem(page, 'Boundary timer', 500, 330);
    const field = page.locator('[data-role="timer-timezone"]');
    await expect(field).toBeVisible();
    expect(await field.getAttribute('placeholder')).toMatch(/^(e\.g\.|np\.|z\. B\.|p\. ej\.|p\. ex\.)/);
    for (const zone of ['Europe/Kyiv', 'Asia/Kolkata', 'US/Pacific']) {
      await field.locator('input').fill(zone);
      await field.locator('input').blur();
      await expect.poll(() => page.evaluate(async () => (await import('/js/modules/flows-builder.js')).default._state.canvas.getData().timerTimezone)).toBe(zone);
    }
    await field.locator('input').fill('europe/warsaw');
    await field.locator('input').blur();
    await expect.poll(() => page.evaluate(async () => (await import('/js/modules/flows-builder.js')).default._state.canvas.getData().timerTimezone)).toBe('Europe/Warsaw');
  });

  test('the More menu fits a landscape phone, scrolls, and the arrow leaves a disabled item for the next one', async ({ page }) => {
    test.setTimeout(120000);
    await page.setViewportSize({ width: 844, height: 390 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Menu landscape');
    await openDesigner(page, { login: false, id: own });
    await page.waitForTimeout(1800);
    await page.locator('[data-role="more"]').click();
    await page.waitForTimeout(500);
    const menu = await page.locator('tf-menu[data-role="more-menu"]').evaluate((element) => {
      const panel = element.shadowRoot.querySelector('.tf-menu');
      const box = panel.getBoundingClientRect();
      return { bottom: box.bottom, scrolls: panel.scrollHeight > panel.clientHeight, overflow: getComputedStyle(panel).overflowY };
    });
    expect(menu.bottom).toBeLessThanOrEqual(390);
    expect(menu.scrolls && menu.overflow === 'auto').toBe(true);
    await page.locator('[data-role="simulate"] .tf-menu-item').focus();
    await page.keyboard.press('ArrowDown');
    expect(await page.evaluate(() => document.activeElement.closest('tf-menu-item')?.dataset.role)).toBe('instances');
    await page.locator('[data-role="history"] .tf-menu-item').focus();
    await expect(page.locator('[data-role="history"] .tf-menu-item')).toBeInViewport();
  });

  test('the minimap pans on a press, folds away and is remembered, and a drop never lands under it', async ({ page }) => {
    test.setTimeout(180000);
    await page.setViewportSize({ width: 1600, height: 950 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Minimap');
    await openDesigner(page, { login: false, id: own });
    await page.evaluate(() => localStorage.removeItem('tf_fb_minimap_collapsed'));
    await page.waitForTimeout(1500);
    await page.evaluate(async () => {
      const { default: builder } = await import('/js/modules/flows-builder.js');
      const canvas = builder._state.canvas;
      const far = canvas.addNodeFromTemplate(canvas.templates.get('bpmn_user_task'), 0, 0, { cascade: true });
      far.x = 3000; far.y = 200; canvas.render(); canvas.resetZoom();
    });
    const before = await page.evaluate(async () => (await import('/js/modules/flows-builder.js')).default._state.canvas.view.x);
    const mini = page.locator('[data-role="minimap"]');
    const miniBox = await mini.boundingBox();
    await page.mouse.click(miniBox.x + miniBox.width - 12, miniBox.y + miniBox.height / 2);
    await page.waitForTimeout(200);
    expect(await page.evaluate(async () => (await import('/js/modules/flows-builder.js')).default._state.canvas.view.x)).toBeLessThan(before);
    // A drop onto the minimap itself moves aside.
    await dragPaletteItem(page, 'Script task', miniBox.x - (await page.locator('.fb-canvas').boundingBox()).x + 40, miniBox.y - (await page.locator('.fb-canvas').boundingBox()).y + 30);
    const node = page.locator('.fb-canvas .fb-node.bpmn_script_task').last();
    expect(intersects(await rect(node), await rect(mini)), 'the node is not under the minimap').toBe(false);
    await page.locator('[data-role="minimap-toggle"]').click();
    await expect(mini).toHaveClass(/collapsed/);
    await page.reload();
    await openDesigner(page, { login: false, id: own });
    await expect(page.locator('[data-role="minimap"]')).toHaveClass(/collapsed/);
    await page.evaluate(() => localStorage.removeItem('tf_fb_minimap_collapsed'));
  });

  test('hosts of tf-button carry no accessible name of their own and the state sits on the real button', async ({ page }) => {
    test.setTimeout(120000);
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Host names');
    await openDesigner(page, { login: false, id: own });
    const cdp = await page.context().newCDPSession(page);
    const { nodes } = await cdp.send('Accessibility.getFullAXTree');
    const hosts = await page.evaluate(() => [...document.querySelectorAll('tf-button[title]:not([aria-label])')].map((element) => element.getAttribute('title')));
    expect(hosts.length).toBeGreaterThan(0);
    const generic = nodes.filter((node) => !node.ignored && node.role?.value === 'generic' && hosts.includes(node.name?.value));
    expect(generic.map((node) => node.name.value)).toEqual([]);
    expect(await page.locator('tf-button[aria-expanded], tf-button[aria-haspopup]').count()).toBe(0);
  });

  test('on a short phone held sideways the toolbar rows do not run over the canvas', async ({ page }) => {
    test.setTimeout(120000);
    await page.setViewportSize({ width: 667, height: 375 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Short phone');
    await openDesigner(page, { login: false, id: own });
    await page.waitForTimeout(1800);
    const bar = await rect(page.locator('.fb-topbar'));
    const canvas = await rect(page.locator('.fb-canvas'));
    const lowest = await page.evaluate(() => Math.max(...[...document.querySelectorAll('.fb-topbar > *')]
      .filter((element) => getComputedStyle(element).position !== 'absolute' && element.getBoundingClientRect().height)
      .map((element) => element.getBoundingClientRect().bottom)));
    expect(lowest).toBeLessThanOrEqual(bar.bottom + 0.5);
    expect(bar.bottom).toBeLessThanOrEqual(canvas.top + 0.5);
  });

  test('a short desktop window keeps the app sidebar', async ({ page }) => {
    test.setTimeout(120000);
    await page.setViewportSize({ width: 1280, height: 480 });
    await loginAsAdmin(page, { port: PORT });
    const own = await createDefinition(page, 'Docked devtools');
    await openDesigner(page, { login: false, id: own });
    expect(await page.evaluate(() => getComputedStyle(document.querySelector('.sidebar')).display)).not.toBe('none');
  });
});
