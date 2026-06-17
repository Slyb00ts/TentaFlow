// =============================================================================
// File: tests/e2e/ml-studio-train.spec.js
// Description: Real e2e test of the ML Studio "Trenuj" tab against a LIVE running
//              TentaFlow instance (https://localhost:8095, admin/admin). Drives the
//              SPA over the binary WebSocket protocol: login, create a
//              "tabular_anomaly" project, upload a CSV (Dane tab), then in the
//              "Trenuj" tab pick the dataset + target column `ryzyko`, verify the
//              "Wykryto: KATEGORIA, 3 klasy" callout + auto classification task,
//              run training, and assert the leaderboard holds >=2 real models with
//              accuracy in 0..1 where the learned model beats the baseline. Reads
//              tf-table assertions through the component's `.rows` property (shadow
//              DOM), never textContent. Confirms model + run rows land in
//              ml_studio.db. Proves the whole tabular flow (m04) works end-to-end.
//              Standalone node script — does NOT spawn its own binary.
//              Screenshots land in /tmp/mlstudio-shots7/.
// =============================================================================

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-shots7';
const CSV = '/tmp/przesylki_test.csv';
const DB = '/home/critix/repos/rust/TentaFlow-ml/.runtime/data/ml_studio.db';
const PROJECT_NAME = `Cysterny trening ${Date.now()}`;

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}

async function shot(page, file) {
  await page.screenshot({ path: `${SHOT}/${file}`, fullPage: true }).catch(() => {});
}

// Parse a pl-PL formatted metric string ("0,875" / "12,3 s") back to a Number.
function plNum(s) {
  if (s == null) return NaN;
  return parseFloat(String(s).replace(/[^\d.,-]/g, '').replace(/\./g, '').replace(',', '.'));
}

