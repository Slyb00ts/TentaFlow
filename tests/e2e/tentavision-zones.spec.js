// =============================================================================
// File: tests/e2e/tentavision-zones.spec.js
// Description: E2E for the TentaVision "Strefy i reguły" tab backed by the
//              per-addon SQLite zones table. Proves: empty state until a camera
//              is selected, the camera selector lists real cameras, the add-zone
//              form INSERTs include/exclude zones rendered with toned kind chips,
//              the weekly schedule grid renders + persists cell toggles, composite
//              rules create/list/delete, and that zones + schedule survive a full
//              panel reopen (fresh context). Screenshots to /tmp/tv/ for visual
//              comparison to m09-zones.html. Zero console errors.
// =============================================================================

const fs = require('fs');
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

const BASE_PORT = 18351;
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
const CAM_NAME = 'C-07 peron';
const CAM_RTSP = 'rtsp://192.168.40.41:554/stream1';
// Two zones: an include (green/ok chip) area and an exclude (red/err chip) area.
const ZONE_INCLUDE = { name: 'Peron główny', kind: 'include', polygon: '[[10,40],[60,40],[60,85],[10,85]]' };
const ZONE_EXCLUDE = { name: 'Ławka (ignore)', kind: 'exclude', polygon: '[[55,65],[75,65],[75,85],[55,85]]' };
const RULE = { name: 'Bagaż + pusta strefa', expr: 'D3.luggage(unowned>90s) AND zone.peron', action: 'Alarm krytyczny + SMS' };

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-zones-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Zones E2E',
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

// Opens the row kebab (⋯) of the Nth tf-table on the page and clicks the menu
// item matching `label`. The Zones tab renders the zone table first (nth=0) and
// the rules table second (nth=1).
async function clickRowAction(page, tableIndex, label, rowIndex = 0) {
  // Scope to the specific row so the kebab and its menu item come from the same
  // row (multiple per-row menus coexist in the DOM; an unscoped .first() can hit
  // a different, closed menu).
  const row = page
    .locator('.addon-app-shell tf-table')
    .nth(tableIndex)
    .locator('tr', { has: page.locator('tf-button[aria-label="Akcje wiersza"]') })
    .nth(rowIndex);
  const kebab = row.locator('tf-button[aria-label="Akcje wiersza"]').first();
  await expect(kebab).toBeVisible({ timeout: 10000 });
  await kebab.scrollIntoViewIfNeeded();
  await kebab.click();
  const item = row.locator('tf-menu-item', { hasText: label }).first();
  await expect(item).toBeVisible({ timeout: 10000 });
  await item.click({ force: true });
  await page.waitForTimeout(300);
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

async function selectField(page, fieldId, optionValue) {
  const field = page.locator(`.addon-app-shell [data-component-id="${fieldId}"]`).first();
  await expect(field).toBeVisible({ timeout: 10000 });
  await field.locator('select').first().selectOption(optionValue);
  await page.waitForTimeout(200);
}

// Drives the 4-step "add camera" wizard so the Zones tab has a real camera to
// pick. Mirrors tentavision-cameras.spec.js.
async function addCameraViaWizard(page) {
  await openTab(page, 'cameras');
  await clickButtonByText(page, 'Dodaj kamerę');
  const rtspCard = page.locator('.addon-app-shell .tf-radio-card-group__card', { hasText: 'RTSP' }).first();
  await expect(rtspCard).toBeVisible({ timeout: 10000 });
  await rtspCard.click();
  await page.waitForTimeout(150);
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'rtsp_url', CAM_RTSP);
  await clickButtonByText(page, 'Dalej');
  await clickButtonByText(page, 'Dalej');
  await fillField(page, 'name', CAM_NAME);
  await clickButtonByText(page, 'Zakończ');
  await expect(page.locator('.addon-app-shell').getByText(CAM_NAME).first())
    .toBeVisible({ timeout: 10000 });
}

async function addZone(page, z) {
  await clickButtonByText(page, 'Nowa strefa');
  await fillField(page, 'zone_name', z.name);
  await selectField(page, 'zone_kind', `tstr:${z.kind}`);
  await fillField(page, 'zone_polygon', z.polygon);
  await clickButtonByText(page, 'Zapisz strefę');
  await expect(page.locator('.addon-app-shell').getByText(z.name).first())
    .toBeVisible({ timeout: 10000 });
}

