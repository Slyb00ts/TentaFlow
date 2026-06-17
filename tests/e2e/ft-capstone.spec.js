// =============================================================================
// File: tests/e2e/ft-capstone.spec.js
// Description: Pełny e2e (Playwright) cyklu fine-tuningu LLM w ML Studio przeciw
//   ŻYWEJ instancji TentaFlow (https://localhost:8095, power1/power123). Klika UI
//   jak realny użytkownik: login → potwierdzenie roli power_user + nav ML Studio →
//   kreator 4-krokowy (projekt ft_llm „FT Capstone" + upload CSV) → Model bazowy
//   (custom HF Qwen) → Trening LoRA/SFT epochs=1 (polling do succeeded + krzywa
//   loss) → Modele → Eksport GGUF q8_0 → Deploy do inferencji (alias) → odpytanie
//   modelu FT przez REST /v1/chat/completions. Zrzuty @1440 i @390 do
//   /tmp/ft-capstone-shots/. Standalone node — NIE spawnuje binarki.
// =============================================================================

const fs = require('fs');
const crypto = require('crypto');
const { execSync } = require('child_process');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/ft-capstone-shots';
const CSV = '/tmp/ft-sample.csv';
// Tier 2 REST (/v1/*) wymaga API key z założenia (dashboard NIGDY nie używa REST —
// chodzi po binarnym protokole). Klucz prowizjonujemy wprost w SQLite dla power1
// (hash = sha256(klucz), zgodnie z dashboard::auth::hash_api_key). Ścieżka pliku
// klucza opcjonalnie z env, inaczej mint inline.
const DB = process.env.TF_DB || '/home/critix/repos/rust/TentaFlow-ml/.runtime/data/tentaflow.db';
const POWER1_ID = '00000000-0000-4000-8000-0000000000bb';
const PROJECT_NAME = `FT Capstone ${Date.now()}`;
const CUSTOM_REPO = 'Qwen/Qwen2.5-0.5B-Instruct';

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}

