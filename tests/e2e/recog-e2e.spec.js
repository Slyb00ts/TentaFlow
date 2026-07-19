// =============================================================================
// File: tests/e2e/recog-e2e.spec.js
// Description: Pełny e2e (Playwright) cyklu ROZPOZNAWANIA (RF-DETR) w ML Studio
//   przeciw ŻYWEJ instancji TentaFlow (https://localhost:8095, power1/power123).
//   Klika UI jak realny użytkownik: login → nav ML Studio → kreator 4-krokowy
//   (typ recognition „ADR E2E", krok danych pominięty) → zakładka Dane:
//   rejestracja datasetu COCO przez ŚCIEŻKĘ (/tmp/coco-mini) → zakładka Schemat:
//   wybór datasetu, wariant nano, epochs=2, resolution=384, Uruchom trening →
//   polling LIVE do „zakończony" (succeeded) z train loss + mAP@50 → zakładka
//   Modele: model rfdetr-nano z akcją „Wykryj na zdjęciu" → modal detekcji:
//   upload /tmp/detect-test.jpg, próg 0.3 (i ewentualnie 0.1) → lista detekcji
//   lub pusta lista (bez błędu). Zrzuty @1440 i @390 do /tmp/recog-e2e-shots/.
//   Standalone node — NIE spawnuje binarki.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/recog-e2e-shots';
const COCO_PATH = '/tmp/coco-mini';
const DETECT_IMG = '/tmp/detect-test.jpg';
const PROJECT_NAME = `ADR E2E ${Date.now()}`;
const DATASET_NAME = 'ADR mini';

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}

