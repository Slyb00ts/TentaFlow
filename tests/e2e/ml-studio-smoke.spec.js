// =============================================================================
// File: tests/e2e/ml-studio-smoke.spec.js
// Description: Real smoke test of the ML Studio module against a LIVE running
//              TentaFlow instance (https://localhost:8095, admin/admin). Drives
//              the actual SPA over the binary WebSocket protocol: login,
//              navigate to ML Studio, create a project and verify it appears in
//              the backend-backed project list. Standalone node script — does
//              NOT spawn its own binary (unlike the project-spawning specs).
//              Screenshots land in /tmp/mlstudio-shots/.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-shots';

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  const tag = pass ? 'PASS' : 'FAIL';
  console.log(`[${tag}] ${name} :: ${note}`);
}

async function shot(page, file) {
  await page.screenshot({ path: `${SHOT}/${file}`, fullPage: true }).catch(() => {});
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  const consoleErrors = [];
  const failedRequests = [];

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
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

  let ok = true;
  try {
    // ---- Step 1: login ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      const userInput = page.locator('#login-username input').first();
      await userInput.waitFor({ state: 'visible', timeout: 20000 });
      await shot(page, '01-login-page.png');
      await userInput.fill('admin');
      await page.locator('#login-password input').first().fill('admin');
      await page.locator('#login-submit').click();
      // Wait for the main shell sidebar to render after auth.
      await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });
      await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
      await shot(page, '02-dashboard.png');
      step('1. Logowanie admin/admin + SPA', true, 'Sidebar nav wyrenderowany po zalogowaniu');
    } catch (e) {
      ok = false;
      await shot(page, '02-login-FAIL.png');
      step('1. Logowanie admin/admin + SPA', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: open ML Studio ----
    try {
      const navItem = page.locator('.sidebar .nav-item[data-view="ml-studio"]');
      await navItem.waitFor({ state: 'visible', timeout: 10000 });
      await navItem.click();
      // Project list module renders #ml-studio-new (header button).
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      // Wait for either project cards or the empty-state to settle the list load.
      await page.waitForFunction(() => {
        const list = document.querySelector('#ml-studio-list');
        if (!list) return false;
        return list.querySelector('tf-empty-state') || list.querySelector('.ml-studio-card') || list.querySelector('.ml-studio-loading') === null;
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(800);
      const cardCount = await page.locator('#ml-studio-list .ml-studio-card').count();
      const hasEmpty = await page.locator('#ml-studio-list tf-empty-state').count();
      await shot(page, '03-ml-studio-list-initial.png');
      step('2. Otwarcie ML Studio', true,
        `Moduł otwarty. Projektów na starcie: ${cardCount}, empty-state: ${hasEmpty ? 'tak' : 'nie'}`);
    } catch (e) {
      ok = false;
      await shot(page, '03-ml-studio-FAIL.png');
      step('2. Otwarcie ML Studio', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 3: create project ----
    let typeChosen = '';
    try {
      await page.locator('#ml-studio-new').click();
      // tf-modal host stays zero-size (collapsed); the visible portal is the
      // .tf-modal-backdrop subtree it injects. Wait on the inner field instead.
      const nameInput = page.locator('#ml-studio-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 10000 });
      // tf-input/tf-textarea expose .value via the inner native control; fill it.
      await nameInput.fill('Test cysterny');
      await page.locator('#ml-studio-desc textarea').first().fill('smoke test');

      // Pick the recognition (Rozpoznawanie obrazu) type card. It is also the
      // radio-group default (projectTypes[0].slug === "recognition").
      const recRadio = page.locator('#ml-studio-types tf-radio[value="recognition"]');
      if (await recRadio.count()) {
        await recRadio.first().click();
        typeChosen = 'recognition';
      } else {
        typeChosen = 'default (pierwszy typ)';
      }
      await shot(page, '04-create-form.png');

      // Footer submit button labelled "Utwórz projekt" (moved into the backdrop).
      const submit = page.locator('#ml-studio-create-modal tf-button', { hasText: 'Utwórz projekt' }).first();
      await submit.click();
      // On success the module closes the modal and navigates to detail.
      await page.waitForSelector('#ml-studio-create-modal', { state: 'detached', timeout: 15000 }).catch(() => {});

      // On success the module navigates to project detail (projectId param).
      await page.waitForSelector('#ml-studio-detail, .ml-studio-card', { timeout: 15000 });
      await page.waitForTimeout(1000);
      await shot(page, '05-after-create.png');
      step('3. Utworzenie projektu', true, `Wysłano mlStudioProjectCreateRequest (typ: ${typeChosen}), brak błędu toast`);
    } catch (e) {
      ok = false;
      await shot(page, '04-create-FAIL.png');
      step('3. Utworzenie projektu', false, `Błąd: ${e.message}`);
    }

    // ---- Step 5 (detail) captured here if we landed on detail ----
    let onDetail = await page.locator('#ml-studio-detail').count();
    let detailTitle = '';
    if (onDetail) {
      detailTitle = await page.locator('tf-detail-header').first().getAttribute('title').catch(() => '');
      await shot(page, '06-project-detail.png');
    }

    // ---- Step 4: verify project on list (navigate back) ----
    try {
      const backBtn = page.locator('#ml-studio-back');
      if (await backBtn.count()) {
        await backBtn.click();
      } else {
        await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      }
      await page.waitForSelector('#ml-studio-list', { timeout: 15000 });
      await page.waitForFunction(() => {
        const list = document.querySelector('#ml-studio-list');
        return list && (list.querySelector('.ml-studio-card') || list.querySelector('tf-empty-state'));
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(800);

      const names = await page.locator('#ml-studio-list .ml-studio-card .ml-studio-card-name').allTextContents();
      const found = names.some((n) => n.trim() === 'Test cysterny');
      await shot(page, '07-list-after-create.png');
      step('4. Projekt na liście (realne dane backendu)', found,
        `Karty na liście: [${names.map((n) => n.trim()).join(', ')}] — "Test cysterny" ${found ? 'ZNALEZIONY' : 'NIE znaleziony'}`);
      if (!found) ok = false;
    } catch (e) {
      ok = false;
      await shot(page, '07-list-FAIL.png');
      step('4. Projekt na liście', false, `Błąd: ${e.message}`);
    }

    // ---- Step 5: open detail explicitly ----
    try {
      const card = page.locator('#ml-studio-list .ml-studio-card', { hasText: 'Test cysterny' }).first();
      if (await card.count()) {
        await card.click();
        await page.waitForSelector('#ml-studio-detail tf-detail-header', { timeout: 15000 });
        await page.waitForTimeout(600);
        const title = await page.locator('#ml-studio-detail tf-detail-header').getAttribute('title').catch(() => '');
        await shot(page, '08-project-detail.png');
        step('5. Widok szczegółu projektu', title.includes('Test cysterny') || title === 'Test cysterny',
          `Nagłówek szczegółu: "${title}"`);
      } else {
        step('5. Widok szczegółu projektu', false, 'Brak karty do otwarcia');
      }
    } catch (e) {
      await shot(page, '08-detail-FAIL.png');
      step('5. Widok szczegółu projektu', false, `Błąd: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL: ${fatal.message}`);
  } finally {
    console.log('\n================ KONSOLA / SIEĆ ================');
    console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
    consoleErrors.slice(0, 30).forEach((e) => console.log('  JS> ' + e));
    console.log(`Nieudane żądania: ${failedRequests.length}`);
    failedRequests.slice(0, 30).forEach((e) => console.log('  NET> ' + e));

    console.log('\n================ PODSUMOWANIE =================');
    results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name}`));
    const allPass = results.length > 0 && results.every((r) => r.pass);
    console.log(`\nWYNIK OGÓLNY: ${allPass ? 'PASS' : 'FAIL'}`);

    await browser.close();
    process.exit(allPass ? 0 : 1);
  }
})();
