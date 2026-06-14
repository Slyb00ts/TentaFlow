// =============================================================================
// File: tests/e2e/tentavision-live.spec.js
// Description: E2E for the TentaVision "Live view" tab. Proves: empty state with
//              a "Dodaj kamerę" CTA before any camera exists, one tile per REAL
//              camera (from the per-addon SQLite cameras table) with a name + a
//              toned online/offline status chip, the 1/4/9/16 grid-size segmented
//              control, and that the chosen grid size persists across a full panel
//              reopen (fresh context) — it is written to the settings table.
//              ONLINE tiles render a real `<tf-video-stream>` (SDK VideoStream →
//              MSE over the BINARY `streamSubscribeRequest`; the dashboard
//              renderer also attaches the binary detection overlay). OFFLINE tiles
//              degrade honestly to a placeholder (no stream). Nothing touches REST:
//              the suite asserts ZERO console errors (no 401 stream spam). A manual
//              "Odśwież" button re-renders the grid. Screenshots to /tmp/tv/ for
//              visual comparison to m02-live-view.html.
// =============================================================================

const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { execFileSync } = require('child_process');
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
  'streams.subscribe',
  'sql.read',
  'sql.write',
];

const SHOT_DIR = '/tmp/tv';

// Four real cameras with mixed status so the grid shows both online (ok) and
// offline (err) status chips. Canonical `cam_<uuid v4>` ids so the online tiles'
// `camera:<id>` subscribe stream ids pass the camera ACL. Names mirror the
// mockup's tile labels.
const CAMERAS = [
  { id: `cam_${crypto.randomUUID()}`, name: 'C-01 brama wjazdowa', status: 'online' },
  { id: `cam_${crypto.randomUUID()}`, name: 'C-04 wjazd-2', status: 'online' },
  { id: `cam_${crypto.randomUUID()}`, name: 'C-07 peron', status: 'online' },
  { id: `cam_${crypto.randomUUID()}`, name: 'C-12 magazyn', status: 'offline' },
];

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-live-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Live E2E',
    permissions: PERMISSIONS,
  });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

// Per-addon SQLite database the host created for this instance.
function addonDbPath() {
  return path.join(
    os.homedir(), '.tentaflow', 'orgs', 'org-default', 'addons', addonId, 'data.db',
  );
}

// Seeds the cameras directly into the addon DB. The Live tab READS them through
// db::list_cameras exactly as in prod; the wizard is exercised by the cameras
// spec, so here we seed the table directly to get a deterministic 4-up grid.
function seedCameras() {
  const now = Math.floor(Date.now() / 1000);
  const esc = (s) => String(s).replace(/'/g, "''");
  const stmts = CAMERAS.map((c, i) =>
    `INSERT INTO cameras (id, name, location, rtsp_url, onvif_url, status, fps, detectors, created_at, updated_at) `
    + `VALUES ('${esc(c.id)}', '${esc(c.name)}', 'Strefa ${i + 1}', `
    + `'rtsp://192.168.40.${41 + i}:554/stream1', '', '${esc(c.status)}', 5, 'D1', ${now}, ${now});`);
  execFileSync('sqlite3', [addonDbPath(), stmts.join('\n')], { encoding: 'utf8' });
}

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

test.describe('TentaVision Live view — real camera grid + grid-size persistence', () => {
  test('empty state CTA, real tiles, grid-size persists', async ({ page, browser }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Live tab BEFORE any camera: empty state with a CTA to add a camera. ---
    await openTab(page, 'live');
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell tf-empty-state tf-button', { hasText: 'Dodaj kamerę' }).first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/live-empty.png`, fullPage: true });

    // --- Seed 4 real cameras, then reopen the tab so the grid renders tiles. ---
    seedCameras();
    await openTab(page, 'overview');
    await openTab(page, 'live');

    // Default layout is 4-up: one tile per camera, each with name + status chip.
    for (const c of CAMERAS) {
      await expect(page.locator('.addon-app-shell').getByText(c.name).first())
        .toBeVisible({ timeout: 10000 });
    }
    // Online (ok) and offline (err) status chips both present.
    await expect(page.locator('.addon-app-shell').getByText('online', { exact: true }).first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('offline', { exact: true }).first()).toBeVisible();
    // The segmented grid-size control is present.
    await expect(page.locator('.addon-app-shell tf-segmented').first()).toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/live-grid.png`, fullPage: true });

    // Video-tile path: every ONLINE tile renders a real <tf-video-stream> bound
    // to its `camera:<id>` subscribe stream id (MSE over the binary protocol).
    // The OFFLINE camera degrades honestly to a placeholder (no stream).
    await expect(page.locator('.addon-app-shell tf-video-stream').first())
      .toBeVisible({ timeout: 10000 });
    const onlineCount = CAMERAS.filter((c) => c.status === 'online').length;
    await expect(page.locator('.addon-app-shell tf-video-stream')).toHaveCount(onlineCount, { timeout: 10000 });
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });

    // The manual "Odśwież" button re-renders the grid; it must not emit console
    // errors (and never opens a REST route — the rewrite is binary-only).
    const refreshBtn = page.locator('.addon-app-shell tf-button', { hasText: 'Odśwież' }).first();
    await expect(refreshBtn).toBeVisible({ timeout: 10000 });
    await refreshBtn.click();
    await page.waitForTimeout(600);
    await expect(page.locator('.addon-app-shell tf-video-stream').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/live-real.png`, fullPage: true });

    // --- Change grid size to 9 via the segmented control. tf-segmented replaces
    // its <option> children with .tf-seg-opt buttons on build, so click those. ---
    const seg = page.locator('.addon-app-shell tf-segmented').first();
    await seg.scrollIntoViewIfNeeded();
    await seg.locator('.tf-seg-opt[data-value="9"]').first().click();
    await page.waitForTimeout(600);
    await expect(seg).toHaveAttribute('value', '9', { timeout: 10000 });

    // --- Persistence: reopen in a fresh, isolated context. The grid size 9 must
    // survive (served from the settings table). ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'live');

    // The segmented control mounts on the persisted value 9.
    const seg2 = page2.locator('.addon-app-shell tf-segmented').first();
    await expect(seg2).toBeVisible({ timeout: 10000 });
    await expect(seg2).toHaveAttribute('value', '9', { timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/live-persist.png`, fullPage: true });

    // No live-stream WebSocket is opened (honest placeholder), so there must be
    // zero console errors — in particular no 401s.
    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
