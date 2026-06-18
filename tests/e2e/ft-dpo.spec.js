// =============================================================================
// File: tests/e2e/ft-dpo.spec.js
// Description: E2E (Playwright) cyklu fine-tuningu DPO (Direct Preference
//   Optimization) w ML Studio przeciw ŻYWEJ instancji TentaFlow
//   (https://localhost:8095, power1/power123). Klika UI jak realny użytkownik:
//   login → rola power_user + nav ML Studio → kreator 4-krokowy (projekt ft_llm
//   „DPO Test" + upload /tmp/ft-dpo-sample.csv z kolumnami prompt/chosen/rejected)
//   → Model bazowy (custom HF Qwen2.5-0.5B-Instruct) → objective=DPO, metoda LoRA,
//   epochs=1 → Trening (polling do succeeded + krzywa loss) → Modele. Przechwytuje
//   payload mlStudioFtTrainStartRequest (patch ApiBinary.one) by udowodnić
//   objective:"dpo" w żądaniu RPC. Zrzuty @1440 i @390 do /tmp/ft-dpo-shots/.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/ft-dpo-shots';
const CSV = '/tmp/ft-dpo-sample.csv';
const PROJECT_NAME = `DPO Test ${Date.now()}`;
const CUSTOM_REPO = 'Qwen/Qwen2.5-0.5B-Instruct';

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}

