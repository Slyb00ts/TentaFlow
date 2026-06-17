// =============================================================================
// File: tests/e2e/ml-studio-mac-mlx-deploy.spec.js
// Description: LIVE e2e — deploy an MLX model on the Mac mini (Node C) and chat
//              with it, ENTIRELY over the binary protocol (no REST /v1). The
//              merged safetensors model was built on Node B and transferred to C
//              (~/mlx-models/ft-b9323d8b). A model record on C points node_id→C
//              and gguf_path→that dir. We drive C's dashboard (https://C:18093):
//              login → ML Studio → project → Modele → "Wdróż" (ml_studio_ft_deploy
//              → service_manifest_deploy LOCAL on C → embedded MLX load+register)
//              → "Zapytaj" (mlStudioFtChatRequest → run_local_chat → MLX engine).
//              Screenshots in /tmp/mac-mlx-shots/.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://192.168.11.23:18093';
const SHOT = '/tmp/mac-mlx-shots';
const PROJECT_ID = 'mlx-mac-proj-0001';
const MODEL_PREFIX = 'b9323d8bmlxmac';

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
    // ---- Step 1: login → ML Studio → open project → Modele ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      await page.locator('#login-username input').first().fill('admin');
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
      step('1. Login (binary) → projekt → Modele', true, `Node C, projekt ${PROJECT_ID}`);
    } catch (e) {
      await shot(page, '01-FAIL.png');
      step('1. Login → projekt → Modele', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: deploy MLX on the Mac via the "Wdróż" button (binary protocol) ----
    try {
      const flags = await page.evaluate((prefix) => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        const r = (t?.rows || []).find((x) => String(x._modelId || '').startsWith(prefix));
        return r ? { found: true, canDeploy: !!r._canDeploy, canChat: !!r._canChat } : { found: false };
      }, MODEL_PREFIX);
      if (!flags.found) throw new Error('brak wiersza modelu');
      if (!flags.canChat) {
        if (!flags.canDeploy) throw new Error(`model nie pokazuje Wdróż (canDeploy=${flags.canDeploy})`);
        await page.getByText('Wdróż', { exact: true }).first().click();
        // Deploy ładuje MLX (988 MB safetensors) — poczekaj aż pojawi się „Zapytaj".
        await page.waitForFunction((prefix) => {
          const t = document.querySelector('#ml-studio-models-table tf-table');
          const r = (t?.rows || []).find((x) => String(x._modelId || '').startsWith(prefix));
          return r && r._canChat;
        }, MODEL_PREFIX, { timeout: 180000 });
      }
      await shot(page, '02-deployed.png');
      step('2. Deploy MLX na Macu (binary „Wdróż" → service_manifest_deploy)', true, 'model wdrożony — akcja „Zapytaj" dostępna');
    } catch (e) {
      await shot(page, '02-FAIL.png');
      step('2. Deploy MLX na Macu', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 3: chat over the binary protocol (mlStudioFtChatRequest) ----
    try {
      await page.getByText('Zapytaj', { exact: true }).first().click();
      const input = page.locator('#ml-studio-chat-input textarea').first();
      await input.waitFor({ state: 'visible', timeout: 10000 });
      await input.fill('Jaka jest stolica Polski? Odpowiedz krótko.');
      let answer = '';
      let lastErr = '';
      const deadline = Date.now() + 180000;
      while (Date.now() < deadline) {
        await page.locator('#ml-studio-chat-send').click();
        await page.waitForFunction(() => {
          const host = document.querySelector('#ml-studio-chat-answer');
          if (!host) return false;
          const pre = host.querySelector('.ml-studio-chat-text');
          if (pre && pre.textContent.trim()) return true;
          return /nieudane/i.test(host.textContent || '');
        }, { timeout: 30000 }).catch(() => {});
        const out = await page.evaluate(() => {
          const host = document.querySelector('#ml-studio-chat-answer');
          const pre = host?.querySelector('.ml-studio-chat-text');
          return { answer: pre ? pre.textContent.trim() : '', html: host ? host.textContent.trim() : '' };
        });
        if (out.answer) { answer = out.answer; break; }
        lastErr = out.html || 'pusta';
        await page.waitForTimeout(6000);
      }
      await shot(page, '03-chat.png');
      step('3. Czat z modelem MLX (binary mlStudioFtChatRequest)', !!answer,
        answer ? `odpowiedź="${answer.slice(0, 200)}"` : `brak odpowiedzi 180s; ostatnio: ${lastErr.slice(0, 160)}`);
    } catch (e) {
      await shot(page, '03-FAIL.png');
      step('3. Czat z modelem MLX', false, `Błąd: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL: ${fatal.message}`);
  } finally {
    console.log('\n=== KONSOLA ==='); console.log(`JS errors: ${consoleErrors.length}`);
    consoleErrors.slice(0, 15).forEach((e) => console.log('  JS> ' + e));
    console.log('\n=== PODSUMOWANIE ===');
    results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} — ${r.note}`));
    const allPass = results.length > 0 && results.every((r) => r.pass);
    console.log(`\nWYNIK: ${allPass ? 'PASS' : 'FAIL'}`);
    await browser.close();
    process.exit(allPass ? 0 : 1);
  }
})();
