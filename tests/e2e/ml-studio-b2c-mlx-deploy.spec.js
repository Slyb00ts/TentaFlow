// =============================================================================
// File: tests/e2e/ml-studio-b2c-mlx-deploy.spec.js
// Description: LIVE 3-node e2e — model MLX zbudowany na Node B, deploy na Node C
//              (Mac) z auto-transferem artefaktu B→C przez mesh, czat z C. Wszystko
//              przez protokół binarny z dashboardu orkiestratora (Node A,
//              https://localhost:8095). Model record na A ma node_id=B + gguf_path
//              wskazujący artefakt na B. Deploy z target_node_id=C → Core zleca B
//              wypchnięcie artefaktu do C → deploy na C → czat routuje do C.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/b2c-mlx-shots';
const PROJECT_ID = 'mlx-b2c-0001';
const MODEL_PREFIX = 'mlxb2cmodel01';
const NODE_C = '9dd34a07a7a804374510f15ca5f62ccbe0c0e9111709639ee7a3dcec2156055f';

const results = [];
function step(name, pass, note) { results.push({ name, pass, note }); console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`); }
async function shot(page, f) { await page.screenshot({ path: `${SHOT}/${f}`, fullPage: true }).catch(() => {}); }

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  const errs = [];
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  page.on('console', (m) => { if (m.type() === 'error') errs.push(m.text()); });
  page.on('pageerror', (e) => errs.push('pageerror: ' + e.message));

  try {
    // ---- Step 1: login → projekt → Modele ----
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
      const tab = page.locator('#ml-studio-tabs tf-tab[label="Modele"]');
      await tab.waitFor({ state: 'attached', timeout: 15000 });
      await tab.click();
      await page.waitForSelector('#ml-studio-models-table tf-table', { timeout: 15000 });
      await page.waitForTimeout(800);
      step('1. Login → projekt B→C → Modele', true, `projekt ${PROJECT_ID}`);
    } catch (e) { await shot(page, '01-FAIL.png'); step('1. Login → Modele', false, e.message); throw e; }

    // ---- Step 2: Wdróż na Node C (transfer artefaktu B→C) ----
    try {
      await page.getByText('Wdróż', { exact: true }).first().click();
      await page.locator('#ml-studio-deploy-node').waitFor({ state: 'attached', timeout: 10000 });
      await page.waitForTimeout(1200); // poczekaj aż meshNodeListRequest wypełni select
      // Ustaw węzeł docelowy = C i odpal change (deployFtModel czyta detail.value).
      const set = await page.evaluate((nodeC) => {
        const sel = document.querySelector('#ml-studio-deploy-node');
        if (!sel) return 'brak select';
        try { sel.value = nodeC; } catch (_) {}
        sel.dispatchEvent(new CustomEvent('change', { detail: { value: nodeC } }));
        return 'ok';
      }, NODE_C);
      if (set !== 'ok') throw new Error('nie ustawiono węzła C: ' + set);
      await shot(page, '02-deploy-modal.png');
      await page.locator('#ml-studio-deploy-go').click();
      // Transfer ~1 GB B→C + deploy + rejestracja — czekamy aż pojawi się „Zapytaj".
      await page.waitForFunction((prefix) => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        const r = (t?.rows || []).find((x) => String(x._modelId || '').startsWith(prefix));
        return r && r._canChat;
      }, MODEL_PREFIX, { timeout: 360000 });
      await shot(page, '03-deployed.png');
      step('2. Deploy MLX B→C (transfer artefaktu + deploy na C)', true, 'artefakt przeniesiony, model wdrożony na C — „Zapytaj" dostępne');
    } catch (e) { await shot(page, '02-FAIL.png'); step('2. Deploy MLX B→C', false, e.message); throw e; }

    // ---- Step 3: czat (routuje do C przez mesh) ----
    try {
      await page.getByText('Zapytaj', { exact: true }).first().click();
      const input = page.locator('#ml-studio-chat-input textarea').first();
      await input.waitFor({ state: 'visible', timeout: 10000 });
      await input.fill('Jaka jest stolica Polski? Odpowiedz krótko.');
      let answer = '', last = '';
      const deadline = Date.now() + 200000;
      while (Date.now() < deadline) {
        await page.locator('#ml-studio-chat-send').click();
        await page.waitForFunction(() => {
          const h = document.querySelector('#ml-studio-chat-answer');
          if (!h) return false;
          const pre = h.querySelector('.ml-studio-chat-text');
          if (pre && pre.textContent.trim()) return true;
          return /nieudane/i.test(h.textContent || '');
        }, { timeout: 40000 }).catch(() => {});
        const out = await page.evaluate(() => {
          const h = document.querySelector('#ml-studio-chat-answer');
          const pre = h?.querySelector('.ml-studio-chat-text');
          return { answer: pre ? pre.textContent.trim() : '', html: h ? h.textContent.trim() : '' };
        });
        if (out.answer) { answer = out.answer; break; }
        last = out.html || 'pusta';
        await page.waitForTimeout(6000);
      }
      await shot(page, '04-chat.png');
      step('3. Czat z modelem na C (mesh A→C)', !!answer, answer ? `odpowiedź="${answer.slice(0, 200)}"` : `brak odp 200s; ostatnio: ${last.slice(0, 160)}`);
    } catch (e) { await shot(page, '04-FAIL.png'); step('3. Czat z modelem na C', false, e.message); }
  } catch (fatal) { console.log('FATAL: ' + fatal.message); }
  finally {
    console.log('\n=== KONSOLA === JS errors: ' + errs.length); errs.slice(0, 15).forEach((e) => console.log('  JS> ' + e));
    console.log('\n=== PODSUMOWANIE ===');
    results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} — ${r.note}`));
    const all = results.length > 0 && results.every((r) => r.pass);
    console.log(`\nWYNIK: ${all ? 'PASS' : 'FAIL'}`);
    await browser.close();
    process.exit(all ? 0 : 1);
  }
})();
