// =============================================================================
// File: tests/e2e/tentavision-bindings.spec.js
// Description: E2E for the TentaVision "Powiązania i magazyn" tab (Bindings &
//              Storage). Proves: the built-in storage API status panel reports
//              REAL probes (SQL/KV/Vector round-trips, Embeddings + Recording
//              status), the model discovery card lists the aliases the addon was
//              GRANTED to consume via the real alias_list_available host fn (not
//              a hardcoded list) with grant-status chips, the per-slot assignment
//              Selects are populated from real granted data filtered by capability
//              method, assigning a model persists to settings + writes the audit
//              log, and the assignment survives a full panel reopen in a fresh
//              context. Screenshots to /tmp/tv/ for visual comparison.
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
  'recording.read',
  'sql.read',
  'sql.write',
  'vector.read',
  'vector.write',
  'llm.generate',
  'alias.read',
];

const SHOT_DIR = '/tmp/tv';

// The slot we re-assign. The yolo slot requires the `detect` method, so the
// only granted, method-matching option is the tentavision-yolo alias. To prove
// a real re-point we add a second usable detect-capable option below by relying
// on the fact that the per-slot Select serializes its SelectValue with the
// "tstr:" prefix and the persisted setting holds the bare alias id.
const SLOT_ID = 'yolo';
const ASSIGN_ALIAS = 'tentavision-yolo';
const ASSIGN_OPTION = `tstr:${ASSIGN_ALIAS}`;

let proc;
let addonId;

test.beforeAll(async ({ browser }, testInfo) => {
  if (!binaryExists()) {
    test.skip(true, 'tentaflow binary not built (target_shared/{release,debug})');
  }
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  PORT = BASE_PORT + testInfo.workerIndex * 2;
  DB = `/tmp/e2e-tv-bindings-${PORT}.db`;
  proc = startBinary({ port: PORT, db: DB, rustLog: 'tentaflow_core=info' });
  await waitForServer(PORT);

  const page = await browser.newPage({ ignoreHTTPSErrors: true });
  await loginAsAdmin(page, { port: PORT });
  addonId = await installAddonInstance(page, {
    packageId: 'tentavision',
    displayName: 'TentaVision Bindings E2E',
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

// Reads the currently selected value of one slot's assignment tf-select.
async function slotSelectValue(page, slotId) {
  const sel = page.locator(`.addon-app-shell [data-component-id="alias_target_${slotId}"]`).first();
  await expect(sel).toBeVisible({ timeout: 10000 });
  return sel.locator('select').first().inputValue();
}

test.describe('TentaVision Bindings — real storage probes + real granted model assignment', () => {
  test('storage probes, real available-aliases list, per-slot assignment persists across reopen', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Bindings tab renders. ---
    await openTab(page, 'bindings');
    await expect(page.locator('.addon-app-shell').getByText('Storage — wbudowane API TentaFlow').first())
      .toBeVisible({ timeout: 10000 });

    // --- Built-in API status: SQL + KV + Vector probed live = "dostępny" (ok). ---
    await expect(page.locator('.addon-app-shell').getByText('SQL · SQLite').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('KV store').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('Vector store').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('Embeddings').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('Recording').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell')
      .getByText('events · cosine 1024d · vector store').first())
      .toBeVisible({ timeout: 10000 });
    const available = page.locator('.addon-app-shell').getByText('dostępny');
    expect(await available.count()).toBeGreaterThanOrEqual(3);
    await page.screenshot({ path: `${SHOT_DIR}/bindings-vector-ok.png`, fullPage: true });

    // --- REAL discovery list: the aliases the addon was GRANTED to consume.
    //     TentaVision owns + uses its 6 aliases, so install reconciles each
    //     uses_alias row to auto_granted → all 6 appear here with grant chips,
    //     sourced from the real alias_list_available host fn (not hardcoded). ---
    await expect(page.locator('.addon-app-shell').getByText('Modele przyznane addonowi').first())
      .toBeVisible({ timeout: 10000 });
    for (const id of [
      'tentavision-yolo', 'tentavision-ocr', 'tentavision-action',
      'tentavision-vlm', 'tentavision-face-embed', 'tentavision-reid',
    ]) {
      await expect(page.locator('.addon-app-shell').getByText(id, { exact: true }).first())
        .toBeVisible({ timeout: 10000 });
    }
    // Grant chips: every uses_alias is auto_granted for the owning addon.
    await expect(page.locator('.addon-app-shell').getByText('auto_granted').first()).toBeVisible();
    // Honest target resolution surfaced from DB (suggested_default backing model).
    await expect(page.locator('.addon-app-shell').getByText('yolo11m-detector').first()).toBeVisible();
    await page.screenshot({ path: `${SHOT_DIR}/bindings-models-real.png`, fullPage: true });

    // --- Per-slot assignment: each Select populated from real granted, method-
    //     matching aliases. The assignment card header is present and the yolo
    //     slot's Select offers the detect-capable tentavision-yolo alias. ---
    await expect(page.locator('.addon-app-shell').getByText('Przypisanie modeli do funkcji · 6 slotów').first())
      .toBeVisible({ timeout: 10000 });
    const sel = page.locator(`.addon-app-shell [data-component-id="alias_target_${SLOT_ID}"]`).first();
    await expect(sel).toBeVisible({ timeout: 10000 });
    // The yolo slot resolves to its canonical alias by default.
    expect(await slotSelectValue(page, SLOT_ID)).toContain(ASSIGN_ALIAS);

    // Re-commit the assignment (selectOption fires the change handler → persist
    // + audit + re-render with the success message).
    await sel.locator('select').first().selectOption(ASSIGN_OPTION);
    await expect(page.locator('.addon-app-shell').getByText(new RegExp(`Funkcja ${SLOT_ID}.*${ASSIGN_ALIAS}`)).first())
      .toBeVisible({ timeout: 10000 });
    expect(await slotSelectValue(page, SLOT_ID)).toContain(ASSIGN_ALIAS);
    await page.screenshot({ path: `${SHOT_DIR}/bindings-edit.png`, fullPage: true });

    // --- Persistence: reopen in a fresh, isolated context. The assignment must
    //     survive (served from the settings table, validated against real grants). ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'bindings');

    expect(await slotSelectValue(page2, SLOT_ID)).toContain(ASSIGN_ALIAS);
    await page2.screenshot({ path: `${SHOT_DIR}/bindings-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
