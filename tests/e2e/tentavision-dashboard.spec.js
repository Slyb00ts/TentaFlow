// =============================================================================
// File: tests/e2e/tentavision-dashboard.spec.js
// Description: E2E for the TentaVision Dashboard (overview) tab backed by the
//              per-addon SQLite database. Proves the dashboard renders the KPI
//              tile row, the latest-alarms section and the 24h activity heatmap
//              from REAL data: KPIs and the heatmap are computed from the cameras
//              and alarms tables. Captures /tmp/tv/dashboard.png (empty DB) and
//              /tmp/tv/dashboard-withdata.png (after seeding cameras + alarms)
//              for visual comparison against the m01-dashboard.html mockup.
// =============================================================================

const fs = require('fs');
const os = require('os');
const path = require('path');
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

const BASE_PORT = 18281;
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

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-dashboard-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Dashboard E2E',
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

async function fillField(page, fieldId, value) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 10000 });
  const input = field.locator('input, textarea').first();
  await input.click();
  await input.fill(value);
  await input.dispatchEvent('input');
  await page.waitForTimeout(150);
}

// Drives the 4-step "add camera" wizard for an RTSP source. Mirrors the cameras
// spec so the dashboard test creates real cameras through the addon's own flow.
async function addCamera(page, name, rtsp) {
  await clickButtonByText(page, 'Dodaj kamerę');
  const rtspCard = page.locator('.addon-app-shell .tf-radio-card-group__card', { hasText: 'RTSP' }).first();
  await expect(rtspCard).toBeVisible({ timeout: 10000 });
  await rtspCard.click();
  await page.waitForTimeout(150);
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'rtsp_url', rtsp);
  await clickButtonByText(page, 'Dalej');
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'name', name);
  await clickButtonByText(page, 'Zakończ');
  await expect(page.locator('.addon-app-shell').getByText(name).first())
    .toBeVisible({ timeout: 10000 });
}

// Per-addon SQLite database the host created for this instance.
function addonDbPath() {
  return path.join(
    os.homedir(), '.tentaflow', 'orgs', 'org-default', 'addons', addonId, 'data.db',
  );
}

// Reads the persisted camera ids so seeded alarms reference real cameras (the
// dashboard joins alarms→cameras for the latest-alarms list and the heatmap).
function cameraIds() {
  const out = execFileSync('sqlite3', [addonDbPath(), 'SELECT id FROM cameras ORDER BY name;'], { encoding: 'utf8' });
  return out.split('\n').map((s) => s.trim()).filter(Boolean);
}

// Seeds alarms directly into the addon DB across a spread of severities and
// hours within the last 24h so the KPI count, the latest-alarms cards and the
// heatmap all light up. There is no operator UI flow to raise alarms, so the
// DB is the seeding surface (the dashboard still READS them exactly as in prod).
function seedAlarms(ids) {
  const now = Math.floor(Date.now() / 1000);
  const rows = [
    [ids[0], 'critical', 'D2', 'podejrzenie agresji', now - 600],
    [ids[0], 'warning', 'D1', 'nieczytelna tablica ADR', now - 1800],
    [ids[1] || ids[0], 'warning', 'D3', 'pozostawiony bagaz > 90s', now - 3600],
    [ids[1] || ids[0], 'info', 'D6', 'pojazd w strefie zakazu', now - 7200],
    [ids[0], 'critical', 'D2', 'przekroczenie strefy', now - 10800],
    [ids[1] || ids[0], 'info', 'D5', 'detekcja ruchu', now - 14400],
    [ids[0], 'warning', 'D1', 'slaba czytelnosc', now - 21600],
  ];
  const stmts = rows.map((r, i) => {
    const esc = (s) => String(s).replace(/'/g, "''");
    return `INSERT INTO alarms (id, camera_id, severity, type, message, thumb_ref, ts, status) `
      + `VALUES ('alm-seed-${i}', '${esc(r[0])}', '${esc(r[1])}', '${esc(r[2])}', '${esc(r[3])}', '', ${r[4]}, 'new');`;
  });
  execFileSync('sqlite3', [addonDbPath(), stmts.join('\n')], { encoding: 'utf8' });
}

test.describe('TentaVision Dashboard — KPIs, alarms, heatmap from SQLite', () => {
  test('empty dashboard, then populated with real cameras + alarms', async ({ page, browser }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Empty DB: dashboard is the default/overview tab. KPI tiles show 0/0,
    // latest-alarms shows the empty state, heatmap shows its empty state. ---
    await expect(page.locator('.addon-app-shell tf-stat-card').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/dashboard.png`, fullPage: true });

    // KPI row must have the four tiles.
    expect(await page.locator('.addon-app-shell tf-stat-card').count()).toBeGreaterThanOrEqual(4);
    // No alarms yet -> latest-alarms empty state.
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });

    // --- Seed REAL data: two cameras via the wizard, alarms via the addon DB. ---
    await openTab(page, 'cameras');
    await addCamera(page, 'C-01 brama', 'rtsp://192.168.40.41:554/stream1');
    await addCamera(page, 'C-04 wjazd', 'rtsp://192.168.40.42:554/stream1');

    const ids = cameraIds();
    expect(ids.length).toBeGreaterThanOrEqual(2);
    seedAlarms(ids);

    // --- Reopen the dashboard so it re-reads the DB. Use a fresh context to
    // prove the numbers come from SQLite, not in-memory state. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);

    // KPI tiles present and the alarms KPI is now non-zero (7 seeded).
    await expect(page2.locator('.addon-app-shell tf-stat-card').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page2.locator('.addon-app-shell').getByText('7', { exact: false }).first())
      .toBeVisible({ timeout: 10000 });

    // Latest-alarms cards rendered (no empty-state in the alarms section now).
    await expect(page2.locator('.addon-app-shell').getByText('podejrzenie agresji').first())
      .toBeVisible({ timeout: 10000 });

    // Heatmap grid present with cells.
    await expect(page2.locator('.addon-app-shell tf-heatmap').first())
      .toBeVisible({ timeout: 10000 });
    const litCells = await page2.locator('.addon-app-shell tf-heatmap .tf-heatmap-cell[data-level]').count();
    expect(litCells).toBeGreaterThan(0);

    await page2.screenshot({ path: `${SHOT_DIR}/dashboard-withdata.png`, fullPage: true });
    await page2.locator('.addon-app-shell tf-heatmap').first().scrollIntoViewIfNeeded();
    await page2.waitForTimeout(200);
    await page2.locator('.addon-app-shell tf-heatmap').first()
      .screenshot({ path: `${SHOT_DIR}/dashboard-heatmap.png` });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
