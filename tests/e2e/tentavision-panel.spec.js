// =============================================================================
// File: tests/e2e/tentavision-panel.spec.js
// Description: E2E tests for the TentaVision addon panel (addons-pro,
//              14-panel CBOR UI). Regression for the GridTrack BigInt render
//              crash: the overview must render and tab navigation must work
//              without any browser console errors.
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
const BASE_PORT = 18221;
let PORT;
let DB;

const PERMISSIONS = [
  'ui',
  'cameras.read',
  'cameras.write',
  'cameras.snapshot',
  'streams.subscribe',
  'recording.read',
  'sql.read',
  'sql.write',
];

// Panels reachable from the nav tabs (subset exercised; overview is the
// entry panel and the GridTrack regression target).
const NAV_PANELS = ['live', 'cameras', 'profiles', 'alarms', 'search', 'settings'];

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tentavision-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision E2E',
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
}

test.describe('TentaVision — panel rendering', () => {
  test('overview opens on first attempt and renders without console errors', async ({ page }) => {
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // Nav tabs prove the PanelShell rendered; overview tab is the entry.
    await expect(page.locator('tf-tab#overview')).toBeVisible({ timeout: 10000 });
    // Overview content (slot) must contain rendered components, not stay on
    // the loading placeholder.
    await expect(page.locator('.addon-app-shell [data-component-id]').first())
      .toBeVisible({ timeout: 10000 });

    const text = errors.join('\n');
    expect(text).not.toMatch(/handleSlotContent THREW/);
    expect(text).not.toMatch(/no renderer registered/);
    expect(text).not.toMatch(/BigInt/);
    expect(errors, diagnostics(errors, proc)).toEqual([]);
  });

  test('nav tabs switch panels without console errors', async ({ page }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);
    await expect(page.locator('tf-tab#overview')).toBeVisible({ timeout: 10000 });

    for (const panel of NAV_PANELS) {
      const before = errors.length;
      const tab = page.locator(`tf-tab#${panel}`);
      await tab.evaluate((el) => el.scrollIntoView({ inline: 'center', block: 'nearest' }));
      await tab.click();
      // Each panel pushes fresh SlotContent — wait until the slot holds at
      // least one rendered component after the switch.
      await expect(page.locator('.addon-app-shell [data-component-id]').first())
        .toBeVisible({ timeout: 10000 });
      const tabErrors = errors.slice(before);
      expect(tabErrors, `console errors on panel '${panel}':\n${tabErrors.join('\n')}\n${diagnostics([], proc)}`).toEqual([]);
    }
  });
});
