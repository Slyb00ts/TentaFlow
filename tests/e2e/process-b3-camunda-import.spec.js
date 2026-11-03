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
    <bpmn:subProcess id="Sub_1" name="Notify">
      <bpmn:startEvent id="SubStart_1"/>
      <bpmn:serviceTask id="Task_Inner" name="Send mail" camunda:delegateExpression="\${mailer}"/>
      <bpmn:endEvent id="SubEnd_1"/>
      <bpmn:sequenceFlow id="SubFlow_a" sourceRef="SubStart_1" targetRef="Task_Inner"/>
      <bpmn:sequenceFlow id="SubFlow_b" sourceRef="Task_Inner" targetRef="SubEnd_1"/>
    </bpmn:subProcess>
    <bpmn:endEvent id="End_1"/>
    <bpmn:sequenceFlow id="Flow_a" sourceRef="Start_1" targetRef="Task_Charge"/>
    <bpmn:sequenceFlow id="Flow_b" sourceRef="Task_Charge" targetRef="Gate_1"/>
    <bpmn:sequenceFlow id="Flow_big" name="big" sourceRef="Gate_1" targetRef="Task_Review">
      <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">\${amount > 100}</bpmn:conditionExpression>
    </bpmn:sequenceFlow>
    <bpmn:sequenceFlow id="Flow_small" sourceRef="Gate_1" targetRef="Sub_1"/>
    <bpmn:sequenceFlow id="Flow_sub" sourceRef="Sub_1" targetRef="End_1"/>
    <bpmn:sequenceFlow id="Flow_c" sourceRef="Task_Review" targetRef="End_1"/>
  </bpmn:process>
</bpmn:definitions>`;

const SECOND_XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:camunda="http://camunda.org/schema/1.0/bpmn" id="Definitions_2" targetNamespace="http://bpmn.io/schema/bpmn">
  <bpmn:process id="Process_Second" name="Second" isExecutable="true">
    <bpmn:startEvent id="Start_1"/>
    <bpmn:serviceTask id="Task_Second" name="Second step" camunda:delegateExpression="\${secondBean}"/>
    <bpmn:endEvent id="End_1"/>
    <bpmn:sequenceFlow id="Flow_a" sourceRef="Start_1" targetRef="Task_Second"/>
    <bpmn:sequenceFlow id="Flow_b" sourceRef="Task_Second" targetRef="End_1"/>
  </bpmn:process>
</bpmn:definitions>`;

