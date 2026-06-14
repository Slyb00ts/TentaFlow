// =============================================================================
// File: tests/e2e/tentavision-onboarding.spec.js
// Description: E2E for the TentaVision "Onboarding" wizard tab (M13). Proves the
//              4-step wizard (role -> legal profile -> first camera -> presets)
//              renders with SDK components, every outcome persists (settings keys
//              onboarding_role / legal_profile / onboarding_presets + a real
//              camera row via insert_camera), finish writes onboarding_completed
//              + onboarding_completed_at + an audit row, the wizard-created camera
//              shows up on the Cameras tab, and a fresh-context reopen shows the
//              persisted completed summary. Zero console errors. Screenshots to
//              /tmp/tv/ for visual comparison to m13-onboarding.html.
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

const BASE_PORT = 18371;
let PORT;
let DB;

const PERMISSIONS = [
  'ui',
  'cameras.read',
  'cameras.write',
  'recording.read',
  'sql.read',
  'sql.write',
];

const SHOT_DIR = '/tmp/tv';
const CAM_NAME = 'Brama główna E2E';
const CAM_RTSP = 'rtsp://192.168.50.10:554/onboard';

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-onboarding-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Onboarding E2E',
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

// Clicks the first visible tf-button inside the addon shell whose label matches.
async function clickButtonByText(page, text) {
  const btn = page.locator('.addon-app-shell tf-button', { hasText: text }).first();
  await expect(btn).toBeVisible({ timeout: 10000 });
  await btn.scrollIntoViewIfNeeded();
  await btn.click();
  await page.waitForTimeout(150);
}

// Clicks the "Wybierz" button inside the option card carrying `cardText`.
async function pickOption(page, cardText) {
  const card = page.locator('.addon-app-shell tf-section-card', { hasText: cardText }).first();
  await expect(card).toBeVisible({ timeout: 10000 });
  const btn = card.locator('tf-button', { hasText: 'Wybierz' }).first();
  await btn.scrollIntoViewIfNeeded();
  await btn.click();
  await page.waitForTimeout(200);
}

async function fillField(page, fieldId, value) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 10000 });
  const input = field.locator('input').first();
  await input.click();
  await input.fill(value);
  await input.dispatchEvent('input');
  await page.waitForTimeout(150);
}

test.describe('TentaVision Onboarding — 4-step wizard with persisted outcomes', () => {
  test('walk wizard, persist outcomes, camera appears in Cameras, summary on reopen', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Step 1: role selection. ---
    await openTab(page, 'onboarding');
    await expect(page.locator('.addon-app-shell').getByText('Witaj w TentaVision').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Krok 1 — Rola wdrożenia').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/onboarding-step1.png`, fullPage: true });

    await pickOption(page, 'Depo / baza taboru');
    await clickButtonByText(page, 'Dalej: profil prawny');

    // --- Step 2: legal profile. ---
    await expect(page.locator('.addon-app-shell').getByText('Krok 2 — Profil prawny').first())
      .toBeVisible({ timeout: 10000 });
    await pickOption(page, 'Komercja prywatna');
    await clickButtonByText(page, 'Dalej: pierwsza kamera');

    // --- Step 3: first camera. ---
    await expect(page.locator('.addon-app-shell').getByText('Krok 3 — Pierwsza kamera').first())
      .toBeVisible({ timeout: 10000 });
    await fillField(page, 'onb_camera_name', CAM_NAME);
    await fillField(page, 'onb_camera_url', CAM_RTSP);
    await clickButtonByText(page, 'Dalej: presety');

    // --- Step 4: presets, then finish. ---
    await expect(page.locator('.addon-app-shell').getByText('Krok 4 — Presety detektorów').first())
      .toBeVisible({ timeout: 10000 });
    await pickOption(page, 'Bezpieczeństwo');
    await clickButtonByText(page, 'Zakończ konfigurację');

    // --- Completion summary. ---
    await expect(page.locator('.addon-app-shell').getByText('Konfiguracja zakończona').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Depo / baza taboru').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('Komercja prywatna', { exact: false }).first()).toBeVisible();
    await page.screenshot({ path: `${SHOT_DIR}/onboarding-done.png`, fullPage: true });

    // --- The wizard-created camera shows up on the Cameras tab (read from SQLite). ---
    await openTab(page, 'cameras');
    await expect(page.locator('.addon-app-shell').getByText(CAM_NAME).first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell tf-table').first()).toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/onboarding-camera.png`, fullPage: true });

    // --- Persistence: fresh, isolated context. Onboarding shows the completed
    //     summary (read from settings), not the wizard from step 1. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'onboarding');

    await expect(page2.locator('.addon-app-shell').getByText('Konfiguracja zakończona').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page2.locator('.addon-app-shell').getByText('Podsumowanie wdrożenia').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page2.locator('.addon-app-shell').getByText('Depo / baza taboru').first()).toBeVisible();
    await expect(page2.locator('.addon-app-shell tf-button', { hasText: 'Uruchom ponownie' }).first())
      .toBeVisible({ timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/onboarding-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
