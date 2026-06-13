// =============================================================================
// File: tests/e2e/tentavision-audit.spec.js
// Description: E2E for the TentaVision "Audyt i RODO" (M10) tab backed by the
//              per-addon SQLite audit_log (append-only, FNV-1a hash-chained).
//              Proves: empty state in a fresh DB; an alarm decision (raised +
//              decided in the Alarm Center) INSERTs a real audit row; the Audit
//              tab shows that hash-chained entry with actor/action=alarm_decision
//              /target, a truncated chain hash, and a green "Łańcuch
//              zweryfikowany" indicator computed from verify_audit_chain();
//              expanding a row reveals the before/after JSON; editing a retention
//              value persists (db::set_setting) across a fresh context reopen.
//              Screenshots → /tmp/tv/ for visual comparison to m10-audit.html.
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
const CAM_NAME = 'C-04 wjazd glowny';
const CAM_RTSP = 'rtsp://192.168.40.44:554/stream1';

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-audit-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Audit E2E',
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

// Drives the 4-step "add camera" wizard for an RTSP source so an alarm can
// reference a real camera. Mirrors the alarms/cameras specs.
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

test.describe('TentaVision Audit — hash-chained log + chain verify + retention persist', () => {
  test('empty state, alarm decision writes audit row, verified chain, expand JSON, retention persist', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Audit tab empty state in the fresh DB. ---
    await openTab(page, 'audit');
    await expect(page.locator('.addon-app-shell tf-empty-state').first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/audit-empty.png`, fullPage: true });

    // --- Seed one real camera, then raise + decide an alarm so the Alarm Center
    // writes a hash-chained audit_log row (actor/action=alarm_decision). ---
    await openTab(page, 'cameras');
    await addCamera(page, CAM_NAME, CAM_RTSP);

    await openTab(page, 'alarms');
    await clickButtonByText(page, 'Symuluj alarm');
    await expect(page.locator('.addon-app-shell').getByText('agresja').first())
      .toBeVisible({ timeout: 10000 });
    const confirmBtn = page.locator('.addon-app-shell tf-button', { hasText: /^Potwierdź$/ }).first();
    await expect(confirmBtn).toBeVisible({ timeout: 10000 });
    await confirmBtn.scrollIntoViewIfNeeded();
    await confirmBtn.click();
    await expect(page.locator('.addon-app-shell').getByText(/Zapisano decyzję/).first())
      .toBeVisible({ timeout: 10000 });

    // --- Back to Audit: the real hash-chained entry must show, with the
    // alarm_decision action chip and a green chain-verified indicator. ---
    await openTab(page, 'audit');
    await expect(page.locator('.addon-app-shell').getByText('alarm_decision').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText('Łańcuch zweryfikowany').first())
      .toBeVisible({ timeout: 10000 });
    // The append-only header counter reflects at least one entry.
    await expect(page.locator('.addon-app-shell').getByText(/wpisów/).first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/audit-log.png`, fullPage: true });

    // --- Expand the entry → before/after JSON snapshots appear. Each row carries
    // a "Szczegóły" toggle button that dispatches audit-row-expand. ---
    await clickButtonByText(page, 'Szczegóły');
    await page.waitForTimeout(300);
    await expect(page.locator('.addon-app-shell').getByText('Poprzedni hash').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page.locator('.addon-app-shell').getByText(/"status"/).first())
      .toBeVisible({ timeout: 10000 });
    await page.screenshot({ path: `${SHOT_DIR}/audit-expand.png`, fullPage: true });

    // --- Edit retention for class A (default 730 → 800) and save. ---
    const editBtn = page.locator('.addon-app-shell tf-button', { hasText: /^Edytuj$/ }).first();
    await expect(editBtn).toBeVisible({ timeout: 10000 });
    await editBtn.scrollIntoViewIfNeeded();
    await editBtn.click();
    // The retention number input for class A mounts pre-filled; overwrite it.
    await fillField(page, 'retention_input_a', '800');
    await clickButtonByText(page, 'Zapisz');
    await expect(page.locator('.addon-app-shell').getByText(/Zapisano retencję klasy A: 800 dni/).first())
      .toBeVisible({ timeout: 10000 });

    // --- Persistence: reopen in a fresh, isolated context. The override must
    // survive, served from the SQLite settings table. ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'audit');
    await expect(page2.locator('.addon-app-shell').getByText('800 dni').first())
      .toBeVisible({ timeout: 10000 });
    // The audit entry + verified chain still render from SQLite.
    await expect(page2.locator('.addon-app-shell').getByText('alarm_decision').first())
      .toBeVisible({ timeout: 10000 });
    await expect(page2.locator('.addon-app-shell').getByText('Łańcuch zweryfikowany').first())
      .toBeVisible({ timeout: 10000 });
    await page2.screenshot({ path: `${SHOT_DIR}/audit-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
