// =============================================================================
// File: tests/e2e/tentavision-search.spec.js
// Description: E2E for the TentaVision "Wyszukiwarka historyczna" (M6) tab. Two
//              modes are now REAL: attribute mode runs a structured SQL query
//              over the per-addon alarms table (no AI) and renders real matching
//              rows; text mode embeds the query via llm_generate and runs a k-NN
//              over the `events` vector namespace. When no embedding model is
//              deployed in the test env, text mode shows the HONEST
//              model-unavailable message (never the old "brak backendu", never
//              fabricated hits). Image/plate keep an honest vision-pipeline
//              placeholder. The "Reindeksuj zdarzenia" action backfills
//              embeddings. Mode/query/recents still persist across a reopen.
//              Screenshots → /tmp/tv/. Zero console errors expected.
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
  'vector.read',
  'vector.write',
  'llm.generate',
];

const SHOT_DIR = '/tmp/tv';

// Real cameras so the search camera-scope select is populated from db::list_cameras.
const CAMERAS = [
  { id: 'cam-srch-01', name: 'C-07 peron', status: 'online' },
  { id: 'cam-srch-02', name: 'C-04 wjazd', status: 'online' },
  { id: 'cam-srch-03', name: 'C-15 parking', status: 'offline' },
];

// Real alarms so attribute (SQL) search returns real rows.
const ALARMS = [
  { id: 'alm-s-1', camera: 'cam-srch-02', severity: 'critical', type: 'agresja', message: 'podejrzenie agresji przy wjezdzie' },
  { id: 'alm-s-2', camera: 'cam-srch-01', severity: 'warning', type: 'ADR', message: 'nieczytelna tablica ADR na peronie' },
  { id: 'alm-s-3', camera: 'cam-srch-03', severity: 'info', type: 'pojazd', message: 'pojazd w strefie zakazu na parkingu' },
  { id: 'alm-s-4', camera: 'cam-srch-02', severity: 'critical', type: 'agresja', message: 'bojka grupy osob przy wjezdzie' },
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

function seedData() {
  const now = Math.floor(Date.now() / 1000);
  const esc = (s) => String(s).replace(/'/g, "''");
  const camStmts = CAMERAS.map((c, i) =>
    `INSERT INTO cameras (id, name, location, rtsp_url, onvif_url, status, fps, detectors, created_at, updated_at) `
    + `VALUES ('${esc(c.id)}', '${esc(c.name)}', 'Strefa ${i + 1}', `
    + `'rtsp://192.168.40.${41 + i}:554/stream1', '', '${esc(c.status)}', 5, 'D1', ${now}, ${now});`);
  const almStmts = ALARMS.map((a, i) =>
    `INSERT INTO alarms (id, camera_id, severity, type, message, thumb_ref, ts, status) `
    + `VALUES ('${esc(a.id)}', '${esc(a.camera)}', '${esc(a.severity)}', '${esc(a.type)}', `
    + `'${esc(a.message)}', '', ${now - i * 60}, 'new');`);
  execFileSync('sqlite3', [addonDbPath(), [...camStmts, ...almStmts].join('\n')], { encoding: 'utf8' });
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

test.describe('TentaVision Wyszukiwarka — REAL attribute SQL + REAL/honest semantic, persistence', () => {
  test('attribute search returns real rows; text search is real or honest; recents persist', async ({ page, browser }) => {
    test.setTimeout(180000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    seedData();
    await openTab(page, 'search');

    // --- Mode selector: all four modes present as cards. ---
    const shell = page.locator('.addon-app-shell');
    await expect(shell.locator('tf-radio[value="text"]')).toBeVisible({ timeout: 10000 });
    await expect(shell.locator('tf-radio[value="attribute"]')).toBeVisible();
    await expect(shell.locator('tf-radio[value="image"]')).toBeVisible();
    await expect(shell.locator('tf-radio[value="plate"]')).toBeVisible();
    await expect(shell.getByText('Tekst (semantyczne)').first()).toBeVisible();
    await expect(shell.getByText('Atrybut (formularz)').first()).toBeVisible();
    await expect(shell.getByText('Tablica rejestracyjna').first()).toBeVisible();

    // Default mode = text: section cards render, "Szukaj" present.
    await expect(shell.locator('tf-section-card').first()).toBeVisible({ timeout: 10000 });
    await expect(shell.getByText('Zapytanie semantyczne').first()).toBeVisible();
    await expect(shell.locator('tf-button', { hasText: 'Szukaj' }).first()).toBeVisible();

    // ===== ATTRIBUTE MODE: REAL SQL search over alarms. =====
    await pickMode(page, 'attribute');
    await expect(shell.getByText('Atrybuty zdarzenia').first()).toBeVisible({ timeout: 10000 });

    // Type "agresja" into the structured type field and submit → real rows.
    const typeInput = shell.locator('tf-input input').first();
    await typeInput.fill('agresja');
    await page.waitForTimeout(400);
    await shell.locator('tf-button', { hasText: 'Szukaj' }).first().click();
    await page.waitForTimeout(800);

    // Two "agresja" alarms exist → real result cards with their messages.
    await expect(shell.getByText(/Znaleziono \d+ zdarzeń/).first()).toBeVisible({ timeout: 10000 });
    await expect(shell.getByText('podejrzenie agresji przy wjezdzie').first()).toBeVisible();
    await expect(shell.getByText('bojka grupy osob przy wjezdzie').first()).toBeVisible();
    // The non-matching info alarm must NOT appear.
    await expect(shell.getByText('pojazd w strefie zakazu na parkingu')).toHaveCount(0);
    await page.screenshot({ path: `${SHOT_DIR}/search-attribute-real.png`, fullPage: true });

    // ===== TEXT MODE: embed query → vector k-NN, or honest model-unavailable. =====
    await pickMode(page, 'text');
    const QUERY = 'agresja przy wjezdzie';
    const semInput = shell.locator('tf-textarea textarea, tf-input input').first();
    await semInput.fill(QUERY);
    await page.waitForTimeout(400);

    // Reindex first so any deployed embedding model has the alarms indexed.
    await shell.locator('tf-button', { hasText: 'Reindeksuj zdarzenia' }).first().click();
    await page.waitForTimeout(1500);

    await shell.locator('tf-button', { hasText: 'Szukaj' }).first().click();
    await page.waitForTimeout(1200);

    // The outcome is EITHER real semantic hits (model deployed) OR the honest
    // model-unavailable message — never the old "brak backendu", never fake hits.
    const bodyText = await shell.innerText();
    const modelUnavailable = /Model embedding[oó]w niedost[eę]pny/i.test(bodyText);
    const realHits = /Znaleziono \d+ zdarzeń/.test(bodyText) || /podobie[nń]stwo \d+%/.test(bodyText);
    expect(modelUnavailable || realHits, `text-search outcome not recognized:\n${bodyText.slice(0, 600)}`).toBeTruthy();
    // The legacy blanket placeholder must be gone.
    expect(/brak backendu/i.test(bodyText)).toBeFalsy();
    // The submitted query appears in recents.
    await expect(shell.getByText('Ostatnie wyszukiwania').first()).toBeVisible({ timeout: 10000 });
    await expect(shell.getByText(QUERY).first()).toBeVisible();
    await page.screenshot({ path: `${SHOT_DIR}/search-text-real.png`, fullPage: true });

    // --- Persistence: reopen in a fresh, isolated context. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'search');

    const shell2 = page2.locator('.addon-app-shell');
    await expect(shell2.locator('tf-radio-group[cards]').first()).toHaveAttribute('value', 'text', { timeout: 10000 });
    const restored = await shell2.evaluate((root) => {
      const els = root.querySelectorAll('tf-textarea textarea, tf-input input');
      return Array.from(els).map((e) => e.value).filter(Boolean);
    });
    expect(restored.some((v) => v.includes('agresja'))).toBeTruthy();
    await expect(shell2.getByText('Ostatnie wyszukiwania').first()).toBeVisible({ timeout: 10000 });
    await expect(shell2.getByText(QUERY).first()).toBeVisible();
    await page2.screenshot({ path: `${SHOT_DIR}/search-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
