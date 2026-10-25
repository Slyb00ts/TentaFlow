// =============================================================================
// File: tests/e2e/services-direct-http.spec.js
// Description: UI smoke after the QUIC-sidecar → direct-http migration. Confirms
//              the Services screen and the engine catalog still render and that
//              the deploy wizard opens — i.e. the manifest/transport changes did
//              not break the deploy surface. A real container deploy is verified
//              separately (heavy: image build + GPU), this guards the UI path.
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

const PORT = 18112;
const DB = '/tmp/e2e-direct-http.db';
let proc;

test.beforeAll(async () => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built — run cargo build');
  }
  proc = startBinary({ port: PORT, db: DB });
  await waitForServer(PORT);
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

test.describe('Services — direct-http migration smoke', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page, { port: PORT });
  });

  test('Services screen mounts', async ({ page }) => {
    await page.goto(`${baseUrl(PORT)}/#/services`);
    await page.waitForLoadState('networkidle');
    // The services screen renders a subheader (#services-sub) and a "new
    // service" entry point (#svc-new) regardless of how many services exist.
    const screen = page
      .locator('#services-sub, #svc-new')
      .or(page.getByText(/Us[łl]ugi|Services/i))
      .first();
    await expect(screen).toBeVisible({ timeout: 10000 });
  });

  // The catalog opens on a target picker; engines are listed only after a
  // target (here the local node) is chosen.
  async function openLocalCatalog(page) {
    await page.goto(`${baseUrl(PORT)}/#/catalog`);
    await page.locator('.target-card[data-target-kind="local"]').click();
    await expect(page.locator('#catalog-tabs')).toBeVisible({ timeout: 10000 });
  }

  test('engine catalog lists deployable engines', async ({ page }) => {
    await openLocalCatalog(page);
    // The catalog renders one tile per manifest engine (vllm, whisper, ...).
    // At least one deployable engine must surface — proof the manifests still
    // parse after the transport flip to direct-http.
    await expect(page.locator('.catalog-card[data-engine-id]').first()).toBeVisible({ timeout: 15000 });
  });

  test('deploy wizard opens for an engine', async ({ page }) => {
    await openLocalCatalog(page);
    // Managed-CLI coding agents route to the accounts screen instead of a
    // wizard (icon "users"), so open a real engine.
    const deploy = page.locator('tf-button[data-engine-deploy]:not([icon="users"])').first();
    await expect(deploy).toBeVisible({ timeout: 15000 });
    await deploy.click();
    // The deploy wizard (engine-deploy-wizard.js) renders into a tf-window.
    await expect(page.locator('tf-window').first()).toBeVisible({ timeout: 8000 });
  });
});
