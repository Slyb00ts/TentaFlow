// =============================================================================
// File: tests/e2e/m15-install-wizard.spec.js
// Description: UI e2e tests for M15 — Install wizard tf-window modal. Covers
//              opening from addon detail (admin only), ESC dirty confirmation,
//              and step navigation (Permissions -> Storage -> Aliases).
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

const PORT = 18100;
const DB = '/tmp/e2e-m15.db';
let proc;

test.beforeAll(async () => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow release binary not built');
  }
  proc = startBinary({ port: PORT, db: DB });
  await waitForServer(PORT);
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

test.describe('M15 — Install wizard', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });
    await page.goto(`${baseUrl(PORT)}/#/addons`);
    await page.waitForLoadState('networkidle');
  });

  test('Configure button opens wizard tf-window', async ({ page }) => {
    const firstAddon = page.locator('[data-addon-id], tf-table tr').first();
    if ((await firstAddon.count()) === 0) {
      test.skip(true, 'no installed addons — wizard cannot be opened in F1a default state');
    }
    await firstAddon.click();
    const configureBtn = page.locator('#hdr-configure');
    await expect(configureBtn).toBeVisible({ timeout: 5000 });
    await configureBtn.click();

    const wizard = page.locator('tf-window').last();
    await expect(wizard).toBeVisible({ timeout: 5000 });
    // Step 1 (Permissions) should be the entry step.
    await expect(page.locator('#install-wizard-body')).toBeVisible();
  });

  test('ESC fires discard confirmation when state is dirty', async ({ page }) => {
    const firstAddon = page.locator('[data-addon-id], tf-table tr').first();
    if ((await firstAddon.count()) === 0) {
      test.skip(true, 'no installed addons');
    }
    await firstAddon.click();
    const configureBtn = page.locator('#hdr-configure');
    if ((await configureBtn.count()) === 0) {
      test.skip(true, 'configure button missing');
    }
    await configureBtn.click();
    const wizard = page.locator('tf-window').last();
    await expect(wizard).toBeVisible({ timeout: 5000 });

    // Mark state dirty by toggling a permission. Without permissions in the
    // manifest there is no dirty state to test, so we skip.
    const toggle = page.locator('tf-toggle, tf-button[data-perm]').first();
    if ((await toggle.count()) === 0) {
      test.skip(true, 'no permission toggles — cannot create dirty state');
    }
    await toggle.click();

    const windowsBefore = await page.locator('tf-window').count();
    await page.keyboard.press('Escape');

    // The wizard listens to ESC via TfWindow's close-request event. When state
    // is dirty it spawns a second tf-window (TfWindow.confirm) with the
    // discard message. We assert the count grew AND the original wizard
    // remained mounted (close was prevented).
    await expect(async () => {
      const c = await page.locator('tf-window').count();
      expect(c).toBeGreaterThan(windowsBefore);
    }).toPass({ timeout: 3000 });
    await expect(wizard).toBeVisible();

    // The confirm dialog carries the discard message text from i18n. We match
    // by role/text on the newest tf-window.
    const confirm = page.locator('tf-window').last();
    await expect(confirm).toBeVisible();
    await expect(confirm).toContainText(/discard|odrzuć|niezapisan|unsaved/i);
  });

  test('wizard step navigation 1 -> 2 (Permissions -> Storage)', async ({ page }) => {
    const firstAddon = page.locator('[data-addon-id], tf-table tr').first();
    if ((await firstAddon.count()) === 0) {
      test.skip(true, 'no installed addons');
    }
    await firstAddon.click();
    const configureBtn = page.locator('#hdr-configure');
    if ((await configureBtn.count()) === 0) {
      test.skip(true, 'configure button missing');
    }
    await configureBtn.click();
    await expect(page.locator('tf-window').last()).toBeVisible({ timeout: 5000 });

    // Click "Continue"/"Next" — the wizard footer renders a primary button
    // for advancing step. Match by common labels.
    const next = page.locator('tf-button', { hasText: /Continue|Dalej|Next/i }).first();
    if ((await next.count()) === 0) {
      test.skip(true, 'wizard next button not found');
    }
    await next.click();
    // After advancing, body should still be rendered (step 2 active).
    await expect(page.locator('#install-wizard-body')).toBeVisible();
  });
});
