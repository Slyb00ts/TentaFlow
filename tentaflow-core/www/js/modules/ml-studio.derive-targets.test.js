// =============================================================================
// Plik: modules/ml-studio.derive-targets.test.js
// Opis: Testy jednostkowe funkcji `deriveTrainTargets` z ml-studio.js — wyprowadzanie
//       celów treningu (detection zawsze, classifier dla atrybutu list/classifier
//       o ≥2 wartościach, ocr dla atrybutu ocr). Funkcja nie jest eksportowana z
//       modułu (moduł ciągnie zależności DOM), więc jej źródło jest wycinane z pliku
//       przez dopasowanie nawiasów i ewaluowane w izolacji — testujemy REALNY kod.
// =============================================================================

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

// ---- ekstrakcja funkcji z realnego źródła modułu ----

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'ml-studio.js'), 'utf8');

function extractFunction(src, name) {
  const marker = `function ${name}(`;
  const start = src.indexOf(marker);
  if (start < 0) throw new Error(`nie znaleziono definicji: ${name}`);
  // Od nawiasu klamrowego ciała funkcji idziemy licząc bilans { }.
  const bodyStart = src.indexOf('{', start);
  let depth = 0;
  let i = bodyStart;
  for (; i < src.length; i += 1) {
    const ch = src[i];
    if (ch === '{') depth += 1;
    else if (ch === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  const fnSrc = src.slice(start, i + 1);
  // eslint-disable-next-line no-new-func
  return new Function(`${fnSrc}\nreturn ${name};`)();
}

const deriveTrainTargets = extractFunction(source, 'deriveTrainTargets');

// ---- harness ----

const results = [];
function test(name, fn) {
  try {
    fn();
    results.push({ name, ok: true });
  } catch (err) {
    results.push({ name, ok: false, err });
  }
}
function assert(cond, msg) {
  if (!cond) throw new Error(msg || 'assert failed');
}
function assertEq(actual, expected, msg) {
  const a = JSON.stringify(actual);
  const b = JSON.stringify(expected);
  if (a !== b) throw new Error(`${msg || 'assertEq'}: expected ${b}, got ${a}`);
}

// ---- helpery domenowe ----

const attrList = (name, values) => ({ name, type: 'list', list: { values } });
const attrClassifier = (name, values) => ({ name, type: 'classifier', classifier: { values } });
const attrOcr = (name) => ({ name, type: 'ocr', ocr: {} });
const cls = (name, attributes) => ({ name, attributes });

// ---- testy ----

test('zawsze zwraca cel detection jako pierwszy', () => {
  const out = deriveTrainTargets({ classes: [] }, null);
  assert(Array.isArray(out), 'wynik powinien być tablicą');
  assertEq(out[0], { task: 'detection' }, 'pierwszy cel musi być detection');
});

test('atrybut list o ≥2 wartościach → classifier z values i sourceClasses', () => {
  const schema = {
    classes: [cls('tablica', [attrList('stan', ['czysta', 'brudna', 'uszkodzona'])])],
  };
  const out = deriveTrainTargets(schema, null);
  const clf = out.find((t) => t.task === 'classifier');
  assert(clf, 'powinien powstać cel classifier');
  assertEq(clf.attribute, 'stan');
  assertEq(clf.values, ['czysta', 'brudna', 'uszkodzona']);
  assertEq(clf.sourceClasses, ['tablica']);
});

test('atrybut classifier o ≥2 wartościach → classifier', () => {
  const schema = { classes: [cls('znak', [attrClassifier('rodzaj', ['A', 'B'])])] };
  const out = deriveTrainTargets(schema, null);
  const clf = out.find((t) => t.task === 'classifier');
  assert(clf, 'typ classifier też generuje cel classifier');
  assertEq(clf.values, ['A', 'B']);
});

test('atrybut list z <2 wartości jest ignorowany', () => {
  const schema = { classes: [cls('tablica', [attrList('stan', ['czysta'])])] };
  const out = deriveTrainTargets(schema, null);
  assert(!out.some((t) => t.task === 'classifier'), 'jedna wartość = brak classifiera');
  assertEq(out.length, 1, 'tylko detection');
});

test('atrybut ocr → cel ocr z sourceClasses', () => {
  const schema = { classes: [cls('tablica', [attrOcr('numer')])] };
  const out = deriveTrainTargets(schema, null);
  const ocr = out.find((t) => t.task === 'ocr');
  assert(ocr, 'powinien powstać cel ocr');
  assertEq(ocr.attribute, 'numer');
  assertEq(ocr.sourceClasses, ['tablica']);
});

test('ten sam atrybut w wielu klasach → agregacja sourceClasses i sumy wartości', () => {
  const schema = {
    classes: [
      cls('tablica_pl', [attrList('stan', ['czysta', 'brudna'])]),
      cls('tablica_de', [attrList('stan', ['brudna', 'uszkodzona'])]),
    ],
  };
  const out = deriveTrainTargets(schema, null);
  const clf = out.find((t) => t.task === 'classifier' && t.attribute === 'stan');
  assert(clf, 'jeden cel classifier dla wspólnego atrybutu');
  assertEq(clf.sourceClasses, ['tablica_pl', 'tablica_de']);
  assertEq(clf.values, ['czysta', 'brudna', 'uszkodzona'], 'wartości sumowane bez duplikatów');
});

test('cocoCategories zawęża sourceClasses do istniejących kategorii datasetu', () => {
  const schema = {
    classes: [
      cls('tablica_pl', [attrList('stan', ['czysta', 'brudna'])]),
      cls('tablica_de', [attrList('stan', ['czysta', 'brudna'])]),
    ],
  };
  const out = deriveTrainTargets(schema, [{ name: 'tablica_pl' }]);
  const clf = out.find((t) => t.task === 'classifier');
  assertEq(clf.sourceClasses, ['tablica_pl'], 'tablica_de nie ma cropów w COCO');
  assertEq(clf.values, ['czysta', 'brudna'], 'wartości nadal z pełnego schematu');
});

// ---- raport ----

let passed = 0;
for (const r of results) {
  if (r.ok) {
    passed += 1;
    console.log(`✓ ${r.name}`);
  } else {
    console.log(`✗ ${r.name}\n    ${r.err && r.err.message}`);
  }
}
console.log(`\n${passed}/${results.length} tests passed`);
if (passed !== results.length) process.exit(1);
