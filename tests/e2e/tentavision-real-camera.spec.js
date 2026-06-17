// =============================================================================
// File: tests/e2e/tentavision-real-camera.spec.js
// Description: Drives an ALREADY-RUNNING instance (TF_PORT, default 8091) — does
//              NOT spawn a binary. Logs in (admin/admin), installs TentaVision,
//              adds the real UniFi RTSPS camera via the wizard, assigns the
//              seeded Camera Analysis flow via the "Flow analizy" selector, then
//              opens Live view and screenshots. For manual real-camera testing.
// =============================================================================

const fs = require('fs');
const { test, expect } = require('@playwright/test');
const { loginAsAdmin } = require('./helpers/auth');
const { installAddonInstance } = require('./helpers/addon-setup');

const PORT = Number(process.env.TF_PORT || 8091);
const SHOT_DIR = '/tmp/tv';
const CAM_NAME = process.env.TF_CAM_NAME || 'UniFi ADR';
const CAM_URL = process.env.TF_CAM_URL || 'rtsps://192.168.0.1:7441/RFERQF7EWhhbG9Un?enableSrtp';
const PERMISSIONS = [
  'ui', 'cameras.read', 'cameras.write', 'cameras.snapshot',
  'streams.subscribe', 'sql.read', 'sql.write', 'events', 'alias.read',
];

let addonId;

// Manual harness: requires an already-running instance (TF_PORT) AND a reachable
// RTSP/RTSPS camera (TF_CAM_URL). Skips cleanly when no instance is up so a bare
// CI `playwright test` does not fail on it.
test.beforeAll(async ({ browser }) => {
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  try {
    await loginAsAdmin(page, { port: PORT });
    addonId = await installAddonInstance(page, {
      packageId: 'tentavision',
      displayName: 'TentaVision Real Camera',
      permissions: PERMISSIONS,
    });
  } catch (e) {
    test.skip(true, `no running instance on :${PORT} (manual real-camera harness): ${e.message}`);
  } finally {
    await page.close();
  }
});

async function openPanel(page) {
  const navItem = page.locator(`.addon-app-nav-item[data-addon-id="${addonId}"]`);
  await expect(navItem).toBeVisible({ timeout: 15000 });
  await navItem.click();
  await expect(page.locator('tf-tab#overview')).toBeVisible({ timeout: 15000 });
}

async function openTab(page, id) {
  const tab = page.locator(`tf-tab#${id}`);
  await tab.evaluate((el) => el.scrollIntoView({ inline: 'center', block: 'nearest' }));
  await tab.click();
  await expect(page.locator('.addon-app-shell [data-component-id]').first()).toBeVisible({ timeout: 15000 });
}

async function clickButtonByText(page, text) {
  const btn = page.locator('.addon-app-shell tf-button', { hasText: text }).first();
  await expect(btn).toBeVisible({ timeout: 15000 });
  await btn.scrollIntoViewIfNeeded();
  await btn.click();
}

async function fillField(page, fieldId, value) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 15000 });
  const input = field.locator('input, textarea').first();
  await input.click();
  await input.fill(value);
  await input.dispatchEvent('input');
  await page.waitForTimeout(150);
}

async function clickRowAction(page, label) {
  const kebab = page.locator('.addon-app-shell tf-table tf-button[aria-label="Akcje wiersza"]').first();
  await expect(kebab).toBeVisible({ timeout: 15000 });
  await kebab.scrollIntoViewIfNeeded();
  await kebab.click();
  const item = page.locator('.addon-app-shell tf-menu-item', { hasText: label }).first();
  await expect(item).toBeVisible({ timeout: 15000 });
  await item.click({ force: true });
  await page.waitForTimeout(300);
}

test('add real RTSPS camera via UI + assign Camera Analysis flow', async ({ page }) => {
  test.skip(!addonId, 'no running instance / addon (manual real-camera harness)');
  test.setTimeout(180000);
  await loginAsAdmin(page, { port: PORT });
  await openPanel(page);
  await openTab(page, 'cameras');

  // Add the camera via the 4-step wizard (RTSP source).
  await clickButtonByText(page, 'Dodaj kamerę');
  const rtspCard = page.locator('.addon-app-shell .tf-radio-card-group__card', { hasText: 'RTSP' }).first();
  await expect(rtspCard).toBeVisible({ timeout: 15000 });
  await rtspCard.click();
  await page.waitForTimeout(150);
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'rtsp_url', CAM_URL);
  await clickButtonByText(page, 'Dalej');
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'name', CAM_NAME);
  await clickButtonByText(page, 'Zakończ');

  await expect(page.locator('.addon-app-shell').getByText(CAM_NAME).first()).toBeVisible({ timeout: 20000 });
  await page.screenshot({ path: `${SHOT_DIR}/real-cam-added.png`, fullPage: true });

  // Assign the seeded Camera Analysis flow via the per-camera selector.
  await clickRowAction(page, 'Flow analizy');
  const sel = page.locator('.addon-app-shell [data-component-id="camera_flow_select"]').first();
  await expect(sel).toBeVisible({ timeout: 15000 });
  await sel.locator('select').first().selectOption({ label: 'Camera Analysis' });
  await expect(page.locator('.addon-app-shell').getByText('Przypisano flow analizy do kamery.').first())
    .toBeVisible({ timeout: 15000 });
  await page.screenshot({ path: `${SHOT_DIR}/real-cam-flow-assigned.png`, fullPage: true });

  // Open Live view and let the tile come online, then screenshot.
  await openTab(page, 'live');
  await page.waitForTimeout(8000);
  await page.screenshot({ path: `${SHOT_DIR}/real-cam-live.png`, fullPage: true });
});
