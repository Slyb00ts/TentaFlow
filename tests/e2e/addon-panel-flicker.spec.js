// =============================================================================
// File: tests/e2e/addon-panel-flicker.spec.js
// Description: A bound control in an addon panel keeps the value the user gave
//              it. Drives the real TentaVision settings tab (its toggle handlers
//              update the addon's draft without echoing the bound key, so any
//              server push re-sends the old value) and samples the controls
//              every 100 ms after each change: a toggle or select that flips
//              back, even for one sample, fails the test.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { installAddonInstance, collectConsoleErrors, diagnostics } = require('./helpers/addon-setup');

const BASE_PORT = 18431;
const PERMISSIONS = ['ui', 'cameras.read', 'cameras.write', 'sql.read', 'sql.write'];
const SAMPLE_MS = 6000;

let PORT;
let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  proc = startBinary({ port: PORT, db: `/tmp/e2e-addon-flicker-${PORT}.db` });
  await waitForServer(PORT);
  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Flicker E2E',
    permissions: PERMISSIONS,
  });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

async function openSettings(page) {
  const navItem = page.locator(`.addon-app-nav-item[data-addon-id="${addonId}"]`);
  await expect(navItem).toBeVisible({ timeout: 10000 });
  await navItem.click();
  await expect(page.locator(`.addon-app-shell[data-addon="${addonId}"]`)).toBeVisible({ timeout: 10000 });
  const tab = page.locator('tf-tab#settings');
  await expect(tab).toBeVisible({ timeout: 10000 });
  await tab.evaluate((el) => el.scrollIntoView({ inline: 'center', block: 'nearest' }));
  await tab.click();
  await expect(page.locator('.addon-app-shell [data-component-id="set_notify_sms_enabled"]').first())
    .toBeVisible({ timeout: 10000 });
}

// Samples `read()` every 100 ms for SAMPLE_MS and returns every value seen.
async function sample(page, read) {
  const seen = [];
  const t0 = Date.now();
  while (Date.now() - t0 < SAMPLE_MS) {
    seen.push(await read());
    await page.waitForTimeout(100);
  }
  return seen;
}

test.describe('Addon panel — bound controls never bounce back', () => {
  test('toggles and a select keep the user value while the addon catches up', async ({ page }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openSettings(page);

    const toggles = ['set_notify_sms_enabled', 'set_notify_email_enabled', 'set_notify_webhook_enabled'];
    for (const key of toggles) {
      const sw = page.locator(`.addon-app-shell [data-component-id="${key}"] [role="switch"]`).first();
      if (!(await sw.count())) continue;
      await sw.scrollIntoViewIfNeeded();
      const before = await sw.getAttribute('aria-checked');
      const want = before === 'true' ? 'false' : 'true';
      await sw.click();
      // Several quick flips in a row are the worst case for a stale echo.
      await sw.click();
      await sw.click();
      const seen = await sample(page, () => sw.getAttribute('aria-checked'));
      expect(seen.filter((v) => v !== want), `${key}: ${seen.join(',')}`).toEqual([]);
    }

    const select = page.locator('.addon-app-shell [data-component-id="set_legal_profile"] select').first();
    await select.scrollIntoViewIfNeeded();
    await select.selectOption('tstr:public_transport');
    const seenSel = await sample(page, () => select.inputValue());
    expect(seenSel.filter((v) => v !== 'tstr:public_transport'), seenSel.join(',')).toEqual([]);

    expect(errors, diagnostics(errors, proc)).toEqual([]);
  });
});
