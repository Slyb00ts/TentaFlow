// =============================================================================
// Plik: modules/vision-detections-overlay.ocr-votes.test.js
// Opis: Testy jednostkowe głosowania OCR tablic z vision-detections-overlay.js
//       (`zapiszGlosyOcr` + `zwyciezcaOcr`). Metody są częścią klasy ciągnącej
//       DOM, więc — jak w ml-studio.derive-targets.test.js — ich źródło jest
//       wycinane z realnego pliku i ewaluowane w izolacji na atrapie `this`.
//       Testujemy REALNY kod, nie kopię.
// =============================================================================

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import test from 'node:test';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'vision-detections-overlay.js'), 'utf8');

/** Wycina ciało metody klasy po nazwie, licząc bilans nawiasów klamrowych. */
function extractMethod(src, name) {
  const marker = `\n  ${name}(`;
  const start = src.indexOf(marker);
  if (start < 0) throw new Error(`nie znaleziono metody: ${name}`);
  const bodyStart = src.indexOf('{', start);
  let depth = 0;
  for (let i = bodyStart; i < src.length; i++) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) return src.slice(start + 1, i + 1);
    }
  }
  throw new Error(`niezbilansowane nawiasy w metodzie: ${name}`);
}

/** Stała modułu (np. `const OCR_MIN_SCORE = 0.5;`) z realnego źródła. */
function extractConst(src, name) {
  const m = new RegExp(`const\\s+${name}\\s*=\\s*([^;]+);`).exec(src);
  if (!m) throw new Error(`nie znaleziono stałej: ${name}`);
  return Number(eval(m[1])); // eslint-disable-line no-eval
}

const OCR_MIN_SCORE = extractConst(source, 'OCR_MIN_SCORE');
const TRACK_META_TTL_MS = extractConst(source, 'TRACK_META_TTL_MS');

// Obie metody w jednym obiekcie — `zwyciezcaOcr` czyta to, co zapisał `zapiszGlosyOcr`.
const factory = new Function(
  'OCR_MIN_SCORE',
  'TRACK_META_TTL_MS',
  `return { ${extractMethod(source, 'zapiszGlosyOcr')}, ${extractMethod(source, 'zwyciezcaOcr')} };`,
);
const methods = factory(OCR_MIN_SCORE, TRACK_META_TTL_MS);

function makeOverlay() {
  return { ocrGlosy: new Map(), ...methods };
}

// Obie pewności NAD progiem — inaczej test sprawdzałby odsiew, a nie kolejność.
const CONF_LOW = OCR_MIN_SCORE + 0.05;
const CONF_HIGH = Math.min(0.99, OCR_MIN_SCORE + 0.2);

function plate(tekst, extra = {}) {
  return { klasa: 'tablica_rejestracyjna', track_id: 7, tekst, ...extra };
}

test('odczyt powyżej progu wygrywa i nie rzuca wyjątkiem', () => {
  const o = makeOverlay();
  // Regresja: pętla głosowania odwoływała się do nieistniejącej zmiennej `score`
  // (miary nazywają się `conf`/`tekst_conf`), więc KAŻDA ramka z odczytaną
  // tablicą kończyła się `ReferenceError` w handlerze WS — a że rzut leciał
  // przed zapisem ramki do bufora, overlay przestawał się aktualizować.
  o.zapiszGlosyOcr([plate('KR12345', { tekst_conf: CONF_HIGH })]);
  assert.equal(o.zwyciezcaOcr(7), 'KR12345');
});

test('liczba głosów decyduje przed pewnością odczytu', () => {
  const o = makeOverlay();
  o.zapiszGlosyOcr([plate('KR12345', { tekst_conf: CONF_LOW })]);
  o.zapiszGlosyOcr([plate('KR12345', { tekst_conf: CONF_LOW })]);
  o.zapiszGlosyOcr([plate('WX99999', { tekst_conf: CONF_HIGH })]);
  assert.equal(o.zwyciezcaOcr(7), 'KR12345');
});

test('przy remisie wygrywa wyższa pewność ODCZYTU, nie ramki', () => {
  const o = makeOverlay();
  // Po jednym głosie na odczyt; `score` (pewność detektora ramki) jest celowo
  // odwrotna do `tekst_conf`, żeby test wykrył użycie złej miary w tie-breaku.
  o.zapiszGlosyOcr([plate('KR12345', { tekst_conf: CONF_HIGH, score: 0.51 })]);
  o.zapiszGlosyOcr([plate('WX99999', { tekst_conf: CONF_LOW, score: 0.99 })]);
  assert.equal(o.zwyciezcaOcr(7), 'KR12345');
});

test('odczyt poniżej progu nie głosuje wcale', () => {
  const o = makeOverlay();
  o.zapiszGlosyOcr([plate('KR12345', { tekst_conf: OCR_MIN_SCORE - 0.01 })]);
  assert.equal(o.zwyciezcaOcr(7), null);
});

test('brak tekst_conf schodzi na score ramki', () => {
  const o = makeOverlay();
  o.zapiszGlosyOcr([plate('KR12345', { score: CONF_HIGH })]);
  assert.equal(o.zwyciezcaOcr(7), 'KR12345');
});

test('pusty tekst, obca klasa i brak track_id są pomijane', () => {
  const o = makeOverlay();
  o.zapiszGlosyOcr([
    plate('', { tekst_conf: CONF_HIGH }),
    { klasa: 'tablica_adr', track_id: 7, tekst: '99/3257', tekst_conf: CONF_HIGH },
    { klasa: 'tablica_rejestracyjna', track_id: 0, tekst: 'KR12345', tekst_conf: CONF_HIGH },
  ]);
  assert.equal(o.zwyciezcaOcr(7), null);
});