async function shotBoth(page, base) {
  const out = {};
  for (const [tag, w, h] of [['1440', 1440, 900], ['390', 390, 844]]) {
    await page.setViewportSize({ width: w, height: h });
    await page.waitForTimeout(350);
    const file = `${SHOT}/${base}@${tag}.png`;
    await page.screenshot({ path: file, fullPage: true }).catch(() => {});
    const hScroll = await page.evaluate(() =>
      document.documentElement.scrollWidth - document.documentElement.clientWidth);
    out[tag] = { file, hScroll };
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(200);
  return out;
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  if (!fs.existsSync(CSV)) { console.log(`FATAL: brak ${CSV}`); process.exit(1); }

  const consoleErrors = [];
  const failedRequests = [];

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();

  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(msg.text()); });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));
  page.on('requestfailed', (req) => failedRequests.push(`${req.method()} ${req.url()} :: ${req.failure()?.errorText}`));

  const facts = {
    role: '', navPresent: false, datasetProfile: '', objectiveSelected: '',
    rpcObjective: '', rpcSeen: false, trainStatus: '', trainLoss: '',
    modelName: '', responsive: [],
  };

  try {
    // ---- Krok 1: login power1/power123 + rola + nav ML Studio ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
      const userInput = page.locator('#login-username input').first();
      await userInput.waitFor({ state: 'visible', timeout: 20000 });
      await userInput.fill('power1');
      await page.locator('#login-password input').first().fill('power123');
      await page.locator('#login-submit').click();
      await page.waitForSelector('.sidebar .nav-item[data-view], aside, nav', { timeout: 20000 });
      await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(800);

      const authMe = await page.evaluate(async () => {
        try {
          const mod = await import('/js/protocol/api-binary-shim.js');
          const api = mod.ApiBinary;
          if (api && api.one) return { ok: true, raw: await api.one('authMeRequest') };
          return { ok: false, err: 'ApiBinary.one niedostępne' };
        } catch (e) { return { ok: false, err: String(e && e.message || e) }; }
      });
      let role = '';
      if (authMe.ok && authMe.raw) {
        const u = authMe.raw.user || authMe.raw;
        role = String(u.role ?? u.role_slug ?? authMe.raw.role ?? '');
      }
      facts.role = role;

      // Patch ApiBinary.one by przechwycić payload startu treningu (dowód objective:"dpo").
      await page.evaluate(async () => {
        const mod = await import('/js/protocol/api-binary-shim.js');
        const api = mod.ApiBinary;
        if (api && api.one && !api.__dpoPatched) {
          const orig = api.one.bind(api);
          window.__rpcCapture = [];
          api.one = async (method, payload) => {
            if (method === 'mlStudioFtTrainStartRequest') {
              window.__rpcCapture.push({ method, payload: JSON.parse(JSON.stringify(payload || {})) });
            }
            return orig(method, payload);
          };
          api.__dpoPatched = true;
        }
      });

      const navItem = page.locator('.sidebar .nav-item[data-view="ml-studio"]');
      facts.navPresent = (await navItem.count()) > 0;
      await shotBoth(page, '00-after-login');

      if (!facts.navPresent) {
        step('1. Login power1 → rola power_user + nav ML Studio', false,
          `REGRESJA: brak nav ML Studio. role=${role || '(brak)'}`);
        throw new Error('Brak nav ML Studio');
      }
      await navItem.first().click();
      await page.waitForSelector('#ml-studio-new, .ml-studio', { timeout: 15000 });
      await page.waitForTimeout(500);

      const pass = role === 'power_user' && facts.navPresent;
      step('1. Login power1 → rola power_user + nav ML Studio', pass,
        `authMe role="${role || '(brak)'}" | nav ML Studio=${facts.navPresent}`);
    } catch (e) {
      await shotBoth(page, '00-login-FAIL').catch(() => {});
      step('1. Login power1 → rola power_user + nav ML Studio', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 2: kreator 4-kroki → projekt ft_llm + upload CSV DPO ----
    try {
      await page.locator('#ml-studio-new').click();
      const nameInput = page.locator('#ml-studio-wiz-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 15000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-wiz-desc textarea').first().fill('e2e DPO: para chosen/rejected');
      await page.locator('#ml-studio-wiz-next').click();

      const ftRadio = page.locator('#ml-studio-wiz-types tf-radio[value="ft_llm"]').first();
      await ftRadio.waitFor({ state: 'attached', timeout: 10000 });
      await ftRadio.click();
      await page.waitForTimeout(300);
      await page.locator('#ml-studio-wiz-next').click();

      // Upload CSV DPO przez tf-file-input (pułapka: setInputFiles nie wyzwala change).
      await page.waitForSelector('#ml-studio-wiz-file', { timeout: 10000 });
      const nativeFile = page.locator('#ml-studio-wiz-file input.tf-file-input-native').first();
      await nativeFile.waitFor({ state: 'attached', timeout: 10000 });
      await nativeFile.setInputFiles(CSV);
      await page.evaluate(() => {
        const fi = document.querySelector('#ml-studio-wiz-file');
        const native = fi?.querySelector('input.tf-file-input-native');
        if (fi && native && native.files && native.files.length) {
          fi.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { files: native.files } }));
        }
      });
      await page.waitForSelector('#ml-studio-wiz-file-info:not([hidden])', { timeout: 10000 }).catch(() => {});
      await page.waitForTimeout(300);
      await shotBoth(page, '01-wizard-dane-dpo');
      await page.locator('#ml-studio-wiz-next').click();

      const createBtn = page.locator('#ml-studio-wiz-create');
      await createBtn.waitFor({ state: 'visible', timeout: 10000 });
      await createBtn.click();

      const outcome = await Promise.race([
        page.waitForSelector('#ml-studio-tabs', { timeout: 25000 }).then(() => 'tabs').catch(() => null),
        page.waitForSelector('tf-toast, .tf-toast', { timeout: 25000 }).then(() => 'toast').catch(() => null),
      ]);
      await page.waitForTimeout(600);
      const toastTxt = await page.evaluate(() => {
        const t = document.querySelector('tf-toast, .tf-toast');
        return t ? (t.textContent || '').replace(/\s+/g, ' ').trim() : '';
      });
      await shotBoth(page, '02-overview');

      if (outcome !== 'tabs' || !(await page.locator('#ml-studio-tabs').count())) {
        step('2. Kreator → projekt ft_llm "DPO Test" + upload CSV DPO', false,
          `Backend odrzucił utworzenie. Toast: "${toastTxt || '(brak)'}"`);
        throw new Error(`Tworzenie projektu nie powiodło się (toast="${toastTxt}")`);
      }
      step('2. Kreator → projekt ft_llm "DPO Test" + upload CSV DPO', true,
        `Projekt "${PROJECT_NAME}" utworzony; CSV ft-dpo-sample.csv (prompt/chosen/rejected) załadowany`);
    } catch (e) {
      await shotBoth(page, '02-wizard-FAIL').catch(() => {});
      step('2. Kreator → projekt ft_llm + upload CSV DPO', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 2b: profilowanie datasetu — sprawdź czy profiler nie odrzuca chosen/rejected ----
    try {
      const dataTab = page.locator('#ml-studio-tabs tf-tab[label="Dane"]');
      await dataTab.waitFor({ state: 'attached', timeout: 10000 });
      await dataTab.click();
      await page.waitForTimeout(1500);
      await page.waitForLoadState('networkidle', { timeout: 8000 }).catch(() => {});
      await shotBoth(page, '03-dane-profil');

      const profile = await page.evaluate(() => {
        const root = document.querySelector('.ml-studio, #ml-studio-tabs')?.parentElement || document.body;
        const txt = (root.innerText || '').replace(/\s+/g, ' ').trim();
        // Szukaj nazw kolumn i ew. komunikatu błędu profilera.
        const hasPrompt = /prompt/i.test(txt);
        const hasChosen = /chosen/i.test(txt);
        const hasRejected = /rejected/i.test(txt);
        const err = (txt.match(/(błąd|error|nieobsługiwan|nieprawidłow|odrzuc)[^.]{0,120}/i) || [])[0] || '';
        // toast
        const toast = document.querySelector('tf-toast, .tf-toast');
        const toastTxt = toast ? (toast.textContent || '').replace(/\s+/g, ' ').trim() : '';
        return { hasPrompt, hasChosen, hasRejected, err, toastTxt, sample: txt.slice(0, 400) };
      });
      facts.datasetProfile = `prompt=${profile.hasPrompt} chosen=${profile.hasChosen} rejected=${profile.hasRejected}`;
      const profilerRejected = !!profile.err || /nieobsług|nieprawidłow|odrzuc/i.test(profile.toastTxt);
      // Profiler OK = brak twardego błędu. Kolumny chosen/rejected mogą być pokazane
      // jako zwykłe kolumny tekstowe — to jest poprawne dla DPO.
      const pass = !profilerRejected;
      step('2b. Profilowanie datasetu DPO (chosen/rejected nie odrzucone)', pass,
        pass
          ? `Profiler OK. Kolumny: ${facts.datasetProfile}. ${profile.hasChosen && profile.hasRejected ? 'chosen+rejected rozpoznane' : 'UWAGA: nie widać chosen/rejected w UI — fragment: ' + profile.sample.slice(0, 120)}`
          : `PROFILER ODRZUCIŁ FORMAT DPO. err="${profile.err}" toast="${profile.toastTxt}"`);
    } catch (e) {
      await shotBoth(page, '03-dane-FAIL').catch(() => {});
      step('2b. Profilowanie datasetu DPO', false, `Błąd: ${e.message}`);
      // Nie przerywaj — chcemy spróbować dalej, ale to zgłoszone.
    }

    // ---- Krok 3: Model bazowy custom Qwen + objective=DPO + metoda LoRA + epochs=1 ----
    try {
      const modelTab = page.locator('#ml-studio-tabs tf-tab[label="Model bazowy"]');
      await modelTab.waitFor({ state: 'attached', timeout: 10000 });
      await modelTab.click();
      await page.waitForSelector('.ml-studio-ft-model-card[data-model="__custom__"]', { timeout: 15000 });
      await page.waitForTimeout(400);

      await page.locator('.ml-studio-ft-model-card[data-model="__custom__"]').click();
      const repoInput = page.locator('#ml-studio-ft-custom-repo input').first();
      await repoInput.waitFor({ state: 'visible', timeout: 10000 });
      await repoInput.click();
      await repoInput.fill(CUSTOM_REPO);
      await page.waitForTimeout(200);

      // Objective = DPO (karta data-objective="dpo").
      const dpoCard = page.locator('.ml-studio-ft-axis-card[data-objective="dpo"]');
      await dpoCard.waitFor({ state: 'attached', timeout: 10000 });
      await dpoCard.click();
      await page.waitForTimeout(200);
      // Metoda LoRA.
      await page.locator('.ml-studio-ft-method-card[data-method="lora"]').click();
      await page.waitForTimeout(200);

      // epochs = 1
      const epochsInput = page.locator('#ml-studio-ft-hp-epochs input').first();
      await epochsInput.fill('1');
      await page.waitForTimeout(150);

      // Zrzut z zaznaczoną kartą DPO (przed save, żeby było widać wybór).
      await shotBoth(page, '04-objective-dpo-wybrany');

      // Potwierdź zaznaczenie DPO klasą .selected.
      facts.objectiveSelected = await page.evaluate(() => {
        const c = document.querySelector('.ml-studio-ft-axis-card[data-objective="dpo"]');
        return c && c.classList.contains('selected') ? 'dpo' : (c ? 'NIE-zaznaczone' : 'brak-karty');
      });

      await page.locator('#ml-studio-ft-save').click();
      await page.waitForTimeout(400);
      await shotBoth(page, '05-model-bazowy-dpo');

      const repoVal = await repoInput.inputValue().catch(() => '');
      const pass = repoVal === CUSTOM_REPO && facts.objectiveSelected === 'dpo';
      step('3. Model bazowy custom Qwen + objective=DPO + LoRA + epochs=1', pass,
        `repo="${repoVal}" objective(UI)="${facts.objectiveSelected}" metoda=LoRA epochs=1`);
    } catch (e) {
      await shotBoth(page, '05-model-FAIL').catch(() => {});
      step('3. Model bazowy + objective DPO', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 4: Trening → uruchom → polling do succeeded + dowód objective:"dpo" w RPC ----
    try {
      const trainTab = page.locator('#ml-studio-tabs tf-tab[label="Trening"]');
      await trainTab.waitFor({ state: 'attached', timeout: 10000 });
      await trainTab.click();
      await page.waitForSelector('#ml-studio-ft-run', { timeout: 15000 });
      await page.waitForTimeout(400);
      await shotBoth(page, '06-trening-przed-startem');

      await page.locator('#ml-studio-ft-run').click();

      // Po starcie sprawdź przechwycony payload RPC.
      await page.waitForTimeout(800);
      const cap = await page.evaluate(() => window.__rpcCapture || []);
      if (cap.length) {
        facts.rpcSeen = true;
        facts.rpcObjective = String(cap[cap.length - 1].payload?.objective ?? '');
      }

      await page.waitForSelector('#ml-studio-ft-live .ml-studio-ft-live', { timeout: 15000 }).catch(() => {});
      const status = await page.waitForFunction(() => {
        const badge = document.querySelector('#ml-studio-ft-status-badge');
        const t = badge ? (badge.textContent || '') : '';
        return /zakończony/i.test(t) ? 'succeeded' : (/błąd/i.test(t) ? 'failed' : false);
      }, { timeout: 120000 }).then((h) => h.jsonValue()).catch(() => false);
      facts.trainStatus = String(status);

      await page.waitForTimeout(500);
      await shotBoth(page, '07-trening-live');

      const lossInfo = await page.evaluate(() => {
        const svg = document.querySelector('.ml-studio-ft-loss-svg');
        const lines = svg ? svg.querySelectorAll('polyline, path').length : 0;
        const chart = document.querySelector('.ml-studio-ft-chart-title, .ml-studio-ft-chart-empty');
        const kpiNodes = Array.from(document.querySelectorAll('.ml-studio-ft-kpi'));
        const kpiTxt = kpiNodes.map((n) => n.textContent.replace(/\s+/g, ' ').trim()).join(' | ');
        // train loss wartość: KPI "train loss" → val
        let trainLossVal = '';
        for (const n of kpiNodes) {
          const lbl = n.querySelector('.lbl'); const val = n.querySelector('.val');
          if (lbl && /train loss/i.test(lbl.textContent) && val) trainLossVal = val.textContent.trim();
        }
        return { svgPresent: !!svg, lines, chartSection: !!chart, kpi: kpiTxt, trainLossVal };
      });
      facts.trainLoss = lossInfo.trainLossVal;

      const lossFilled = !!lossInfo.trainLossVal && lossInfo.trainLossVal !== '—';
      const rpcOk = facts.rpcObjective === 'dpo';
      const pass = status === 'succeeded' && rpcOk && lossFilled;
      step('4. Trening DPO → succeeded + objective:"dpo" w RPC + train_loss', pass,
        `status=${status} | RPC objective="${facts.rpcObjective}" (przechwycony=${facts.rpcSeen}) | train_loss="${facts.trainLoss || '—'}" | KPI: ${lossInfo.kpi.slice(0, 120)}`);
      if (status !== 'succeeded') throw new Error(`Trening nie succeeded: ${status}`);
    } catch (e) {
      await shotBoth(page, '07-trening-FAIL').catch(() => {});
      step('4. Trening DPO → succeeded', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 5: zakładka Modele — model DPO widoczny ----
    try {
      const modelsTab = page.locator('#ml-studio-tabs tf-tab[label="Modele"]');
      await modelsTab.click();
      await page.waitForSelector('#ml-studio-models-table tf-table', { timeout: 15000 });
      await page.waitForFunction(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return t && (t.rows || []).length > 0;
      }, { timeout: 15000 });
      await page.waitForTimeout(400);
      await shotBoth(page, '08-modele');

      const rows = await page.evaluate(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return (t.rows || []).map((r) => ({
          model: r.model, framework: r.framework, baseModel: r.baseModel,
        }));
      });
      facts.modelName = rows[0]?.model || '';
      const pass = rows.length >= 1;
      step('5. Zakładka Modele: model DPO widoczny', pass,
        `modele=${rows.length} | pierwszy: "${facts.modelName}" (silnik=${rows[0]?.framework}, base=${rows[0]?.baseModel})`);
    } catch (e) {
      await shotBoth(page, '08-modele-FAIL').catch(() => {});
      step('5. Zakładka Modele', false, `Błąd: ${e.message}`);
      throw e;
    }
  } catch (fatal) {
    console.log(`\nFATAL (przerwano dalsze kroki): ${fatal.message}`);
  } finally {
    console.log('\n================ KONSOLA / SIEĆ ================');
    console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
    consoleErrors.slice(0, 30).forEach((e) => console.log('  JS> ' + e));
    console.log(`Nieudane żądania: ${failedRequests.length}`);
    failedRequests.slice(0, 30).forEach((e) => console.log('  NET> ' + e));

    console.log('\n================ FAKTY ========================');
    console.log(`  rola (authMe): ${facts.role || '(brak)'}`);
    console.log(`  nav ML Studio: ${facts.navPresent}`);
    console.log(`  profil datasetu: ${facts.datasetProfile || '(brak)'}`);
    console.log(`  objective wybrany w UI: ${facts.objectiveSelected || '(brak)'}`);
    console.log(`  objective w RPC: ${facts.rpcObjective || '(brak)'} (przechwycony=${facts.rpcSeen})`);
    console.log(`  status treningu: ${facts.trainStatus || '(brak)'}`);
    console.log(`  train_loss: ${facts.trainLoss || '(brak)'}`);
    console.log(`  model DPO w rejestrze: ${facts.modelName || '(brak)'}`);

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
