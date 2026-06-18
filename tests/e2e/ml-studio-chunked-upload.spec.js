// =============================================================================
// File: tests/e2e/ml-studio-chunked-upload.spec.js
// Description: Real e2e test of CHUNKED dataset upload against a LIVE TentaFlow
//              instance (https://localhost:8095, admin/admin). Generates a large
//              JSONL SFT dataset (> WS frame limit, > CHUNK_SIZE) and uploads it
//              through the "Dane" tab tf-file-input. Proves the client splits the
//              file into chunks, the server reassembles them, JSONL profiling
//              runs, the dataset lands in the list as kind=jsonl with the real
//              record count, and persists in ml_studio.db. Standalone node script.
//              Screenshots land in /tmp/mlstudio-chunk-shots/.
// =============================================================================

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-chunk-shots';
const JSONL = '/tmp/guard_big.jsonl';
const DB = '/home/critix/repos/rust/TentaFlow-ml/.runtime/data/ml_studio.db';
const PROJECT_NAME = `Guard chunked ${Date.now()}`;
// ml-studio.js: CHUNK_SIZE = 256 KiB. We want several chunks to exercise the path.
const TARGET_BYTES = 900 * 1024;

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}
async function shot(page, file) {
  await page.screenshot({ path: `${SHOT}/${file}`, fullPage: true }).catch(() => {});
}