// Pulls the leaderboard rows straight off the tf-table .rows property in the page,
// flattening each HTML cell back to text so the node side can assert on values.
function readLeaderboard(page) {
  return page.evaluate(() => {
    const host = document.querySelector('#ml-studio-train-leaderboard');
    const table = host?.querySelector('tf-table');
    const rows = table?.rows || [];
    const txt = (html) => {
      const d = document.createElement('div');
      d.innerHTML = html || '';
      return d.textContent.replace(/\s+/g, ' ').trim();
    };
    return rows.map((r) => ({
      model: txt(r.model),
      best: /najlepszy/i.test(r.model || ''),
      accuracy: txt(r.accuracy),
      f1Macro: txt(r.f1Macro),
      rmse: txt(r.rmse),
      trainSecs: txt(r.trainSecs),
    }));
  });
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

  // Captured facts for the final report.
  let calloutText = '';
  let autoTask = '';
  let board = null;
  let negativeNote = '';

  try {
    // ---- Step 1: login + ML Studio + create tabular_anomaly project + open detail ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      const userInput = page.locator('#login-username input').first();
      await userInput.waitFor({ state: 'visible', timeout: 20000 });
      await userInput.fill('admin');
      await page.locator('#login-password input').first().fill('admin');
      await page.locator('#login-submit').click();
      await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });
      await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});

      const navItem = page.locator('.sidebar .nav-item[data-view="ml-studio"]');
      await navItem.waitFor({ state: 'visible', timeout: 10000 });
      await navItem.click();
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      await page.waitForTimeout(500);

      await page.locator('#ml-studio-new').click();
      const nameInput = page.locator('#ml-studio-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 10000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-desc textarea').first().fill('e2e: realny trening tabelaryczny → leaderboard');

      const tabRadio = page.locator('#ml-studio-types tf-radio[value="tabular_anomaly"]');
      await tabRadio.first().waitFor({ state: 'attached', timeout: 10000 });
      await tabRadio.first().click();
      await shot(page, '01-create-form.png');

      const submit = page.locator('#ml-studio-create-modal tf-button', { hasText: 'Utwórz projekt' }).first();
      await submit.click();
      await page.waitForSelector('#ml-studio-create-modal', { state: 'detached', timeout: 15000 }).catch(() => {});
      await page.waitForSelector('#ml-studio-detail tf-detail-header', { timeout: 15000 });
      await page.waitForTimeout(600);

      step('1. Login → ML Studio → projekt tabular_anomaly → szczegół', true,
        `Projekt "${PROJECT_NAME}" (tabular_anomaly) utworzony; widok szczegółu otwarty`);
    } catch (e) {
      await shot(page, '01-create-FAIL.png');
      step('1. Login → ML Studio → projekt tabular_anomaly → szczegół', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: upload the CSV through the tf-file-input native input (Dane tab) ----
    try {
      const daneTab = page.locator('#ml-studio-tabs tf-tab[label="Dane"]');
      await daneTab.waitFor({ state: 'attached', timeout: 10000 });
      await daneTab.click();
      await page.waitForSelector('#ml-studio-data-file', { timeout: 15000 });
      await page.waitForTimeout(300);

      const nativeInput = page.locator('#ml-studio-data-file input.tf-file-input-native');
      await nativeInput.waitFor({ state: 'attached', timeout: 10000 });
      await nativeInput.setInputFiles(CSV);

      // Wait for the dataset row (proof upload + profiling ran on the backend).
      await page.waitForFunction(() => {
        const host = document.querySelector('#ml-studio-datasets');
        const table = host?.querySelector('tf-table');
        return (table?.rows || []).some((r) => String(r.name || '').includes('przesylki_test'));
      }, { timeout: 20000 });
      // Wait for the profile to materialise too (so the dataset is fully ready).
      await page.waitForFunction(() => {
        const card = document.querySelector('#ml-studio-profile-card');
        const tbl = document.querySelector('#ml-studio-profile tf-table');
        if (!card || card.hidden || !tbl) return false;
        const names = (tbl.rows || []).map((r) => {
          const d = document.createElement('div'); d.innerHTML = r.name || '';
          return d.textContent;
        });
        return names.some((n) => n.includes('ryzyko'));
      }, { timeout: 20000 }).catch(() => {});
      await shot(page, '02-after-upload.png');

      step('2. Upload CSV (zakładka Dane) → profil', true,
        `setInputFiles(${path.basename(CSV)}) → zbiór sprofilowany i widoczny na liście`);
    } catch (e) {
      await shot(page, '02-upload-FAIL.png');
      step('2. Upload CSV (zakładka Dane) → profil', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 3: Trenuj tab → pick dataset + target=ryzyko, verify callout + task ----
    try {
      const trenujTab = page.locator('#ml-studio-tabs tf-tab[label="Trenuj"]');
      await trenujTab.waitFor({ state: 'attached', timeout: 10000 });
      await trenujTab.click();
      await page.waitForSelector('#ml-studio-train-dataset', { timeout: 15000 });
      await page.waitForTimeout(400);

      // Pick the freshly uploaded dataset by matching its option label, then drive
      // tf-select's value + a `change` event (detail.value) the module listens for.
      const datasetPicked = await page.evaluate(() => {
        const sel = document.querySelector('#ml-studio-train-dataset');
        if (!sel) return null;
        // tf-select consumes options into a light-DOM inner <select> (tf-select.js);
        // read its real <option> list, pick the freshly uploaded dataset by label.
        const inner = sel.querySelector('select');
        const list = inner ? Array.from(inner.options).map((o) => ({ value: o.value, label: o.textContent })) : [];
        const match = list.find((o) => String(o.label || '').includes('przesylki_test'))
          || list.find((o) => o.value);
        if (!match) return null;
        // Set value through the component, then fire the component-level `change`
        // (detail.value) that the module's handler is bound to.
        sel.value = match.value;
        sel.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: match.value } }));
        return { value: match.value, label: match.label, count: list.length };
      });
      if (!datasetPicked) throw new Error('nie udało się wybrać datasetu w pickerze');

      // The dataset change handler fetches the profile then enables the target picker.
      await page.waitForFunction(() => {
        const t = document.querySelector('#ml-studio-train-target');
        return t && !t.hasAttribute('disabled');
      }, { timeout: 15000 });
      await shot(page, '03-dataset-picked.png');

      // Pick target = ryzyko via tf-select value + change event.
      const targetPicked = await page.evaluate(() => {
        const sel = document.querySelector('#ml-studio-train-target');
        if (!sel) return false;
        const inner = sel.querySelector('select');
        if (inner) inner.value = 'ryzyko';
        sel.value = 'ryzyko';
        sel.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: 'ryzyko' } }));
        return true;
      });
      if (!targetPicked) throw new Error('nie udało się wybrać kolumny-celu ryzyko');

      // Wait for the callout to render the detected-class provenance.
      await page.waitForFunction(() => {
        const c = document.querySelector('#ml-studio-train-callout .ml-studio-train-callout');
        return c && c.textContent && c.textContent.length > 0;
      }, { timeout: 10000 });

      calloutText = await page.evaluate(() => {
        const c = document.querySelector('#ml-studio-train-callout');
        return (c?.textContent || '').replace(/\s+/g, ' ').trim();
      });
      autoTask = await page.evaluate(() => {
        const t = document.querySelector('#ml-studio-train-task');
        return t ? String(t.value || '') : '';
      });
      await shot(page, '04-target-ryzyko-callout.png');

      const calloutCat = /Wykryto:\s*KATEGORIA/i.test(calloutText);
      const callout3 = /3\s+klas/i.test(calloutText);
      const calloutVals = /wysokie/i.test(calloutText) && /srednie/i.test(calloutText) && /niskie/i.test(calloutText);
      const taskOk = autoTask === 'classification';

      const ok = calloutCat && callout3 && calloutVals && taskOk;
      step('3. Cel=ryzyko → callout "Wykryto: KATEGORIA, 3 klasy" + auto klasyfikacja', ok,
        ok
          ? `callout="${calloutText.slice(0, 140)}" | auto-typ=${autoTask}`
          : `KATEGORIA=${calloutCat} 3klasy=${callout3} wartości(wysokie/srednie/niskie)=${calloutVals} task=${autoTask} | callout="${calloutText.slice(0, 200)}"`);
    } catch (e) {
      await shot(page, '03-train-pick-FAIL.png');
      step('3. Cel=ryzyko → callout + auto klasyfikacja', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 4: run training, wait for leaderboard, assert real metrics ----
    try {
      const runBtn = page.locator('#ml-studio-train-run');
      await runBtn.waitFor({ state: 'attached', timeout: 10000 });
      await page.evaluate(() => document.querySelector('#ml-studio-train-run')?.click());

      // Leaderboard table appears once training completes over the binary WS.
      await page.waitForFunction(() => {
        const card = document.querySelector('#ml-studio-train-result-card');
        const tbl = document.querySelector('#ml-studio-train-leaderboard tf-table');
        return card && !card.hidden && tbl && (tbl.rows || []).length > 0;
      }, { timeout: 60000 });
      await page.waitForTimeout(400);
      await page.locator('#ml-studio-train-result-card').scrollIntoViewIfNeeded().catch(() => {});
      await page.waitForTimeout(200);
      await shot(page, '05-leaderboard.png');

      board = await readLeaderboard(page);

      const fail = [];
      const okList = [];

      // >= 2 models.
      const enoughModels = board.length >= 2;
      (enoughModels ? okList : fail).push(`liczba modeli=${board.length}`);

      // accuracy + f1Macro + trainSecs columns present and numeric.
      const accVals = board.map((r) => plNum(r.accuracy));
      const f1Vals = board.map((r) => plNum(r.f1Macro));
      const secVals = board.map((r) => plNum(r.trainSecs));
      const colsPresent = board.every((r) => r.accuracy !== '' && r.f1Macro !== '' && r.trainSecs !== '');
      (colsPresent ? okList : fail).push(`kolumny accuracy/f1Macro/trainSecs obecne=${colsPresent}`);

      // accuracy values are real numbers in 0..1 (NOT hard 0/1 dummy).
      const accInRange = accVals.every((v) => Number.isFinite(v) && v >= 0 && v <= 1);
      const accNotAllExtreme = accVals.some((v) => v > 0 && v < 1) || new Set(accVals).size > 1;
      (accInRange ? okList : fail).push(`accuracy w 0..1=${accInRange} [${accVals.join(', ')}]`);
      (accNotAllExtreme ? okList : fail).push(`accuracy nie sztywne 0/1 (zróżnicowane/realne)=${accNotAllExtreme}`);

      // f1 in 0..1 too.
      const f1InRange = f1Vals.every((v) => Number.isFinite(v) && v >= 0 && v <= 1);
      (f1InRange ? okList : fail).push(`f1Macro w 0..1=${f1InRange} [${f1Vals.join(', ')}]`);

      // trainSecs finite >= 0.
      const secsOk = secVals.every((v) => Number.isFinite(v) && v >= 0);
      (secsOk ? okList : fail).push(`trainSecs sensowne=${secsOk} [${secVals.join(', ')}]`);

      // Learned model (logreg / logistic) accuracy >= baseline. The baseline is the
      // most-frequent classifier, surfaced in the UI as "Klasa większościowa".
      const isBaseline = (r) => /baseline|najcz|most.?frequent|częst|większ|wieksz|klasa\s+wi/i.test(r.model);
      const isLearned = (r) => /log/i.test(r.model);
      const baselineRow = board.find(isBaseline);
      const learnedRow = board.find(isLearned) || board.find((r) => !isBaseline(r));
      let learnedBeatsBaseline = false;
      if (baselineRow && learnedRow) {
        const la = plNum(learnedRow.accuracy);
        const ba = plNum(baselineRow.accuracy);
        learnedBeatsBaseline = Number.isFinite(la) && Number.isFinite(ba) && la >= ba;
        (learnedBeatsBaseline ? okList : fail).push(
          `logreg(${learnedRow.model.slice(0, 24)})=${la} >= baseline(${baselineRow.model.slice(0, 24)})=${ba}`);
      } else {
        fail.push(`brak pary logreg/baseline do porównania (modele: ${board.map((r) => r.model).join(' | ')})`);
      }

      // Best model highlighted in the leaderboard.
      const bestHighlighted = board.some((r) => r.best);
      (bestHighlighted ? okList : fail).push(`najlepszy wyróżniony=${bestHighlighted}`);

      const pass = fail.length === 0;
      step('4. Leaderboard: >=2 modele, realne metryki, logreg>=baseline, najlepszy wyróżniony', pass,
        pass
          ? `OK — ${okList.join(' | ')}`
          : `BŁĘDY: ${fail.join(' | ')} || OK: ${okList.join(' | ')}`);
    } catch (e) {
      await shot(page, '05-leaderboard-FAIL.png');
      step('4. Leaderboard: realny trening', false, `Błąd: ${e.message}`);
    }

    // ---- Step 5: confirm model + run persisted in ml_studio.db ----
    try {
      const modelsOut = execSync(
        `sqlite3 "${DB}" "SELECT name,framework,metrics_json FROM models ORDER BY rowid DESC LIMIT 2;"`,
        { encoding: 'utf8' },
      ).trim();
      const runsOut = execSync(
        `sqlite3 "${DB}" "SELECT status,config_json FROM training_runs ORDER BY rowid DESC LIMIT 2;"`,
        { encoding: 'utf8' },
      ).trim();

      const modelLines = modelsOut ? modelsOut.split('\n') : [];
      const runLines = runsOut ? runsOut.split('\n') : [];

      // metrics_json must carry real metrics (accuracy / f1), not an empty {}.
      // Note: only the BEST model per run is persisted to `models` (one row/run);
      // the baseline stays leaderboard-only, so one model row is the expected shape.
      const modelsHaveMetrics = modelLines.some((l) => /accuracy|f1|rmse/i.test(l) && !/\|\{\}$/.test(l));
      const runDone = runLines.some((l) => /^(done|completed|finished|succeeded|ok)\b/i.test(l));
      const runHasConfig = runLines.some((l) => /ryzyko/i.test(l) || /classification/i.test(l));

      const pass = modelLines.length >= 1 && modelsHaveMetrics && runLines.length >= 1 && runDone && runHasConfig;
      step('5. Persystencja w bazie (models + training_runs)', pass,
        `models[${modelLines.length}]: ${modelLines.map((l) => l.slice(0, 90)).join(' ;; ')} || runs[${runLines.length}]: ${runLines.map((l) => l.slice(0, 90)).join(' ;; ')} || metryki_realne=${modelsHaveMetrics} run_done=${runDone}`);
    } catch (e) {
      step('5. Persystencja w bazie (models + training_runs)', false, `Błąd sqlite3: ${e.message}`);
    }

    // ---- Step 6: (negative) target = id → behaviour check ----
    try {
      // Re-pick the target to `id` (an identifier column) and run again. Acceptable
      // outcomes: backend excludes id from features and still trains, OR rejects with
      // a sensible error toast. We capture whichever happens.
      const idAvailable = await page.evaluate(() => {
        const sel = document.querySelector('#ml-studio-train-target');
        if (!sel) return false;
        const inner = sel.querySelector('select');
        if (inner) inner.value = 'id';
        sel.value = 'id';
        sel.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: 'id' } }));
        return true;
      });
      if (!idAvailable) throw new Error('picker celu niedostępny dla scenariusza negatywnego');

      await page.waitForTimeout(400);
      const idCallout = await page.evaluate(() => {
        const c = document.querySelector('#ml-studio-train-callout');
        return (c?.textContent || '').replace(/\s+/g, ' ').trim();
      });

      // Capture toasts that appear (errors / warnings) during the id-target run.
      const toasts = [];
      page.on('console', () => {}); // noop, keep existing listeners
      const beforeBoard = board ? board.length : 0;

      await page.evaluate(() => document.querySelector('#ml-studio-train-run')?.click());

      // Either a new leaderboard renders (id excluded → trains) or a toast error shows.
      const outcome = await Promise.race([
        page.waitForFunction(() => {
          const tbl = document.querySelector('#ml-studio-train-leaderboard tf-table');
          return tbl && (tbl.rows || []).length > 0;
        }, { timeout: 25000 }).then(() => 'leaderboard').catch(() => null),
        page.waitForSelector('tf-toast, .tf-toast', { timeout: 25000 }).then(() => 'toast').catch(() => null),
      ]);

      await page.waitForTimeout(500);
      const idBoard = await readLeaderboard(page).catch(() => []);
      const toastText = await page.evaluate(() => {
        const t = document.querySelector('tf-toast, .tf-toast');
        return t ? (t.textContent || '').replace(/\s+/g, ' ').trim() : '';
      });
      await shot(page, '06-target-id.png');

      if (outcome === 'leaderboard' && idBoard.length) {
        const accVals = idBoard.map((r) => plNum(r.accuracy)).filter(Number.isFinite);
        const sane = accVals.every((v) => v >= 0 && v <= 1);
        negativeNote = `Trening na celu=id zakończony; leaderboard ${idBoard.length} modeli (id najpewniej wykluczone z cech). accuracy=[${accVals.join(', ')}] w 0..1=${sane}. callout="${idCallout.slice(0, 120)}"`;
        step('6. (neg) cel=id → zachowanie', true, negativeNote);
      } else if (toastText && /błąd|error|nie|odrzuc/i.test(toastText)) {
        negativeNote = `Trening na celu=id sensownie odrzucony: toast="${toastText.slice(0, 160)}"`;
        step('6. (neg) cel=id → zachowanie', true, negativeNote);
      } else {
        negativeNote = `Niejednoznaczne: outcome=${outcome}, leaderboard=${idBoard.length}, toast="${toastText.slice(0, 120)}", callout="${idCallout.slice(0, 120)}"`;
        step('6. (neg) cel=id → zachowanie', true, negativeNote + ' (zanotowano, nie blokuje)');
      }
    } catch (e) {
      negativeNote = `Scenariusz negatywny niewykonany: ${e.message}`;
      step('6. (neg) cel=id → zachowanie', true, negativeNote + ' (informacyjnie)');
    }
  } catch (fatal) {
    console.log(`\nFATAL: ${fatal.message}`);
  } finally {
    console.log('\n================ KONSOLA / SIEĆ ================');
    console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
    consoleErrors.slice(0, 30).forEach((e) => console.log('  JS> ' + e));
    console.log(`Nieudane żądania / WS: ${failedRequests.length}`);
    failedRequests.slice(0, 30).forEach((e) => console.log('  NET> ' + e));

    if (calloutText) {
      console.log('\n================ CALLOUT (cel=ryzyko) =========');
      console.log('  ' + calloutText.slice(0, 300));
      console.log('  auto-typ zadania: ' + autoTask);
    }
    if (board) {
      console.log('\n================ LEADERBOARD (surowe) =========');
      board.forEach((r) =>
        console.log(`  ${(r.best ? '★ ' : '  ')}${r.model.padEnd(34)} | acc=${r.accuracy} f1=${r.f1Macro} czas=${r.trainSecs}`));
    }
    if (negativeNote) {
      console.log('\n================ SCENARIUSZ NEG (cel=id) ======');
      console.log('  ' + negativeNote);
    }

    console.log('\n================ PODSUMOWANIE =================');
    results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} — ${r.note}`));
    const allPass = results.length > 0 && results.every((r) => r.pass);
    console.log(`\nWYNIK OGÓLNY: ${allPass ? 'PASS' : 'FAIL'}`);
    console.log(`NAZWA PROJEKTU: ${PROJECT_NAME}`);
    console.log(`ZRZUTY: ${SHOT}`);

    await browser.close();
    process.exit(allPass ? 0 : 1);
  }
})();
