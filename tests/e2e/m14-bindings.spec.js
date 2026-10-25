// =============================================================================
// File: tests/e2e/m14-bindings.spec.js
// Description: UI e2e tests for M14 — Bindings tab in addon detail. Covers
//              tab rendering (storage cards + alias list), heuristic warning
//              banner, and the "Manage in M16" navigation link.
// =============================================================================

const { test, expect } = require('@playwright/test');
const {
  startBinary,
  stopBinary,
  waitForServer,
  binaryExists,
  baseUrl,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { installAddonInstance } = require('./helpers/addon-setup');

const PORT = 18099;
const DB = '/tmp/e2e-m14.db';
let proc;

test.beforeAll(async ({ browser }) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow release binary not built');
  }
  proc = startBinary({ port: PORT, db: DB });
  await waitForServer(PORT);
  // The Bindings tab lives in an addon's detail view, so one addon instance must exist.
  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  const addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'M14 bindings E2E',
    permissions: ['ui'],
  });
  // The tab attributes aliases to an addon by name prefix, so an alias named
  // after the instance is what makes the heuristic banner appear.
  await page.evaluate(async (alias) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    await ApiBinary.action('modelAliasCreateRequest', { alias, targetModel: 'm14-target' });
  }, `${addonId}-chat`);
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

test.describe('M14 — Addon detail > Bindings', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });
    await page.goto(`${baseUrl(PORT)}/#/addons`);
    await page.locator('[data-addon-card]').first().click();
    await page.locator('tf-tab#bindings').click();
  });

  test('Bindings tab renders inside addon detail', async ({ page }) => {
    await expect(page.locator('.addon-bindings')).toBeVisible({ timeout: 10000 });
    await expect(page.locator('#bindings-refresh')).toBeVisible();
  });

  test('heuristic warning banner is rendered', async ({ page }) => {
    await expect(page.locator('.addon-bindings tf-chip[status="warn"]')).toBeVisible({ timeout: 10000 });
  });

  test('"Manage in M16" link navigates to Services aliases', async ({ page }) => {
    await page.locator('#bindings-open-m16').click();
    await expect(page).toHaveURL(/services/);
  });
});
