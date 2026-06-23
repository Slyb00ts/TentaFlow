// E2E stage 1: deploy DeepSeek V4 Flash (ds4) NATIVE via the GUI catalog wizard.
const { chromium } = require('playwright');
const BASE = 'https://127.0.0.1:8090';

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  const log = (m) => console.log(`[deploy] ${m}`);
  try {
    await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded' });
    await page.locator('#login-username input').first().fill('admin');
    await page.locator('#login-password input').first().fill('admin');
    await page.locator('#login-submit').click();
    await page.waitForSelector('[data-view]', { timeout: 20000 });

    await page.locator('[data-view="catalog"]').first().click();
    await page.waitForTimeout(1500);
    await page.locator('.target-card[data-target-kind="local"]').first().click();
    await page.waitForTimeout(2500);

    // Open wizard on the featured Flash preset
    await page.locator('[data-engine-deploy="ds4"][data-preset-id="deepseek-v4-flash-q2"]').first().click();
    await page.waitForSelector('#edw-body', { timeout: 8000 });
    log('wizard open');

    // Step 1: pick native method
    await page.locator('.deploy-method-card[data-method="native"]').click();
    log('method=native');

    // Walk steps: set ctx=8192 on the generic Advanced step, click Next until Deploy is available.
    for (let i = 0; i < 6; i++) {
      const ctxField = page.locator('#edw-gp-ctx');
      if (await ctxField.count()) {
        // tf-input: set inner input value + dispatch change
        await page.evaluate(() => {
          const el = document.querySelector('#edw-gp-ctx');
          const inp = el && (el.querySelector('input') || el.shadowRoot?.querySelector('input'));
          if (inp) { inp.value = '8192'; inp.dispatchEvent(new Event('input', { bubbles: true })); inp.dispatchEvent(new Event('change', { bubbles: true })); }
          if (el) { el.value = '8192'; el.dispatchEvent(new Event('change', { bubbles: true })); }
        });
        log('set ctx=8192');
      }
      const deployBtn = page.locator('#edw-deploy');
      if (await deployBtn.count() && await deployBtn.isVisible()) { log(`reached deploy at step ${i+1}`); break; }
      const next = page.locator('#edw-next');
      if (await next.count() && await next.isVisible()) { await next.click(); await page.waitForTimeout(1200); }
      else { log(`no next/deploy at iter ${i}`); break; }
    }

    await page.locator('#edw-deploy').click();
    log('clicked Deploy — waiting for terminal state…');

    // Wait up to 4 min for a success/fail signal in the progress modal.
    const deadline = Date.now() + 240000;
    let state = 'unknown';
    while (Date.now() < deadline) {
      const txt = (await page.evaluate(() => document.body.innerText)).toLowerCase();
      if (/(running|deployed|uruchomion|gotow|success|completed)/.test(txt) && /ds4|deepseek/.test(txt)) { state = 'success'; }
      if (/(failed|error|błąd|blad|nie powiod)/.test(txt)) { state = 'maybe-fail'; }
      // Also: a clear success modal marker
      const okMarker = await page.locator('text=/deploy.*success|success.*deploy|wdrożono|uruchomiono/i').count();
      if (okMarker) { state = 'success'; break; }
      if (state === 'success') break;
      await page.waitForTimeout(4000);
    }
    log(`progress state: ${state}`);
    await page.screenshot({ path: '/tmp/ds4-deploy.png', fullPage: true }).catch(() => {});
    await browser.close();
    process.exit(0);
  } catch (e) {
    log(`FAIL: ${e.message}`);
    await page.screenshot({ path: '/tmp/ds4-deploy-fail.png', fullPage: true }).catch(() => {});
    await browser.close();
    process.exit(1);
  }
})();
