// =============================================================================
// File: tests/e2e/tentavision-cameras.spec.js
// Description: E2E for the TentaVision Cameras tab backed by per-addon SQLite.
//              Proves: empty state, the 4-step "add camera" wizard INSERTs a
//              row, the table lists it from the DB, and the camera PERSISTS
//              across a full panel reopen (data comes from SQLite, not memory).
//              Captures screenshots to /tmp/tv/ for visual comparison to the
//              m03-cameras.html mockup.
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

const BASE_PORT = 18261;
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
const CAM_NAME = 'C-23 wjazd-ADR-2';
const CAM_RTSP = 'rtsp://192.168.40.41:554/stream1';

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-cameras-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Cameras E2E',
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
  // Wait until the cameras slot rendered at least one component.
  await expect(page.locator('.addon-app-shell [data-component-id]').first())
    .toBeVisible({ timeout: 10000 });
}

// Clicks the first visible tf-button inside the addon shell whose label matches.
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
  // Commit via Input event the addon listens for (and blur for good measure).
  await input.dispatchEvent('input');
  await page.waitForTimeout(150);
}

test.describe('TentaVision Cameras — SQLite CRUD + persistence', () => {
  test('empty state, add via wizard, list, and persist across reopen', async ({ page, browser }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);
    await openCameras(page);

    // --- Empty state: no cameras in the fresh DB -> empty-state CTA. ---
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/cameras-empty.png`, fullPage: true });

    // --- Drive the 4-step wizard: RTSP source -> url -> next -> next -> name -> finish. ---
    await clickButtonByText(page, 'Dodaj kamerę');
    // Step 0: choose RTSP source card (RadioCardGroup renders .tf-radio-card-group__card).
    const rtspCard = page.locator('.addon-app-shell .tf-radio-card-group__card', { hasText: 'RTSP' }).first();
    await expect(rtspCard).toBeVisible({ timeout: 10000 });
    await rtspCard.click();
    await page.waitForTimeout(150);
    await clickButtonByText(page, 'Dalej');

    // Step 1: RTSP url.
    await fillField(page, 'rtsp_url', CAM_RTSP);
    await clickButtonByText(page, 'Dalej');

    // Step 2: test step — skip straight to metadata.
    await clickButtonByText(page, 'Dalej');

    // Step 3: metadata name, then finish.
    await fillField(page, 'name', CAM_NAME);
    await clickButtonByText(page, 'Zakończ');

    // --- The new camera must now appear in the list (read from SQLite). ---
    await expect(page.locator('.addon-app-shell').getByText(CAM_NAME).first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell tf-table').first()).toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/cameras-list.png`, fullPage: true });

    // --- Persistence: full reopen in a fresh, isolated browser context (no
    // shared localStorage/JWT, no in-memory addon state) — forces re-login and
    // a clean panel load. The camera must still be there, served from SQLite. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openCameras(page2);

    await expect(page2.locator('.addon-app-shell').getByText(CAM_NAME).first())
      .toBeVisible({ timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/cameras-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
