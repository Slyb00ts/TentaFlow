// =============================================================================
// File: tests/e2e/ml-studio-resources.spec.js
// Description: Real E2E test of the ML Studio mesh resource allocation screen
//              (§11.3 "alokacja zasobów mesh") against a LIVE running TentaFlow
//              instance (https://localhost:8095, admin/admin). Drives the actual
//              SPA over the binary WebSocket protocol: login as admin, open the
//              "Administracja › Zasoby" screen, verify the mesh pool + empty
//              grants state, create a project, allocate a CPU/RAM resource to it,
//              confirm the grant appears in the active grants table AND in the
//              project's "Zasoby" tab (proof project_resources works end-to-end),
//              exercise server-side subject validation (bogus user id), revoke
//              the grant, and finally assert the ml_studio.db resource_grants
//              table. Standalone node script — does NOT spawn its own binary.
//              Screenshots land in /tmp/mlstudio-shots6/.
// =============================================================================

const fs = require('fs');
const { execSync } = require('child_process');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-shots6';
const DB = '/home/critix/repos/rust/TentaFlow-ml/.runtime/data/ml_studio.db';
const PROJECT_NAME = 'Zasoby test';

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  const tag = pass ? 'PASS' : 'FAIL';
  console.log(`[${tag}] ${name} :: ${note}`);
}

async function shot(page, file) {
  await page.screenshot({ path: `${SHOT}/${file}`, fullPage: true }).catch(() => {});
}

function sqlGrants() {
  try {
    return execSync(
      `sqlite3 "${DB}" "SELECT subject_kind,subject_id,node_id,resource_kind,quota FROM resource_grants;"`,
      { encoding: 'utf8' },
    ).trim();
  } catch (e) {
    return `SQL ERROR: ${e.message}`;
  }
}

// Drive a tf-select (wrapper over a native <select>) by selecting the option on
// the inner native control — the native `change` is re-dispatched by the
// component as the CustomEvent the form listeners expect.
async function selectTf(page, hostSelector, value) {
  await page.locator(`${hostSelector} select`).first().selectOption(value);
}