const UNSUPPORTED_XML = `<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:camunda="http://camunda.org/schema/1.0/bpmn" id="Definitions_1" targetNamespace="http://bpmn.io/schema/bpmn">
  <bpmn:process id="Process_Bad" isExecutable="true">
    <bpmn:startEvent id="Start_1"/>
    <bpmn:scriptTask id="Task_Script" name="Compute discount" scriptFormat="groovy"><bpmn:script>println 1</bpmn:script></bpmn:scriptTask>
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
  // The model is withheld, yet the element still has its name.
  expect(fatal.find((row) => row.code === 'CAMUNDA_SCRIPT_UNSUPPORTED').element).toBe('Compute discount (Task_Script)');
  expect(fatal[0].message).not.toMatch(/^CAMUNDA_/);
  await expect(page.locator('[data-act="import"]')).toHaveCount(0);
  // The result window took focus when it opened.
  expect(await page.evaluate(() => document.activeElement?.closest?.('.fb-import-review') != null)).toBe(true);

  // Back to the XML: the pasted document is still there, and Escape inside the editor does not close the form.
  await page.locator('[data-act="back"]').click();
  const form = page.locator('.tf-act-window');
  await expect(form.locator('tf-code-editor')).toBeVisible();
  expect(await form.locator('tf-code-editor').evaluate((editor) => editor.value)).toBe(UNSUPPORTED_XML);
  await form.locator('tf-code-editor').click();
  await page.keyboard.press('Escape');
  await expect(form.locator('[data-act="submit"]')).toBeVisible();
  // Escape from outside the editor with text in it asks before discarding.
  await form.locator('.tf-act__subject').click();
  await page.keyboard.press('Escape');
  await expect(page.locator('tf-button[data-action="confirm"]')).toBeVisible();
  await page.locator('tf-button[data-action="cancel"]').click();
  await expect(form.locator('tf-code-editor')).toBeVisible();
  await form.locator('[data-act="cancel"]').click();
  await expect(form).toHaveCount(0);
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

  // Escape on a result nobody has acted on asks before dropping it.
  await page.keyboard.press('Escape');
  await expect(page.locator('tf-button[data-action="confirm"]')).toBeVisible();
  await page.locator('tf-button[data-action="cancel"]').click();
  await expect(page.locator('tf-button[data-action="confirm"]')).toHaveCount(0);
  await expect(review).toBeVisible();

  // Importing is the consent; only then is the draft in the editor.
  await review.locator('xpath=ancestor::tf-window').locator('[data-act="import"]').click();
  await expect(review.locator('[data-consent]')).toContainText('Loaded as a draft');
  await expect(page.locator('.fb-canvas .fb-node.bpmn_service_task')).toHaveCount(1);
  await expect(page.locator('.fb-canvas .fb-node.bpmn_service_task[data-node-id="Task_Charge"]')).toHaveClass(/fb-needs-attention/);
  await expect(page.locator('.fb-canvas .fb-node.bpmn_service_task[data-node-id="Task_Charge"] .fb-process-kind')).toHaveAttribute('data-kind', 'unbound');
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"] .fb-process-warn')).toHaveAttribute('aria-label', /Not bound yet.*delegateExpression:\$\{chargeBean\}/);

  // A language switch remounts the screen: the draft, the marks and the result window come back in the new language.
  const setLanguage = (code) => page.evaluate(async (language) => { const { I18n } = await import('/js/i18n.js'); await I18n.setLanguage(language); }, code);
  await setLanguage('pl');
  // The old window fades for a moment; the new one is the one in the new language.
  await expect(page.locator('tf-window .fb-import-review [data-consent][message*="Wczytano jako szkic"]')).toHaveCount(1, { timeout: 15000 });
  await expect(page.locator('tf-window:not(.tf-window-closing) .fb-import-review')).toHaveCount(1, { timeout: 15000 });
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).toHaveClass(/fb-needs-attention/);
  await expect(page.locator('[data-role="attention"]')).toBeVisible();
  const polish = await tableRows(page.locator('tf-window .fb-import-review section[data-severity="warning"] tf-table'));
  expect(polish.some((row) => /element zast\u0119pczy/.test(row.message))).toBe(true);
  await setLanguage('en');
  await expect(page.locator('tf-window .fb-import-review [data-consent][message*="Loaded as a draft"]')).toHaveCount(1, { timeout: 15000 });
  await expect(page.locator('tf-window .fb-import-review')).toHaveCount(1, { timeout: 15000 });

  // A line of the result takes the author to its element.
  await page.locator('tf-window .fb-import-review section[data-severity="warning"] tbody tr').first().click();
  await expect(page.locator('tf-window .fb-import-review')).toHaveCount(0, { timeout: 5000 });
  await expect(page.locator('.fb-canvas .fb-node.selected')).toHaveCount(1);

  // The banner lists what needs the author and jumps through it.
  const banner = page.locator('[data-role="attention"]');
  await expect(banner).toBeVisible();
  const count = async () => Number(/^(\d+) element/.exec(await banner.getAttribute('message'))[1]);
  const total = await count();
  expect(total).toBeGreaterThanOrEqual(4);
  await expect(banner).toHaveAttribute('live', '');
  const selectedId = () => page.evaluate(() => document.querySelector('.fb-canvas .fb-node.selected')?.dataset.nodeId ?? null);
  await page.keyboard.press('Escape');
  await expect(page.locator('.fb-canvas .fb-node.selected')).toHaveCount(0);
  const visited = [];
  for (let step = 0; step < total; step += 1) {
    await page.locator('[data-role="next-attention"]').click();
    await expect.poll(selectedId).not.toBeNull();
    visited.push(await selectedId());
    await expect(page.locator('[data-role="attention-status"]')).toContainText(`${step + 1} of ${total}`);
    // The jump brings the element into view, whatever body it is in.
    const inView = await page.evaluate(() => {
      const box = document.querySelector('.fb-canvas .fb-node.selected').getBoundingClientRect();
      const canvas = document.querySelector('.fb-canvas').getBoundingClientRect();
      return box.left >= canvas.left - 1 && box.right <= canvas.right + 1 && box.top >= canvas.top - 1 && box.bottom <= canvas.bottom + 1;
    });
    expect(inView).toBe(true);
  }
  expect(new Set(visited).size).toBe(total);
  expect(visited[0]).toBe('Task_Charge');
  // Document order puts the step inside the sub-process after the main-body review task and last overall.
  expect(visited.indexOf('Task_Inner')).toBeGreaterThan(visited.indexOf('Task_Review'));
  expect(visited[total - 1]).toBe('Task_Inner');
  // From inside the sub-process the jump wraps back out to the main body.
  await page.locator('[data-role="next-attention"]').click();
  await expect.poll(selectedId).toBe('Task_Charge');
  await expect(page.locator('[data-role="next-attention"]')).toBeEnabled();

  // The draft saves with the placeholder and reloads with it.
  await page.locator('[data-role="save"]').click();
  await expect.poll(async () => (await storedModel(page)).nodes.some((row) => row.kind.ServiceTask?.implementation?.Unbound), { timeout: 15000 }).toBe(true);
  const stored = await storedModel(page);
  const inner = stored.nodes.find((row) => row.kind.SubProcess).kind.SubProcess.body.nodes.find((row) => row.id === 'Task_Inner');
  expect(inner.kind.ServiceTask.implementation.Unbound.origin).toBe('delegateExpression:${mailer}');
  expect(stored.nodes.find((row) => row.id === 'Task_Charge').kind.ServiceTask.implementation.Unbound.origin).toBe('delegateExpression:${chargeBean}');
  await page.reload();
  await expect(page.locator('#sidebar, .sidebar, nav').first()).toBeVisible({ timeout: 15000 });
  await openDesigner(page, { login: false });
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).toHaveClass(/fb-needs-attention/);
  await expect(page.locator('[data-role="attention"]')).toBeVisible();
  // Only the placeholder survives a reload: what the import warned about is not stored.
  await expect(page.locator('[data-role="attention"]')).toHaveAttribute('message', /^2 elements need/);

  // Publication is refused by name, before the server is asked.
  await page.locator('[data-role="publish"]').click();
  const refusal = page.locator('[data-role="error"]');
  await expect(refusal).toContainText('Not published. These steps have no implementation yet');
  await expect(refusal).toContainText('“Charge card”');
  await expect(refusal).toContainText('“Send mail”');
  expect((await refusal.getAttribute('message')).split('Not published').length).toBe(2);

  // Binding an implementation clears the marker; the banner counts one less.
  await page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]').click();
  await expect(page.locator('[data-unbound-notice]')).toHaveAttribute('message', /Not bound yet — imported from Camunda: delegateExpression:\$\{chargeBean\}/);
  const options = await page.locator('tf-select[data-process="implementation"] select option:not([disabled])').evaluateAll((list) => list.map((option) => option.value));
  expect(options).toEqual(['flow', 'tool', 'block', 'agent'].filter((value) => options.includes(value)));
  expect(options).not.toContain('unbound');
  // Choosing only a category keeps the step unbound, with its origin and the named refusal.
  await page.locator('tf-select[data-process="implementation"] select').selectOption('tool');
  await expect(page.locator('[data-unbound-notice]')).toHaveAttribute('message', /delegateExpression:\$\{chargeBean\}/);
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).toHaveClass(/fb-needs-attention/);
  await page.locator('[data-process="addonId"] input').fill('notes-1');
  await page.locator('[data-process="addonId"] input').blur();
  await expect(page.locator('[data-unbound-notice]')).toHaveCount(1);
  await page.locator('[data-role="publish"]').click();
  await expect(refusal).toContainText('“Charge card”');
  await page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]').click();
  await page.locator('[data-process="toolName"] input').fill('search');
  await page.locator('[data-process="toolName"] input').blur();
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).not.toHaveClass(/fb-needs-attention/);
  await expect(page.locator('[data-unbound-notice]')).toHaveCount(0);
  await expect(page.locator('[data-role="attention"]')).toHaveAttribute('message', /^1 element needs/);
  // The sub-process that still holds an unbound step carries a marker of its own on the main canvas.
  await expect(marked(page)).toHaveCount(1);
  await expect(marked(page)).toHaveAttribute('data-node-id', 'Sub_1');
  await expect(marked(page)).toHaveAttribute('data-attention', 'inside');
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('importing over unsaved work asks first: cancelling keeps the result and the draft, confirming replaces the draft', async ({ page }) => {
  test.setTimeout(180000);
  const errors = collectConsoleErrors(page);
  await openDesigner(page);
  await submitImport(page, CAMUNDA_XML);
  const review = page.locator('tf-window .fb-import-review');
  await expect(review).toBeVisible({ timeout: 15000 });
  // The stored draft holds the previous test's drawing: it is not dirty, yet replacing it asks.
  await page.locator('[data-act="import"]').click();
  await expect(page.locator('tf-button[data-action="confirm"]')).toBeVisible();
  await page.locator('tf-button[data-action="confirm"]').click();
  await expect(review.locator('[data-consent]')).toContainText('Loaded as a draft');
  await expect(review.locator('[data-consent]')).toContainText('autosave keeps it as the draft');
  await page.locator('[data-act="close"]').click();
  await expect(review).toHaveCount(0);
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).toBeVisible();

  // A second import over this draft asks again.
  await submitImport(page, SECOND_XML);
  await expect(review).toBeVisible({ timeout: 15000 });
  await page.locator('[data-act="import"]').click();
  await expect(page.locator('tf-button[data-action="confirm"]')).toBeVisible();
  await page.locator('tf-button[data-action="cancel"]').click();
  await expect(page.locator('tf-button[data-action="confirm"]')).toHaveCount(0);
  await expect(review).toBeVisible();
  await expect(page.locator('[data-act="import"]')).not.toHaveAttribute('disabled', '');
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).toBeVisible();
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Second"]')).toHaveCount(0);

  await page.locator('[data-act="import"]').click();
  await page.locator('tf-button[data-action="confirm"]').click();
  await expect(review.locator('[data-consent]')).toContainText('Loaded as a draft');
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Second"]')).toBeVisible();
  await expect(page.locator('.fb-canvas .fb-node[data-node-id="Task_Charge"]')).toHaveCount(0);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});

test('a clean TentaFlow export imports directly, with one toast and no result window', async ({ page }) => {
  test.setTimeout(120000);
  const errors = collectConsoleErrors(page);
  await openDesigner(page);
  const exported = await page.evaluate(async (id) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    return (await ApiBinary.one('processXmlExportRequest', { definitionId: id })).xml;
  }, definitionId);
  await page.evaluate(() => document.querySelectorAll('tf-toast').forEach((toast) => toast.remove()));
  await submitImport(page, exported);
  // The stored draft is drawn, so the replacement is asked about even though nothing is diagnosed.
  await expect(page.locator('tf-button[data-action="confirm"]')).toBeVisible({ timeout: 15000 });
  await page.locator('tf-button[data-action="confirm"]').click();
  await expect(page.locator('tf-toast[message="Imported into draft; save to keep it"]')).toHaveCount(1, { timeout: 15000 });
  await page.waitForTimeout(500);
  await expect(page.locator('tf-toast[message="Imported into draft; save to keep it"]')).toHaveCount(1);
  await expect(page.locator('tf-window .fb-import-review')).toHaveCount(0);
  expect(errors, diagnostics(errors, proc)).toEqual([]);
});
