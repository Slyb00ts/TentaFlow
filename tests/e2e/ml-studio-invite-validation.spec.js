// =============================================================================
// File: tests/e2e/ml-studio-invite-validation.spec.js
// Description: Real smoke test of the ML Studio invitee-validation rule against
//              a LIVE TentaFlow instance (https://localhost:8095). Decision under
//              test: a project owner may invite ONLY Power Users. The server gate
//              (#[policy(PowerUser)] on the owner + invitee role validation) must
//              REJECT inviting a plain `user` (zwykly, ...aa) and ACCEPT inviting a
//              `power_user` (power1, ...bb). Verified end-to-end through the UI
//              (toast + members list) and confirmed in the ml_studio.db project_members
//              table scoped to the freshly created project. Also re-checks the §11.2
//              Power User nav gate for a plain user. Standalone node script — does NOT
//              spawn its own binary. Screenshots land in /tmp/mlstudio-shots4/.
// =============================================================================

const fs = require('fs');
const { execSync } = require('child_process');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-shots4';
const ML_DB = '/home/critix/repos/rust/TentaFlow-ml/.runtime/data/ml_studio.db';
// Unique per run — backend enforces UNIQUE(org_id, name); a fixed name collides on re-run.
const PROJECT_NAME = `Walidacja zaproszen ${Date.now().toString(36).slice(-5)}`;
// Plain `user` — must be REJECTED (the key negative test).
const ZWYKLY_ID = '00000000-0000-4000-8000-0000000000aa';
// `power_user` — must be ACCEPTED.
const POWER1_ID = '00000000-0000-4000-8000-0000000000bb';

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}

async function shot(page, file) {
  await page.screenshot({ path: `${SHOT}/${file}`, fullPage: true }).catch(() => {});
}

// Wires console/network/WS error sinks onto a page and returns the buffers.
function attachDiagnostics(page, label) {
  const consoleErrors = [];
  const failedRequests = [];
  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(`[${label}] ${msg.text()}`); });
  page.on('pageerror', (err) => consoleErrors.push(`[${label}] pageerror: ${err.message}`));
  page.on('requestfailed', (req) => {
    failedRequests.push(`[${label}] ${req.method()} ${req.url()} :: ${req.failure()?.errorText}`);
  });
  page.on('websocket', (ws) => {
    ws.on('socketerror', (e) => failedRequests.push(`[${label}] WS error ${ws.url()} :: ${e}`));
  });
  return { consoleErrors, failedRequests };
}

// Logs in via the SPA login form and waits for the sidebar to paint.
async function login(page, user, pass) {
  await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
  const userInput = page.locator('#login-username input').first();
  await userInput.waitFor({ state: 'visible', timeout: 20000 });
  await userInput.fill(user);
  await page.locator('#login-password input').first().fill(pass);
  await page.locator('#login-submit').click();
  await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });
  await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
}

// Reads members from the open shadow root of the #ml-studio-members tf-table.
async function readMembers(page) {
  return page.evaluate((ids) => {
    const t = document.querySelector('#ml-studio-members');
    const rows = t?.shadowRoot ? [...t.shadowRoot.querySelectorAll('tbody tr')] : [];
    const rowTexts = rows.map((r) => r.textContent.replace(/\s+/g, ' ').trim());
    return {
      rowCount: rows.length,
      rowTexts,
      hasZwykly: rowTexts.some((tx) => tx.includes(ids.zwykly)),
      hasPower1: rowTexts.some((tx) => tx.includes(ids.power1)),
    };
  }, { zwykly: ZWYKLY_ID, power1: POWER1_ID });
}

// Drains any visible toast text (error/success) for evidence.
async function readToast(page) {
  return page.evaluate(() => {
    const els = [...document.querySelectorAll('tf-toast, .tf-toast, tf-toast-host')];
    const texts = [];
    for (const el of els) {
      const t = (el.shadowRoot || el).textContent?.replace(/\s+/g, ' ').trim();
      if (t) texts.push(t);
    }
    return texts.join(' | ');
  });
}