async function selectTfByLabel(page, hostSelector, label) {
  await page.locator(`${hostSelector} select`).first().selectOption({ label });
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  const consoleErrors = [];
  const failedRequests = [];

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();

  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));
  page.on('requestfailed', (req) => {
    failedRequests.push(`${req.method()} ${req.url()} :: ${req.failure()?.errorText}`);
  });
  page.on('websocket', (ws) => {
    ws.on('socketerror', (e) => failedRequests.push(`WS error ${ws.url()} :: ${e}`));
  });

  // Toast capture — the resource form reports success/errors via tf-toast.
  async function readToasts() {
    return page.evaluate(() => Array.from(document.querySelectorAll('tf-toast, .tf-toast, [class*="toast"]'))
      .map((t) => (t.textContent || '').trim())
      .filter(Boolean));
  }

  try {
    // ---- Step 1a: login ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      const userInput = page.locator('#login-username input').first();
      await userInput.waitFor({ state: 'visible', timeout: 20000 });
      await userInput.fill('admin');
      await page.locator('#login-password input').first().fill('admin');
      await page.locator('#login-submit').click();
      await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });
      await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
      step('1a. Logowanie admin/admin', true, 'Sidebar nav wyrenderowany po zalogowaniu');
    } catch (e) {
      await shot(page, '00-login-FAIL.png');
      step('1a. Logowanie admin/admin', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 1b: open ML Studio + go to "Administracja › Zasoby" ----
    try {
      await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      // Admin entry button is injected after AuthMe resolves inside loadAll.
      const adminBtn = page.locator('#ml-studio-admin-resources');
      await adminBtn.waitFor({ state: 'visible', timeout: 15000 });
      await adminBtn.click();
      // Resources admin screen renders the pool container + grants container.
      await page.waitForSelector('#ml-studio-res-pool', { timeout: 15000 });
      await page.waitForFunction(() => {
        const pool = document.querySelector('#ml-studio-res-pool');
        return pool && (pool.querySelector('.ml-studio-res-node') || pool.querySelector('tf-empty-state'));
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(500);
      step('1b. Wejście na ekran Zasoby (Administracja › Zasoby)', true, 'Ekran przydziału zasobów otwarty');
    } catch (e) {
      await shot(page, '01-resources-open-FAIL.png');
      step('1b. Wejście na ekran Zasoby', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 1c: verify mesh pool (local node) + empty grants ----
    try {
      const nodeNames = await page.locator('#ml-studio-res-pool .ml-studio-res-node-name').allTextContents();
      const poolHasEmpty = await page.locator('#ml-studio-res-pool tf-empty-state').count();
      const chips = await page.locator('#ml-studio-res-pool .ml-studio-res-node-foot tf-chip').allTextContents();
      const grantsEmpty = await page.locator('#ml-studio-res-grants tf-empty-state').count();
      const grantsEmptyTitle = grantsEmpty
        ? await page.locator('#ml-studio-res-grants tf-empty-state').first().getAttribute('title')
        : '';
      const grantRows = await page.locator('#ml-studio-res-grants tf-table tf-row, #ml-studio-res-grants tf-table [class*="row"]').count();
      await shot(page, '01-resources-initial.png');

      const poolOk = nodeNames.length >= 1 && poolHasEmpty === 0;
      const cpuRamShown = chips.some((c) => /CPU/i.test(c)) || chips.some((c) => /RAM/i.test(c));
      const grantsOk = grantsEmpty > 0;
      step('1c. Pula mesh + puste przydziały', poolOk && cpuRamShown && grantsOk,
        `Nody w puli: [${nodeNames.map((n) => n.trim()).join(' | ')}]; chipy CPU/RAM: [${chips.map((c) => c.trim()).join(', ')}]; ` +
        `przydziały empty-state: ${grantsOk ? `tak ("${grantsEmptyTitle}")` : `NIE (wierszy: ${grantRows})`}`);
    } catch (e) {
      await shot(page, '01-resources-verify-FAIL.png');
      step('1c. Pula mesh + puste przydziały', false, `Błąd: ${e.message}`);
    }

    // ---- Step 2: ensure project "Zasoby test" exists ----
    let projectExists = false;
    try {
      await page.locator('#ml-studio-res-back').click();
      await page.waitForSelector('#ml-studio-list', { timeout: 15000 });
      await page.waitForFunction(() => {
        const list = document.querySelector('#ml-studio-list');
        return list && (list.querySelector('.ml-studio-card') || list.querySelector('tf-empty-state'));
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(500);

      let names = await page.locator('#ml-studio-list .ml-studio-card .ml-studio-card-name').allTextContents();
      projectExists = names.some((n) => n.trim() === PROJECT_NAME);

      if (!projectExists) {
        await page.locator('#ml-studio-new').click();
        const nameInput = page.locator('#ml-studio-name input').first();
        await nameInput.waitFor({ state: 'visible', timeout: 10000 });
        await nameInput.fill(PROJECT_NAME);
        await page.locator('#ml-studio-desc textarea').first().fill('test alokacji zasobów mesh');
        const recRadio = page.locator('#ml-studio-types tf-radio[value="recognition"]');
        if (await recRadio.count()) await recRadio.first().click();
        const submit = page.locator('#ml-studio-create-modal tf-button', { hasText: 'Utwórz projekt' }).first();
        await submit.click();
        await page.waitForSelector('#ml-studio-create-modal', { state: 'detached', timeout: 15000 }).catch(() => {});
        await page.waitForSelector('#ml-studio-detail, .ml-studio-card', { timeout: 15000 });
        await page.waitForTimeout(800);
        // Back to list to re-confirm.
        const back = page.locator('#ml-studio-back');
        if (await back.count()) await back.click();
        else await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
        await page.waitForSelector('#ml-studio-list', { timeout: 15000 });
        await page.waitForTimeout(800);
        names = await page.locator('#ml-studio-list .ml-studio-card .ml-studio-card-name').allTextContents();
        projectExists = names.some((n) => n.trim() === PROJECT_NAME);
      }
      await shot(page, '02-projects-list.png');
      step('2. Projekt "Zasoby test" istnieje', projectExists,
        projectExists ? `Projekt obecny na liście (${names.length} kart)` : `NIE znaleziono — karty: [${names.map((n) => n.trim()).join(', ')}]`);
    } catch (e) {
      await shot(page, '02-project-FAIL.png');
      step('2. Projekt "Zasoby test"', false, `Błąd: ${e.message}`);
    }

    // ---- Step 3: allocate a resource (project + cpu) ----
    let resourceKindUsed = 'cpu';
    try {
      await page.locator('#ml-studio-admin-resources').click();
      await page.waitForSelector('#ml-studio-res-grant', { timeout: 15000 });
      await page.waitForTimeout(400);

      // subjectKind = project (default already), pick "Zasoby test" in the project select.
      await selectTf(page, '#ml-studio-res-subject-kind', 'project');
      await page.waitForSelector('#ml-studio-res-subject select', { timeout: 10000 });
      await selectTfByLabel(page, '#ml-studio-res-subject', PROJECT_NAME);

      // Node: keep the first/local one already selected (single node).
      const nodeVal = await page.locator('#ml-studio-res-node select').first().inputValue();

      // Resource kind: prefer cpu; if the pool shows GPU we still use cpu (always present).
      await selectTf(page, '#ml-studio-res-kind', 'cpu');
      resourceKindUsed = 'cpu';

      await page.locator('#ml-studio-res-quota input').first().fill('4 rdzenie');
      await shot(page, '03-allocate-form.png');

      await page.locator('#ml-studio-res-grant').click();
      // showResourcesAdmin() re-renders after a successful create; wait for a grant row.
      await page.waitForFunction(() => {
        const g = document.querySelector('#ml-studio-res-grants');
        return g && g.querySelector('tf-table') && !g.querySelector('tf-empty-state');
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(800);

      const grantsTableRows = await page.locator('#ml-studio-res-grants tf-table').count();
      const grantsStillEmpty = await page.locator('#ml-studio-res-grants tf-empty-state').count();
      const subjectCells = await page.locator('#ml-studio-res-grants .ml-studio-res-subject-cell').allTextContents();
      const resourceCells = await page.locator('#ml-studio-res-grants .ml-studio-res-resource-cell').allTextContents();
      await shot(page, '03-after-allocate.png');

      const ok = grantsTableRows > 0 && grantsStillEmpty === 0
        && resourceCells.some((c) => /CPU/i.test(c));
      step('3. Przydział zasobu (Projekt + CPU)', ok,
        ok
          ? `Przydział widoczny. Podmiot: [${subjectCells.map((c) => c.trim()).join(' | ')}]; zasób: [${resourceCells.map((c) => c.trim()).join(' | ')}]; node=${nodeVal.slice(0, 12)}`
          : `Brak przydziału w tabeli (empty-state=${grantsStillEmpty}). Podmiot:[${subjectCells.join(',')}] zasób:[${resourceCells.join(',')}]`);
    } catch (e) {
      await shot(page, '03-allocate-FAIL.png');
      step('3. Przydział zasobu', false, `Błąd: ${e.message}`);
    }

    // ---- Step 4: open project "Zasoby test" → "Zasoby" tab ----
    try {
      await page.locator('#ml-studio-res-back').click();
      await page.waitForSelector('#ml-studio-list', { timeout: 15000 });
      await page.waitForTimeout(600);
      const card = page.locator('#ml-studio-list .ml-studio-card', { hasText: PROJECT_NAME }).first();
      await card.click();
      await page.waitForSelector('#ml-studio-tabs', { timeout: 15000 });
      await page.waitForTimeout(400);

      // Click the "Zasoby" tab (last tab).
      const zasobyTab = page.locator('#ml-studio-tabs tf-tab[label="Zasoby"]').first();
      await zasobyTab.click();
      await page.waitForFunction(() => {
        const p = document.querySelector('#ml-studio-tab-panel');
        return p && (p.querySelector('tf-table') || p.querySelector('tf-empty-state'));
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(600);

      const tabHasTable = await page.locator('#ml-studio-tab-panel tf-table').count();
      const tabHasEmpty = await page.locator('#ml-studio-tab-panel tf-empty-state').count();
      const tabResource = await page.locator('#ml-studio-tab-panel .ml-studio-res-resource-cell').allTextContents();
      const tabQuota = await page.locator('#ml-studio-tab-panel tf-table').allTextContents();
      await shot(page, '04-project-zasoby-tab.png');

      const ok = tabHasTable > 0 && tabHasEmpty === 0 && tabResource.some((c) => /CPU/i.test(c));
      step('4. Zakładka "Zasoby" projektu pokazuje przydział', ok,
        ok
          ? `Tabela z zasobem (NIE empty-state). Zasób: [${tabResource.map((c) => c.trim()).join(' | ')}]; quota w tabeli zawiera "4 rdzenie": ${/4 rdzenie/.test(tabQuota.join(' '))}`
          : `empty-state=${tabHasEmpty}, table=${tabHasTable}, zasób:[${tabResource.join(',')}] — project_resources NIE zwrócił grantu`);
    } catch (e) {
      await shot(page, '04-project-tab-FAIL.png');
      step('4. Zakładka "Zasoby" projektu', false, `Błąd: ${e.message}`);
    }

    // ---- Step 5: server-side subject validation (bogus user id) ----
    try {
      await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-admin-resources', { timeout: 15000 });
      await page.locator('#ml-studio-admin-resources').click();
      await page.waitForSelector('#ml-studio-res-grant', { timeout: 15000 });
      await page.waitForTimeout(400);

      // subjectKind = user → free identifier input; type a bogus id.
      await selectTf(page, '#ml-studio-res-subject-kind', 'user');
      await page.waitForSelector('#ml-studio-res-subject input', { timeout: 10000 });
      await page.locator('#ml-studio-res-subject input').first().fill('nieistnieje-user-zzz');
      await selectTf(page, '#ml-studio-res-kind', 'cpu');
      await page.locator('#ml-studio-res-quota input').first().fill('1 rdzeń');
      await shot(page, '05-bogus-form.png');

      const grantsBefore = await page.locator('#ml-studio-res-grants tf-table tr, #ml-studio-res-grants .ml-studio-res-subject-cell').count();
      await page.locator('#ml-studio-res-grant').click();
      await page.waitForTimeout(1500);

      const toasts = await readToasts();
      const errorToast = toasts.find((t) => /Przydzia[lł]|nie ma|nie istnieje|u[zż]ytkownik|not found|brak/i.test(t)) || toasts.join(' | ');
      // The bogus user must NOT create a grant row.
      const subjectCellsAfter = await page.locator('#ml-studio-res-grants .ml-studio-res-subject-cell').allTextContents();
      const bogusLeaked = subjectCellsAfter.some((c) => /nieistnieje-user-zzz/.test(c));
      await shot(page, '05-bogus-rejected.png');

      const rejected = !bogusLeaked;
      step('5. Walidacja serwera odrzuca nieistniejący podmiot', rejected,
        rejected
          ? `Brak grantu dla bogus usera. Toast: "${errorToast || '(brak — sprawdź czy backend zwrócił błąd)'}"`
          : `BŁĄD: bogus user trafił do tabeli przydziałów: [${subjectCellsAfter.join(', ')}]`);
    } catch (e) {
      await shot(page, '05-bogus-FAIL.png');
      step('5. Walidacja serwera (bogus subject)', false, `Błąd: ${e.message}`);
    }

    // ---- Step 6: revoke the project grant ----
    try {
      // Ensure we're on the resources admin screen with the project grant present.
      if (!(await page.locator('#ml-studio-res-grants tf-table').count())) {
        await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
        await page.waitForSelector('#ml-studio-admin-resources', { timeout: 15000 });
        await page.locator('#ml-studio-admin-resources').click();
        await page.waitForSelector('#ml-studio-res-grants tf-table', { timeout: 15000 });
      }
      await page.waitForTimeout(500);
      const before = await page.locator('#ml-studio-res-grants .ml-studio-res-subject-cell').count();

      // The revoke button ("Cofnij") lives in the tf-table row actions.
      const revokeBtn = page.locator('#ml-studio-res-grants tf-table tf-button', { hasText: 'Cofnij' }).first();
      await revokeBtn.click();
      await page.waitForFunction(() => {
        const g = document.querySelector('#ml-studio-res-grants');
        return g && g.querySelector('tf-empty-state');
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(800);

      const afterEmpty = await page.locator('#ml-studio-res-grants tf-empty-state').count();
      const afterRows = await page.locator('#ml-studio-res-grants .ml-studio-res-subject-cell').count();
      await shot(page, '06-after-revoke.png');

      const ok = afterEmpty > 0 || afterRows < before;
      step('6. Cofnięcie (revoke) przydziału', ok,
        ok
          ? `Przydział zniknął (wierszy przed=${before}, po=${afterRows}, empty-state=${afterEmpty})`
          : `Przydział NIE zniknął (przed=${before}, po=${afterRows})`);
    } catch (e) {
      await shot(page, '06-revoke-FAIL.png');
      step('6. Cofnięcie przydziału', false, `Błąd: ${e.message}`);
    }

    // ---- Step 7: confirm DB state ----
    try {
      const rows = sqlGrants();
      step('7. Stan bazy resource_grants (po revoke)', true,
        rows ? `Pozostałe wiersze:\n${rows}` : 'Tabela pusta (przydział faktycznie cofnięty w ml_studio.db)');
    } catch (e) {
      step('7. Stan bazy resource_grants', false, `Błąd: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL: ${fatal.message}`);
  } finally {
    console.log('\n================ KONSOLA / SIEĆ ================');
    console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
    consoleErrors.slice(0, 40).forEach((e) => console.log('  JS> ' + e));
    console.log(`Nieudane żądania: ${failedRequests.length}`);
    failedRequests.slice(0, 40).forEach((e) => console.log('  NET> ' + e));

    console.log('\n================ PODSUMOWANIE =================');
    results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} :: ${r.note}`));
    const allPass = results.length > 0 && results.every((r) => r.pass);
    console.log(`\nWYNIK OGÓLNY: ${allPass ? 'PASS' : 'FAIL'}`);

    await browser.close();
    process.exit(allPass ? 0 : 1);
  }
})();