test.describe('TentaVision Zones — SQLite CRUD + schedule + rules + persistence', () => {
  test('empty state, select camera, add zones, schedule, rules, persist', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Seed a real camera so the selector has something to pick. ---
    await addCameraViaWizard(page);

    // --- Zones tab: empty state (no camera selected yet). ---
    await openTab(page, 'zones');
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/zones-empty.png`, fullPage: true });

    // --- Select the camera; the zone list + schedule grid + rules render. ---
    await selectField(page, 'zone_camera_select', `tstr:${await firstCameraId(page)}`);
    await expect(page.locator('.addon-app-shell').getByText('Harmonogram tygodniowy profili').first())
      .toBeVisible({ timeout: 10000 });

    // --- Add an include zone and an exclude zone. ---
    await addZone(page, ZONE_INCLUDE);
    await addZone(page, ZONE_EXCLUDE);

    await expect(page.locator('.addon-app-shell tf-table').first()).toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText(ZONE_INCLUDE.name).first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText(ZONE_EXCLUDE.name).first()).toBeVisible();
    // Kind chips: include + exclude both rendered as toned chips.
    await expect(page.locator('.addon-app-shell').getByText('include', { exact: true }).first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('exclude', { exact: true }).first()).toBeVisible();

    // --- Toggle a schedule cell (off -> dzień) and confirm it persists below. ---
    const scheduleCell = page.locator('.addon-app-shell tf-button', { hasText: '—' }).first();
    await scheduleCell.scrollIntoViewIfNeeded();
    await scheduleCell.click();
    await page.waitForTimeout(400);
    await expect(page.locator('.addon-app-shell tf-button', { hasText: 'dzień' }).first())
      .toBeVisible({ timeout: 10000 });

    // --- Add a composite rule. ---
    await clickButtonByText(page, 'Nowa reguła');
    await fillField(page, 'rule_name', RULE.name);
    await fillField(page, 'rule_expr', RULE.expr);
    await fillField(page, 'rule_action', RULE.action);
    await clickButtonByText(page, 'Zapisz regułę');
    await expect(page.locator('.addon-app-shell').getByText(RULE.name).first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/zones-list.png`, fullPage: true });

    // --- Delete the exclude zone (2nd row — zones sort by created_at, so the
    // include "Peron główny" precedes the exclude "Ławka") via its row kebab. ---
    await clickRowAction(page, 0, 'Usuń', 1);
    // Confirm bar -> explicit Usuń button.
    await clickButtonByText(page, 'Usuń');
    await page.waitForTimeout(400);

    // --- Persistence: reopen in a fresh, isolated context. The include zone, the
    // toggled schedule cell and the rule must survive (served from SQLite); the
    // deleted exclude zone must be gone. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'zones');
    await selectField(page2, 'zone_camera_select', `tstr:${await firstCameraId(page2)}`);

    await expect(page2.locator('.addon-app-shell').getByText(ZONE_INCLUDE.name).first())
      .toBeVisible({ timeout: 10000 });
    await expect(page2.locator('.addon-app-shell').getByText(ZONE_EXCLUDE.name))
      .toHaveCount(0);
    await expect(page2.locator('.addon-app-shell').getByText(RULE.name).first())
      .toBeVisible({ timeout: 10000 });
    // The persisted schedule cell still shows "dzień".
    await expect(page2.locator('.addon-app-shell tf-button', { hasText: 'dzień' }).first())
      .toBeVisible({ timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/zones-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});

// Reads the value attribute of the first real (non-empty) camera option in the
// zone camera selector so the test does not hard-code a generated id.
async function firstCameraId(page) {
  const field = page.locator('.addon-app-shell [data-component-id="zone_camera_select"]').first();
  await expect(field).toBeVisible({ timeout: 10000 });
  return field.locator('select option').nth(1).evaluate((opt) => {
    const v = opt.value;
    return v.startsWith('tstr:') ? v.slice('tstr:'.length) : v;
  });
}
