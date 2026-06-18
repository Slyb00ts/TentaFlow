// Chat-only: model już wdrożony na C (transfer B→C + deploy potwierdzone w logach C).
// Z dashboardu A: projekt → Modele → „Zapytaj" → MlChat routuje do C → odpowiedź.
const { chromium } = require('playwright');
const BASE = 'https://localhost:8095';
const PROJECT_ID = 'mlx-b2c-0001';
(async () => {
  const b = await chromium.launch({ headless: true });
  const ctx = await b.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  let answer = '', last = '';
  try {
    await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
    await page.locator('#login-username input').first().fill('power1');
    await page.locator('#login-password input').first().fill('power123');
    await page.locator('#login-submit').click();
    await page.waitForSelector('.sidebar .nav-item[data-view="ml-studio"]', { timeout: 20000 });
    await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
    await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
    await page.waitForTimeout(600);
    await page.locator(`[data-project-id="${PROJECT_ID}"]`).first().click();
    await page.waitForTimeout(800);
    await page.locator('#ml-studio-tabs tf-tab[label="Modele"]').click();
    await page.waitForSelector('#ml-studio-models-table tf-table', { timeout: 15000 });
    await page.waitForTimeout(800);
    await page.getByText('Zapytaj', { exact: true }).first().click();
    const input = page.locator('#ml-studio-chat-input textarea').first();
    await input.waitFor({ state: 'visible', timeout: 10000 });
    await input.fill('Jaka jest stolica Polski? Odpowiedz krótko.');
    const deadline = Date.now() + 200000;
    while (Date.now() < deadline) {
      await page.locator('#ml-studio-chat-send').click();
      await page.waitForFunction(() => {
        const h = document.querySelector('#ml-studio-chat-answer');
        const pre = h && h.querySelector('.ml-studio-chat-text');
        return (pre && pre.textContent.trim()) || /nieudane/i.test((h && h.textContent) || '');
      }, { timeout: 40000 }).catch(() => {});
      const out = await page.evaluate(() => {
        const h = document.querySelector('#ml-studio-chat-answer');
        const pre = h && h.querySelector('.ml-studio-chat-text');
        return { a: pre ? pre.textContent.trim() : '', html: h ? h.textContent.trim() : '' };
      });
      if (out.a) { answer = out.a; break; }
      last = out.html; await page.waitForTimeout(6000);
    }
  } catch (e) { console.log('ERR: ' + e.message); }
  console.log(answer ? `PASS :: odpowiedź="${answer.slice(0, 200)}"` : `FAIL :: brak; ostatnio: ${last.slice(0, 160)}`);
  await b.close();
  process.exit(answer ? 0 : 1);
})();
