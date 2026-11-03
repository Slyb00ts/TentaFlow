// =============================================================================
// File: tests/e2e/process-b3-camunda-import.spec.js
// Description: The Camunda 7 import in the BPMN designer against a real node.
//              A document with a service task named by a delegate expression,
//              an exclusive gateway with a ${...} condition and a default flow,
//              and a user task with candidate groups goes through the real
//              import window: the result lists the diagnostics by severity and
//              the JUEL -> CEL translations, nothing is loaded until the author
//              imports, the Unbound step and the flagged elements are marked
//              with a banner that jumps through them, the draft saves and
//              reloads with the placeholder, publication is refused naming the
//              step, and binding an implementation clears its marker.
// =============================================================================

const path = require('path');
const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists, renderConfig } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18481;
const RUNTIME = path.join(__dirname, '../../.runtime/e2e-process-b3-camunda-import');
const PROCESS_NAME = 'Camunda import process';
const CAMUNDA_XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:camunda="http://camunda.org/schema/1.0/bpmn" id="Definitions_1" targetNamespace="http://bpmn.io/schema/bpmn">
  <bpmn:process id="Process_Order" name="Order" isExecutable="true">
    <bpmn:startEvent id="Start_1"/>
    <bpmn:serviceTask id="Task_Charge" name="Charge card" camunda:delegateExpression="\${chargeBean}"/>
    <bpmn:exclusiveGateway id="Gate_1" name="Big order?" default="Flow_small"/>
    <bpmn:userTask id="Task_Review" name="Review order" camunda:candidateGroups="finance"/>
    <bpmn:endEvent id="End_1"/>
    <bpmn:sequenceFlow id="Flow_a" sourceRef="Start_1" targetRef="Task_Charge"/>
    <bpmn:sequenceFlow id="Flow_b" sourceRef="Task_Charge" targetRef="Gate_1"/>
    <bpmn:sequenceFlow id="Flow_big" name="big" sourceRef="Gate_1" targetRef="Task_Review">
      <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">\${amount > 100}</bpmn:conditionExpression>
    </bpmn:sequenceFlow>
    <bpmn:sequenceFlow id="Flow_small" sourceRef="Gate_1" targetRef="End_1"/>
    <bpmn:sequenceFlow id="Flow_c" sourceRef="Task_Review" targetRef="End_1"/>
  </bpmn:process>
</bpmn:definitions>`;

const UNSUPPORTED_XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:camunda="http://camunda.org/schema/1.0/bpmn" id="Definitions_1" targetNamespace="http://bpmn.io/schema/bpmn">
  <bpmn:process id="Process_Bad" isExecutable="true">
    <bpmn:startEvent id="Start_1"/>
    <bpmn:scriptTask id="Task_Script" scriptFormat="groovy"><bpmn:script>println 1</bpmn:script></bpmn:scriptTask>
    <bpmn:endEvent id="End_1"/>
    <bpmn:sequenceFlow id="Flow_a" sourceRef="Start_1" targetRef="Task_Script"/>
    <bpmn:sequenceFlow id="Flow_b" sourceRef="Task_Script" targetRef="End_1"/>
  </bpmn:process>
</bpmn:definitions>`;

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
  definitionId = await createDefinition(page, PROCESS_NAME);
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((resolve) => setTimeout(resolve, 1500));
});

function createDefinition(page, name) {
  return page.evaluate(async (processName) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    const { emptyProcessModel } = await import('/js/modules/flows-builder/bpmn.js');
    const saved = await ApiBinary.one('processDefinitionSaveRequest', {
      commandId: crypto.randomUUID(), definitionId: null, expectedRevision: 0,
      name: processName, description: '', model: emptyProcessModel(),
    });
    return saved.definition.definitionId;
  }, name);
}

async function openDesigner(page, { login = true } = {}) {
  if (login) await loginAsAdmin(page, { port: PORT });
  await page.evaluate(async (target) => {
    const { openFlowBuilder } = await import('/js/modules/flows-builder.js');
    openFlowBuilder(target, { mode: 'bpmn' });
  }, definitionId);
  await expect(page.locator('.fb-canvas')).toBeVisible({ timeout: 15000 });
  await expect(page.locator('.fb-palette-category[data-cat="tentaflow"]')).toBeAttached({ timeout: 15000 });
}

