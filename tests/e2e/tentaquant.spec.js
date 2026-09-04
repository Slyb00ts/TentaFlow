// =============================================================================
// File: tests/e2e/tentaquant.spec.js
// Description: End-to-end suite for the TentaQuant screens Q01–Q05. Boots an
//              isolated tentaflow instance (own port, sqlite db and
//              TENTAFLOW_HOME) and drives the UI the way an administrator
//              would: the laboratory list with nothing installed, installing a
//              laboratory from the Addons catalog, entering it (the single-lab
//              route rule of plan §19.8), the dashboard counters, creating a
//              project and opening its share window. Every scenario is checked
//              at a desktop and at a 390 px viewport, where nothing may scroll
//              sideways.
//
//              TentaQuant is the first MULTI-INSTANCE native app, so the suite
//              installs its own instance instead of seeding one: the instance
//              row, its database and its permission matrix are created by the
//              same path a user takes, which is the only way the route's
//              `?instance=` contract is exercised end to end.
// =============================================================================

const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { startBinary, stopBinary, waitForServer, binaryExists } = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');

const PORT = 18311;
const WORK_DIR = path.join(os.tmpdir(), `tentaflow-e2e-tentaquant-${PORT}`);
const DB = path.join(WORK_DIR, 'tentaquant.db');
const HOME = path.join(WORK_DIR, 'home');
const WWW_DIR = path.join(__dirname, '../../tentaflow-core/www');

const LAB_NAME = 'Kwanty R&D';
const PROJECT_NAME = 'Grover 4-kubitowy';

let server = null;

test.beforeAll(async () => {
  test.skip(!binaryExists(), 'tentaflow binary not built (target_shared/{release-fast,release,debug})');
  fs.rmSync(WORK_DIR, { recursive: true, force: true });
  fs.mkdirSync(WORK_DIR, { recursive: true });
  server = startBinary({ port: PORT, db: DB, home: HOME, env: { TENTAFLOW_WWW_DIR: WWW_DIR } });
  await waitForServer(PORT, 60000);
});

test.afterAll(async () => {
  if (!server) return;
  const exited = new Promise((resolve) => server.once('exit', resolve));
  stopBinary(server);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 10000))]);
  server = null;
});

// ---------------------------------------------------------------------------
// Page helpers
// ---------------------------------------------------------------------------

// Collects console errors + uncaught exceptions; a test fails on any of them.
function trackErrors(page) {
  const errors = [];
  page.on('console', (m) => { if (m.type() === 'error') errors.push(`[console] ${m.text().slice(0, 400)}`); });
  page.on('pageerror', (e) => errors.push(`[pageerror] ${e.message.slice(0, 400)}`));
  return errors;
}

async function open(page) {
  // The frontend is served from disk and its asset hash differs from the build
  // embedded in the binary, which raises the "new version" overlay — irrelevant
  // here, so it is hidden before the app boots.
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const st = document.createElement('style');
      st.textContent = '.update-overlay{display:none!important}';
      document.head.appendChild(st);
    });
  });
  await loginAsAdmin(page, { port: PORT });
}

async function gotoTentaQuant(page) {
  await page.goto(`https://127.0.0.1:${PORT}/#/tentaquant`);
  await expect(page.locator('#tq-root')).toBeVisible();
}

// The whole point of the responsive pass: no screen may scroll sideways.
async function expectNoHorizontalOverflow(page) {
  const overflow = await page.evaluate(() => ({
    doc: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    main: (() => {
      const m = document.getElementById('main');
      return m ? m.scrollWidth - m.clientWidth : 0;
    })(),
  }));
  expect(overflow.doc, 'document does not scroll sideways').toBeLessThanOrEqual(1);
  expect(overflow.main, '#main does not scroll sideways').toBeLessThanOrEqual(1);
}

