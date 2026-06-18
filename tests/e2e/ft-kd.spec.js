// =============================================================================
// File: tests/e2e/ft-kd.spec.js
// Description: E2E (Playwright) trzeciego celu fine-tuningu — KD (Knowledge
//   Distillation, trl GKDTrainer) w ML Studio przeciw ŻYWEJ instancji TentaFlow
//   (https://localhost:8095, power1/power123). Klika UI jak realny użytkownik:
//   login → rola power_user + nav ML Studio → kreator 4-krokowy (projekt ft_llm
//   „KD Test" + upload /tmp/ft-sample.csv prompt/response) → Model bazowy (student:
//   custom HF Qwen2.5-0.5B-Instruct) → objective=KD. Potwierdza, że pole
//   „Model-nauczyciel" pojawia się TYLKO dla KD. Sprawdza walidację client-side:
//   KD bez teacher → toast „KD wymaga modelu-nauczyciela" i BRAK żądania RPC.
//   Następnie wpisuje teacher (ten sam Qwen2.5-0.5B-Instruct), metoda LoRA,
//   epochs=1 → Trening (polling do succeeded). Przechwytuje payload
//   mlStudioFtTrainStartRequest (patch ApiBinary.one) by udowodnić objective:"kd"
//   ORAZ teacherModel w żądaniu RPC. Modele → model KD w rejestrze. Zrzuty @1440
//   i @390 do /tmp/ft-kd-shots/.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/ft-kd-shots';
const CSV = '/tmp/ft-sample.csv';
const PROJECT_NAME = `KD Test ${Date.now()}`;
const CUSTOM_REPO = 'Qwen/Qwen2.5-0.5B-Instruct';
const TEACHER_REPO = 'Qwen/Qwen2.5-0.5B-Instruct';

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
    role: '', navPresent: false, objectiveSelected: '',
    teacherFieldVisibleNonKd: null, teacherFieldVisibleKd: null,
    validationToast: '', rpcCountAfterEmptyTeacher: -1,
    rpcObjective: '', rpcTeacher: '', rpcSeen: false,
    trainStatus: '', trainLoss: '', modelName: '',
    responsive: [],
  };

  try {
    // ---- Krok 1: login power1/power123 + rola + nav ML Studio + patch RPC ----
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

      // Patch ApiBinary.one — przechwyć KAŻDE wywołanie startu treningu (dowód
      // objective:"kd" + teacherModel oraz licznik prób przy walidacji pustego teacher).
      await page.evaluate(async () => {
        const mod = await import('/js/protocol/api-binary-shim.js');
        const api = mod.ApiBinary;
        if (api && api.one && !api.__kdPatched) {
          const orig = api.one.bind(api);
          window.__rpcCapture = [];
          api.one = async (method, payload) => {
            if (method === 'mlStudioFtTrainStartRequest') {
              window.__rpcCapture.push({ method, payload: JSON.parse(JSON.stringify(payload || {})) });
            }
            return orig(method, payload);
          };
          api.__kdPatched = true;
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

    // ---- Krok 2: kreator 4-kroki → projekt ft_llm + upload CSV ----
    try {
      await page.locator('#ml-studio-new').click();
      const nameInput = page.locator('#ml-studio-wiz-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 15000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-wiz-desc textarea').first().fill('e2e KD: distylacja nauczyciel→student');
      await page.locator('#ml-studio-wiz-next').click();

      const ftRadio = page.locator('#ml-studio-wiz-types tf-radio[value="ft_llm"]').first();
      await ftRadio.waitFor({ state: 'attached', timeout: 10000 });
      await ftRadio.click();
      await page.waitForTimeout(300);
      await page.locator('#ml-studio-wiz-next').click();

      // Upload CSV przez tf-file-input (pułapka: setInputFiles nie wyzwala własnego change).
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
      await shotBoth(page, '01-wizard-dane');
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
        step('2. Kreator → projekt ft_llm "KD Test" + upload CSV', false,
          `Backend odrzucił utworzenie. Toast: "${toastTxt || '(brak)'}"`);
        throw new Error(`Tworzenie projektu nie powiodło się (toast="${toastTxt}")`);
      }
      step('2. Kreator → projekt ft_llm "KD Test" + upload CSV', true,
        `Projekt "${PROJECT_NAME}" utworzony przez 4-krokowy kreator; CSV ft-sample.csv (prompt/response) załadowany`);
    } catch (e) {
      await shotBoth(page, '02-wizard-FAIL').catch(() => {});
      step('2. Kreator → projekt ft_llm + upload CSV', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 3: Model bazowy custom student + objective=KD + pole teacher pojawia się ----
    try {
      const modelTab = page.locator('#ml-studio-tabs tf-tab[label="Model bazowy"]');
      await modelTab.waitFor({ state: 'attached', timeout: 10000 });
      await modelTab.click();
      await page.waitForSelector('.ml-studio-ft-model-card[data-model="__custom__"]', { timeout: 15000 });
      await page.waitForTimeout(400);

      // Student = custom Qwen.
      await page.locator('.ml-studio-ft-model-card[data-model="__custom__"]').click();
      const repoInput = page.locator('#ml-studio-ft-custom-repo input').first();
      await repoInput.waitFor({ state: 'visible', timeout: 10000 });
      await repoInput.click();
      await repoInput.fill(CUSTOM_REPO);
      await page.waitForTimeout(200);

      // Najpierw potwierdź, że pole teacher jest UKRYTE zanim wybierzemy KD
      // (objective domyślny != kd). Sprawdzamy realny computed display.
      facts.teacherFieldVisibleNonKd = await page.evaluate(() => {
        const f = document.querySelector('#ml-studio-ft-teacher-field');
        if (!f) return 'brak-pola';
        const disp = getComputedStyle(f).display;
        return disp === 'none' ? false : true;
      });

      // Objective = KD (karta data-objective="kd").
      const kdCard = page.locator('.ml-studio-ft-axis-card[data-objective="kd"]');
      await kdCard.waitFor({ state: 'attached', timeout: 10000 });
      await kdCard.click();
      await page.waitForTimeout(300);

      // Po wyborze KD pole teacher POWINNO się pojawić.
      facts.teacherFieldVisibleKd = await page.evaluate(() => {
        const f = document.querySelector('#ml-studio-ft-teacher-field');
        if (!f) return 'brak-pola';
        const disp = getComputedStyle(f).display;
        return disp === 'none' ? false : true;
      });

      // Metoda LoRA.
      await page.locator('.ml-studio-ft-method-card[data-method="lora"]').click();
      await page.waitForTimeout(200);

      // epochs = 1
      const epochsInput = page.locator('#ml-studio-ft-hp-epochs input').first();
      await epochsInput.fill('1');
      await page.waitForTimeout(150);

      // Zaznaczenie KD klasą .selected.
      facts.objectiveSelected = await page.evaluate(() => {
        const c = document.querySelector('.ml-studio-ft-axis-card[data-objective="kd"]');
        return c && c.classList.contains('selected') ? 'kd' : (c ? 'NIE-zaznaczone' : 'brak-karty');
      });

      // Zrzut: KD zaznaczony + widoczne pole teacher (jeszcze puste).
      await shotBoth(page, '03-objective-kd-teacher-widoczny');

      await page.locator('#ml-studio-ft-save').click();
      await page.waitForTimeout(400);

      const repoVal = await repoInput.inputValue().catch(() => '');
      const teacherFieldOk = facts.teacherFieldVisibleNonKd === false && facts.teacherFieldVisibleKd === true;
      const pass = repoVal === CUSTOM_REPO && facts.objectiveSelected === 'kd' && teacherFieldOk;
      step('3. Model bazowy student + objective=KD + pole teacher tylko dla KD', pass,
        `student="${repoVal}" objective(UI)="${facts.objectiveSelected}" metoda=LoRA epochs=1 | pole teacher: nie-KD=${facts.teacherFieldVisibleNonKd} KD=${facts.teacherFieldVisibleKd} (oczek. false→true)`);
    } catch (e) {
      await shotBoth(page, '03-model-FAIL').catch(() => {});
      step('3. Model bazowy + objective KD + pole teacher', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 4a: WALIDACJA — KD bez teacher → toast + BRAK żądania RPC ----
    try {
      const trainTab = page.locator('#ml-studio-tabs tf-tab[label="Trening"]');
      await trainTab.waitFor({ state: 'attached', timeout: 10000 });
      await trainTab.click();
      await page.waitForSelector('#ml-studio-ft-run', { timeout: 15000 });
      await page.waitForTimeout(400);

      // Pole teacher żyje na zakładce „Model bazowy", nie tu — sprawdzamy stan cfg
      // przez to, że w kroku 3 NIE wpisaliśmy nauczyciela (pole zostało puste).
      // Zarejestruj MutationObserver na kontenerze toastów PRZED klikiem, bo toast
      // znika po duration (tf-toast-out) — odczyt po stałym czasie bywa za późny.
      await page.evaluate(() => {
        window.__toastTexts = [];
        // utils.js toast() renderuje div.toast.toast-<kind> w div.toast-container
        // (NIE <tf-toast>); treść w textContent. Łapiemy dodane węzły .toast.
        const push = (n) => {
          const txt = (n.textContent || '').replace(/\s+/g, ' ').trim();
          if (txt) window.__toastTexts.push(txt);
        };
        const obs = new MutationObserver((muts) => {
          for (const m of muts) {
            m.addedNodes && m.addedNodes.forEach((n) => {
              if (n.nodeType !== 1) return;
              if (n.matches && n.matches('.toast')) push(n);
              if (n.querySelectorAll) n.querySelectorAll('.toast').forEach(push);
            });
          }
        });
        obs.observe(document.body, { childList: true, subtree: true });
        window.__toastObs = obs;
      });

      const rpcBefore = await page.evaluate(() => (window.__rpcCapture || []).length);
      await page.locator('#ml-studio-ft-run').click();
      await page.waitForTimeout(1200);

      facts.validationToast = await page.evaluate(() => {
        const live = document.querySelector('.toast-container .toast');
        const liveTxt = live ? (live.textContent || '').replace(/\s+/g, ' ').trim() : '';
        const all = (window.__toastTexts || []);
        const kd = all.find((t) => /KD wymaga/i.test(t));
        return kd || liveTxt || all[all.length - 1] || '';
      });
      facts.rpcCountAfterEmptyTeacher = await page.evaluate(() => (window.__rpcCapture || []).length);
      const teacherValBefore = '(pole na zakł. Model bazowy — pozostawione puste w kroku 3)';

      await shotBoth(page, '04-walidacja-pusty-teacher');

      const toastOk = /KD wymaga modelu-nauczyciela/i.test(facts.validationToast);
      const noRpc = facts.rpcCountAfterEmptyTeacher === rpcBefore;
      const pass = toastOk && noRpc;
      step('4a. Walidacja: KD bez teacher → toast + brak żądania RPC', pass,
        `teacher input przed="${teacherValBefore}" | toast="${facts.validationToast || '(brak)'}" (oczek. „KD wymaga…") | RPC startów: przed=${rpcBefore} po=${facts.rpcCountAfterEmptyTeacher} (oczek. bez zmiany)`);
    } catch (e) {
      await shotBoth(page, '04-walidacja-FAIL').catch(() => {});
      step('4a. Walidacja: KD bez teacher', false, `Błąd: ${e.message}`);
      // Nie przerywaj — kontynuujemy z wpisanym teacher.
    }

    // ---- Krok 4b: wpisz teacher → uruchom → succeeded + objective:"kd" + teacherModel ----
    try {
      // Pole teacher jest na zakładce Model bazowy — wróć, wpisz, zapisz.
      const modelTab = page.locator('#ml-studio-tabs tf-tab[label="Model bazowy"]');
      await modelTab.click();
      await page.waitForSelector('#ml-studio-ft-teacher input', { timeout: 10000 });
      await page.waitForTimeout(300);

      const teacherInput = page.locator('#ml-studio-ft-teacher input').first();
      await teacherInput.waitFor({ state: 'visible', timeout: 10000 });
      await teacherInput.click();
      await teacherInput.fill(TEACHER_REPO);
      // Wyzwól event 'input', na który nasłuchuje handler (cfg.teacherModel).
      await page.evaluate((repo) => {
        const inp = document.querySelector('#ml-studio-ft-teacher input');
        if (inp) {
          inp.value = repo;
          inp.dispatchEvent(new Event('input', { bubbles: true }));
        }
      }, TEACHER_REPO);
      await page.waitForTimeout(200);
      await page.locator('#ml-studio-ft-save').click();
      await page.waitForTimeout(400);

      const teacherValAfter = await teacherInput.inputValue().catch(() => '');

      // Trening → uruchom.
      const trainTab = page.locator('#ml-studio-tabs tf-tab[label="Trening"]');
      await trainTab.click();
      await page.waitForSelector('#ml-studio-ft-run', { timeout: 15000 });
      await page.waitForTimeout(400);
      await shotBoth(page, '05-trening-przed-startem');

      const rpcBefore = await page.evaluate(() => (window.__rpcCapture || []).length);
      await page.locator('#ml-studio-ft-run').click();

      // Sprawdź przechwycony payload RPC (objective + teacherModel).
      await page.waitForTimeout(1000);
      const cap = await page.evaluate(() => window.__rpcCapture || []);
      if (cap.length > rpcBefore) {
        facts.rpcSeen = true;
        const last = cap[cap.length - 1].payload || {};
        facts.rpcObjective = String(last.objective ?? '');
        facts.rpcTeacher = String(last.teacherModel ?? last.teacher_model ?? '');
      }

      await page.waitForSelector('#ml-studio-ft-live .ml-studio-ft-live', { timeout: 15000 }).catch(() => {});
      const status = await page.waitForFunction(() => {
        const badge = document.querySelector('#ml-studio-ft-status-badge');
        const t = badge ? (badge.textContent || '') : '';
        return /zakończony/i.test(t) ? 'succeeded' : (/błąd/i.test(t) ? 'failed' : false);
      }, { timeout: 120000 }).then((h) => h.jsonValue()).catch(() => false);
      facts.trainStatus = String(status);

      await page.waitForTimeout(500);
      await shotBoth(page, '06-trening-live');

      const lossInfo = await page.evaluate(() => {
        const kpiNodes = Array.from(document.querySelectorAll('.ml-studio-ft-kpi'));
        const kpiTxt = kpiNodes.map((n) => n.textContent.replace(/\s+/g, ' ').trim()).join(' | ');
        let trainLossVal = '';
        for (const n of kpiNodes) {
          const lbl = n.querySelector('.lbl'); const val = n.querySelector('.val');
          if (lbl && /train loss/i.test(lbl.textContent) && val) trainLossVal = val.textContent.trim();
        }
        return { kpi: kpiTxt, trainLossVal };
      });
      facts.trainLoss = lossInfo.trainLossVal;

      const lossFilled = !!lossInfo.trainLossVal && lossInfo.trainLossVal !== '—';
      const rpcOk = facts.rpcObjective === 'kd';
      const teacherOk = facts.rpcTeacher === TEACHER_REPO;
      const pass = teacherValAfter === TEACHER_REPO && status === 'succeeded' && rpcOk && teacherOk && lossFilled;
      step('4b. KD: teacher wpisany → trening succeeded + objective:"kd" + teacherModel w RPC + train_loss', pass,
        `teacher input="${teacherValAfter}" | status=${status} | RPC objective="${facts.rpcObjective}" teacherModel="${facts.rpcTeacher}" (przechwycony=${facts.rpcSeen}) | train_loss="${facts.trainLoss || '—'}" | KPI: ${lossInfo.kpi.slice(0, 120)}`);
      if (status !== 'succeeded') throw new Error(`Trening nie succeeded: ${status}`);
    } catch (e) {
      await shotBoth(page, '06-trening-FAIL').catch(() => {});
      step('4b. KD: teacher → trening succeeded + RPC', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 5: zakładka Modele — model KD widoczny ----
    try {
      const modelsTab = page.locator('#ml-studio-tabs tf-tab[label="Modele"]');
      await modelsTab.click();
      await page.waitForSelector('#ml-studio-models-table tf-table', { timeout: 15000 });
      await page.waitForFunction(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return t && (t.rows || []).length > 0;
      }, { timeout: 15000 });
      await page.waitForTimeout(400);
      await shotBoth(page, '07-modele');

      const rows = await page.evaluate(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return (t.rows || []).map((r) => ({
          model: r.model, framework: r.framework, baseModel: r.baseModel,
        }));
      });
      facts.modelName = rows[0]?.model || '';
      const pass = rows.length >= 1;
      step('5. Zakładka Modele: model KD widoczny', pass,
        `modele=${rows.length} | pierwszy: "${facts.modelName}" (silnik=${rows[0]?.framework}, base=${rows[0]?.baseModel})`);
    } catch (e) {
      await shotBoth(page, '07-modele-FAIL').catch(() => {});
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
    console.log(`  pole teacher — nie-KD widoczne: ${facts.teacherFieldVisibleNonKd}`);
    console.log(`  pole teacher — KD widoczne: ${facts.teacherFieldVisibleKd}`);
    console.log(`  objective wybrany w UI: ${facts.objectiveSelected || '(brak)'}`);
    console.log(`  WALIDACJA pusty teacher — toast: ${facts.validationToast || '(brak)'}`);
    console.log(`  WALIDACJA pusty teacher — RPC startów po: ${facts.rpcCountAfterEmptyTeacher}`);
    console.log(`  objective w RPC: ${facts.rpcObjective || '(brak)'} (przechwycony=${facts.rpcSeen})`);
    console.log(`  teacherModel w RPC: ${facts.rpcTeacher || '(brak)'}`);
    console.log(`  status treningu: ${facts.trainStatus || '(brak)'}`);
    console.log(`  train_loss: ${facts.trainLoss || '(brak)'}`);
    console.log(`  model KD w rejestrze: ${facts.modelName || '(brak)'}`);

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
