// =============================================================================
// Plik: tests/e2e-annot/specs/annotacje.spec.ts
// Opis: Test E2E pełnej pętli edytora anotacji COCO w ML Studio (TentaFlow):
//       login → kreator (recognition) → rejestracja datasetu /tmp/coco-mini →
//       zakładka Anotacje → galeria 24 obrazów + ramki SVG → edycja (dodanie
//       ramki przez pointer na SVG, fallback RPC) → zapis → trwałość na dysku.
//       Asercje trwałości na dysku robi osobny krok (Bash) poza tym spec.
// =============================================================================

import { test, expect, Page } from '@playwright/test';
import * as fs from 'fs';

const SHOTS = '/tmp/annot-e2e-shots';
const ARTIFACT = `${SHOTS}/run-state.json`;

// Pomocniczy stan przekazywany do kroku Bash (image_id, file_name, split, liczby).
type RunState = {
  galleryCount: number;
  rectCountForAnnotatedImage: number;
  chosenFileName: string;
  chosenImageId: number;
  annCountBefore: number;
  saveVia: 'ui-pointer' | 'fallback-rpc' | 'none';
  toastSaveSeen: boolean;
  errors: string[];
};

function writeState(s: Partial<RunState>) {
  let cur: any = {};
  try { cur = JSON.parse(fs.readFileSync(ARTIFACT, 'utf8')); } catch { /* brak pliku = pierwszy zapis */ }
  fs.mkdirSync(SHOTS, { recursive: true });
  fs.writeFileSync(ARTIFACT, JSON.stringify({ ...cur, ...s }, null, 2));
}

// Ustawia rozmiar viewportu i daje layoutowi chwilę na reflow (bez sleepów na akcje).
async function setViewport(page: Page, w: number, h: number) {
  await page.setViewportSize({ width: w, height: h });
}

// tf-input trzyma natywny <input>; pisanie przez .fill() na nested input
// nie zawsze odpala listenery hosta, więc ustawiamy .value na custom-elemencie
// i emitujemy 'input' (tak jak robi to UI w wizardzie i polu ścieżki COCO).
async function setTfInput(page: Page, id: string, value: string) {
  await page.evaluate(({ id, value }) => {
    const el: any = document.getElementById(id);
    if (!el) throw new Error(`brak tf-input #${id}`);
    el.value = value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }, { id, value });
}

// Klik realnego przycisku tf-button (light DOM lub fallback) po id hosta.
async function clickHost(page: Page, id: string) {
  await page.locator(`#${id}`).click();
}

