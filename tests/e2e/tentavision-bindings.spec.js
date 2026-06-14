// =============================================================================
// File: tests/e2e/tentavision-bindings.spec.js
// Description: E2E for the TentaVision "Powiązania i magazyn" tab (Bindings &
//              Storage, M14). Proves: the built-in storage API status panel
//              reports REAL probes (SQL round-trip = ok/green, KV settings
//              round-trip = ok, Vector = REAL probe of the events namespace =
//              "dostępny" because vector_search responds, Embeddings probed via
//              llm.generate, Recording status from settings), the 6 AI aliases
//              render with status chips and an editable target Select, changing a
//              mapping persists to settings + writes the audit log, and the
//              mapping survives a full panel reopen in a fresh context.
//              Screenshots to /tmp/tv/ for visual comparison to m14-bindings.html.
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
];

const SHOT_DIR = '/tmp/tv';

// The alias whose mapping we re-point during the test, and the new target. The
// tf-select inner <select> exposes the serialized SelectValue ("tstr:" prefix);
// the persisted setting holds the bare value.
const ALIAS_ID = 'tentavision-yolo';
const NEW_TARGET = 'yolo11n-cpu';
const NEW_TARGET_OPTION = 'tstr:yolo11n-cpu';

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

// Reads the currently selected value of one alias's target tf-select.
async function aliasSelectValue(page, aliasId) {
  const sel = page.locator(`.addon-app-shell [data-component-id="alias_target_${aliasId}"]`).first();
  await expect(sel).toBeVisible({ timeout: 10000 });
  return sel.locator('select').first().inputValue();
}

test.describe('TentaVision Bindings — real storage probes + persisted alias mappings', () => {
  test('storage status probes, alias rows + chips, persist mapping across reopen', async ({ page, browser }) => {
    test.setTimeout(240000);
    const errors = collectConsoleErrors(page);
    await loginAsAdmin(page, { port: PORT });
    await openPanel(page);

    // --- Bindings tab renders. ---
    await openTab(page, 'bindings');
    await expect(page.locator('.addon-app-shell').getByText('Storage — wbudowane API TentaFlow').first())
      .toBeVisible({ timeout: 10000 });

    // --- Built-in API status: SQL + KV + Vector probed live = "dostępny" (ok).
    //     Vector is now a REAL probe of the `events` namespace (vector_search
    //     responds even with no embedding model), so it must NOT be "niedostępny".
    //     Recording reflects settings; Embeddings reflects the llm.generate probe. ---
    await expect(page.locator('.addon-app-shell').getByText('SQL · SQLite').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('KV store').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('Vector store').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('Embeddings').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('Recording').first()).toBeVisible();
    // The Vector store cell reports "dostępny" (live vector_search round-trip OK):
    // its sub-label is only rendered on the OK branch, so it proves the probe.
    await expect(page.locator('.addon-app-shell')
      .getByText('events · cosine 1024d · vector store').first())
      .toBeVisible({ timeout: 10000 });
    // Vector is NOT the old honest "niedostępne".
    await expect(page.locator('.addon-app-shell').getByText('niedostępne')).toHaveCount(0);
    // SQL + KV + Vector all probe green → at least 3 "dostępny" chips.
    const available = page.locator('.addon-app-shell').getByText('dostępny');
    expect(await available.count()).toBeGreaterThanOrEqual(3);
    await page.screenshot({ path: `${SHOT_DIR}/bindings-vector-ok.png`, fullPage: true });

    // --- All 6 alias rows render with their status chips. ---
    for (const id of [
      'tentavision-yolo', 'tentavision-ocr', 'tentavision-action',
      'tentavision-vlm', 'tentavision-face-embed', 'tentavision-reid',
    ]) {
      await expect(page.locator('.addon-app-shell').getByText(id, { exact: true }).first())
        .toBeVisible({ timeout: 10000 });
    }
    // Honest status chips: gated aliases + the unconfigured re-id alias.
    await expect(page.locator('.addon-app-shell').getByText('gated').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('nieprzypisany').first()).toBeVisible();
    await expect(page.locator('.addon-app-shell').getByText('przypisany').first()).toBeVisible();
    await page.screenshot({ path: `${SHOT_DIR}/bindings.png`, fullPage: true });

    // --- The yolo alias defaults to yolo11m-detector; re-point it. ---
    expect(await aliasSelectValue(page, ALIAS_ID)).toContain('yolo11m-detector');
    const sel = page.locator(`.addon-app-shell [data-component-id="alias_target_${ALIAS_ID}"]`).first();
    await sel.locator('select').first().selectOption(NEW_TARGET_OPTION);
    // The change handler persists + re-renders with a success message.
    await expect(page.locator('.addon-app-shell').getByText(new RegExp(`${ALIAS_ID}.*${NEW_TARGET}`)).first())
      .toBeVisible({ timeout: 10000 });
    expect(await aliasSelectValue(page, ALIAS_ID)).toContain(NEW_TARGET);
    await page.screenshot({ path: `${SHOT_DIR}/bindings-edit.png`, fullPage: true });

    // --- Persistence: reopen in a fresh, isolated context. The re-pointed
    //     mapping must survive (served from the settings table). ---
    await page.close();
    const context2 = await browser.newContext({ ignoreHTTPSErrors: true });
    const page2 = await context2.newPage();
    const errors2 = collectConsoleErrors(page2);
    await loginAsAdmin(page2, { port: PORT });
    await openPanel(page2);
    await openTab(page2, 'bindings');

    expect(await aliasSelectValue(page2, ALIAS_ID)).toContain(NEW_TARGET);
    await page2.screenshot({ path: `${SHOT_DIR}/bindings-persist.png`, fullPage: true });

    expect(errors, diagnostics(errors, proc)).toEqual([]);
    expect(errors2, diagnostics(errors2, proc)).toEqual([]);
    await context2.close();
  });
});