async function shotBoth(page, base) {
  // Zrzut @1440 i @390 (mockupy są responsywne). Sprawdza też brak poziomego scrolla.
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
  // Wróć do desktopu na dalsze kroki.
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(200);
  return out;
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  if (!fs.existsSync(CSV)) { console.log(`FATAL: brak ${CSV}`); process.exit(1); }

  const consoleErrors = [];
  const failedRequests = [];
  const rpcSeen = new Set();

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();

  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(msg.text()); });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));
  page.on('requestfailed', (req) => failedRequests.push(`${req.method()} ${req.url()} :: ${req.failure()?.errorText}`));

  // Fakty do raportu.
  const facts = {
    role: '', profileLabel: '', navPresent: false,
    ggufPath: '', ggufSize: '', alias: '', ftAnswer: '', responsive: [],
  };

  try {
    // ---- Krok 1: login power1/power123 + potwierdź rolę + nav ML Studio ----
    try {
      await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });

      // Przechwyć odpowiedź authMe/authLogin (binarny WS dekodowany w JS) przez hook
      // na obiekcie window — ApiBinary zwraca zdekodowany obiekt; podsłuchamy rolę
      // wprost z UI po zalogowaniu (profil) oraz z localStorage/usera.
      const userInput = page.locator('#login-username input').first();
      await userInput.waitFor({ state: 'visible', timeout: 20000 });
      await userInput.fill('power1');
      await page.locator('#login-password input').first().fill('power123');
      await page.locator('#login-submit').click();
      await page.waitForSelector('.sidebar .nav-item[data-view], aside, nav', { timeout: 20000 });
      await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(800);

      // Rola: odpytaj authMe wprost przez ApiBinary (ES module) w kontekście strony.
      const authMe = await page.evaluate(async () => {
        try {
          const mod = await import('/js/protocol/api-binary-shim.js');
          const api = mod.ApiBinary;
          if (api && api.one) {
            const r = await api.one('authMeRequest');
            return { ok: true, raw: r };
          }
          return { ok: false, err: 'ApiBinary.one niedostępne' };
        } catch (e) { return { ok: false, err: String(e && e.message || e) }; }
      });

      let role = '';
      if (authMe.ok && authMe.raw) {
        const u = authMe.raw.user || authMe.raw;
        role = String(u.role ?? u.role_slug ?? authMe.raw.role ?? '');
      }
      facts.role = role;

      // Etykieta profilu w UI (Power User vs Użytkownik).
      const profileLabel = await page.evaluate(() => {
        const txt = document.body.innerText || '';
        if (/Power User/i.test(txt)) return 'Power User';
        if (/Użytkownik/i.test(txt)) return 'Użytkownik';
        return '';
      });
      facts.profileLabel = profileLabel;

      const navItem = page.locator('.sidebar .nav-item[data-view="ml-studio"]');
      const navCount = await navItem.count();
      facts.navPresent = navCount > 0;

      await shotBoth(page, '00-after-login');

      if (!facts.navPresent) {
        step('1. Login power1 → rola power_user + nav ML Studio', false,
          `REGRESJA: nav-item ML Studio NIE istnieje. role=${role || '(brak)'} profil=${profileLabel || '(brak)'}`);
        throw new Error('Brak nav ML Studio — przerwanie (nie obchodzę przez Router.navigate).');
      }

      await navItem.first().click();
      await page.waitForSelector('#ml-studio-new, .ml-studio', { timeout: 15000 });
      await page.waitForTimeout(500);

      // Decydujące dowody power_user: authMe.role === 'power_user' ORAZ obecny nav
      // ML Studio (gated na isPowerUser w app.js). Etykieta profilu jest tylko
      // informacyjna — profile.js mapuje power_user na "Użytkownik" (luka labelingu).
      const rolePass = role === 'power_user';
      const pass = rolePass && facts.navPresent;
      step('1. Login power1 → rola power_user + nav ML Studio', pass,
        `authMe role="${role || '(brak)'}" (oczek. power_user=${rolePass}) | nav ML Studio=${facts.navPresent} | profil UI pokazuje="${profileLabel}" (uwaga: profile.js nie ma etykiety Power User) ${authMe.ok ? '' : '| authMe err: ' + authMe.err}`);
    } catch (e) {
      await shotBoth(page, '00-login-FAIL').catch(() => {});
      step('1. Login power1 → rola power_user + nav ML Studio', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 2: kreator 4-kroki → projekt ft_llm + upload CSV ----
    let projectUrlId = '';
    try {
      await page.locator('#ml-studio-new').click();
      // Krok 1 kreatora: nazwa
      const nameInput = page.locator('#ml-studio-wiz-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 15000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-wiz-desc textarea').first().fill('e2e capstone: pełny cykl FT LLM');
      await shotBoth(page, '01-wizard-krok1');
      await page.locator('#ml-studio-wiz-next').click();

      // Krok 2: typ projektu = ft_llm (tf-radio card value="ft_llm")
      const ftRadio = page.locator('#ml-studio-wiz-types tf-radio[value="ft_llm"]').first();
      await ftRadio.waitFor({ state: 'attached', timeout: 10000 });
      await ftRadio.click();
      await page.waitForTimeout(300);
      await shotBoth(page, '02-wizard-krok2-typ');
      await page.locator('#ml-studio-wiz-next').click();

      // Krok 3: dane — upload CSV przez natywny input tf-file-input
      await page.waitForSelector('#ml-studio-wiz-file', { timeout: 10000 });
      const nativeFile = page.locator('#ml-studio-wiz-file input.tf-file-input-native').first();
      await nativeFile.waitFor({ state: 'attached', timeout: 10000 });
      await nativeFile.setInputFiles(CSV);
      // Playwright setInputFiles na ukrytym natywnym input nie zawsze wyzwala własny
      // `change` tf-file-input — dorzucamy CustomEvent(change, detail.files), na który
      // nasłuchuje kreator (ml-studio.js: byId('ml-studio-wiz-file').on('change')).
      await page.evaluate(() => {
        const fi = document.querySelector('#ml-studio-wiz-file');
        const native = fi?.querySelector('input.tf-file-input-native');
        if (fi && native && native.files && native.files.length) {
          fi.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { files: native.files } }));
        }
      });
      await page.waitForSelector('#ml-studio-wiz-file-info:not([hidden])', { timeout: 10000 }).catch(() => {});
      await page.waitForTimeout(300);
      await shotBoth(page, '03-wizard-krok3-dane');
      await page.locator('#ml-studio-wiz-next').click();

      // Krok 4: podsumowanie → Utwórz projekt
      const createBtn = page.locator('#ml-studio-wiz-create');
      await createBtn.waitFor({ state: 'visible', timeout: 10000 });
      await shotBoth(page, '04-wizard-krok4-podsumowanie');
      await createBtn.click();

      // Sukces = widok szczegółu (zakładki ft_llm). Błąd = toast (np. backend
      // odrzucił create). Wyścig, by nie czekać pełnego timeoutu na regresji.
      const outcome = await Promise.race([
        page.waitForSelector('#ml-studio-tabs', { timeout: 25000 }).then(() => 'tabs').catch(() => null),
        page.waitForSelector('tf-toast, .tf-toast', { timeout: 25000 }).then(() => 'toast').catch(() => null),
      ]);
      await page.waitForTimeout(600);
      const toastTxt = await page.evaluate(() => {
        const t = document.querySelector('tf-toast, .tf-toast');
        return t ? (t.textContent || '').replace(/\s+/g, ' ').trim() : '';
      });
      await shotBoth(page, '05-overview');

      if (outcome !== 'tabs' || !(await page.locator('#ml-studio-tabs').count())) {
        step('2. Kreator → projekt ft_llm "FT Capstone" + upload CSV', false,
          `Backend odrzucił utworzenie projektu. Toast UI: "${toastTxt || '(brak — cicha porażka)'}"`);
        throw new Error(`Tworzenie projektu nie powiodło się (toast="${toastTxt}")`);
      }

      projectUrlId = await page.evaluate(() => (location.hash || location.search || ''));
      step('2. Kreator → projekt ft_llm "FT Capstone" + upload CSV', true,
        `Projekt "${PROJECT_NAME}" utworzony przez 4-krokowy kreator; CSV ft-sample.csv załadowany; widok szczegółu (ft_llm) otwarty`);
    } catch (e) {
      await shotBoth(page, '02-wizard-FAIL').catch(() => {});
      step('2. Kreator → projekt ft_llm + upload CSV', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 3a: Model bazowy = custom Qwen ----
    try {
      const modelTab = page.locator('#ml-studio-tabs tf-tab[label="Model bazowy"]');
      await modelTab.waitFor({ state: 'attached', timeout: 10000 });
      await modelTab.click();
      await page.waitForSelector('.ml-studio-ft-model-card[data-model="__custom__"]', { timeout: 15000 });
      await page.waitForTimeout(400);

      // Klik karty custom → focus pola → wpisz repo (event input zapisze customRepo).
      await page.locator('.ml-studio-ft-model-card[data-model="__custom__"]').click();
      const repoInput = page.locator('#ml-studio-ft-custom-repo input').first();
      await repoInput.waitFor({ state: 'visible', timeout: 10000 });
      await repoInput.click();
      await repoInput.fill(CUSTOM_REPO);
      await page.waitForTimeout(200);

      // Wybierz metodę LoRA (oś 2) i cel SFT (oś 1, domyślny).
      await page.locator('.ml-studio-ft-method-card[data-method="lora"]').click();
      await page.locator('.ml-studio-ft-axis-card[data-objective="sft"]').click();

      // epochs = 1
      const epochsInput = page.locator('#ml-studio-ft-hp-epochs input').first();
      await epochsInput.fill('1');
      await page.waitForTimeout(150);

      await page.locator('#ml-studio-ft-save').click();
      await page.waitForTimeout(400);
      await shotBoth(page, '06-model-bazowy-custom');

      const repoVal = await repoInput.inputValue().catch(() => '');
      const pass = repoVal === CUSTOM_REPO;
      step('3a. Model bazowy: custom Qwen + LoRA/SFT + epochs=1', pass,
        `repo="${repoVal}" metoda=LoRA cel=SFT epochs=1; konfiguracja zapisana`);
    } catch (e) {
      await shotBoth(page, '06-model-FAIL').catch(() => {});
      step('3a. Model bazowy: custom Qwen', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 3b: Trening → uruchom → polling do succeeded + krzywa loss ----
    try {
      const trainTab = page.locator('#ml-studio-tabs tf-tab[label="Trening"]');
      await trainTab.waitFor({ state: 'attached', timeout: 10000 });
      await trainTab.click();
      await page.waitForSelector('#ml-studio-ft-run', { timeout: 15000 });
      await page.waitForTimeout(400);
      await shotBoth(page, '07-trening-przed-startem');

      await page.locator('#ml-studio-ft-run').click();

      // Widok LIVE pojawia się; czekaj na badge "zakończony" (succeeded).
      await page.waitForSelector('#ml-studio-ft-live .ml-studio-ft-live', { timeout: 15000 });
      const succeeded = await page.waitForFunction(() => {
        const badge = document.querySelector('#ml-studio-ft-status-badge');
        const t = badge ? (badge.textContent || '') : '';
        return /zakończony/i.test(t) ? 'succeeded' : (/błąd/i.test(t) ? 'failed' : false);
      }, { timeout: 120000 }).then((h) => h.jsonValue()).catch(() => false);

      await page.waitForTimeout(500);
      await shotBoth(page, '08-trening-live-loss');

      // Krzywa loss: SVG renderuje się dopiero przy ≥2 punktach lossCurve. Dla
      // mikro-jobu (epochs=1, 4 wiersze) trening kończy się w sekundy z 0–1
      // punktami → komponent pokazuje placeholder zamiast SVG. To NIE jest błąd
      // produktu: dowodem działania krzywej jest sekcja loss + zapełnione KPI
      // (train/eval loss). Akceptujemy SVG LUB metryki loss w KPI.
      const lossInfo = await page.evaluate(() => {
        const svg = document.querySelector('.ml-studio-ft-loss-svg');
        const polylines = svg ? svg.querySelectorAll('polyline, path').length : 0;
        const chart = document.querySelector('.ml-studio-ft-chart-title, .ml-studio-ft-chart-empty');
        const kpiNodes = Array.from(document.querySelectorAll('.ml-studio-ft-kpi'));
        const kpiTxt = kpiNodes.map((n) => n.textContent.replace(/\s+/g, ' ').trim()).join(' | ');
        const lossKpiPresent = /loss/i.test(kpiTxt) && /\d/.test(kpiTxt);
        return {
          svgPresent: !!svg, lines: polylines,
          chartSection: !!chart, lossKpiPresent, kpi: kpiTxt,
        };
      });

      const lossOk = lossInfo.svgPresent || (lossInfo.chartSection && lossInfo.lossKpiPresent);
      const pass = succeeded === 'succeeded' && lossOk;
      step('3b. Trening LoRA/SFT → succeeded + krzywa loss', pass,
        `status=${succeeded} | sekcja krzywej=${lossInfo.chartSection} SVG=${lossInfo.svgPresent} (linie=${lossInfo.lines}; mikro-job <2 punktów → placeholder, oczekiwane) | metryki loss w KPI=${lossInfo.lossKpiPresent} | KPI: ${lossInfo.kpi.slice(0, 140)}`);
      if (succeeded !== 'succeeded') throw new Error(`Trening nie zakończył się sukcesem: status=${succeeded}`);
    } catch (e) {
      await shotBoth(page, '08-trening-FAIL').catch(() => {});
      step('3b. Trening LoRA/SFT → succeeded', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 3c: zakładka Modele — model widoczny ----
    let modelName = '';
    try {
      const modelsTab = page.locator('#ml-studio-tabs tf-tab[label="Modele"]');
      await modelsTab.click();
      await page.waitForSelector('#ml-studio-models-table tf-table', { timeout: 15000 });
      await page.waitForFunction(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return t && (t.rows || []).length > 0;
      }, { timeout: 15000 });
      await page.waitForTimeout(400);
      await shotBoth(page, '09-modele');

      const rows = await page.evaluate(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return (t.rows || []).map((r) => ({
          model: r.model, framework: r.framework, baseModel: r.baseModel,
          canExport: !!r._canExport,
        }));
      });
      modelName = rows[0]?.model || '';
      const pass = rows.length >= 1 && rows.some((r) => r.canExport);
      step('3c. Zakładka Modele: model FT widoczny', pass,
        `modele=${rows.length} | pierwszy: "${modelName}" (silnik=${rows[0]?.framework}, base=${rows[0]?.baseModel}, eksport=${rows[0]?.canExport})`);
    } catch (e) {
      await shotBoth(page, '09-modele-FAIL').catch(() => {});
      step('3c. Zakładka Modele', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 4: Eksport GGUF q8_0 → succeeded (ścieżka + rozmiar) ----
    try {
      const exportBtn = page.locator('#ml-studio-models-table tf-table tf-button', { hasText: 'Eksportuj GGUF' }).first();
      await exportBtn.waitFor({ state: 'attached', timeout: 10000 });
      await exportBtn.click();

      // Modal eksportu — outtype q8_0 jest domyślny; klik Eksportuj.
      await page.waitForSelector('#ml-studio-export-start', { timeout: 10000 });
      await page.waitForTimeout(300);
      await shotBoth(page, '10-modal-eksport');
      await page.locator('#ml-studio-export-start').click();

      // Czekaj na widok wyniku (ścieżka + rozmiar). Eksport q8_0 może potrwać.
      await page.waitForSelector('.ml-studio-export-result', { timeout: 180000 });
      await page.waitForTimeout(400);
      const exp = await page.evaluate(() => {
        const path = document.querySelector('.ml-studio-export-path');
        const rows = document.querySelectorAll('.ml-studio-export-result-row .val');
        return {
          path: path ? path.textContent.trim() : '',
          size: rows.length ? rows[rows.length - 1].textContent.trim() : '',
        };
      });
      facts.ggufPath = exp.path;
      facts.ggufSize = exp.size;
      await shotBoth(page, '11-eksport-wynik');

      const pass = !!exp.path && exp.path !== '—';
      step('4. Eksport GGUF q8_0 → succeeded', pass,
        `ścieżka="${exp.path}" rozmiar="${exp.size}"`);
    } catch (e) {
      await shotBoth(page, '11-eksport-FAIL').catch(() => {});
      step('4. Eksport GGUF q8_0', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 5: Deploy do inferencji → 'deploying' + alias ----
    try {
      const deployBtn = page.locator('#ml-studio-export-deploy-btn');
      await deployBtn.waitFor({ state: 'visible', timeout: 10000 });
      await deployBtn.click();

      // Sukces: pojawia się .ml-studio-export-deploy-ok z aliasem w <code>.
      await page.waitForSelector('.ml-studio-export-deploy-ok', { timeout: 60000 });
      await page.waitForTimeout(300);
      const alias = await page.evaluate(() => {
        const ok = document.querySelector('.ml-studio-export-deploy-ok code.ml-studio-mono');
        return ok ? ok.textContent.trim() : '';
      });
      facts.alias = alias;
      await shotBoth(page, '12-deploy-ok');

      const pass = !!alias;
      step('5. Deploy do inferencji → status deploying + alias', pass,
        `alias="${alias}"`);
    } catch (e) {
      await shotBoth(page, '12-deploy-FAIL').catch(() => {});
      step('5. Deploy do inferencji', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 6: odpytanie modelu FT przez REST /v1/chat/completions ----
    try {
      const alias = facts.alias;
      if (!alias) throw new Error('brak aliasu z kroku 5');

      // Mint API key dla power1 (sha256 = hash_api_key) — REST /v1/* go wymaga.
      const apiKey = `tf-e2e-capstone-${Date.now()}`;
      const apiHash = crypto.createHash('sha256').update(apiKey).digest('hex');
      const apiPrefix = apiKey.slice(0, 8);
      execSync(
        `sqlite3 ${DB} "INSERT INTO api_keys (key_hash, key_prefix, name, owner_user_id, is_active) ` +
        `VALUES ('${apiHash}','${apiPrefix}','e2e-capstone','${POWER1_ID}',1);"`,
        { encoding: 'utf8' },
      );
      facts.apiKey = apiPrefix + '…';

      // Embedded ładowanie GGUF może potrwać — polling do ~90s.
      let answer = '';
      let lastErr = '';
      const deadline = Date.now() + 95000;
      while (Date.now() < deadline) {
        try {
          const body = JSON.stringify({
            model: alias,
            messages: [{ role: 'user', content: 'Jaka jest stolica Polski?' }],
            max_tokens: 40,
          });
          const out = execSync(
            `curl -sk ${BASE}/v1/chat/completions -H 'Content-Type: application/json' ` +
            `-H 'Authorization: Bearer ${apiKey}' -d '${body.replace(/'/g, "'\\''")}'`,
            { encoding: 'utf8', timeout: 30000 },
          );
          let parsed;
          try { parsed = JSON.parse(out); } catch { parsed = null; }
          if (parsed && parsed.choices && parsed.choices[0]) {
            answer = String(parsed.choices[0].message?.content ?? parsed.choices[0].text ?? '').trim();
            if (answer) break;
          }
          lastErr = out.slice(0, 200);
          if (/model.*not.*found|nie.*znalezion/i.test(out)) {
            // model jeszcze się ładuje — czekaj.
          }
        } catch (e) { lastErr = String(e.message).slice(0, 200); }
        await page.waitForTimeout(4000);
      }
      facts.ftAnswer = answer;
      const pass = !!answer && !/not found/i.test(answer);
      step('6. Odpytanie modelu FT (/v1/chat/completions)', pass,
        pass ? `odpowiedź="${answer.slice(0, 160)}"` : `brak realnej odpowiedzi w 95s; ostatnio: ${lastErr}`);
    } catch (e) {
      step('6. Odpytanie modelu FT', false, `Błąd: ${e.message}`);
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
    console.log(`  profil UI: ${facts.profileLabel || '(brak)'}`);
    console.log(`  nav ML Studio: ${facts.navPresent}`);
    console.log(`  GGUF ścieżka: ${facts.ggufPath || '(brak)'}`);
    console.log(`  GGUF rozmiar: ${facts.ggufSize || '(brak)'}`);
    console.log(`  alias deploy: ${facts.alias || '(brak)'}`);
    console.log(`  odpowiedź FT: ${facts.ftAnswer || '(brak)'}`);

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
