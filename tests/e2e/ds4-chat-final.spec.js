const { chromium } = require('playwright');
const BASE = 'https://127.0.0.1:8090';
(async () => {
  const b = await chromium.launch({ headless: true });
  const ctx = await b.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  const log = (m) => console.log(`[chat] ${m}`);
  try {
    await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded' });
    await page.locator('#login-username input').first().fill('admin');
    await page.locator('#login-password input').first().fill('admin');
    await page.locator('#login-submit').click();
    await page.waitForSelector('[data-view]', { timeout: 20000 });
    await page.locator('[data-view="chat"]').first().click();
    await page.waitForTimeout(2000);
    if (await page.locator('#chat-new').count()) { await page.locator('#chat-new').click(); await page.waitForTimeout(1200); }
    // Chat has no flow picker: every turn must go to Default Chat by its id.
    if (await page.locator('#chat-flow').count()) throw new Error('#chat-flow selector still rendered — chat must run Default Chat only');
    const q = 'W jednym zdaniu: jaka jest stolica Polski i nad jaka rzeka lezy?';
    await page.locator('#chat-input textarea, #chat-input input').first().fill(q);
    await page.locator('#chat-send').click();
    log(`Q: ${q}`);
    // Wait until the assistant bubble text stabilizes (streaming done)
    let last = '', stable = 0;
    for (let i = 0; i < 60; i++) {
      await page.waitForTimeout(1500);
      const t = await page.evaluate(() => { const b=Array.from(document.querySelectorAll('.msg-row.assistant .bubble')); return b.length?(b[b.length-1].textContent||'').trim():''; });
      if (t && t === last) { stable++; if (stable >= 3) break; } else { stable = 0; }
      last = t;
    }
    const perf = await page.evaluate(() => { const p=Array.from(document.querySelectorAll('.msg-row.assistant .bubble-perf')).pop(); return p?(p.textContent||'').trim():''; });
    log(`FULL ANSWER:\n${last}`);
    log(`PERF: ${perf}`);
    await b.close(); process.exit(last ? 0 : 2);
  } catch (e) { log(`FAIL: ${e.message}`); await b.close(); process.exit(1); }
})();
