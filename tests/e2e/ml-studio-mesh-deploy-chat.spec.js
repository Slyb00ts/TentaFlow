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

    // ---- Step 2: DEPLOY the mesh model from A by clicking the UI "Wdróż" button ----
    try {
      // Confirm the row exposes the deploy/chat affordance (rebuilt JS).
      const flags = await page.evaluate((prefix) => {
        const table = document.querySelector('#ml-studio-models-table tf-table');
        const row = (table?.rows || []).find((r) => String(r._modelId || '').startsWith(prefix));
        return row ? { found: true, canDeploy: !!row._canDeploy, canChat: !!row._canChat } : { found: false };
      }, MODEL_PREFIX);
      if (!flags.found) throw new Error('brak wiersza modelu w tabeli');

      if (flags.canChat) {
        step('2. Deploy modelu z mesh (A→B)', true, 'model już wdrożony wcześniej — pomijam Wdróż');
      } else {
        if (!flags.canDeploy) throw new Error('model nie pokazuje akcji Wdróż (brak eksportu?)');
        // tf-button text pierces shadow DOM via Playwright text engine.
        const deployBtn = page.getByText('Wdróż', { exact: true }).first();
        await deployBtn.waitFor({ state: 'visible', timeout: 10000 });
        await deployBtn.click();
        // deployFtModel → toast + renderModelsTab; the "Zapytaj" action appears
        // once the model metrics carry inference_model_name. Poll the row flag.
        await page.waitForFunction((prefix) => {
          const table = document.querySelector('#ml-studio-models-table tf-table');
          const row = (table?.rows || []).find((r) => String(r._modelId || '').startsWith(prefix));
          return row && row._canChat;
        }, MODEL_PREFIX, { timeout: 30000 });
        step('2. Deploy modelu z mesh (A→B, klik „Wdróż")', true, 'deploy OK — pojawiła się akcja „Zapytaj"');
      }
      await shot(page, '02-deploy.png');
    } catch (e) {
      await shot(page, '02-FAIL.png');
      step('2. Deploy modelu z mesh', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 3: USE the model — click "Zapytaj", send a prompt, read the answer ----
    try {
      // Open the chat modal via the row "Zapytaj" action (before the modal's own send btn exists).
      const askBtn = page.getByText('Zapytaj', { exact: true }).first();
      await askBtn.waitFor({ state: 'visible', timeout: 10000 });
      await askBtn.click();
      const input = page.locator('#ml-studio-chat-input textarea').first();
      await input.waitFor({ state: 'visible', timeout: 10000 });
      await input.fill('Jaka jest stolica Polski? Odpowiedz krótko.');

      // Embedded GGUF load on B can take a while; retry send until an answer renders.
      let answer = '';
      let lastErr = '';
      const deadline = Date.now() + 160000;
      while (Date.now() < deadline) {
        await page.locator('#ml-studio-chat-send').click();
        try {
          await page.waitForFunction(() => {
            const host = document.querySelector('#ml-studio-chat-answer');
            if (!host) return false;
            const pre = host.querySelector('.ml-studio-chat-text');
            if (pre && pre.textContent.trim()) return true;
            // error bubble also resolves the wait so we can read it
            return /nieudane/i.test(host.textContent || '');
          }, { timeout: 20000 }).catch(() => {});
          const out = await page.evaluate(() => {
            const host = document.querySelector('#ml-studio-chat-answer');
            const pre = host?.querySelector('.ml-studio-chat-text');
            return { answer: pre ? pre.textContent.trim() : '', html: host ? host.textContent.trim() : '' };
          });
          if (out.answer) { answer = out.answer; break; }
          lastErr = out.html || 'pusta odpowiedź';
        } catch (e) { lastErr = String(e.message || e); }
        await page.waitForTimeout(5000);
      }
      await shot(page, '03-chat.png');
      const pass = !!answer;
      step('3. Zapytanie modelu przez mesh (UI „Zapytaj" → A→MlChat→B)', pass,
        pass ? `odpowiedź="${answer.slice(0, 160)}"` : `brak odpowiedzi w 160s; ostatnio: ${lastErr.slice(0, 160)}`);
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