// Installs one TentaQuant instance through Addons → Katalog, which is exactly
// what the "+ Nowe laboratorium" tile points at.
async function installLaboratory(page, name) {
  await page.goto(`https://127.0.0.1:${PORT}/#/addons`);
  await page.locator('.addons-view-switch [data-view="catalog"]').click();
  const card = page.locator('[data-catalog-card="tentaquant"]');
  await expect(card).toBeVisible({ timeout: 30000 });
  await card.locator('[data-act="install"]').click();

  const win = page.locator('tf-window').last();
  await expect(win).toBeVisible();
  await win.locator('#inst-name input').fill(name);
  await win.locator('[data-action="confirm"]').click();
  await expect(win).toHaveCount(0, { timeout: 60000 });
}

// ---------------------------------------------------------------------------
// Q01 — the laboratory list
// ---------------------------------------------------------------------------

test.describe.serial('TentaQuant', () => {
  test('Q01 shows the empty laboratory list with the install tile and the membership hint', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);

    await expect(page.locator('.tq-page-title')).toHaveText('TentaQuant');
    // Nothing is installed yet: no tile but the one that creates a laboratory.
    await expect(page.locator('.q-card[data-lab]')).toHaveCount(0);
    await expect(page.locator('.card-new[data-new-lab]')).toHaveCount(1);
    await expect(page.locator('tf-empty-state')).toBeVisible();
    // Membership comes from the instance matrix in Addons — the screen says so.
    await expect(page.locator('#tq-root tf-alert')).toHaveAttribute('message', /quant\.read/);
    await expect(page.locator('.tq-table-footer')).toContainText('0');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q01 at 390 px keeps every tile in one column and never scrolls sideways', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await expectNoHorizontalOverflow(page);
  });

  // -------------------------------------------------------------------------
  // Q02 — one laboratory is entered directly (plan §19.8)
  // -------------------------------------------------------------------------

  test('installing one laboratory makes the route open it instead of the list', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await installLaboratory(page, LAB_NAME);
    await gotoTentaQuant(page);

    // With exactly one laboratory there is no choice to make: the screen goes
    // straight in and the route names the instance it opened.
    await expect(page.locator('.tf-detail-header .d-name')).toContainText(LAB_NAME);
    await expect(page).toHaveURL(/#\/tentaquant\?instance=tentaquant-/);
    await expect(page.locator('.tf-detail-header .d-sub')).toContainText('tentaquant · instancja');

    // Only the tiers with a backend are claimed.
    await expect(page.locator('.tf-detail-header .tier')).toHaveCount(2);
    await expect(page.locator('.tf-detail-header .tier.t0')).toContainText('T0');
    await expect(page.locator('.tf-detail-header .tier.t1')).toContainText('T1');

    // Only the tabs whose screens exist.
    await expect(page.locator('#tq-tabs tf-tab')).toHaveCount(2);
    await expect(page.locator('#tq-tabs tf-tab#dashboard')).toBeVisible();
    await expect(page.locator('#tq-tabs tf-tab#projects')).toBeVisible();

    // Four KPI cards, all of them numbers LabOverview returns.
    await expect(page.locator('.tq-kpi tf-stat-card')).toHaveCount(4);
    await expect(page.locator('.tq-kpi tf-stat-card').first()).toHaveAttribute('value', '0');
    // "Zacznij od" offers exactly the one action that exists today.
    await expect(page.locator('.start-card')).toHaveCount(1);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('the breadcrumb goes back to the laboratory list', async ({ page }) => {
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('.tq-crumbs a').first().click();
    await expect(page.locator('.tq-page-title')).toHaveText('TentaQuant');
    await expect(page.locator('.q-card[data-lab]')).toHaveCount(1);
    await expect(page.locator('.q-card[data-lab]')).toContainText(LAB_NAME);
    // A laboratory only the installer can enter reads as "tylko Ty".
    await expect(page.locator('.q-card[data-lab] .qc-owner')).toContainText('tylko Ty');
  });

  // -------------------------------------------------------------------------
  // Q03 + Q04 — projects and the new-project window
  // -------------------------------------------------------------------------

  test('Q04 creates a private project that lands in "Moje projekty"', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);

    await page.locator('.tf-detail-header [data-act="new-project"]').click();
    const win = page.locator('tf-window.tq-modal');
    await expect(win).toBeVisible();
    // Only "pusty" is offered: examples and templates do not exist yet.
    await expect(win.locator('tf-choice-group').first().locator('tf-choice-card')).toHaveCount(1);
    // Private is the default.
    await expect(win.locator('#tq-np-visibility')).toHaveAttribute('value', 'private');

    await win.locator('#tq-np-name input').fill(PROJECT_NAME);
    await win.locator('[data-act="create"]').click();
    await expect(win).toHaveCount(0, { timeout: 30000 });

    await page.locator('#tq-tabs tf-tab#projects').click();
    const card = page.locator('.q-card[data-project]');
    await expect(card).toHaveCount(1);
    await expect(card).toContainText(PROJECT_NAME);
    await expect(card.locator('.qc-owner')).toContainText('Właściciel: Ty');
    await expect(card.locator('.qc-owner')).toContainText('prywatny');

    // Three sections, each with its head, and the footer summary under them.
    await expect(page.locator('.tq-section-head')).toHaveCount(3);
    await expect(page.locator('.tq-table-footer')).toContainText('1 projekt');

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q03 at 390 px stacks the sections without a sideways scroll', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#projects').click();
    await expect(page.locator('.q-card[data-project]')).toHaveCount(1);
    await expectNoHorizontalOverflow(page);
  });

  // -------------------------------------------------------------------------
  // Q05 — sharing
  // -------------------------------------------------------------------------

  test('Q05 shows the owner, the role legend and the laboratory toggle', async ({ page }) => {
    const errors = trackErrors(page);
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#projects').click();
    await page.locator('.q-card[data-project] [data-share]').click();

    const win = page.locator('tf-window.tq-share');
    await expect(win).toBeVisible();
    // The owner row is first and cannot be removed.
    const rows = win.locator('.tq-share-table tbody tr');
    await expect(rows).toHaveCount(1);
    await expect(rows.first()).toContainText('nie można usunąć');
    // All three project roles are explained.
    await expect(win.locator('.role-legend .rl')).toHaveCount(3);
    await expect(win.locator('.role-legend')).toContainText('Przeglądający');
    // A viewer computes only in the browser, without saving results.
    await expect(win.locator('.role-legend')).toContainText('T0');
    // Sharing never grants laboratory access.
    await expect(win.locator('tf-alert').last()).toHaveAttribute('message', /Addons/);
    // The lab-wide toggle exists and reflects the project's visibility.
    await expect(win.locator('#tq-share-lab')).toBeVisible();
    await expect(win.locator('#tq-share-lab')).not.toHaveAttribute('checked', '');
    // The person picker searches every TentaFlow account, so it belongs to
    // whoever may share and is there for an ordinary owner too.
    await expect(win.locator('#tq-share-search')).toBeVisible();

    await win.locator('[data-act="close"]').click();
    await expect(win).toHaveCount(0);

    expect(errors, errors.join('\n')).toEqual([]);
  });

  test('Q05 at 390 px keeps the share table inside its own scroller', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 820 });
    await open(page);
    await gotoTentaQuant(page);
    await page.locator('#tq-tabs tf-tab#projects').click();
    await page.locator('.q-card[data-project] [data-share]').click();
    await expect(page.locator('tf-window.tq-share')).toBeVisible();
    await expectNoHorizontalOverflow(page);
  });

  // -------------------------------------------------------------------------
  // A second laboratory brings the list back (plan §19.8, the other branch)
  // -------------------------------------------------------------------------

  test('with two laboratories the route opens the list again', async ({ page }) => {
    await open(page);
    await installLaboratory(page, 'Sandbox lokalny');
    await gotoTentaQuant(page);

    await expect(page.locator('.tq-page-title')).toHaveText('TentaQuant');
    await expect(page.locator('.q-card[data-lab]')).toHaveCount(2);
    await expect(page.locator('.tq-table-footer')).toContainText('2 laboratoria');

    // The list view renders the same rows through tf-table.
    await page.locator('#tq-lab-view option[value="list"]').click();
    await expect(page.locator('#tq-lab-table tbody tr')).toHaveCount(2);
  });
});
