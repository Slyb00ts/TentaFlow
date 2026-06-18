// =============================================================================
// File: tests/e2e/ml-studio-data.spec.js
// Description: Real e2e test of the ML Studio "Dane" tab against a LIVE running
//              TentaFlow instance (https://localhost:8095, admin/admin). Drives
//              the SPA over the binary WebSocket protocol: login, create a
//              "Dane tabelaryczne i anomalie" project, open it, upload a CSV via
//              tf-file-input, and assert that the column profile is computed FROM
//              the user's file (categorical class counts, numeric/id typing,
//              missing-value ratio). Proves data provenance is real, not hardcode.
//              Standalone node script — does NOT spawn its own binary.
//              Screenshots land in /tmp/mlstudio-shots5/.
// =============================================================================

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-shots5';
const CSV = '/tmp/przesylki_test.csv';
const DB = '/home/critix/repos/rust/TentaFlow-ml/.runtime/data/ml_studio.db';
const PROJECT_NAME = `Cysterny dane ${Date.now()}`;

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
  if (!fs.existsSync(CSV)) {
    console.log(`FATAL: brak pliku testowego ${CSV}`);
    process.exit(1);
  }

  const consoleErrors = [];
  const failedRequests = [];

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();

  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(msg.text()); });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));
  page.on('requestfailed', (req) => {
    failedRequests.push(`${req.method()} ${req.url()} :: ${req.failure()?.errorText}`);
  });
  page.on('websocket', (ws) => {
    ws.on('socketerror', (e) => failedRequests.push(`WS error ${ws.url()} :: ${e}`));
  });

  // Captured profile facts for the final report.
  let profileFacts = null;

  try {
    // ---- Step 1: login + ML Studio + create tabular project + open detail + "Dane" tab ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      const userInput = page.locator('#login-username input').first();
      await userInput.waitFor({ state: 'visible', timeout: 20000 });
      await userInput.fill('admin');
      await page.locator('#login-password input').first().fill('admin');
      await page.locator('#login-submit').click();
      await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });
      await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
      await shot(page, '01-dashboard.png');

      const navItem = page.locator('.sidebar .nav-item[data-view="ml-studio"]');
      await navItem.waitFor({ state: 'visible', timeout: 10000 });
      await navItem.click();
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      await page.waitForTimeout(600);

      // Open the create wizard.
      await page.locator('#ml-studio-new').click();
      const nameInput = page.locator('#ml-studio-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 10000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-desc textarea').first().fill('e2e: profil kolumn z pliku usera');

      // Pick the tabular+anomaly type so that the detail view exposes a "Dane" tab.
      let typeChosen = '';
      const tabRadio = page.locator('#ml-studio-types tf-radio[value="tabular_anomaly"]');
      if (await tabRadio.count()) {
        await tabRadio.first().click();
        typeChosen = 'tabular_anomaly (Dane tabelaryczne i anomalie)';
      } else {
        // Fallback: recognition also has a "Dane" tab.
        const rec = page.locator('#ml-studio-types tf-radio[value="recognition"]');
        if (await rec.count()) { await rec.first().click(); typeChosen = 'recognition (fallback)'; }
        else typeChosen = 'domyślny (pierwszy typ)';
      }
      await shot(page, '02-create-form.png');

      const submit = page.locator('#ml-studio-create-modal tf-button', { hasText: 'Utwórz projekt' }).first();
      await submit.click();
      await page.waitForSelector('#ml-studio-create-modal', { state: 'detached', timeout: 15000 }).catch(() => {});
      await page.waitForSelector('#ml-studio-detail tf-detail-header', { timeout: 15000 });
      await page.waitForTimeout(800);

      // Select the "Dane" tab. For tabular_anomaly it is tab index 0 (default), but
      // we click it explicitly so the data panel renders regardless of project type.
      const daneTab = page.locator('#ml-studio-tabs tf-tab[label="Dane"]');
      await daneTab.waitFor({ state: 'attached', timeout: 10000 });
      await daneTab.click();
      // Wait for the data panel (tf-file-input host) to render.
      await page.waitForSelector('#ml-studio-data-file', { timeout: 15000 });
      await page.waitForTimeout(500);
      await shot(page, '03-dane-tab.png');

      step('1. Login → ML Studio → projekt → szczegół → zakładka Dane', true,
        `Projekt "${PROJECT_NAME}" utworzony (typ: ${typeChosen}); zakładka Dane wyrenderowana (tf-file-input obecny)`);
    } catch (e) {
      await shot(page, '03-dane-FAIL.png');
      step('1. Login → ML Studio → projekt → szczegół → zakładka Dane', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: upload the CSV through the tf-file-input native input ----
    try {
      // tf-file-input renders a hidden native <input type=file class="tf-file-input-native">.
      // setInputFiles sets the files AND fires a native "change", which tf-file-input
      // re-emits as a CustomEvent("change", {detail:{files}}) that the module listens for.
      const nativeInput = page.locator('#ml-studio-data-file input.tf-file-input-native');
      await nativeInput.waitFor({ state: 'attached', timeout: 10000 });
      await nativeInput.setInputFiles(CSV);

      // Upload happens over the binary WS; the dataset then appears in the list.
      // Wait for the dataset row to materialise (proof the upload + profiling ran).
      await page.waitForFunction(() => {
        const host = document.querySelector('#ml-studio-datasets');
        const table = host?.querySelector('tf-table');
        return (table?.rows || []).some((r) => String(r.name || '').includes('przesylki_test'));
      }, { timeout: 20000 });
      await shot(page, '04-after-upload.png');

      // tf-table renders its <table> inside a shadow root, so the profile columns
      // live on the element's .rows JS property — not in host.textContent. Probe
      // that property to decide whether the profile is already populated.
      const profileLoaded = () => page.evaluate(() => {
        const card = document.querySelector('#ml-studio-profile-card');
        const tbl = document.querySelector('#ml-studio-profile tf-table');
        if (!card || card.hidden || !tbl) return false;
        const names = (tbl.rows || []).map((r) => {
          const d = document.createElement('div'); d.innerHTML = r.name || '';
          return d.textContent;
        });
        return names.some((n) => n.includes('ryzyko')) && names.some((n) => n.includes('masa_kg'));
      });

      // The profile card may auto-open from the upload response. If not, trigger the
      // dataset's row-click (handler calls loadProfile(datasetId)).
      const profileShown = await profileLoaded();
      if (!profileShown) {
        // Trigger the dataset's row-click programmatically. tf-table emits
        // "row-click" with detail.row, and the module's handler calls
        // loadProfile(row._datasetId). Dispatching it directly is more robust than
        // a synthetic td click and exercises the exact same code path.
        await page.evaluate(() => {
          const table = document.querySelector('#ml-studio-datasets tf-table');
          const rows = table?.rows || [];
          const row = rows.find((r) => String(r.name || '').includes('przesylki_test'));
          if (table && row) {
            table.dispatchEvent(new CustomEvent('row-click', { detail: { row } }));
          }
        });
      }

      await page.waitForFunction(() => {
        const card = document.querySelector('#ml-studio-profile-card');
        const tbl = document.querySelector('#ml-studio-profile tf-table');
        if (!card || card.hidden || !tbl) return false;
        const names = (tbl.rows || []).map((r) => {
          const d = document.createElement('div'); d.innerHTML = r.name || '';
          return d.textContent;
        });
        return names.some((n) => n.includes('ryzyko')) && names.some((n) => n.includes('masa_kg'));
      }, { timeout: 20000 });
      await page.waitForTimeout(400);
      // Scroll the profile card into view so the screenshot shows the column table.
      await page.locator('#ml-studio-profile-card').scrollIntoViewIfNeeded().catch(() => {});
      await page.waitForTimeout(200);
      await shot(page, '05-profile.png');
      step('2. Upload CSV przez tf-file-input', true,
        `setInputFiles(${path.basename(CSV)}) → zbiór wgrany, profil kolumn wyrenderowany${profileShown ? ' (auto)' : ' (po kliknięciu wiersza)'}`);
    } catch (e) {
      await shot(page, '04-upload-FAIL.png');
      step('2. Upload CSV przez tf-file-input', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 3: assert the column profile reflects the user's file ----
    try {
      // Extract one record per profile row from the rendered tf-table. Each profile
      // row carries the column name, the type chip text, the "wykryto N klas: ..."
      // provenance line (categorical only), the unique count and the missing %.
      const cols = await page.evaluate(() => {
        const host = document.querySelector('#ml-studio-profile');
        const table = host?.querySelector('tf-table');
        const rows = table?.rows || [];
        // tf-table stores HTML strings per cell; parse them back to text.
        const txt = (html) => {
          const d = document.createElement('div');
          d.innerHTML = html || '';
          return d.textContent.replace(/\s+/g, ' ').trim();
        };
        return rows.map((r) => ({
          name: txt(r.name),        // includes the "wykryto N klas: ..." suffix for categoricals
          type: txt(r.type),        // chip label: kategoria / całkowita / zmiennoprzecinkowa / ...
          unique: txt(r.unique),
          missing: txt(r.missing),  // e.g. "20,0%"
          examples: txt(r.examples),
        }));
      });

      const by = (name) => cols.find((c) => c.name.startsWith(name) || c.name.split(' ')[0] === name);
      const ryzyko = by('ryzyko');
      const masa = by('masa_kg');
      const id = by('id');
      const region = by('region');
      const uwagi = by('uwagi');

      const checks = [];
      const fail = [];

      // ryzyko: kategoria + wykryto 3 klasy (wysokie/srednie/niskie) — KLUCZOWY dowód.
      const ryzykoCat = ryzyko && /kategoria/i.test(ryzyko.type);
      const ryzyko3 = ryzyko && /wykryto\s+3\s+klas/i.test(ryzyko.name);
      const ryzykoVals = ryzyko && /wysokie/i.test(ryzyko.name) && /srednie/i.test(ryzyko.name) && /niskie/i.test(ryzyko.name);
      (ryzykoCat && ryzyko3 && ryzykoVals ? checks : fail).push(
        `ryzyko: typ="${ryzyko?.type}", "${ryzyko?.name}"`);

      // masa_kg: liczbowy (całkowita lub zmiennoprzecinkowa).
      const masaNum = masa && /(całkowita|zmiennoprzecinkowa)/i.test(masa.type);
      (masaNum ? checks : fail).push(`masa_kg: typ="${masa?.type}"`);

      // id: NIE kategoria (powinno być całkowita/ID).
      const idNotCat = id && !/kategoria/i.test(id.type);
      (idNotCat ? checks : fail).push(`id: typ="${id?.type}" (nie kategoria)`);

      // region: kategoria, 3 klasy PL/DE/UA.
      const regionCat = region && /kategoria/i.test(region.type);
      const region3 = region && /wykryto\s+3\s+klas/i.test(region.name)
        && /PL/.test(region.name) && /DE/.test(region.name) && /UA/.test(region.name);
      (regionCat && region3 ? checks : fail).push(`region: typ="${region?.type}", "${region?.name}"`);

      // uwagi: % braków > 0 (puste w wierszach 4 i 8 → 2/10 = 20%).
      const missVal = uwagi ? parseFloat(String(uwagi.missing).replace('%', '').replace(',', '.')) : 0;
      const uwagiMissing = uwagi && missVal > 0;
      (uwagiMissing ? checks : fail).push(`uwagi: % braków="${uwagi?.missing}"`);

      profileFacts = { cols, ryzyko, masa, id, region, uwagi, missVal };

      const pass = fail.length === 0 && Boolean(ryzyko && masa && id && region && uwagi);
      step('3. Profil kolumn odzwierciedla plik usera', pass,
        pass
          ? `OK — ${checks.join(' | ')}`
          : `BŁĘDY: ${fail.join(' | ')} || OK: ${checks.join(' | ')}`);
    } catch (e) {
      await shot(page, '05-profile-assert-FAIL.png');
      step('3. Profil kolumn odzwierciedla plik usera', false, `Błąd: ${e.message}`);
    }

    // ---- Step 4: dataset appears in the list with rows=10, columns=5 ----
    try {
      const ds = await page.evaluate(() => {
        const host = document.querySelector('#ml-studio-datasets');
        const table = host?.querySelector('tf-table');
        const rows = table?.rows || [];
        const txt = (html) => {
          const d = document.createElement('div');
          d.innerHTML = html || '';
          return d.textContent.replace(/\s+/g, ' ').trim();
        };
        return rows.map((r) => ({
          name: txt(r.name),
          kind: txt(r.kind),
          rowCount: txt(r.rowCount),
          columnCount: txt(r.columnCount),
        }));
      });
      await shot(page, '06-datasets.png');

      const mine = ds.find((d) => d.name.startsWith('przesylki_test'));
      const rowsOk = mine && String(mine.rowCount).replace(/\D/g, '') === '10';
      const colsOk = mine && String(mine.columnCount).replace(/\D/g, '') === '5';
      const pass = Boolean(mine) && rowsOk && colsOk;
      step('4. Zbiór na liście (nazwa, wiersze=10, kolumny=5)', pass,
        mine
          ? `Zbiór "${mine.name}" · typ=${mine.kind} · wiersze=${mine.rowCount} · kolumny=${mine.columnCount}`
          : `Brak zbioru na liście; widoczne: ${JSON.stringify(ds)}`);
    } catch (e) {
      await shot(page, '06-datasets-FAIL.png');
      step('4. Zbiór na liście', false, `Błąd: ${e.message}`);
    }

    // ---- Step 5: confirm persistence in the ML Studio SQLite DB ----
    try {
      const out = execSync(
        `sqlite3 "${DB}" "SELECT name,kind,row_count,column_count FROM datasets ORDER BY rowid DESC LIMIT 3;"`,
        { encoding: 'utf8' },
      ).trim();
      const lines = out.split('\n');
      const mine = lines.find((l) => l.startsWith('przesylki_test|'));
      const pass = Boolean(mine) && /\|csv\|10\|5$/.test(mine);
      step('5. Persystencja w bazie (sqlite3 datasets)', pass,
        mine ? `Wiersz DB: ${mine} | top3: [${lines.join(' ; ')}]`
             : `Brak wiersza przesylki_test w DB; top3: [${lines.join(' ; ')}]`);
    } catch (e) {
      step('5. Persystencja w bazie (sqlite3 datasets)', false, `Błąd sqlite3: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL: ${fatal.message}`);
  } finally {
    console.log('\n================ KONSOLA / SIEĆ ================');
    console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
    consoleErrors.slice(0, 30).forEach((e) => console.log('  JS> ' + e));
    console.log(`Nieudane żądania: ${failedRequests.length}`);
    failedRequests.slice(0, 30).forEach((e) => console.log('  NET> ' + e));

    if (profileFacts) {
      console.log('\n================ PROFIL (surowe wiersze) ======');
      profileFacts.cols.forEach((c) =>
        console.log(`  ${c.type.padEnd(20)} | ${c.name}  [unik=${c.unique}, braki=${c.missing}]`));
    }

    console.log('\n================ PODSUMOWANIE =================');
    results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} — ${r.note}`));
    const allPass = results.length > 0 && results.every((r) => r.pass);
    console.log(`\nWYNIK OGÓLNY: ${allPass ? 'PASS' : 'FAIL'}`);
    console.log(`NAZWA PROJEKTU: ${PROJECT_NAME}`);

    await browser.close();
    process.exit(allPass ? 0 : 1);
  }
})();
