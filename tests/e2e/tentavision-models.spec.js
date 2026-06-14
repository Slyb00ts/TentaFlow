// =============================================================================
// File: tests/e2e/tentavision-models.spec.js
// Description: E2E for the TentaVision "Modele i runtime" tab backed by the
//              per-addon SQLite models table. Proves: empty state + VRAM budget
//              bar, the form INSERTs models, the registry table lists them from
//              the DB with toned status chips, the VRAM stacked-bar reflects the
//              sum of active models, delete + budget edit persist across a full
//              panel reopen (fresh context). Captures screenshots to /tmp/tv/
//              for visual comparison to m08-models.html.
// =============================================================================

const fs = require('fs');
const { test, expect } = require('@playwright/test');
const {
  startBinary,
  stopBinary,
  waitForServer,
  binaryExists,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const {
  installAddonInstance,
  collectConsoleErrors,
  diagnostics,
} = require('./helpers/addon-setup');

const BASE_PORT = 18341;
let PORT;
let DB;

const PERMISSIONS = [
  'ui',
  'cameras.read',
  'cameras.write',
  'sql.read',
  'sql.write',
];

const SHOT_DIR = '/tmp/tv';
const MODEL_A = { name: 'YOLO11m', vram: '1700', version: 'yolo11m-2026.04' };
const MODEL_B = { name: 'PP-OCRv5', vram: '1300', version: 'ppocrv5-2026.03' };

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-models-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Models E2E',
    permissions: PERMISSIONS,
  });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

async function openPanel(page) {
  const navItem = page.locator(`.addon-app-nav-item[data-addon-id="${addonId}"]`);
  await expect(navItem).toBeVisible({ timeout: 10000 });
  await navItem.click();
  await expect(page.locator(`.addon-app-shell[data-addon="${addonId}"]`)).toBeVisible({ timeout: 10000 });
  await expect(page.locator('tf-tab#overview')).toBeVisible({ timeout: 10000 });
}

async function openTab(page, tabId) {
  const tab = page.locator(`tf-tab#${tabId}`);
  await tab.evaluate((el) => el.scrollIntoView({ inline: 'center', block: 'nearest' }));
  await tab.click();
  await expect(page.locator('.addon-app-shell [data-component-id]').first())
    .toBeVisible({ timeout: 10000 });
}

async function clickButtonByText(page, text) {
  const btn = page.locator('.addon-app-shell tf-button', { hasText: text }).first();
  await expect(btn).toBeVisible({ timeout: 10000 });
  await btn.scrollIntoViewIfNeeded();
  await btn.click();
}

// Opens the first row's kebab (⋯) action menu and clicks the item whose label
// matches. tf-table row_actions render as a per-row tf-menu with tf-menu-item.
async function clickRowAction(page, label) {
  const kebab = page.locator('.addon-app-shell tf-table tf-button[aria-label="Akcje wiersza"]').first();
  await expect(kebab).toBeVisible({ timeout: 10000 });
  await kebab.scrollIntoViewIfNeeded();
  await kebab.click();
  const item = page.locator('.addon-app-shell tf-menu-item', { hasText: label }).first();
  await expect(item).toBeVisible({ timeout: 10000 });
  await item.click({ force: true });
  await page.waitForTimeout(200);
}

async function fillField(page, fieldId, value) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 10000 });
  const input = field.locator('input, textarea').first();
  await input.click();
  await input.fill(value);
  await input.dispatchEvent('input');
  await page.waitForTimeout(150);
}

async function addModel(page, m) {
  await clickButtonByText(page, 'Dodaj model');
  await fillField(page, 'model_name', m.name);
  await fillField(page, 'model_vram', m.vram);
  await fillField(page, 'model_version', m.version);
  // Status defaults to "active" (counts toward VRAM budget), runtime "tensorrt".
  await clickButtonByText(page, 'Zapisz model');
  await expect(page.locator('.addon-app-shell').getByText(m.name).first())
    .toBeVisible({ timeout: 10000 });
}

test.describe('TentaVision Models — SQLite CRUD + VRAM budget + persistence', () => {
  test('empty state, add models, VRAM bar, delete, edit budget, persist', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Models tab empty state in the fresh DB; the VRAM budget bar still shows. ---
    await openTab(page, 'models');
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell tf-bar-chart').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/models-empty.png`, fullPage: true });

    // --- Add two active models; both must appear in the registry table. ---
    await addModel(page, MODEL_A);
    await addModel(page, MODEL_B);

    await expect(page.locator('.addon-app-shell tf-table').first()).toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText(MODEL_A.name).first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText(MODEL_B.name).first()).toBeVisible();
    // VRAM used = 1700 + 1300 = 3000 MB; the budget chip must reflect it.
    await expect(page.locator('.addon-app-shell').getByText(/3000\s*\/\s*\d+\s*MB/).first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/models-list.png`, fullPage: true });

    // --- Edit the budget to a smaller value and save (persists to settings). ---
    await clickButtonByText(page, 'Zmień budżet');
    await fillField(page, 'model_budget_input', '4096');
    await clickButtonByText(page, 'Zapisz');
    await expect(page.locator('.addon-app-shell').getByText(/4096\s*MB/).first())
      .toBeVisible({ timeout: 10000 });

    // --- Delete model B (first row — rows sort by name, so PP-OCRv5 precedes
    // YOLO11m) via the row kebab menu, proving the delete path. ---
    await clickRowAction(page, 'Usuń');
    // Confirm bar -> explicit Usuń button.
    await clickButtonByText(page, 'Usuń');
    await page.waitForTimeout(400);

    // --- Persistence: reopen in a fresh, isolated context. Model A + the edited
    // budget must survive (served from SQLite/settings); model B must be gone. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'models');

    await expect(page2.locator('.addon-app-shell').getByText(MODEL_A.name).first())
      .toBeVisible({ timeout: 10000 });
    await expect(page2.locator('.addon-app-shell').getByText(MODEL_B.name))
      .toHaveCount(0);
    // Edited budget persisted: used 1700 / budget 4096.
    await expect(page2.locator('.addon-app-shell').getByText(/1700\s*\/\s*4096\s*MB/).first())
      .toBeVisible({ timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/models-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