const responsiveLog = [];
async function shotBoth(page, base, { fullPage = true } = {}) {
  // Zrzut @1440 i @390 (mockupy są responsywne). Sprawdza też brak poziomego scrolla.
  const out = {};
  for (const [tag, w, h] of [['1440', 1440, 900], ['390', 390, 844]]) {
    await page.setViewportSize({ width: w, height: h });
    await page.waitForTimeout(350);
    const file = `${SHOT}/${base}@${tag}.png`;
    await page.screenshot({ path: file, fullPage }).catch(() => {});
    const hScroll = await page.evaluate(() =>
      document.documentElement.scrollWidth - document.documentElement.clientWidth);
    out[tag] = { file, hScroll };
    if (tag === '390') responsiveLog.push({ view: base, hScroll });
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(200);
  return out;
}

async function shotCurrent(page, base, tag) {
  // Zrzut bieżącego widoku BEZ zmiany rozmiaru (tf-modal zamyka się na resize).
  const file = `${SHOT}/${base}@${tag}.png`;
  await page.screenshot({ path: file, fullPage: false }).catch(() => {});
  const hScroll = await page.evaluate(() =>
    document.documentElement.scrollWidth - document.documentElement.clientWidth);
  if (tag === '390') responsiveLog.push({ view: base, hScroll });
  return { file, hScroll };
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  if (!fs.existsSync(DETECT_IMG)) { console.log(`FATAL: brak ${DETECT_IMG}`); process.exit(1); }

  const consoleErrors = [];
  const failedRequests = [];

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();

  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(msg.text()); });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));
  page.on('requestfailed', (req) => failedRequests.push(`${req.method()} ${req.url()} :: ${req.failure()?.errorText}`));

  let detectModalWasOpen = null; // czy tf-modal detekcji miał `open` SAM z siebie
  const facts = {
    role: '', navPresent: false,
    dsImages: '', dsClasses: '', dsToast: '',
    trainStatus: '', trainLoss: '', map50: '',
    modelName: '', modelFramework: '',
    detectThreshold: '', detectCount: '', detectClasses: '', detectNote: '',
    responsive: [],
  };

  try {
    // ---- Krok 1: login power1/power123 + nav ML Studio ----
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

      const navItem = page.locator('.sidebar .nav-item[data-view="ml-studio"]');
      facts.navPresent = (await navItem.count()) > 0;
      await shotBoth(page, '00-after-login');

      if (!facts.navPresent) {
        step('1. Login power1 → nav ML Studio', false,
          `nav-item ML Studio NIE istnieje. role=${role || '(brak)'}`);
        throw new Error('Brak nav ML Studio — przerwanie.');
      }
      await navItem.first().click();
      await page.waitForSelector('#ml-studio-new, .ml-studio', { timeout: 15000 });
      await page.waitForTimeout(500);

      const pass = role === 'power_user' && facts.navPresent;
      step('1. Login power1 → nav ML Studio', pass,
        `authMe role="${role || '(brak)'}" | nav ML Studio=${facts.navPresent}`);
    } catch (e) {
      await shotBoth(page, '00-login-FAIL').catch(() => {});
      step('1. Login power1 → nav ML Studio', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 2: kreator 4-kroki → projekt recognition „ADR E2E" ----
    try {
      await page.locator('#ml-studio-new').click();
      // Krok 1: nazwa
      const nameInput = page.locator('#ml-studio-wiz-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 15000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-wiz-desc textarea').first().fill('e2e recognition: pełny cykl RF-DETR (ADR)');
      await page.locator('#ml-studio-wiz-next').click();

      // Krok 2: typ projektu = recognition
      const recogRadio = page.locator('#ml-studio-wiz-types tf-radio[value="recognition"]').first();
      await recogRadio.waitFor({ state: 'attached', timeout: 10000 });
      await recogRadio.click();
      await page.waitForTimeout(300);
      await shotBoth(page, '01-wizard-typ-recognition');
      await page.locator('#ml-studio-wiz-next').click();

      // Krok 3: dane — dla recognition NIE wgrywamy pliku (dataset rejestrujemy
      // potem w zakładce Dane przez ścieżkę). Krok jest opcjonalny → Dalej.
      await page.waitForSelector('#ml-studio-wiz-file', { timeout: 10000 }).catch(() => {});
      await page.waitForTimeout(300);
      await page.locator('#ml-studio-wiz-next').click();

      // Krok 4: podsumowanie → Utwórz projekt
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
      await shotBoth(page, '02-overview-recognition');

      if (outcome !== 'tabs' || !(await page.locator('#ml-studio-tabs').count())) {
        step('2. Kreator → projekt recognition „ADR E2E"', false,
          `Backend odrzucił utworzenie projektu. Toast: "${toastTxt || '(brak)'}"`);
        throw new Error(`Tworzenie projektu nie powiodło się (toast="${toastTxt}")`);
      }
      step('2. Kreator → projekt recognition „ADR E2E"', true,
        `Projekt "${PROJECT_NAME}" utworzony przez 4-krokowy kreator; typ=recognition; widok szczegółu otwarty`);
    } catch (e) {
      await shotBoth(page, '02-wizard-FAIL').catch(() => {});
      step('2. Kreator → projekt recognition', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 3: zakładka Dane → rejestracja datasetu COCO przez ścieżkę ----
    try {
      const daneTab = page.locator('#ml-studio-tabs tf-tab[label="Dane"]');
      await daneTab.waitFor({ state: 'attached', timeout: 10000 });
      await daneTab.click();
      await page.waitForSelector('#ml-studio-recog-path', { timeout: 15000 });
      await page.waitForTimeout(400);

      await page.locator('#ml-studio-recog-path input').first().fill(COCO_PATH);
      await page.locator('#ml-studio-recog-name input').first().fill(DATASET_NAME);
      await shotBoth(page, '03-dane-rejestracja');

      await page.locator('#ml-studio-recog-register').click();

      // Toast „Dataset zarejestrowany: N obrazów, M klas." znika po ~4s — łapiemy go
      // krótkim pollingiem (best-effort). Decydującym dowodem jest jednak wiersz w
      // tabeli „Zarejestrowane zbiory" (#ml-studio-datasets), który czytamy poniżej.
      const toastTxt = await page.evaluate(async () => {
        for (let i = 0; i < 50; i++) {
          const c = document.getElementById('tf-toast-container');
          const msg = c?.querySelector('.tf-toast-message');
          const txt = (msg?.textContent || c?.textContent || '').replace(/\s+/g, ' ').trim();
          if (txt) return txt;
          await new Promise((r) => setTimeout(r, 120));
        }
        return '';
      });
      facts.dsToast = toastTxt;

      // Wiersz w tabeli zarejestrowanych zbiorów: nazwa + wiersze (obrazy) + kolumny (klasy).
      await page.waitForFunction((name) => {
        const t = document.querySelector('#ml-studio-datasets tf-table');
        return t && (t.rows || []).some((r) => String(r.name || '') === name);
      }, DATASET_NAME, { timeout: 30000 });
      await page.waitForTimeout(400);
      const ds = await page.evaluate((name) => {
        const t = document.querySelector('#ml-studio-datasets tf-table');
        const row = (t.rows || []).find((r) => String(r.name || '') === name) || {};
        return { kind: row.kind || row.fileType || '', images: String(row.rowCount ?? row.rows ?? ''), classes: String(row.columnCount ?? row.columns ?? '') };
      }, DATASET_NAME);
      // Wartości z toasta jeśli dostępne, inaczej z tabeli (kolumny WIERSZE/KOLUMNY).
      const m = toastTxt.match(/(\d+)\s*obraz\w*.*?(\d+)\s*klas/i);
      facts.dsImages = m ? m[1] : ds.images;
      facts.dsClasses = m ? m[2] : ds.classes;
      await shotBoth(page, '04-dane-zarejestrowano');

      const pass = Number(facts.dsImages) > 0 && Number(facts.dsClasses) > 0;
      step('3. Dane → rejestracja COCO /tmp/coco-mini', pass,
        `dataset "${DATASET_NAME}" w rejestrze | obrazy=${facts.dsImages || '?'} klasy=${facts.dsClasses || '?'} | toast="${toastTxt || '(zniknął przed odczytem)'}"`);
      if (!pass) throw new Error(`Rejestracja datasetu nie powiodła się (obrazy=${facts.dsImages}, klasy=${facts.dsClasses})`);
    } catch (e) {
      await shotBoth(page, '04-dane-FAIL').catch(() => {});
      step('3. Dane → rejestracja COCO', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 4: zakładka Schemat → dataset + wariant nano + epochs=2 + res=384 → trening ----
    try {
      const schematTab = page.locator('#ml-studio-tabs tf-tab[label="Schemat"]');
      await schematTab.waitFor({ state: 'attached', timeout: 10000 });
      await schematTab.click();
      await page.waitForSelector('#ml-studio-recog-run', { timeout: 15000 });
      // Poczekaj aż select datasetów się zapełni (async list).
      await page.waitForFunction(() => {
        const sel = document.querySelector('#ml-studio-recog-dataset');
        return sel && (sel.querySelectorAll('option').length > 0 || (sel.value && sel.value.length));
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(500);

      // Wariant nano.
      const nanoCard = page.locator('#ml-studio-recog-variants .ml-studio-ft-axis-card[data-variant="nano"]');
      await nanoCard.waitFor({ state: 'visible', timeout: 10000 });
      await nanoCard.click();
      await page.waitForTimeout(200);

      // Hiperparametry: epochs=2, resolution=384 (wyzwól input na natywnym input).
      const setHp = async (key, val) => {
        const input = page.locator(`#ml-studio-recog-hp-${key} input`).first();
        await input.waitFor({ state: 'attached', timeout: 8000 });
        await input.fill(String(val));
        await input.dispatchEvent('input');
        await page.waitForTimeout(100);
      };
      await setHp('epochs', 2);
      await setHp('resolution', 384);
      await page.waitForTimeout(200);
      await shotBoth(page, '05-schemat-wariant-hp');

      await page.locator('#ml-studio-recog-run').click();

      // Widok LIVE → badge „zakończony" (succeeded) / „błąd" (failed).
      await page.waitForSelector('#ml-studio-recog-live .ml-studio-ft-live', { timeout: 15000 });
      await page.waitForTimeout(800);
      await shotBoth(page, '06-trening-live');

      const finalStatus = await page.waitForFunction(() => {
        const badge = document.querySelector('#ml-studio-recog-badge');
        const t = badge ? (badge.textContent || '') : '';
        if (/zakończony/i.test(t)) return 'succeeded';
        if (/błąd/i.test(t)) return 'failed';
        return false;
      }, { timeout: 240000 }).then((h) => h.jsonValue()).catch(() => false);

      await page.waitForTimeout(500);
      await shotBoth(page, '07-trening-zakonczony');

      // Odczyt KPI: train loss + mAP@50.
      const kpi = await page.evaluate(() => {
        const grab = (lbl) => {
          const nodes = Array.from(document.querySelectorAll('#ml-studio-recog-kpi .ml-studio-ft-kpi'));
          for (const n of nodes) {
            const l = (n.querySelector('.lbl')?.textContent || '').trim();
            if (l.toLowerCase() === lbl.toLowerCase()) return (n.querySelector('.val')?.textContent || '').trim();
          }
          return '';
        };
        return { loss: grab('train loss'), map50: grab('mAP@50') };
      });
      facts.trainStatus = String(finalStatus);
      facts.trainLoss = kpi.loss;
      facts.map50 = kpi.map50;

      const haveLoss = kpi.loss && kpi.loss !== '—';
      const haveMap = kpi.map50 && kpi.map50 !== '—';
      const pass = finalStatus === 'succeeded' && (haveLoss || haveMap);
      step('4. Schemat → trening RF-DETR nano (epochs=2, res=384) → succeeded', pass,
        `status=${finalStatus} | train loss=${kpi.loss || '—'} | mAP@50=${kpi.map50 || '—'}`);
      if (finalStatus !== 'succeeded') throw new Error(`Trening nie zakończył się sukcesem: status=${finalStatus}`);
    } catch (e) {
      await shotBoth(page, '07-trening-FAIL').catch(() => {});
      step('4. Schemat → trening RF-DETR', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 5a: zakładka Modele — model rfdetr widoczny ----
    try {
      const modeleTab = page.locator('#ml-studio-tabs tf-tab[label="Modele"]');
      await modeleTab.click();
      await page.waitForSelector('#ml-studio-models-table tf-table', { timeout: 15000 });
      await page.waitForFunction(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return t && (t.rows || []).length > 0;
      }, { timeout: 20000 });
      await page.waitForTimeout(500);
      await shotBoth(page, '08-modele');

      const rows = await page.evaluate(() => {
        const t = document.querySelector('#ml-studio-models-table tf-table');
        return (t.rows || []).map((r) => ({
          model: r.model, framework: r.framework, baseModel: r.baseModel, isRecog: !!r._isRecog,
        }));
      });
      const recogRow = rows.find((r) => r.isRecog) || rows[0] || {};
      facts.modelName = recogRow.model || '';
      facts.modelFramework = recogRow.framework || '';
      const pass = rows.length >= 1 && rows.some((r) => r.isRecog);
      step('5a. Zakładka Modele: model RF-DETR widoczny', pass,
        `modele=${rows.length} | model: "${facts.modelName}" (silnik=${facts.modelFramework}, recog=${recogRow.isRecog})`);
      if (!pass) throw new Error('Brak modelu rfdetr w rejestrze.');
    } catch (e) {
      await shotBoth(page, '08-modele-FAIL').catch(() => {});
      step('5a. Zakładka Modele', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Krok 5b: modal „Wykryj na zdjęciu" → upload + próg → detekcja ----
    try {
      const closeDetectModals = async () => {
        await page.evaluate(() => {
          [...document.querySelectorAll('tf-modal')]
            .filter((x) => x.querySelector('#ml-studio-detect-file'))
            .forEach((x) => x.remove());
        });
        await page.waitForTimeout(200);
      };
      const openDetectModal = async () => {
        await closeDetectModals();
        const detectBtn = page.locator('#ml-studio-models-table tf-table tf-button', { hasText: 'Wykryj na zdjęciu' }).first();
        await detectBtn.waitFor({ state: 'attached', timeout: 10000 });
        await detectBtn.click();
        await page.waitForSelector('#ml-studio-detect-file', { timeout: 10000 });
        // DEFEKT PRODUKTU (do raportu): openRecogDetectPanel() tworzy <tf-modal> i robi
        // appendChild, ale NIGDY nie ustawia atrybutu `open` (inaczej niż openFtExportPanel,
        // który po appendChild woła setAttribute('open')). Bez `open` tf-modal nie dodaje
        // klas --open → backdrop/karta są niewidoczne, choć treść jest w light DOM (więc
        // detekcja działa „w tle"). Wymuszamy `open`, by zrzut pokazał realny modal.
        const opened = await page.evaluate(() => {
          const m = [...document.querySelectorAll('tf-modal')].find((x) => x.querySelector('#ml-studio-detect-file'));
          if (!m) return false;
          const wasOpen = m.hasAttribute('open');
          if (!wasOpen) m.setAttribute('open', '');
          return wasOpen;
        });
        detectModalWasOpen = opened;
        await page.waitForTimeout(400);
      };
      await openDetectModal();
      // Modal to fixed-position overlay — zmiana rozmiaru viewportu go zamyka, więc
      // zrzut bieżącego widoku BEZ resize (shotCurrent), nie shotBoth.
      await shotCurrent(page, '09-modal-detekcji', '1440');

      // Wykonuje jedną detekcję dla zadanego progu i zwraca wynik (lub błąd).
      const runDetect = async (threshold) => {
        // Ustaw próg.
        const thrInput = page.locator('#ml-studio-detect-threshold input').first();
        await thrInput.fill(String(threshold));
        await thrInput.dispatchEvent('input');
        // Wgraj zdjęcie przez ukryty natywny input + CustomEvent('change', detail.files).
        const native = page.locator('#ml-studio-detect-file input.tf-file-input-native').first();
        await native.waitFor({ state: 'attached', timeout: 8000 });
        await native.setInputFiles(DETECT_IMG);
        await page.evaluate(() => {
          const fi = document.querySelector('#ml-studio-detect-file');
          const n = fi?.querySelector('input.tf-file-input-native');
          if (fi && n && n.files && n.files.length) {
            fi.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { files: n.files } }));
          }
        });
        // Pierwsza detekcja ładuje model (.pth) — czekaj na realny wynik / błąd / pustą listę.
        await page.waitForFunction(() => {
          const r = document.querySelector('#ml-studio-detect-result');
          if (!r) return false;
          const t = r.textContent || '';
          if (/Wykryto\s+\d+\s+obiekt/i.test(t)) return true;       // detekcje
          if (/Brak detekcji/i.test(t)) return true;                // pusta lista
          if (/nieudana|błąd|za duże/i.test(t)) return true;        // błąd
          return false;
        }, { timeout: 90000 });
        return await page.evaluate(() => {
          const r = document.querySelector('#ml-studio-detect-result');
          const t = (r?.textContent || '').replace(/\s+/g, ' ').trim();
          const items = Array.from(r?.querySelectorAll('.ml-studio-detect-list li') || [])
            .map((li) => li.textContent.replace(/\s+/g, ' ').trim());
          const cm = t.match(/Wykryto\s+(\d+)\s+obiekt/i);
          const err = /nieudana|błąd|za duże/i.test(t);
          const empty = /Brak detekcji/i.test(t);
          return { text: t, count: cm ? cm[1] : (empty ? '0' : ''), items, err, empty };
        });
      };

      let det = await runDetect(0.3);
      facts.detectThreshold = '0.3';
      // Pusta lista przy 0.3 (słaby model po 2 epokach) → obniż próg do 0.1.
      if (det.empty && !det.err) {
        await page.waitForTimeout(300);
        det = await runDetect(0.1);
        facts.detectThreshold = '0.1';
      }
      facts.detectCount = det.count;
      facts.detectClasses = det.items.slice(0, 12).join(' ; ');
      facts.detectNote = det.text.slice(0, 200);
      // Zrzut wyniku @1440 (modal nadal otwarty, bez resize).
      await shotCurrent(page, '10-detekcja-wynik', '1440');

      // Wersja @390: zmień viewport (zamknie modal), otwórz ponownie, powtórz detekcję
      // z finalnym progiem i zrób zrzut mobilny — bez poziomego scrolla.
      await page.setViewportSize({ width: 390, height: 844 });
      await page.waitForTimeout(400);
      await openDetectModal();
      await shotCurrent(page, '09-modal-detekcji', '390');
      await runDetect(Number(facts.detectThreshold));
      await page.waitForTimeout(300);
      await shotCurrent(page, '10-detekcja-wynik', '390');
      await page.setViewportSize({ width: 1440, height: 900 });
      await page.waitForTimeout(200);

      // KLUCZOWE: brak błędu, realna odpowiedź (detekcje LUB pusta lista). Błąd = FAIL.
      const pass = !det.err && (Number(det.count) > 0 || det.empty);
      step('5b. Detekcja na zdjęciu (RF-DETR)', pass,
        det.err
          ? `BŁĄD detekcji: "${det.text}"`
          : (Number(det.count) > 0
            ? `próg=${facts.detectThreshold} → wykryto ${det.count} obiektów: ${facts.detectClasses}`
            : `próg=${facts.detectThreshold} → pusta lista (brak detekcji powyżej progu) — realna odpowiedź, bez błędu`));
    } catch (e) {
      await shotBoth(page, '10-detekcja-FAIL').catch(() => {});
      step('5b. Detekcja na zdjęciu', false, `Błąd: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL (przerwano dalsze kroki): ${fatal.message}`);
  } finally {
    // Responsywność: zbierz hScroll z każdej pary zrzutów już zrobionych w trakcie.
    console.log('\n================ KONSOLA / SIEĆ ================');
    console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
    consoleErrors.slice(0, 40).forEach((e) => console.log('  JS> ' + e));
    console.log(`Nieudane żądania: ${failedRequests.length}`);
    failedRequests.slice(0, 40).forEach((e) => console.log('  NET> ' + e));

    console.log('\n================ FAKTY ========================');
    console.log(`  rola (authMe): ${facts.role || '(brak)'}`);
    console.log(`  nav ML Studio: ${facts.navPresent}`);
    console.log(`  dataset toast: ${facts.dsToast || '(brak)'}`);
    console.log(`  dataset obrazy/klasy: ${facts.dsImages || '?'} / ${facts.dsClasses || '?'}`);
    console.log(`  trening status: ${facts.trainStatus || '(brak)'}`);
    console.log(`  train loss: ${facts.trainLoss || '(brak)'}`);
    console.log(`  mAP@50: ${facts.map50 || '(brak)'}`);
    console.log(`  model w rejestrze: ${facts.modelName || '(brak)'} (silnik=${facts.modelFramework || '?'})`);
    console.log(`  detekcja próg: ${facts.detectThreshold || '(brak)'}`);
    console.log(`  detekcja liczba: ${facts.detectCount || '(brak)'}`);
    console.log(`  detekcja klasy: ${facts.detectClasses || '(brak)'}`);
    console.log(`  detekcja wynik: ${facts.detectNote || '(brak)'}`);
    console.log(`  DEFEKT UI: modal detekcji miał atrybut 'open' sam z siebie? ${detectModalWasOpen === null ? '(nie sprawdzono)' : detectModalWasOpen} (false = bug: modal niewidoczny dla użytkownika)`);

    console.log('\n============ RESPONSYWNOŚĆ @390 (hScroll px; 0 = brak poziomego scrolla) ====');
    const bad = responsiveLog.filter((r) => r.hScroll > 0);
    responsiveLog.forEach((r) => console.log(`  ${r.hScroll > 0 ? 'SCROLL!' : 'ok'}  ${r.view}: ${r.hScroll}px`));
    console.log(`  Widoki z poziomym scrollem @390: ${bad.length}`);

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