async function openMore(page) {
  await page.locator('[data-role="more"]').click();
  await expect(page.locator('tf-menu[data-role="more-menu"] .tf-menu.open')).toHaveCount(1);
}

// Opens the import window, puts the document into its editor and submits it.
async function submitImport(page, xml) {
  await openMore(page);
  await page.locator('[data-role="import"]').click();
  const form = page.locator('.tf-act-window');
  await expect(form.locator('[data-act="submit"]')).toBeVisible();
  await form.locator('tf-code-editor').evaluate((editor, value) => {
    editor.value = value;
    editor.dispatchEvent(new Event('input', { bubbles: true }));
  }, xml);
  await expect(form.locator('[data-act="submit"]')).not.toHaveAttribute('disabled', '');
  await form.locator('[data-act="submit"]').click();
}

const tableRows = (locator) => locator.evaluate((table) => table.rows);
const storedModel = (page) => page.evaluate(async (id) => {
  const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
  return (await ApiBinary.one('processDefinitionGetRequest', { definitionId: id })).definition.model;
}, definitionId);
const marked = (page) => page.locator('.fb-canvas .fb-node.fb-needs-attention');

test.use({ viewport: { width: 1800, height: 1000 } });

test('a document the importer refuses is explained and loads nothing', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await openDesigner(page);
  await submitImport(page, UNSUPPORTED_XML);
  const review = page.locator('tf-window .fb-import-review');
  await expect(review).toBeVisible({ timeout: 15000 });
  await expect(review.locator('[data-consent]')).toContainText('Nothing was imported');
  const fatal = await tableRows(review.locator('section[data-severity="fatal"] tf-table'));
  expect(fatal.length).toBeGreaterThan(0);
  expect(fatal.some((row) => row.code === 'CAMUNDA_SCRIPT_UNSUPPORTED')).toBe(true);
  await expect(page.locator('[data-act="import"]')).toHaveCount(0);
  await page.locator('[data-act="close"]').click();
  await expect(page.locator('tf-window .fb-import-review')).toHaveCount(0);
  await expect(page.locator('.fb-canvas .fb-node')).toHaveCount(2);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a Camunda document is read first, imported as a draft with its placeholders marked, saved, refused on publication and bound', async ({ page }) => {
  test.setTimeout(240000);
  const errors = collectConsoleErrors(page);
  await openDesigner(page);
  await submitImport(page, CAMUNDA_XML);

  // The result is read before anything is loaded.
  const review = page.locator('tf-window .fb-import-review');
  await expect(review).toBeVisible({ timeout: 15000 });
  await expect(page.locator('.fb-canvas .fb-node')).toHaveCount(2);
  await expect(review.locator('[data-summary]')).toHaveAttribute('message', /Import check: .*warning/);
  await expect(review.locator('[data-consent]')).toContainText('accepts the translated expressions below as yours');
  const warnings = await tableRows(review.locator('section[data-severity="warning"] tf-table'));
  const codes = warnings.map((row) => row.code);
  expect(codes).toContain('CAMUNDA_SERVICE_UNBOUND');
  expect(codes).toContain('CAMUNDA_CANDIDATES_SKIPPED');
  const unbound = warnings.find((row) => row.code === 'CAMUNDA_SERVICE_UNBOUND');
  expect(unbound.element).toBe('Charge card (Task_Charge)');
  expect(unbound.message).toContain('no TentaFlow counterpart');
  const translations = await tableRows(review.locator('section[data-translations] tf-table'));
  const condition = translations.find((row) => row.kind === 'condition');
  expect(condition.original).toBe('${amount > 100}');
  expect(condition.translated).toContain('vars.amount > 100');
  expect(condition.kindLabel).toBe('Condition');

  // Importing is the consent; only then is the draft in the editor.
  await review.locator('xpath=ancestor::tf-window').locator('[data-act="import"]').click();
  await expect(review.locator('[data-consent]')).toContainText('Loaded as a draft');
  await expect(page.locator('.fb-canvas .fb-node.bpmn_service_task')).toHaveCount(1);
  await expect(page.locator('.fb-canvas .fb-node.bpmn_service_task[data-node-id="Task_Charge"]')).toHaveClass(/fb-needs-attention/);
  await expect(page.locator('.fb-canvas .fb-node.bpmn_service_task[data-node-id="Task_Charge"] .fb-process-kind')).toHaveAttribute('data-kind', 'unbound');
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"] .fb-process-warn')).toHaveAttribute('aria-label', /Not bound yet.*delegateExpression:\$\{chargeBean\}/);

  // A line of the result takes the author to its element.
  await review.locator('section[data-severity="warning"] tbody tr').first().click();
  await expect(page.locator('tf-window .fb-import-review')).toHaveCount(0, { timeout: 5000 });
  await expect(page.locator('.fb-canvas .fb-node.selected')).toHaveCount(1);

  // The banner lists what needs the author and jumps through it.
  const banner = page.locator('[data-role="attention"]');
  await expect(banner).toBeVisible();
  const total = await marked(page).count();
  expect(total).toBeGreaterThanOrEqual(3);
  await expect(banner).toHaveAttribute('message', new RegExp(`^${total} elements need your attention`));
  const selectedId = () => page.evaluate(() => document.querySelector('.fb-canvas .fb-node.selected')?.dataset.nodeId ?? null);
  await page.keyboard.press('Escape');
  await expect(page.locator('.fb-canvas .fb-node.selected')).toHaveCount(0);
  const visited = [];
  for (let step = 0; step < total; step += 1) {
    await page.locator('[data-role="next-attention"]').click();
    await expect.poll(selectedId).not.toBeNull();
    visited.push(await selectedId());
  }
  expect(new Set(visited).size).toBe(total);
  const documentOrder = await marked(page).evaluateAll((elements) => elements.map((element) => element.dataset.nodeId));
  expect(visited).toEqual(documentOrder);
  expect(documentOrder[0]).toBe('Task_Charge');

  // The draft saves with the placeholder and reloads with it.
  await page.locator('[data-role="save"]').click();
  await expect.poll(async () => (await storedModel(page)).nodes.some((row) => row.kind.ServiceTask?.implementation?.Unbound), { timeout: 15000 }).toBe(true);
  const stored = await storedModel(page);
  expect(stored.nodes.find((row) => row.id === 'Task_Charge').kind.ServiceTask.implementation.Unbound.origin).toBe('delegateExpression:${chargeBean}');
  await page.reload();
  await expect(page.locator('#sidebar, .sidebar, nav').first()).toBeVisible({ timeout: 15000 });
  await openDesigner(page, { login: false });
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).toHaveClass(/fb-needs-attention/);
  await expect(page.locator('[data-role="attention"]')).toBeVisible();
  // Only the placeholder survives a reload: what the import warned about is not stored.
  const unboundOnly = await marked(page).count();
  expect(unboundOnly).toBe(1);

  // Publication is refused by name, before the server is asked.
  await page.locator('[data-role="publish"]').click();
  await expect(page.locator('[data-role="error"]')).toContainText('“Charge card” is not bound to an implementation yet');

  // Binding an implementation clears the marker; the banner counts one less.
  await page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]').click();
  await expect(page.locator('[data-unbound-notice]')).toHaveAttribute('message', /Not bound yet — imported from Camunda: delegateExpression:\$\{chargeBean\}/);
  const options = await page.locator('tf-select[data-process="implementation"] select option:not([disabled])').evaluateAll((list) => list.map((option) => option.value));
  expect(options).toEqual(['flow', 'tool', 'block', 'agent'].filter((value) => options.includes(value)));
  expect(options).not.toContain('unbound');
  await page.locator('tf-select[data-process="implementation"] select').selectOption('flow');
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).not.toHaveClass(/fb-needs-attention/);
  await expect(page.locator('[data-unbound-notice]')).toHaveCount(0);
  await expect(marked(page)).toHaveCount(unboundOnly - 1);
  await expect(page.locator('[data-role="attention"]')).toBeHidden();
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