// Opens the share screen for the named project (owner path) and waits for members.
async function openShare(page, projectName) {
  const card = page.locator('#ml-studio-list .ml-studio-card', { hasText: projectName }).first();
  await card.waitFor({ state: 'visible', timeout: 15000 });
  await card.click();
  await page.waitForSelector('#ml-studio-detail tf-detail-header', { timeout: 15000 });
  await page.waitForTimeout(400);
  const manage = page.locator('#ml-studio-manage-access');
  if (await manage.count()) {
    await manage.click();
  } else {
    await page.locator('#ml-studio-back').click().catch(() => {});
    await page.waitForSelector('#ml-studio-list .ml-studio-card', { timeout: 15000 });
    await page.locator('#ml-studio-list .ml-studio-card', { hasText: projectName })
      .first().locator('[data-share-id]').first().click();
  }
  await page.waitForSelector('#ml-studio-share #ml-studio-members', { timeout: 15000 });
  await page.waitForFunction(() => {
    const t = document.querySelector('#ml-studio-members');
    return t && t.shadowRoot && t.shadowRoot.querySelectorAll('tbody tr').length > 0;
  }, { timeout: 15000 }).catch(() => {});
  await page.waitForTimeout(400);
}

// Fills the invite form (id + default Editor role) and clicks send.
async function sendInvite(page, userId) {
  const inviteUser = page.locator('#ml-studio-invite-user input').first();
  await inviteUser.waitFor({ state: 'visible', timeout: 10000 });
  await inviteUser.fill(userId);
  await page.locator('#ml-studio-invite-send').click();
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  const browser = await chromium.launch({ headless: true });

  const ctxA = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await ctxA.newPage();
  const diagA = attachDiagnostics(page, 'admin');

  let createdProjectId = null;

  try {
    // ---- Step 1: login admin + create project + open share screen ----
    try {
      await login(page, 'admin', 'admin');
      await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      await page.waitForTimeout(400);
      await page.locator('#ml-studio-new').click();
      const nameInput = page.locator('#ml-studio-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 10000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-desc textarea').first().fill('test walidacji zaproszen — tylko Power User');
      const recRadio = page.locator('#ml-studio-types tf-radio[value="recognition"]');
      if (await recRadio.count()) await recRadio.first().click();
      await shot(page, '01-create-form.png');

      const submit = page.locator('#ml-studio-create-modal tf-button', { hasText: 'Utwórz projekt' }).first();
      await submit.click();
      await page.waitForSelector('#ml-studio-create-modal', { state: 'detached', timeout: 15000 }).catch(() => {});
      await page.waitForSelector('#ml-studio-detail tf-detail-header, .ml-studio-card', { timeout: 15000 });
      await page.waitForTimeout(800);

      createdProjectId = await page.evaluate(() => {
        const h = location.hash || location.search || '';
        const m = h.match(/projectId[=:/]+([0-9a-fA-F-]{36})/);
        return m ? m[1] : null;
      });

      // Reach the list, then open the share screen for the new project.
      const back = page.locator('#ml-studio-back');
      if (await back.count()) await back.click();
      else await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-list .ml-studio-card', { timeout: 15000 });
      await page.waitForTimeout(400);

      await openShare(page, PROJECT_NAME);
      const before = await readMembers(page);
      await shot(page, '02-share-before-invite.png');

      const hasInviteForm = await page.locator('#ml-studio-invite-send').count();
      step('1. Admin: projekt utworzony + ekran udostępniania z formularzem Zaproś', hasInviteForm > 0,
        `projectId=${createdProjectId ?? 'n/d'}; wierszy członków: ${before.rowCount} [${before.rowTexts.join(' || ')}]; formularz zaproszenia: ${hasInviteForm > 0}`);
    } catch (e) {
      await shot(page, '01-setup-FAIL.png');
      step('1. Admin: projekt + ekran udostępniania', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: invite ZWYKLY (plain user) — MUST be REJECTED ----
    try {
      await sendInvite(page, ZWYKLY_ID);
      // Allow time for the server PolicyDenied/validation response + any toast/reload.
      await page.waitForTimeout(2500);
      const toast = await readToast(page);
      const after = await readMembers(page);
      await shot(page, '03-invite-zwykly-rejected.png');

      // Rejection criteria: zwykly NOT on the members list of THIS project.
      const rejected = !after.hasZwykly;
      const errorToast = /power\s*user|nie\s*można|odmow|policy|denied|błąd|tylko\s+u/i.test(toast);
      step('2. Zwykły user (...aa) ODRZUCONY (tylko Power User)', rejected,
        rejected
          ? `Zwykly NIE pojawił się na liście (poprawne odrzucenie). Komunikat/toast: "${toast || '(brak/nieprzechwycony)'}"; toast wskazuje błąd: ${errorToast}; wierszy: ${after.rowCount}`
          : `FAIL WALIDACJI: zwykly (...aa) POJAWIŁ SIĘ na liście członków mimo że NIE jest Power Userem. Toast: "${toast}". Wiersze: [${after.rowTexts.join(' || ')}]`);
    } catch (e) {
      await shot(page, '03-invite-zwykly-FAIL.png');
      step('2. Zwykły user (...aa) ODRZUCONY', false, `Błąd: ${e.message}`);
    }

    // ---- Step 3: invite POWER1 (power_user) — MUST be ACCEPTED ----
    try {
      await sendInvite(page, POWER1_ID);
      await page.waitForFunction((id) => {
        const t = document.querySelector('#ml-studio-members');
        if (!t || !t.shadowRoot) return false;
        return [...t.shadowRoot.querySelectorAll('tbody tr')].some((r) => r.textContent.includes(id));
      }, POWER1_ID, { timeout: 12000 }).catch(() => {});
      await page.waitForTimeout(500);
      const toast = await readToast(page);
      const after = await readMembers(page);
      await shot(page, '04-invite-power1-accepted.png');

      const power1Row = after.rowTexts.find((tx) => tx.includes(POWER1_ID)) || '';
      const isEditor = /edytor/i.test(power1Row);
      const isPendingOrActive = /oczekuj|aktywn|zapros/i.test(power1Row);
      const accepted = after.hasPower1;
      step('3. Power User (...bb) PRZYJĘTY (rola Edytor)', accepted,
        accepted
          ? `power1 na liście: "${power1Row}" (rola Edytor: ${isEditor}, status oczekuje/aktywny: ${isPendingOrActive}); toast: "${toast || '(brak)'}"`
          : `FAIL: power1 (...bb) NIE pojawił się na liście mimo że jest Power Userem. Toast: "${toast}". Wiersze: [${after.rowTexts.join(' || ')}]`);
    } catch (e) {
      await shot(page, '04-invite-power1-FAIL.png');
      step('3. Power User (...bb) PRZYJĘTY', false, `Błąd: ${e.message}`);
    }

    // ---- Step 4: DB confirmation scoped to the fresh project ----
    try {
      let dbRows = '';
      if (createdProjectId) {
        dbRows = execSync(
          `sqlite3 ${ML_DB} "SELECT project_id,user_id,role,status FROM project_members WHERE project_id='${createdProjectId}' ORDER BY rowid DESC;"`,
          { encoding: 'utf8' }
        ).trim();
      } else {
        // Fall back: resolve the project_id by name (projects PK is `project_id`).
        dbRows = execSync(
          `sqlite3 ${ML_DB} "SELECT pm.project_id,pm.user_id,pm.role,pm.status FROM project_members pm JOIN projects p ON p.project_id=pm.project_id WHERE p.name='${PROJECT_NAME.replace(/'/g, "''")}' ORDER BY pm.rowid DESC;"`,
          { encoding: 'utf8' }
        ).trim();
      }
      const lines = dbRows ? dbRows.split('\n') : [];
      const hasZwyklyDb = lines.some((l) => l.includes(ZWYKLY_ID));
      const hasPower1Db = lines.some((l) => l.includes(POWER1_ID));
      const ok = hasPower1Db && !hasZwyklyDb;
      step('4. Baza ml_studio.db: ...bb obecny, ...aa NIEobecny (świeży projekt)', ok,
        `project_members dla projektu ${createdProjectId ?? PROJECT_NAME}:\n      ${lines.join('\n      ') || '(brak wierszy)'}\n      → power1 (...bb) w bazie: ${hasPower1Db}; zwykly (...aa) w bazie: ${hasZwyklyDb}`);
    } catch (e) {
      step('4. Baza ml_studio.db', false, `Błąd zapytania SQL: ${e.message}`);
    }

    // ---- Step 5: regression — admin still lists projects normally ----
    try {
      await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-list', { timeout: 15000 });
      await page.waitForFunction(() => {
        const list = document.querySelector('#ml-studio-list');
        return list && list.querySelector('.ml-studio-card');
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(400);
      const reg = await page.evaluate((pname) => {
        const cards = [...document.querySelectorAll('#ml-studio-list .ml-studio-card')];
        const found = cards.some((c) => c.querySelector('.ml-studio-card-name')?.textContent.trim() === pname);
        return { cardCount: cards.length, found };
      }, PROJECT_NAME);
      await shot(page, '05-regression-list.png');
      step('5. Regresja: admin nadal widzi listę projektów (plaster nie zepsuty)', reg.cardCount > 0 && reg.found,
        `Kart na liście: ${reg.cardCount}; nowy projekt widoczny: ${reg.found}`);
    } catch (e) {
      await shot(page, '05-regression-FAIL.png');
      step('5. Regresja: lista projektów', false, `Błąd: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL (admin): ${fatal.message}`);
  }

  await ctxA.close();

  // ---- Step 6 (optional): §11.2 nav gate for plain user ----
  const ctxB = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const pageB = await ctxB.newPage();
  const diagB = attachDiagnostics(pageB, 'zwykly');
  try {
    await login(pageB, 'zwykly', 'admin');
    await shot(pageB, '06-zwykly-sidebar.png');
    const gate = await pageB.evaluate(() => {
      const items = [...document.querySelectorAll('.sidebar .nav-item[data-view]')].map((el) => el.dataset.view);
      const roleText = document.querySelector('.user-chip .role')?.textContent.trim() ?? null;
      return { items, roleText, hasMlStudio: items.includes('ml-studio') };
    });
    step('6. §11.2 — zwykły user NIE widzi „ML Studio" w nav', !gate.hasMlStudio,
      `Rola w UI: "${gate.roleText}"; nav: [${gate.items.join(', ')}]`);
  } catch (e) {
    await shot(pageB, '06-zwykly-FAIL.png');
    step('6. §11.2 gate Power User', false, `Błąd: ${e.message}`);
  }
  await ctxB.close();

  // ---- Report ----
  const consoleErrors = [...diagA.consoleErrors, ...diagB.consoleErrors];
  const failedRequests = [...diagA.failedRequests, ...diagB.failedRequests];

  console.log('\n================ KONSOLA / SIEĆ ================');
  console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
  consoleErrors.slice(0, 40).forEach((e) => console.log('  JS> ' + e));
  console.log(`Nieudane żądania / WS: ${failedRequests.length}`);
  failedRequests.slice(0, 40).forEach((e) => console.log('  NET> ' + e));

  console.log('\n================ PODSUMOWANIE =================');
  results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} — ${r.note}`));
  const allPass = results.length > 0 && results.every((r) => r.pass);
  console.log(`\nZrzuty: ${SHOT}/`);
  console.log(`WYNIK OGÓLNY: ${allPass ? 'PASS' : 'FAIL'}`);

  await browser.close();
  process.exit(allPass ? 0 : 1);
})();
