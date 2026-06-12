// =============================================================================
// File: tests/e2e/sdk-showcase-panel.spec.js
// Description: E2E tests for the sdk-showcase addon panel (CBOR UI runtime).
//              Covers: deterministic first-attempt panel open, reactive Live
//              counter (every click must count — regression for the state
//              revision drift fixed in b5316b65), close/reopen, all catalog
//              tabs, and the SQL / KV / Vector demo actions. Every test also
//              asserts zero browser console errors.
// =============================================================================

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

// Base port/db — offset per Playwright worker so --repeat-each (which may
// schedule repeats on parallel workers) never collides on port or SQLite.
const BASE_PORT = 18121;
let PORT;
let DB;

// Permissions the panel exercises: UI channel, toasts, KV storage,
// per-addon SQL, vector namespace, refresh event publish.
const PERMISSIONS = [
  'ui',
  'notifications',
  'storage.read',
  'storage.write',
  'sql.read',
  'sql.write',
  'vector.read',
  'vector.write',
  'events.publish',
];

const CATALOG_TABS = ['molecules', 'layout', 'data', 'form', 'action', 'feedback', 'specialized'];

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-sdk-showcase-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  // One-time setup: install + grant + enable the sdk-showcase instance.
  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'sdk-showcase',
    displayName: 'SDK Showcase E2E',
    permissions: PERMISSIONS,
  });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

/** Clicks the sidebar Apps entry for the installed instance. */
async function openPanelFromAppsMenu(page) {
  const navItem = page.locator(`.addon-app-nav-item[data-addon-id="${addonId}"]`);
  await expect(navItem).toBeVisible({ timeout: 10000 });
  await navItem.click();
}

/** Waits until the Live tab content is fully rendered. */
async function waitForLiveTab(page) {
  await expect(page.locator('.addon-app-shell')).toBeVisible({ timeout: 10000 });
  await expect(page.locator('[data-component-id="tab-live"]')).toBeVisible({ timeout: 10000 });
  await expect(page.locator('[data-component-id="live-counter"]')).toBeVisible();
}

function assertNoConsoleErrors(errors) {
  expect(errors, diagnostics(errors, proc)).toEqual([]);
}

/** Clicks a nav tab; tf-tabs scrolls horizontally, so center the tab first. */
async function clickTab(page, tabId) {
  const tab = page.locator(`tf-tab#${tabId}`);
  await tab.evaluate((el) => el.scrollIntoView({ inline: 'center', block: 'nearest' }));
  await tab.click();
}

test.describe('sdk-showcase — panel open & Live tab', () => {
  test('panel opens on the FIRST attempt with shell + Live content, zero console errors', async ({ page }) => {
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanelFromAppsMenu(page);
    await waitForLiveTab(page);

    // Shell carries the addon/panel identity.
    await expect(page.locator(`.addon-app-shell[data-addon="${addonId}"][data-panel="main"]`)).toBeVisible();

    // Specific failure classes seen in the field must never appear.
    const text = errors.join('\n');
    expect(text).not.toMatch(/handleSlotContent THREW/);
    expect(text).not.toMatch(/no renderer registered/);
    assertNoConsoleErrors(errors);
  });

  test('increment clicked 10x — counter shows exactly 10 (no dropped clicks)', async ({ page }) => {
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanelFromAppsMenu(page);
    await waitForLiveTab(page);

    const counter = page.locator('[data-component-id="live-counter"]');
    await expect(counter).toHaveText('0');

    const btn = page.locator('[data-component-id="btn-increment"]');
    for (let i = 1; i <= 10; i++) {
      await btn.click();
      // Every single click must be reflected — this is the regression guard
      // for the "only every ~4th click counted" revision-drift bug.
      await expect(counter, diagnostics(errors, proc)).toHaveText(String(i), { timeout: 5000 });
    }

    assertNoConsoleErrors(errors);
  });

  test('close panel, reopen — counter resets and clicks still register', async ({ page }) => {
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanelFromAppsMenu(page);
    await waitForLiveTab(page);

    const btn = page.locator('[data-component-id="btn-increment"]');
    const counter = page.locator('[data-component-id="live-counter"]');
    await btn.click();
    await expect(counter).toHaveText('1');

    // Navigate away (sends PanelClose) and back (fresh PanelOpen + epoch).
    await page.locator('.sidebar .nav-item[data-view="dashboard"]').click();
    await expect(page.locator('.addon-app-shell')).toHaveCount(0);
    await openPanelFromAppsMenu(page);
    await waitForLiveTab(page);

    // New panel session: counter restarted, and clicks must register again
    // (regression guard for the revision reset on reopen).
    await expect(counter).toHaveText('0');
    for (let i = 1; i <= 3; i++) {
      await btn.click();
      await expect(counter, diagnostics(errors, proc)).toHaveText(String(i), { timeout: 5000 });
    }

    assertNoConsoleErrors(errors);
  });
});

test.describe('sdk-showcase — catalog tabs', () => {
  test('every catalog tab renders content with zero console errors', async ({ page }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanelFromAppsMenu(page);
    await waitForLiveTab(page);

    for (const tab of CATALOG_TABS) {
      const before = errors.length;
      await clickTab(page, tab);
      const section = page.locator(`[data-component-id="catalog-${tab}"]`);
      await expect(section, diagnostics(errors, proc)).toBeVisible({ timeout: 10000 });
      // The section must actually contain rendered samples, not be empty.
      const captions = section.locator(`[data-component-id^="cat-${tab}-hdr-"]`);
      expect(await captions.count(), `tab '${tab}' rendered no component samples`).toBeGreaterThan(0);

      const tabErrors = errors.slice(before);
      expect(tabErrors, `console errors on tab '${tab}':\n${tabErrors.join('\n')}`).toEqual([]);
    }
  });
});

test.describe('sdk-showcase — SQL / KV / Vector demos', () => {
  test('demo buttons update the result text with success messages', async ({ page }) => {
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanelFromAppsMenu(page);
    await waitForLiveTab(page);

    await clickTab(page, 'storage');
    const result = page.locator('[data-component-id="storage-result"]');
    await expect(result).toBeVisible({ timeout: 10000 });

    await page.locator('[data-component-id="btn-kv-demo"]').click();
    await expect(result, diagnostics(errors, proc)).toContainText('KV round-trip OK', { timeout: 10000 });

    await page.locator('[data-component-id="btn-sql-demo"]').click();
    await expect(result, diagnostics(errors, proc)).toContainText('SQL suite OK', { timeout: 10000 });

    await page.locator('[data-component-id="btn-vector-demo"]').click();
    await expect(result, diagnostics(errors, proc)).toContainText('Vector suite OK', { timeout: 10000 });

    assertNoConsoleErrors(errors);
  });
});