test('Edytor anotacji COCO — pełna pętla z zapisem i trwałością', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', (m) => { if (m.type() === 'error') errors.push(`[console] ${m.text()}`); });
  page.on('pageerror', (e) => errors.push(`[pageerror] ${e.message}`));

  // -------------------------------------------------------------------------
  // KROK 1: Login + wejście do ML Studio.
  // -------------------------------------------------------------------------
  await setViewport(page, 1440, 900);
  await page.goto('/');
  await expect(page.locator('#login-form')).toBeVisible();
  await setTfInput(page, 'login-username', 'power1');
  await setTfInput(page, 'login-password', 'power123');
  await page.locator('#login-form #login-submit').click();

  // Po zalogowaniu nawigacja staje się dostępna — czekamy na nav ML Studio.
  const nav = page.locator('.nav-item[data-view="ml-studio"]');
  await expect(nav).toBeVisible({ timeout: 30000 });
  await nav.click();
  await page.waitForLoadState('networkidle').catch(() => {});

  // -------------------------------------------------------------------------
  // KROK 2: Kreator → projekt recognition „Annot E2E".
  // Sterujemy kreatorem przez prawdziwe kontrolki: nazwa, wybór typu (radio
  // card recognition), przejście Dalej × do „Utwórz projekt".
  // -------------------------------------------------------------------------
  // Otwórz kreator — przycisk „nowy projekt" w widoku listy ML Studio.
  const newBtn = page.locator('#ml-studio-new, [id*="ml-studio-new"], button:has-text("Nowy projekt")').first();
  await newBtn.click();
  await expect(page.locator('#ml-studio-wiz-name')).toBeVisible({ timeout: 20000 });

  await setTfInput(page, 'ml-studio-wiz-name', 'Annot E2E');
  await clickHost(page, 'ml-studio-wiz-next'); // krok 1 → 2 (typ)

  // Krok 2: wybór typu recognition na tf-radio-group.
  await expect(page.locator('#ml-studio-wiz-types')).toBeVisible();
  await page.evaluate(() => {
    const grp: any = document.getElementById('ml-studio-wiz-types');
    grp.value = 'recognition';
    grp.dispatchEvent(new CustomEvent('change', { detail: { value: 'recognition' }, bubbles: true }));
  });
  // Klik karty recognition dla pewności (gdyby value setter nie wystarczył).
  const recogCard = page.locator('#ml-studio-wiz-types tf-radio[value="recognition"]');
  if (await recogCard.count()) await recogCard.first().click().catch(() => {});

  await clickHost(page, 'ml-studio-wiz-next'); // 2 → 3 (dane, opcjonalne)
  await clickHost(page, 'ml-studio-wiz-next'); // 3 → 4 (podsumowanie)
  await expect(page.locator('#ml-studio-wiz-create')).toBeVisible();
  await clickHost(page, 'ml-studio-wiz-create');

  // Po utworzeniu UI nawiguje do projektu (pokazuje zakładki tf-tabs).
  await expect(page.locator('#ml-studio-tabs')).toBeVisible({ timeout: 30000 });

  // -------------------------------------------------------------------------
  // KROK 3: Zakładka Dane → rejestracja /tmp/coco-mini (toast „17 klas").
  // -------------------------------------------------------------------------
  await page.locator('#ml-studio-tabs tf-tab[label="Dane"]').click();
  await expect(page.locator('#ml-studio-recog-path')).toBeVisible({ timeout: 20000 });
  await setTfInput(page, 'ml-studio-recog-path', '/tmp/coco-mini');
  await clickHost(page, 'ml-studio-recog-register');

  // Toast rejestracji: „... obrazów, 17 klas."
  const toastRegister = page.locator('.tf-toast, tf-toast, [class*="toast"]').filter({ hasText: '17 klas' });
  await expect(toastRegister.first()).toBeVisible({ timeout: 30000 });

  // -------------------------------------------------------------------------
  // KROK 4: Zakładka Anotacje → wybór datasetu → galeria 24 obrazy + ramki.
  // -------------------------------------------------------------------------
  await page.locator('#ml-studio-tabs tf-tab[label="Anotacje"]').click();
  await expect(page.locator('#ml-studio-annot-dataset')).toBeVisible({ timeout: 20000 });

  // Dataset wybiera się automatycznie (pierwszy coco_path) — czekamy na galerię.
  const thumbs = page.locator('#ml-studio-annot-gallery .ml-studio-annot-thumb');
  await expect(thumbs.first()).toBeVisible({ timeout: 30000 });
  const galleryCount = await thumbs.count();
  expect(galleryCount).toBe(24);
  writeState({ galleryCount });

  // Licznik w nagłówku galerii: „(24)".
  await expect(page.locator('#ml-studio-annot-count')).toHaveText('(24)');

  // Pierwszy obraz powinien być wczytany na płótnie z ramkami COCO.
  await expect(page.locator('#annot-img')).toBeVisible({ timeout: 20000 });
  await expect(page.locator('#annot-svg')).toBeVisible();

  // Znajdź miniaturę z licznikiem anotacji > 0 i wybierz ją (deterministyczny
  // dowód, że istniejące ramki COCO renderują się jako <rect>).
  const idxAnnotated = await page.evaluate(() => {
    const items = Array.from(document.querySelectorAll('#ml-studio-annot-gallery .ml-studio-annot-thumb'));
    for (let i = 0; i < items.length; i++) {
      const hint = items[i].querySelector('.ml-studio-data-hint');
      const n = Number((hint?.textContent || '0').trim());
      if (n > 0) return i;
    }
    return -1;
  });
  expect(idxAnnotated).toBeGreaterThanOrEqual(0);
  await thumbs.nth(idxAnnotated).click();

  // Po wczytaniu obrazu z anotacjami policz <rect> w SVG (bez .annot-handle —
  // te pojawiają się dopiero po zaznaczeniu; przy wczytaniu jest 0 zaznaczeń).
  await expect(page.locator('#annot-svg rect').first()).toBeVisible({ timeout: 20000 });
  const rectCount = await page.locator('#annot-svg rect:not(.annot-handle)').count();
  expect(rectCount).toBeGreaterThan(0);

  // Zapamiętaj file_name + image_id wybranego obrazu (do asercji na dysku).
  const chosen = await page.evaluate((idx) => {
    const el = document.querySelector(`#ml-studio-annot-gallery .ml-studio-annot-thumb[data-idx="${idx}"]`);
    const fileName = el?.querySelector('span')?.textContent?.trim() || '';
    const annCount = Number((el?.querySelector('.ml-studio-data-hint')?.textContent || '0').trim());
    return { fileName, annCount };
  }, idxAnnotated);
  writeState({
    rectCountForAnnotatedImage: rectCount,
    chosenFileName: chosen.fileName,
    annCountBefore: chosen.annCount,
  });

  // Zrzuty @1440 (galeria + obraz + ramki).
  await page.locator('.ml-studio-annot').screenshot({ path: `${SHOTS}/04-annot-1440.png` });
  await page.screenshot({ path: `${SHOTS}/04-annot-1440-full.png`, fullPage: true });

  // Zrzut @390 (responsywność: grid 240px+1fr może się składać; brak h-scrolla).
  await setViewport(page, 390, 844);
  await page.waitForTimeout(300); // tylko reflow layoutu (nie czekanie na akcję)
  const hScroll390 = await page.evaluate(() =>
    document.documentElement.scrollWidth > document.documentElement.clientWidth + 1);
  await page.locator('.ml-studio-annot').screenshot({ path: `${SHOTS}/04-annot-390.png` });
  await page.screenshot({ path: `${SHOTS}/04-annot-390-full.png`, fullPage: true });
  writeState({ hScroll390 } as any);
  await setViewport(page, 1440, 900);
  await page.waitForTimeout(300);

  // -------------------------------------------------------------------------
  // KROK 5: EDYCJA + ZAPIS — dodanie ramki przez pointer na SVG, potem Zapisz.
  // -------------------------------------------------------------------------
  const svg = page.locator('#annot-svg');
  await expect(svg).toBeVisible();
  const box = await svg.boundingBox();
  if (!box) throw new Error('brak boundingBox SVG');

  // Liczba rect przed edycją (na tym samym, wybranym obrazie).
  const rectBefore = await page.locator('#annot-svg rect:not(.annot-handle)').count();

  // Sekwencja pointerdown→move→up: od (20%,20%) do (40%,40%) obszaru SVG.
  const x0 = box.x + box.width * 0.20, y0 = box.y + box.height * 0.20;
  const x1 = box.x + box.width * 0.40, y1 = box.y + box.height * 0.40;

  let saveVia: RunState['saveVia'] = 'none';
  // Pointer eventy: UI słucha pointerdown/move na svg, pointerup na window.
  await page.mouse.move(x0, y0);
  await page.mouse.down();
  await page.mouse.move((x0 + x1) / 2, (y0 + y1) / 2, { steps: 5 });
  await page.mouse.move(x1, y1, { steps: 5 });
  await page.mouse.up();

  const rectAfterPointer = await page.locator('#annot-svg rect:not(.annot-handle)').count();
  if (rectAfterPointer > rectBefore) {
    saveVia = 'ui-pointer';
  } else {
    // FALLBACK: dorzuć ramkę przez RPC zapisu (zaznaczone w raporcie).
    saveVia = 'fallback-rpc';
    await page.evaluate(async () => {
      const mod = await import('/js/protocol/api-binary-shim.js');
      const ApiBinary: any = mod.ApiBinary;
      // Odczyt aktualnego obrazu/datasetu ze stanu DOM galerii.
      const active = document.querySelector('#ml-studio-annot-gallery .ml-studio-annot-thumb.active') as HTMLElement;
      const idx = Number(active.getAttribute('data-idx'));
      // Pobierz listę i obraz przez te same RPC co edytor, by mieć poprawne id.
      const sel: any = document.getElementById('ml-studio-annot-dataset');
      const datasetId = sel?.value;
      const imgsResp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId });
      const images = JSON.parse(imgsResp.imagesJson ?? imgsResp.images_json ?? '[]');
      const im = images[idx];
      const imgResp = await ApiBinary.one('mlStudioRecogImageRequest', { datasetId, imageId: im.image_id });
      const anns = JSON.parse(imgResp.annotationsJson ?? imgResp.annotations_json ?? '[]');
      const W = imgResp.origWidth ?? imgResp.orig_width ?? im.width;
      const H = imgResp.origHeight ?? imgResp.orig_height ?? im.height;
      const next = anns.map((a: any) => ({ category_id: a.category_id, bbox: a.bbox }));
      // Nowa ramka 20%..40% w jednostkach oryginału.
      next.push({ category_id: anns[0]?.category_id ?? 1, bbox: [Math.round(W * 0.2), Math.round(H * 0.2), Math.round(W * 0.2), Math.round(H * 0.2)] });
      const resp = await ApiBinary.one('mlStudioRecogSaveAnnotationsRequest', {
        datasetId, imageId: im.image_id, annotationsJson: JSON.stringify(next),
      });
      (window as any).__fallbackSaveOk = !!resp.ok;
    });
  }

  let toastSaveSeen = false;
  if (saveVia === 'ui-pointer') {
    await clickHost(page, 'annot-save');
    const toastSave = page.locator('.tf-toast, tf-toast, [class*="toast"]').filter({ hasText: 'Anotacje zapisane' });
    await expect(toastSave.first()).toBeVisible({ timeout: 20000 });
    toastSaveSeen = true;
  } else {
    const ok = await page.evaluate(() => (window as any).__fallbackSaveOk === true);
    expect(ok).toBeTruthy();
  }

  // Zrzut po edycji @1440.
  await page.locator('.ml-studio-annot').screenshot({ path: `${SHOTS}/05-after-edit-1440.png` });

  // Zapisz image_id wybranego obrazu (przez RPC list, by mieć pewne id).
  const chosenImageId = await page.evaluate(async () => {
    const mod = await import('/js/protocol/api-binary-shim.js');
    const ApiBinary: any = mod.ApiBinary;
    const sel: any = document.getElementById('ml-studio-annot-dataset');
    const active = document.querySelector('#ml-studio-annot-gallery .ml-studio-annot-thumb.active') as HTMLElement;
    const idx = Number(active.getAttribute('data-idx'));
    const imgsResp = await ApiBinary.one('mlStudioRecogImagesListRequest', { datasetId: sel?.value });
    const images = JSON.parse(imgsResp.imagesJson ?? imgsResp.images_json ?? '[]');
    return images[idx].image_id;
  });
  writeState({ chosenImageId, saveVia, toastSaveSeen, errors });

  // -------------------------------------------------------------------------
  // KROK 7: Przeładowanie zakładki → ramka wczytana z dysku (licznik +N).
  // (Krok 6 — asercja na dysku — robi osobny skrypt Bash między run #1 a tym.)
  // Tu re-otwieramy zakładkę Anotacje i sprawdzamy licznik wybranego obrazu.
  // -------------------------------------------------------------------------
  await page.locator('#ml-studio-tabs tf-tab[label="Dane"]').click();
  await page.locator('#ml-studio-tabs tf-tab[label="Anotacje"]').click();
  await expect(page.locator('#ml-studio-annot-gallery .ml-studio-annot-thumb').first()).toBeVisible({ timeout: 30000 });

  const reloadedCount = await page.evaluate((fileName) => {
    const items = Array.from(document.querySelectorAll('#ml-studio-annot-gallery .ml-studio-annot-thumb'));
    const el = items.find((e) => (e.querySelector('span')?.textContent?.trim() || '') === fileName);
    return el ? Number((el.querySelector('.ml-studio-data-hint')?.textContent || '0').trim()) : -1;
  }, chosen.fileName);
  writeState({ reloadedAnnCount: reloadedCount } as any);

  // Licznik po przeładowaniu = przed + 1 (dodaliśmy jedną ramkę).
  expect(reloadedCount).toBe(chosen.annCount + 1);

  if (errors.length) console.log('UWAGA — błędy konsoli/strony:', JSON.stringify(errors, null, 2));
});
