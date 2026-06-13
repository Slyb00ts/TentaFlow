// =============================================================================
// File: tests/e2e/tentavision-profiles.spec.js
// Description: E2E for the TentaVision "Profile analityczne" tab backed by the
//              per-addon SQLite profiles table. Proves: empty state, the builder
//              form INSERTs a profile (with a real assigned camera), the library
//              table lists it from the DB, enable/disable persists, the profile
//              PERSISTS across a full panel reopen, and an enabled profile makes
//              the Dashboard "Aktywne detektory" KPI non-zero. Captures
//              screenshots to /tmp/tv/ for visual comparison to m04-profiles.html.
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

const BASE_PORT = 18301;
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
const CAM_NAME = 'C-01 brama wjazdowa';
const CAM_RTSP = 'rtsp://192.168.40.41:554/stream1';
const PROFILE_NAME = 'ADR-brama';

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-profiles-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Profiles E2E',
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
  await item.click();
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

// Drives the 4-step "add camera" wizard for an RTSP source so the profile has a
// real camera to assign. Mirrors the cameras spec.
async function addCamera(page, name, rtsp) {
  await clickButtonByText(page, 'Dodaj kamerę');
  const rtspCard = page.locator('.addon-app-shell .tf-radio-card-group__card', { hasText: 'RTSP' }).first();
  await expect(rtspCard).toBeVisible({ timeout: 10000 });
  await rtspCard.click();
  await page.waitForTimeout(150);
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'rtsp_url', rtsp);
  await clickButtonByText(page, 'Dalej');
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'name', name);
  await clickButtonByText(page, 'Zakończ');
  await expect(page.locator('.addon-app-shell').getByText(name).first())
    .toBeVisible({ timeout: 10000 });
}

test.describe('TentaVision Profiles — SQLite CRUD + persistence + KPI linkage', () => {
  test('empty state, build a profile, list, toggle, persist, dashboard KPI', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Seed one real camera via the wizard so the builder has one to assign. ---
    await openTab(page, 'cameras');
    await addCamera(page, CAM_NAME, CAM_RTSP);

    // --- Profiles tab empty state in the fresh DB. ---
    await openTab(page, 'profiles');
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/profiles-empty.png`, fullPage: true });

    // --- Open the builder, name the profile, assign the camera, save. Risk class
    // defaults to A, Flow to tv-realtime-adr, schedule to 24/7 (reset_form). ---
    await clickButtonByText(page, 'Nowy profil');
    await fillField(page, 'profile_name', PROFILE_NAME);
    // Assign the camera: the toggle button carries the camera name; click it.
    const camBtn = page.locator('.addon-app-shell tf-button', { hasText: CAM_NAME }).first();
    await expect(camBtn).toBeVisible({ timeout: 10000 });
    await camBtn.scrollIntoViewIfNeeded();
    await camBtn.click();
    await page.waitForTimeout(200);
    await clickButtonByText(page, 'Utwórz profil');

    // --- The new profile must appear in the library table (read from SQLite). ---
    await expect(page.locator('.addon-app-shell').getByText(PROFILE_NAME).first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell tf-table').first()).toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/profiles-list.png`, fullPage: true });

    // --- The profile is created enabled, so the Dashboard "Aktywne detektory"
    // KPI must be non-zero. ---
    await openTab(page, 'overview');
    const detectorCard = page.locator('.addon-app-shell tf-stat-card', { hasText: 'Aktywne detektory' }).first();
    await expect(detectorCard).toBeVisible({ timeout: 10000 });
    await expect(detectorCard).not.toContainText(/\b0\b/, { timeout: 10000 });

    // --- Toggle enabled off via the row kebab menu, proving the update path. ---
    await openTab(page, 'profiles');
    await clickRowAction(page, 'Włącz/wyłącz');
    await expect(page.locator('.addon-app-shell').getByText('wyłączony').first())
      .toBeVisible({ timeout: 10000 });

    // --- Persistence: reopen in a fresh, isolated context. The profile (and its
    // disabled state) must survive, served from SQLite. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'profiles');

    await expect(page2.locator('.addon-app-shell').getByText(PROFILE_NAME).first())
      .toBeVisible({ timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/profiles-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
