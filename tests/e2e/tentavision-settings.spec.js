// =============================================================================
// File: tests/e2e/tentavision-settings.spec.js
// Description: E2E for the TentaVision "Ustawienia" (M12) tab backed by the
//              per-addon SQLite settings(key,value,updated_at) table. Proves:
//              first-run defaults render (storage paths, runtime, notifications,
//              licenses, legal profile); changing a text path + a toggle + a
//              select and clicking "Zapisz zmiany" persists every key via
//              db::set_setting and writes one hash-chained audit_log summary row
//              (action=settings_change) visible on the Audit tab; the changed
//              values survive a fresh, isolated browser context reopen (served
//              from SQLite). Screenshots → /tmp/tv/ for comparison to
//              m12-settings.html.
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
  'sql.read',
  'sql.write',
];

const SHOT_DIR = '/tmp/tv';

// New values typed/picked during the save round-trip.
const NEW_RECORDINGS_DIR = '/srv/tv-prod/recordings';
// The legal-profile setting persists as the bare value (public_transport), but
// the native <option> value attribute is the serialized SelectValue (tag:value),
// so the tf-select inner <select> exposes/accepts the "tstr:" prefixed form.
const NEW_LEGAL_PROFILE = 'public_transport';
const NEW_LEGAL_OPTION = 'tstr:public_transport';

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-settings-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Settings E2E',
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

// Reads the inner input value of a bound settings field (component id = store key).
async function fieldInput(page, storeKey) {
  return page.locator(`.addon-app-shell [data-component-id="${storeKey}"]`).first().locator('input').first();
}

async function fillSetting(page, storeKey, value) {
  const input = await fieldInput(page, storeKey);
  await expect(input).toBeVisible({ timeout: 10000 });
  await input.click();
  await input.fill(value);
  await input.dispatchEvent('input');
  await page.waitForTimeout(150);
}

test.describe('TentaVision Settings — DB-backed defaults, save + audit, persistence', () => {
  test('defaults render, change path/toggle/select, save persists across fresh context + audit row', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- First-run defaults: every section renders its current (default) values
    // straight from the settings table (absent keys fall back to defaults). ---
    await openTab(page, 'settings');
    await expect(page.locator('.addon-app-shell').getByText('Storage i retencja').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Inference runtime').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Powiadomienia i integracje').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Licencje i klucze').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Profil prawny i AI Act').first())
      .toBeVisible({ timeout: 10000 });
    // The recordings path input shows its default value on a fresh DB.
    await expect(await fieldInput(page, 'set_storage_recordings_dir'))
      .toHaveValue('/mnt/tentavision/recordings', { timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/settings.png`, fullPage: true });

    // --- Change a text path, flip a toggle (SMS, default off), pick a legal
    // profile, then save. ---
    await fillSetting(page, 'set_storage_recordings_dir', NEW_RECORDINGS_DIR);

    // tf-toggle is a non-labelable custom element, so click the inner switch
    // span directly (a click on the wrapping <label> would not toggle it).
    const smsToggle = page.locator('.addon-app-shell [data-component-id="set_notify_sms_enabled"] [role="switch"]').first();
    await expect(smsToggle).toBeVisible({ timeout: 10000 });
    await smsToggle.scrollIntoViewIfNeeded();
    await expect(smsToggle).toHaveAttribute('aria-checked', 'false', { timeout: 10000 });
    await smsToggle.click();
    await expect(smsToggle).toHaveAttribute('aria-checked', 'true', { timeout: 10000 });

    const legalSelect = page.locator('.addon-app-shell [data-component-id="set_legal_profile"]').first();
    await expect(legalSelect).toBeVisible({ timeout: 10000 });
    await legalSelect.locator('select').first().selectOption(NEW_LEGAL_OPTION);
    await page.waitForTimeout(150);

    await clickButtonByText(page, 'Zapisz zmiany');
    await expect(page.locator('.addon-app-shell').getByText(/Zapisano \d+ ustawień/).first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/settings-saved.png`, fullPage: true });

    // --- The save appended ONE hash-chained audit summary row. ---
    await openTab(page, 'audit');
    await expect(page.locator('.addon-app-shell').getByText('settings_change').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Łańcuch zweryfikowany').first())
      .toBeVisible({ timeout: 10000 });

    // --- Persistence: reopen in a fresh, isolated context. Every changed value
    // must survive, served from the SQLite settings table. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'settings');

    await expect(await fieldInput(page2, 'set_storage_recordings_dir'))
      .toHaveValue(NEW_RECORDINGS_DIR, { timeout: 10000 });
    // The legal-profile select reflects the persisted choice.
    await expect(page2.locator('.addon-app-shell [data-component-id="set_legal_profile"] select').first())
      .toHaveValue(NEW_LEGAL_OPTION, { timeout: 10000 });
    // The SMS toggle persisted as on (1). tf-toggle renders a span[role=switch]
    // whose aria-checked reflects the bound store value seeded from SQLite.
    const smsToggle2 = page2.locator('.addon-app-shell [data-component-id="set_notify_sms_enabled"] [role="switch"]').first();
    await expect(smsToggle2).toHaveAttribute('aria-checked', 'true', { timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/settings-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
