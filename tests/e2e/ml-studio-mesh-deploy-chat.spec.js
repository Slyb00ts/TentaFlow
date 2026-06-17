// =============================================================================
// File: tests/e2e/ml-studio-mesh-deploy-chat.spec.js
// Description: LIVE e2e of DEPLOY + USE of an FT LLM model over mesh. The model
//              `b9323d8b…` was trained+exported on Node B (its `gguf_path` lives
//              on B). From Node A (https://localhost:8095, power1) we open the FT
//              project, DEPLOY it from the "Modele" tab — service_manifest_deploy
//              forwards the deploy to B (ServiceDeployRemote), B loads its own
//              local GGUF — then QUERY it ("Zapytaj"): A sends MlChat over mesh,
//              B runs inference on its deployed engine and returns text. Proves
//              the train→export→DEPLOY→USE loop closes from A for a model on B.
//              Standalone node script; screenshots in /tmp/mlstudio-meshchat/.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-meshchat';
const PROJECT_ID = '955a7407-4911-43df-9e5d-dcbd3b9ab010'; // FT Capstone (power1)
const MODEL_PREFIX = 'b9323d8b'; // FT model trained+exported on Node B

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}
async function shot(page, file) {
  await page.screenshot({ path: `${SHOT}/${file}`, fullPage: true }).catch(() => {});
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  const consoleErrors = [];
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(msg.text()); });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));

  try {
    // ---- Step 1: login → ML Studio → open FT project → Modele tab ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      await page.locator('#login-username input').first().fill('power1');
      await page.locator('#login-password input').first().fill('power123');
      await page.locator('#login-submit').click();
      await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });
      await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      await page.waitForTimeout(600);

      const card = page.locator(`[data-project-id="${PROJECT_ID}"]`).first();
      await card.waitFor({ state: 'visible', timeout: 15000 });
      await card.click();
      await page.waitForTimeout(800);
      const modeleTab = page.locator('#ml-studio-tabs tf-tab[label="Modele"]');
      await modeleTab.waitFor({ state: 'attached', timeout: 15000 });
      await modeleTab.click();
      await page.waitForSelector('#ml-studio-models-table tf-table', { timeout: 15000 });
      await page.waitForTimeout(800);
      await shot(page, '01-modele.png');
      step('1. Login → projekt FT → zakładka Modele', true, `Projekt ${PROJECT_ID} otwarty`);
    } catch (e) {
      await shot(page, '01-FAIL.png');
      step('1. Login → projekt FT → zakładka Modele', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: DEPLOY the mesh model from A (routes to B) ----
    try {
      // The row for the exported-but-not-deployed FT model shows a "Wdróż" action.
      // Click it via the table's rowActions builder result in the shadow DOM.
      const deployed = await page.evaluate(async (prefix) => {
        const table = document.querySelector('#ml-studio-models-table tf-table');
        const rows = table?.rows || [];
        const row = rows.find((r) => String(r._modelId || '').startsWith(prefix));
        if (!row) return { ok: false, msg: 'brak wiersza modelu' };
        const alreadyDeployed = Boolean(row._canChat);
        // Deploy through the exact protocol call the "Wdróż" button issues. The
        // handler routes the deploy to the model's owner node over mesh.
        const resp = await window.ApiBinary.one('mlStudioFtDeployRequest', { modelId: row._modelId });
        const status = String(resp?.status || '');
        return {
          ok: status === 'deploying' || alreadyDeployed,
          msg: resp?.error || status || 'brak statusu',
          modelName: resp?.modelName,
          uiHadDeployBtn: Boolean(row._canDeploy),
          uiHadChatBtn: alreadyDeployed,
        };
      }, MODEL_PREFIX);
      await shot(page, '02-deploy.png');
      step('2. Deploy modelu z mesh (A→B, ServiceDeployRemote)', deployed.ok,
        deployed.ok ? `status=${deployed.msg}${deployed.modelName ? ` alias=${deployed.modelName}` : ''} (UI: Wdróż=${deployed.uiHadDeployBtn}, Zapytaj=${deployed.uiHadChatBtn})` : `nie udało się: ${deployed.msg}`);
      if (!deployed.ok) throw new Error(deployed.msg);
    } catch (e) {
      await shot(page, '02-FAIL.png');
      step('2. Deploy modelu z mesh', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 3: USE the model — chat over mesh (A → MlChat → B → answer) ----
    try {
      // Embedded GGUF load on B can take a while; poll the chat call up to ~150s.
      let answer = '';
      let lastErr = '';
      const deadline = Date.now() + 150000;
      while (Date.now() < deadline) {
        const out = await page.evaluate(async (prefix) => {
          const table = document.querySelector('#ml-studio-models-table tf-table');
          const rows = table?.rows || [];
          const row = rows.find((r) => String(r._modelId || '').startsWith(prefix));
          const modelId = row?._modelId;
          if (!modelId) return { answer: '', error: 'brak modelu' };
          try {
            const resp = await window.ApiBinary.one('mlStudioFtChatRequest', {
              modelId, message: 'Jaka jest stolica Polski? Odpowiedz krótko.', maxTokens: 40,
            });
            return { answer: String(resp?.answer ?? '').trim(), error: resp?.error || '' };
          } catch (e) { return { answer: '', error: String(e.message || e) }; }
        }, MODEL_PREFIX);
        if (out.answer) { answer = out.answer; break; }
        lastErr = out.error || 'pusta odpowiedź';
        if (!/not found|nie.*znalez|loading|ładow|503|starting/i.test(lastErr) && lastErr !== 'pusta odpowiedź') {
          // Hard error unrelated to warmup — stop early.
          if (/not trusted|peer|mesh/i.test(lastErr)) break;
        }
        await page.waitForTimeout(5000);
      }
      await shot(page, '03-chat.png');
      const pass = !!answer;
      step('3. Zapytanie modelu przez mesh (A→MlChat→B)', pass,
        pass ? `odpowiedź="${answer.slice(0, 160)}"` : `brak odpowiedzi w 150s; ostatnio: ${lastErr}`);
    } catch (e) {
      await shot(page, '03-FAIL.png');
      step('3. Zapytanie modelu przez mesh', false, `Błąd: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL: ${fatal.message}`);
  } finally {
    console.log('\n================ KONSOLA ================');
    console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
    consoleErrors.slice(0, 20).forEach((e) => console.log('  JS> ' + e));
    console.log('\n================ PODSUMOWANIE =================');
    results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} — ${r.note}`));
    const allPass = results.length > 0 && results.every((r) => r.pass);
    console.log(`\nWYNIK OGÓLNY: ${allPass ? 'PASS' : 'FAIL'}`);
    await browser.close();
    process.exit(allPass ? 0 : 1);
  }
})();
