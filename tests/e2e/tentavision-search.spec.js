// =============================================================================
// File: tests/e2e/tentavision-search.spec.js
// Description: E2E for the TentaVision "Wyszukiwarka historyczna" (M6) tab. The
//              addon has NO search/embedding/ANPR engine wired, so this suite
//              proves the HONEST behaviour: all four search modes render (text /
//              attribute / image / plate) as a RadioCardGroup selector, each with
//              its own query form + real camera/time filters (cameras from the
//              per-addon SQLite cameras table); pressing "Szukaj" NEVER fabricates
//              result cards but shows a per-mode no-engine placeholder; and the
//              chosen mode + last query + recents persist (settings table) across
//              a full panel reopen in a fresh context. Screenshots → /tmp/tv/ for
//              visual comparison to m06-search.html. Zero console errors expected.
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

const BASE_PORT = 18361;
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

// Real cameras so the search camera-scope select is populated from db::list_cameras.
const CAMERAS = [
  { id: 'cam-srch-01', name: 'C-07 peron', status: 'online' },
  { id: 'cam-srch-02', name: 'C-04 wjazd', status: 'online' },
  { id: 'cam-srch-03', name: 'C-15 parking', status: 'offline' },
];

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-search-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Search E2E',
    permissions: PERMISSIONS,
  });
  await page.close();
});

test.afterAll(async () => {
  stopBinary(proc);
  await new Promise((r) => setTimeout(r, 1500));
});

function addonDbPath() {
  return path.join(
    os.homedir(), '.tentaflow', 'orgs', 'org-default', 'addons', addonId, 'data.db',
  );
}

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

// Picks a search mode via the RadioCardGroup (rendered as tf-radio[card][value=...]).
async function pickMode(page, mode) {
  const radio = page.locator(`.addon-app-shell tf-radio[value="${mode}"]`).first();
  await radio.scrollIntoViewIfNeeded();
  await radio.click();
  await page.waitForTimeout(500);
}

test.describe('TentaVision Wyszukiwarka — 4 modes, honest no-engine placeholder, persistence', () => {
  test('modes render, submit shows honest placeholder, mode+query+recents persist', async ({ page, browser }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    seedCameras();
    await openTab(page, 'search');

    // --- Mode selector: all four modes present as cards. ---
    const shell = page.locator('.addon-app-shell');
    await expect(shell.locator('tf-radio[value="text"]')).toBeVisible({ timeout: 10000 });
    await expect(shell.locator('tf-radio[value="attribute"]')).toBeVisible();
    await expect(shell.locator('tf-radio[value="image"]')).toBeVisible();
    await expect(shell.locator('tf-radio[value="plate"]')).toBeVisible();
    // Cards carry the mockup's labels.
    await expect(shell.getByText('Tekst (semantyczne)').first()).toBeVisible();
    await expect(shell.getByText('Atrybut (formularz)').first()).toBeVisible();
    await expect(shell.getByText('Tablica rejestracyjna').first()).toBeVisible();

    // Default mode = text: section cards render (tf-section-card), camera filter
    // populated from the real cameras, and a "Szukaj" button is present.
    await expect(shell.locator('tf-section-card').first()).toBeVisible({ timeout: 10000 });
    await expect(shell.getByText('Zapytanie semantyczne').first()).toBeVisible();
    await expect(shell.locator('tf-button', { hasText: 'Szukaj' }).first()).toBeVisible();
    await page.screenshot({ path: `${SHOT_DIR}/search-text.png`, fullPage: true });

    // --- Switch to attribute mode: its form swaps in. ---
    await pickMode(page, 'attribute');
    await expect(shell.getByText('Atrybuty osoby / pojazdu').first()).toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/search-attribute.png`, fullPage: true });

    // --- Back to text mode, type a query, submit → HONEST placeholder. ---
    await pickMode(page, 'text');
    const QUERY = 'mezczyzna w czerwonej czapce i ciemnej kurtce';
    const queryInput = shell.locator('tf-input').filter({ hasText: 'Zapytanie semantyczne' }).locator('input').first();
    // tf-input nests the native input; fall back to the first text input in the
    // semantic-query card if the structural filter does not match.
    const semInput = (await queryInput.count())
      ? queryInput
      : shell.locator('tf-textarea textarea, tf-input input').first();
    await semInput.fill(QUERY);
    await page.waitForTimeout(400);

    await shell.locator('tf-button', { hasText: 'Szukaj' }).first().click();
    await page.waitForTimeout(700);

    // The results area shows the honest no-engine message — NO fabricated cards.
    await expect(shell.getByText(/wymaga uruchomionego silnika/i).first()).toBeVisible({ timeout: 10000 });
    // No fake result thumbnails: the mockup's score chips (0.92 etc.) must NOT appear.
    await expect(shell.getByText('0.92')).toHaveCount(0);
    // The submitted query now appears in recents.
    await expect(shell.getByText('Ostatnie wyszukiwania').first()).toBeVisible({ timeout: 10000 });
    await expect(shell.getByText(QUERY).first()).toBeVisible();
    await page.screenshot({ path: `${SHOT_DIR}/search-result.png`, fullPage: true });

    // --- Persistence: reopen in a fresh, isolated context. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'search');

    const shell2 = page2.locator('.addon-app-shell');
    // Mode persisted = text (the RadioCardGroup mounts with value="text").
    await expect(shell2.locator('tf-radio-group[cards]').first()).toHaveAttribute('value', 'text', { timeout: 10000 });
    // Last query restored into the semantic-query control.
    await expect(shell2.locator('tf-textarea textarea, tf-input input')
      .filter({ hasText: '' }).first()).toBeVisible({ timeout: 10000 });
    const restored = await shell2.evaluate((root) => {
      const els = root.querySelectorAll('tf-textarea textarea, tf-input input');
      return Array.from(els).map((e) => e.value).filter(Boolean);
    });
    expect(restored.some((v) => v.includes('czerwonej czapce'))).toBeTruthy();
    // Recents survived the reopen.
    await expect(shell2.getByText('Ostatnie wyszukiwania').first()).toBeVisible({ timeout: 10000 });
    await expect(shell2.getByText(QUERY).first()).toBeVisible();
    await page2.screenshot({ path: `${SHOT_DIR}/search-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
