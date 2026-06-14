// =============================================================================
// File: tests/e2e/tentavision-reid.spec.js
// Description: E2E for the TentaVision "Re-ID (D4)" tab — a hard legal gate
//              backed by persisted compliance flags in the per-addon SQLite
//              settings table. Proves: the gate starts BLOCKED (Re-ID query
//              button disabled, "runtime: BLOCKED", red/amber checklist), the
//              row actions toggle + persist each compliance flag (DPIA, FRIA,
//              LegalGrant, profile, audit sync, monitoring), the gate OPENS only
//              once every required flag is satisfied (query enabled, green
//              checklist), and the open verdict survives a full panel reopen in
//              a fresh context (flags read from SQLite). Screenshots to /tmp/tv/
//              for visual comparison to m07-reid.html. Zero console errors.
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
  'sql.read',
  'sql.write',
];

const SHOT_DIR = '/tmp/tv';

// The six compliance preconditions and the row-action labels that satisfy them.
// Mirrors REID_CONDITIONS in addons-pro/tentavision/src/lib.rs.
const GATE_ACTIONS = [
  'Oznacz DPIA jako zatwierdzone',
  'Oznacz FRIA jako ukończoną',
  'Wnioskuj o LegalGrant',
  'Ustaw uprawniony profil',
  'Potwierdź synchronizację audytu',
  'Potwierdź monitoring',
];

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-reid-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Re-ID E2E',
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

async function openReid(page) {
  const tab = page.locator('tf-tab#reid');
  await tab.evaluate((el) => el.scrollIntoView({ inline: 'center', block: 'nearest' }));
  await tab.click();
  await expect(page.locator('.addon-app-shell').getByText('Re-ID osób (D4)').first())
    .toBeVisible({ timeout: 10000 });
}

// Clicks a gate row action by its label and waits for the re-render.
async function clickAction(page, label) {
  const btn = page.locator('.addon-app-shell tf-button', { hasText: label }).first();
  await expect(btn).toBeVisible({ timeout: 10000 });
  await btn.scrollIntoViewIfNeeded();
  await btn.click();
  await page.waitForTimeout(350);
}

// The Re-ID query button (enabled-state probe). It is the button labelled
// "Uruchom zapytanie Re-ID" when the gate is open, "Zapytanie zablokowane" when
// closed. Returns the matching tf-button locator (whichever label is present).
function queryButton(page) {
  return page
    .locator('.addon-app-shell tf-button')
    .filter({ hasText: /Uruchom zapytanie Re-ID|Zapytanie zablokowane/ })
    .first();
}

test.describe('TentaVision Re-ID — legal gate, persisted flags, audit', () => {
  test('blocked gate -> satisfy flags -> open gate -> persists across reopen', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);
    await openReid(page);

    // --- BLOCKED: query disabled, runtime BLOCKED, danger summary visible. ---
    await expect(page.locator('.addon-app-shell').getByText('runtime: BLOCKED').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Moduł Re-ID jest zablokowany.', { exact: false }).first())
      .toBeVisible({ timeout: 10000 });
    const qBlocked = queryButton(page);
    await expect(qBlocked).toBeVisible({ timeout: 10000 });
    // The disabled native <button> inside the tf-button has the disabled attr.
    await expect(qBlocked.locator('button').first()).toBeDisabled();
    // At least one blocked (red) condition chip present.
    await expect(page.locator('.addon-app-shell').getByText('ZABLOKOWANE').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/reid-blocked.png`, fullPage: true });

    // --- Satisfy every required flag via its row action. ---
    for (const label of GATE_ACTIONS) {
      await clickAction(page, label);
    }

    // --- OPEN: query enabled, runtime READY, all conditions OK. ---
    await expect(page.locator('.addon-app-shell').getByText('runtime: READY').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Wszystkie warunki spełnione', { exact: false }).first())
      .toBeVisible({ timeout: 10000 });
    const qOpen = queryButton(page);
    await expect(qOpen).toBeVisible({ timeout: 10000 });
    await expect(qOpen.locator('button').first()).toBeEnabled();
    // No more blocked/pending chips — six OK chips.
    await expect(page.locator('.addon-app-shell').getByText('ZABLOKOWANE')).toHaveCount(0);
    await expect(page.locator('.addon-app-shell').getByText('OCZEKUJE')).toHaveCount(0);
    await page.screenshot({ path: `${SHOT_DIR}/reid-open.png`, fullPage: true });

    // --- Honest placeholder: clicking the enabled query does NOT fake a search. ---
    await qOpen.click();
    await page.waitForTimeout(300);
    await expect(page.locator('.addon-app-shell').getByText('wymaga uruchomionego runtime', { exact: false }).first())
      .toBeVisible({ timeout: 10000 });

    // --- PERSISTENCE: reopen in a fresh, isolated context. Flags come from
    // SQLite, so the gate must still be OPEN. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openReid(page2);

    await expect(page2.locator('.addon-app-shell').getByText('runtime: READY').first())
      .toBeVisible({ timeout: 10000 });
    const qPersist = queryButton(page2);
    await expect(qPersist).toBeVisible({ timeout: 10000 });
    await expect(qPersist.locator('button').first()).toBeEnabled();
    await expect(page2.locator('.addon-app-shell').getByText('ZABLOKOWANE')).toHaveCount(0);
    await page2.screenshot({ path: `${SHOT_DIR}/reid-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
