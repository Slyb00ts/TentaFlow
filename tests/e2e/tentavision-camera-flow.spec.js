// =============================================================================
// File: tests/e2e/tentavision-camera-flow.spec.js
// Description: E2E for assigning a per-camera analysis Flow in TentaVision.
//              Proves the full configurable-pipeline chain through the real UI:
//              the startup-seeded "Camera Analysis" flow is offered in the
//              per-camera "Flow analizy" selector, picking it persists the
//              assignment via camera_update_v1 (core cameras.analysis_flow_id),
//              and the selection survives a full panel reopen (read back from
//              core via camera_get, not the addon's SQLite mirror).
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

const BASE_PORT = 18401;
let PORT;
let DB;

const PERMISSIONS = ['ui', 'cameras.read', 'cameras.write', 'sql.read', 'sql.write'];

const SHOT_DIR = '/tmp/tv';
const CAM_NAME = 'C-23 wjazd-ADR-flow';
const CAM_RTSP = 'rtsp://192.168.40.41:554/stream1';
// Stable id of the startup-seeded camera-analysis flow (db/seed.rs).
const CAMERA_ANALYSIS_FLOW_ID = '00000000-0000-4000-8000-000000000020';

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-camflow-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Camera Flow E2E',
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

async function openCameras(page) {
  const tab = page.locator('tf-tab#cameras');
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

async function fillField(page, fieldId, value) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 10000 });
  const input = field.locator('input, textarea').first();
  await input.click();
  await input.fill(value);
  await input.dispatchEvent('input');
  await page.waitForTimeout(150);
}

// Opens the first row's kebab (⋯) action menu and clicks the item by label.
async function clickRowAction(page, label) {
  const kebab = page.locator('.addon-app-shell tf-table tf-button[aria-label="Akcje wiersza"]').first();
  await expect(kebab).toBeVisible({ timeout: 10000 });
  await kebab.scrollIntoViewIfNeeded();
  await kebab.click();
  const item = page.locator('.addon-app-shell tf-menu-item', { hasText: label }).first();
  await expect(item).toBeVisible({ timeout: 10000 });
  await item.click({ force: true });
  await page.waitForTimeout(250);
}

async function addCameraViaWizard(page) {
  await clickButtonByText(page, 'Dodaj kamerę');
  const rtspCard = page.locator('.addon-app-shell .tf-radio-card-group__card', { hasText: 'RTSP' }).first();
  await expect(rtspCard).toBeVisible({ timeout: 10000 });
  await rtspCard.click();
  await page.waitForTimeout(150);
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'rtsp_url', CAM_RTSP);
  await clickButtonByText(page, 'Dalej');
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'name', CAM_NAME);
  await clickButtonByText(page, 'Zakończ');
  await expect(page.locator('.addon-app-shell').getByText(CAM_NAME).first())
    .toBeVisible({ timeout: 10000 });
}

// Reads the current value of the open flow selector.
async function flowSelectValue(page) {
  const sel = page.locator('.addon-app-shell [data-component-id="camera_flow_select"]').first();
  await expect(sel).toBeVisible({ timeout: 10000 });
  return sel.locator('select').first().inputValue();
}

test.describe('TentaVision — per-camera analysis flow assignment', () => {
  test('seeded flow is offered, assignment persists across reopen', async ({ page, browser }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);
    await openCameras(page);

    // A camera to assign a flow to.
    await addCameraViaWizard(page);

    // --- Open the per-camera flow selector via the "Flow analizy" row action. ---
    await clickRowAction(page, 'Flow analizy');
    const sel = page.locator('.addon-app-shell [data-component-id="camera_flow_select"]').first();
    await expect(sel).toBeVisible({ timeout: 10000 });
    // The startup-seeded "Camera Analysis" flow must be an option.
    await expect(sel.locator('select option', { hasText: 'Camera Analysis' }).first())
      .toHaveCount(1, { timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/camflow-selector.png`, fullPage: true });

    // --- Pick the seeded flow. selectOption fires the change handler ->
    //     camera_update_v1 -> cameras.analysis_flow_id -> success message. ---
    await sel.locator('select').first().selectOption({ label: 'Camera Analysis' });
    await expect(page.locator('.addon-app-shell').getByText('Przypisano flow analizy do kamery.').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/camflow-assigned.png`, fullPage: true });

    // --- Persistence across a full reopen in a fresh, isolated context: the
    //     selector must preselect the assigned flow, read back from core
    //     (camera_get analysis_flow_id), not the addon's SQLite mirror. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openCameras(page2);
    await clickRowAction(page2, 'Flow analizy');
    // The tf-select surfaces typed option values with a `tstr:` value-format
    // prefix in the DOM; the persisted core id is the clean uuid (camera_update
    // validated it exists+active before storing), so assert containment.
    expect(await flowSelectValue(page2)).toContain(CAMERA_ANALYSIS_FLOW_ID);
    await page2.screenshot({ path: `${SHOT_DIR}/camflow-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