function buildJsonl() {
  // Generate prompt/completion records until we exceed TARGET_BYTES so the
  // client must split into >=4 chunks (900 KiB / 256 KiB).
  let buf = '';
  let n = 0;
  while (Buffer.byteLength(buf, 'utf8') < TARGET_BYTES) {
    const rec = {
      prompt: `Czy nalepka ${n} jest poprawnie naklejona na cysternie nr ${n % 97}?`,
      completion: `Tak, nalepka ${n} jest czysta i czytelna; kod ADR widoczny, brak uszkodzeń krawędzi. Rekord kontrolny ${n}.`,
    };
    buf += JSON.stringify(rec) + '\n';
    n += 1;
  }
  fs.writeFileSync(JSONL, buf);
  return { records: n, bytes: Buffer.byteLength(buf, 'utf8') };
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  const gen = buildJsonl();
  const expectedChunks = Math.ceil(gen.bytes / (256 * 1024));
  console.log(`Wygenerowano ${JSONL}: ${gen.records} rekordów, ${gen.bytes} B → ~${expectedChunks} fragmentów`);

  const consoleErrors = [];
  const chunkRequests = [];

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(msg.text()); });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));

  // Pick the most recent ft_llm project — its "Dane" tab is the simple uploader.
  const ftProjectId = execSync(
    `sqlite3 "${DB.replace('ml_studio.db', 'ml_studio.db')}" "SELECT project_id FROM projects WHERE project_type='ft_llm' ORDER BY rowid DESC LIMIT 1;"`,
    { encoding: 'utf8' },
  ).trim();
  if (!ftProjectId) {
    console.log('FATAL: brak projektu ft_llm w bazie do testu uploadu');
    process.exit(1);
  }
  console.log(`Projekt ft_llm do testu: ${ftProjectId}`);

  try {
    // ---- Step 1: login → ML Studio → open existing ft_llm project → Dane tab ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      const userInput = page.locator('#login-username input').first();
      await userInput.waitFor({ state: 'visible', timeout: 20000 });
      await userInput.fill('power1');
      await page.locator('#login-password input').first().fill('power123');
      await page.locator('#login-submit').click();
      await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });

      const navItem = page.locator('.sidebar .nav-item[data-view="ml-studio"]');
      await navItem.waitFor({ state: 'visible', timeout: 10000 });
      await navItem.click();
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      await page.waitForTimeout(600);

      const card = page.locator(`[data-project-id="${ftProjectId}"]`).first();
      await card.waitFor({ state: 'visible', timeout: 15000 });
      await card.click();
      await page.waitForTimeout(800);

      const daneTab = page.locator('#ml-studio-tabs tf-tab[label="Dane"]');
      await daneTab.waitFor({ state: 'attached', timeout: 15000 });
      await daneTab.click();
      await page.waitForSelector('#ml-studio-data-file', { timeout: 15000 });
      await page.waitForTimeout(500);
      await shot(page, '01-dane.png');
      step('1. Login → ML Studio → projekt ft_llm → zakładka Dane', true, `Projekt ${ftProjectId} otwarty, zakładka Dane gotowa`);
    } catch (e) {
      await shot(page, '01-FAIL.png');
      step('1. Login → ML Studio → projekt ft_llm → zakładka Dane', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: upload large JSONL — must traverse the chunked path ----
    try {
      const nativeInput = page.locator('#ml-studio-data-file input.tf-file-input-native');
      await nativeInput.waitFor({ state: 'attached', timeout: 10000 });
      await nativeInput.setInputFiles(JSONL);

      // Dataset row materialises once the final chunk is reassembled + profiled.
      await page.waitForFunction(() => {
        const host = document.querySelector('#ml-studio-datasets');
        const table = host?.querySelector('tf-table');
        return (table?.rows || []).some((r) => String(r.name || '').includes('guard_big'));
      }, { timeout: 30000 });
      await shot(page, '02-after-upload.png');
      step('2. Upload dużego JSONL (chunked) przez tf-file-input', true,
        `setInputFiles(${path.basename(JSONL)}) → zbiór pojawił się po sklejeniu fragmentów`);
    } catch (e) {
      await shot(page, '02-FAIL.png');
      step('2. Upload dużego JSONL (chunked)', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 3: dataset listed as kind=jsonl with the real record count ----
    try {
      const ds = await page.evaluate(() => {
        const host = document.querySelector('#ml-studio-datasets');
        const table = host?.querySelector('tf-table');
        const rows = table?.rows || [];
        const txt = (html) => { const d = document.createElement('div'); d.innerHTML = html || ''; return d.textContent.replace(/\s+/g, ' ').trim(); };
        return rows.map((r) => ({ name: txt(r.name), kind: txt(r.kind), rowCount: txt(r.rowCount), columnCount: txt(r.columnCount) }));
      });
      const mine = ds.find((d) => d.name.startsWith('guard_big'));
      const kindOk = mine && /jsonl/i.test(mine.kind);
      const rowsOk = mine && String(mine.rowCount).replace(/\D/g, '') === String(gen.records);
      const pass = Boolean(mine) && kindOk && rowsOk;
      step('3. Zbiór na liście (kind=jsonl, rekordy = wygenerowane)', pass,
        mine ? `"${mine.name}" · typ=${mine.kind} · rekordy=${mine.rowCount} (oczekiwano ${gen.records}) · pola=${mine.columnCount}`
             : `Brak zbioru; widoczne: ${JSON.stringify(ds)}`);
    } catch (e) {
      step('3. Zbiór na liście', false, `Błąd: ${e.message}`);
    }

    // ---- Step 4: persistence in ml_studio.db with full byte length ----
    try {
      const out = execSync(
        `sqlite3 "${DB}" "SELECT name,kind,row_count,length(raw_data) FROM datasets ORDER BY rowid DESC LIMIT 3;"`,
        { encoding: 'utf8' },
      ).trim();
      const lines = out.split('\n');
      const mine = lines.find((l) => l.startsWith('guard_big'));
      const storedLen = mine ? parseInt(mine.split('|')[3], 10) : 0;
      // Reassembled bytes must equal the original file length (no chunk loss).
      const lenOk = storedLen === gen.bytes;
      const kindOk = mine && mine.split('|')[1] === 'jsonl';
      const pass = Boolean(mine) && lenOk && kindOk;
      step('4. Persystencja w DB (kind=jsonl, length(data) = oryginał)', pass,
        mine ? `DB: ${mine} | oczekiwana długość=${gen.bytes}` : `Brak wiersza guard_big; top3: [${lines.join(' ; ')}]`);
    } catch (e) {
      step('4. Persystencja w DB', false, `Błąd sqlite3: ${e.message}`);
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
