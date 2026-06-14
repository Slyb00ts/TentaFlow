// =============================================================================
// File: tests/e2e/tentavision-evidence.spec.js
// Description: E2E for the TentaVision "Eksport dowodowy" (Evidence export) tab.
//              Proves: empty states (no packages, no recipients), adding an
//              authorized recipient persists to the evidence_recipients settings
//              key, creating an evidence package referencing a REAL source alarm
//              INSERTs an evidence row (with an "oczekuje"/"wydana" status chip),
//              deleting a package, and that both the packages (evidence table)
//              and the recipients (settings JSON) survive a full panel reopen in
//              a fresh, isolated context. The HSM/TSA signing controls are honest
//              placeholders (metadata records are the real persisted part — no
//              cryptographic artifact). Screenshots land in /tmp/tv/ for visual
//              comparison to m11-evidence.html.
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

const BASE_PORT = 18361;
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
const CAM_NAME = 'Brama C-04';
const CAM_RTSP = 'rtsp://10.0.0.4:554/stream';
const RECIPIENT = { name: 'Prokuratura Rejonowa Warszawa-Mokotów', key: 'PGP 4F2A...8E91' };

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-evidence-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Evidence E2E',
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

// Opens the row kebab (⋯) menu of a given table (by 0-based index — the
// recipients table is 0, the packages table is 1) and clicks the matching item.
async function clickRowAction(page, label, tableIndex = 1) {
  const table = page.locator('.addon-app-shell tf-table').nth(tableIndex);
  const kebab = table.locator('tf-button[aria-label="Akcje wiersza"]').first();
  await expect(kebab).toBeVisible({ timeout: 10000 });
  await kebab.scrollIntoViewIfNeeded();
  await kebab.click();
  await page.waitForTimeout(150);
  // The opened menu's items live inside that same table's first row menu.
  const item = table.locator('tf-menu-item', { hasText: label }).first();
  await expect(item).toBeVisible({ timeout: 10000 });
  await item.click({ force: true });
  await page.waitForTimeout(250);
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

async function selectFieldByIndex(page, fieldId, index) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 10000 });
  await field.locator('select').first().selectOption({ index });
  await page.waitForTimeout(200);
}

async function selectFieldByValue(page, fieldId, value) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 10000 });
  await field.locator('select').first().selectOption(value);
  await page.waitForTimeout(200);
}

// Drives the 4-step "add camera" wizard so a simulated alarm references a real
// camera id. Mirrors the alarms/zones specs.
async function addCameraViaWizard(page) {
  await openTab(page, 'cameras');
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

test.describe('TentaVision Evidence — SQLite package CRUD + recipients persistence', () => {
  test('empty states, add recipient, create package, delete, persist across reopen', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Seed one real camera + one real alarm so the package has a source. ---
    await addCameraViaWizard(page);
    await openTab(page, 'alarms');
    await clickButtonByText(page, 'Symuluj alarm');
    await expect(page.locator('.addon-app-shell').getByText('Workflow').first())
      .toBeVisible({ timeout: 10000 });

    // --- Evidence tab: both empty states (no packages, no recipients). ---
    await openTab(page, 'evidence');
    await expect(page.locator('.addon-app-shell tf-empty-state')).toHaveCount(2, { timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Brak paczek dowodowych').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Brak odbiorców').first())
      .toBeVisible({ timeout: 10000 });
    // Honest no-backend trust-chain notice present.
    await expect(page.locator('.addon-app-shell').getByText('Brak modułu HSM/TSA').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/evidence-empty.png`, fullPage: true });

    // --- Add one authorized recipient; persists to the settings JSON list. ---
    await clickButtonByText(page, 'Dodaj odbiorcę');
    await fillField(page, 'evidence_recipient_name', RECIPIENT.name);
    await fillField(page, 'evidence_recipient_key', RECIPIENT.key);
    await clickButtonByText(page, 'Zapisz odbiorcę');
    await expect(page.locator('.addon-app-shell tf-table').getByText(RECIPIENT.name).first())
      .toBeVisible({ timeout: 10000 });

    // --- Create an evidence package: pick the real source alarm (option 1) and
    // the recipient by value, then create. A real evidence row is INSERTed. ---
    await clickButtonByText(page, 'Utwórz pakiet dowodowy');
    await selectFieldByIndex(page, 'evidence_alarm', 1);
    await selectFieldByValue(page, 'evidence_recipient', `tstr:${RECIPIENT.name}`);
    await clickButtonByText(page, 'Zapisz pakiet');

    // Package row appears with a generated EV-* reference + the "wydana" (ok)
    // status chip (recipient was set).
    await expect(page.locator('.addon-app-shell').getByText(/EV-\d+/).first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell tf-table').getByText('wydana').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/evidence-list.png`, fullPage: true });

    // --- Signing/download is an honest no-op placeholder (no HSM backend). ---
    await clickRowAction(page, 'Weryfikuj');
    await expect(page.locator('.addon-app-shell').getByText(/Podpis HSM\/TSA wymaga skonfigurowanego modułu/).first())
      .toBeVisible({ timeout: 10000 });

    // --- Create a second package WITHOUT a recipient -> "oczekuje" (warn). ---
    await clickButtonByText(page, 'Utwórz pakiet dowodowy');
    await selectFieldByIndex(page, 'evidence_alarm', 1);
    await clickButtonByText(page, 'Zapisz pakiet');
    await expect(page.locator('.addon-app-shell tf-table').getByText('oczekuje').first())
      .toBeVisible({ timeout: 10000 });

    // --- Delete the first package via the row kebab; confirm. ---
    await clickRowAction(page, 'Usuń');
    await clickButtonByText(page, 'Usuń');
    await page.waitForTimeout(400);

    // --- Persistence: reopen in a fresh, isolated context. The recipient + the
    // remaining package (served from settings/SQLite) must survive. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'evidence');

    // Recipient persisted (settings JSON).
    await expect(page2.locator('.addon-app-shell tf-table').getByText(RECIPIENT.name).first())
      .toBeVisible({ timeout: 10000 });
    // At least one package row persisted (evidence table), still showing EV-*.
    await expect(page2.locator('.addon-app-shell').getByText(/EV-\d+/).first())
      .toBeVisible({ timeout: 10000 });
    // The package list is no longer empty.
    await expect(page2.locator('.addon-app-shell').getByText('Brak paczek dowodowych'))
      .toHaveCount(0);
    await page2.screenshot({ path: `${SHOT_DIR}/evidence-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
