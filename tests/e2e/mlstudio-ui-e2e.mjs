import { chromium } from 'playwright';

const A_URL = 'https://localhost:8095/';
const CREDS = { username: 'power1', password: 'power123' };
const SHOTS = '/tmp/mlstudio-ui';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  const errs = [];
  page.on('console', (m) => { if (m.type() === 'error') errs.push(m.text()); });
  const shot = async (n) => { await page.screenshot({ path: `${SHOTS}-${n}.png`, fullPage: false }); console.log('  shot:', n); };

  try {
    await page.goto(A_URL, { waitUntil: 'domcontentloaded' });
    // Login via the REAL form (tf-input web components).
    await page.waitForSelector('#login-username', { timeout: 15000 });
    await page.evaluate((creds) => {
      const setV = (id, v) => {
        const el = document.getElementById(id);
        el.value = v;
        const inner = el.shadowRoot?.querySelector('input') || el.querySelector('input');
        if (inner) { inner.value = v; inner.dispatchEvent(new Event('input', { bubbles: true })); }
        el.dispatchEvent(new Event('input', { bubbles: true }));
      };
      setV('login-username', creds.username);
      setV('login-password', creds.password);
    }, CREDS);
    await sleep(300);
    await page.evaluate(() => { document.getElementById('login-form').requestSubmit(); });
    // Wait until logged in (login form gone / app shell present).
    await page.waitForFunction(() => !document.getElementById('login-username'), { timeout: 20000 }).catch(() => {});
    await sleep(2000);
    console.log('[1] logged in as', CREDS.username);
    await shot('1-after-login');

    // Navigate to ML Studio via the global Router.
    await page.evaluate(() => window.Router?.navigate('ml-studio'));
    await sleep(3000);
    const mlVisible = await page.evaluate(() => /ml.?studio/i.test(document.body.innerText) || !!document.querySelector('[class*="ml-studio"]'));
    console.log('[2] ML Studio module visible:', mlVisible);
    await shot('2-mlstudio-projects');

    // Count projects shown + grab their names.
    const projInfo = await page.evaluate(() => {
      const cards = Array.from(document.querySelectorAll('[class*="ml-studio"] [class*="project"], [data-project-id], .ml-studio-project-card'));
      const txt = document.body.innerText;
      return { cardCount: cards.length, hasAdr: /adr|annot|recogn/i.test(txt), hasFt: /capstone|guard|ft|fine/i.test(txt) };
    });
    console.log('[3] projects on screen:', JSON.stringify(projInfo));

    // Open first project (click a project card/link).
    const opened = await page.evaluate(() => {
      const el = document.querySelector('[data-project-id], .ml-studio-project-card, [class*="project-card"]');
      if (el) { el.click(); return true; }
      // fallback: first clickable row in ml-studio
      const row = document.querySelector('[class*="ml-studio"] a, [class*="ml-studio"] [role="button"]');
      if (row) { row.click(); return true; }
      return false;
    });
    await sleep(2500);
    console.log('[4] opened a project:', opened);
    await shot('3-project-detail');

    // Look for ML Studio tabs (Dane/Anotacje/Treningi/Modele or Model bazowy/Trening).
    const tabs = await page.evaluate(() => {
      const t = Array.from(document.querySelectorAll('tf-tabs, [role="tab"], [class*="tab"]')).map((e) => e.textContent?.trim()).filter(Boolean);
      return { tabsText: document.body.innerText.match(/Dane|Anotacje|Treningi|Modele|Model bazowy|Trening|Schemat/g) || [], tabEls: t.slice(0, 12) };
    });
    console.log('[5] ML Studio tabs detected:', JSON.stringify(tabs.tabsText));
    await shot('4-tabs');

    console.log('UI_E2E_DONE; console_errors=' + errs.length);
    if (errs.length) console.log('  errs:', errs.slice(0, 5).join(' | '));
  } catch (e) {
    console.error('UI_E2E_ERR:', e?.message || e);
    await shot('error');
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
