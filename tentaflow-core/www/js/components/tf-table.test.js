// =============================================================================
// File: components/tf-table.test.js
// Description: Tests for <tf-table>'s shadow-root styling contract. A cell
// rendered with renderer="html" lands INSIDE the shadow root, where a document
// stylesheet cannot reach it — a TentaNas sparkline therefore fell back to the
// UA default (fill: black) and painted a filled blob instead of a line. The
// tests below pin the scoped-sheet adoption that fixes it for both TentaNas
// scopes (.nas-root and .nas-modal), pin that the sheets do NOT leak into
// tables on other screens, and pin that the sheets stay COMPLETE: a class that
// reaches a cell and is styled in tentanas.css must be styled here too.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const WWW_ROOT = join(here, '..', '..');

const { window } = await import('../sdk-runtime/_dom-test-harness.js');

// shared-styles.js fetches its stylesheets by served path ("/css/....css").
// Node has no origin behind that path, so serve the real files from disk —
// the test then exercises the SHIPPED css, not a fixture copy of it.
const hostFetch = globalThis.fetch;
globalThis.fetch = (url, init) => {
  const href = String(url);
  const path = href.startsWith('/') ? href : (href.startsWith('http') ? new URL(href).pathname : '');
  if (path.startsWith('/css/')) {
    return Promise.resolve(new window.Response(readFileSync(join(WWW_ROOT, path), 'utf8'), {
      headers: { 'Content-Type': 'text/css' },
    }));
  }
  return hostFetch(url, init);
};

// shared-styles.js feature-detects Constructable Stylesheets through the bare
// `Document` / `CSSStyleSheet` globals, which the harness does not export. With
// them missing the adoption path throws before it ever runs, so expose them the
// way a browser would.
if (typeof globalThis.Document !== 'function' && window.Document) {
  globalThis.Document = window.Document;
}
if (typeof globalThis.CSSStyleSheet !== 'function' && window.CSSStyleSheet) {
  globalThis.CSSStyleSheet = window.CSSStyleSheet;
}

// The scope markers live in index.html and the harness document starts empty.
// Replay the REAL markers from index.html rather than inventing one, so this
// test also fails if a link (or its data-shadow-scope attribute) is ever
// dropped from the page. They must be in place before shared-styles.js loads,
// because it warms the sheet cache from them on import.
const INDEX_HTML = readFileSync(join(WWW_ROOT, 'index.html'), 'utf8');
const MARKER_LINKS = INDEX_HTML.match(/<link[^>]*data-shadow-scope[^>]*>/g) || [];
document.head.innerHTML += MARKER_LINKS.join('\n');

await import('./tf-table.js');
// tf-chip renders into its OWN light DOM (`this.innerHTML = ''; this.appendChild(span)`
// in tf-chip.js), which is exactly the shape of markup that breaks a naive
// `td.innerHTML !== next` comparison — needed registered here so an html-cell
// value containing a bare `<tf-chip>` tag actually upgrades.
await import('./tf-chip.js');

// ---- helpers ---------------------------------------------------------------

const SPARKLINE = '<div class="trend">'
  + '<svg viewBox="0 0 90 22"><polyline points="0,18 30,9 60,13 90,4"></polyline></svg>'
  + '</div>';
const HEALTH = '<div class="health-cell"><span class="health-dot ok"></span><span>OK</span></div>';
const KIND = '<span class="disk-kind nvme">nvme</span>';
// The fleet share name and its caption: the two cells the .nas-root sheet used
// to miss entirely (bold names rendered at normal weight on the main screen).
const NAME = '<span class="mono fw-700">tank/media</span>';
const CAPTION = '<span class="text-3 text-xs">fleet mount off</span>';

// Stylesheet adoption is async (the sheet is fetched once, then cached), so a
// freshly built table has no styles for a tick. Poll instead of guessing a
// delay, so a slow first fetch cannot make the test flap.
async function settle(shadow, expectedSheets) {
  for (let i = 0; i < 50; i += 1) {
    if ((shadow.adoptedStyleSheets || []).length >= expectedSheets) return;
    await new Promise((r) => setTimeout(r, 5));
  }
}

function mountTable(parent) {
  const table = document.createElement('tf-table');
  table.innerHTML = '<tf-column key="trend" label="Trend" renderer="html"></tf-column>'
    + '<tf-column key="health" label="Stan" renderer="html"></tf-column>'
    + '<tf-column key="kind" label="Typ" renderer="html"></tf-column>'
    + '<tf-column key="name" label="Nazwa" renderer="html"></tf-column>'
    + '<tf-column key="caption" label="Opis" renderer="html"></tf-column>';
  parent.appendChild(table);
  table.rows = [{
    trend: SPARKLINE, health: HEALTH, kind: KIND, name: NAME, caption: CAPTION,
  }];
  return table;
}

function mountInScope(className) {
  document.body.innerHTML = '';
  const scope = document.createElement('div');
  scope.className = className;
  document.body.appendChild(scope);
  return mountTable(scope);
}

function selectorsOf(css) {
  return [...css.matchAll(/([^{}]+)\{/g)]
    .map((m) => m[1].split('\n').pop().trim())
    .filter((s) => s && !s.startsWith('@') && !s.includes('*/'));
}

function reset() {
  document.body.innerHTML = '';
}

// ---- tests -----------------------------------------------------------------

test('index.html marks a TentaNas cell sheet for each TentaNas scope', () => {
  for (const [href, scope] of [
    ['/css/tentanas-cells.css', '.nas-root'],
    ['/css/tentanas-modal-cells.css', '.nas-modal'],
  ]) {
    const marker = MARKER_LINKS.find((l) => l.includes(href));
    assert.ok(marker, `index.html must link ${href} with a shadow scope`);
    assert.match(marker, new RegExp(`data-shadow-scope="\\${scope}"`));
    // Its selectors are unprefixed and generic (.mono, .text-3, .muted), so the
    // sheet must never reach the document cascade of another screen.
    assert.match(marker, /media="not all"/);
  }
});

test('an html cell inside .nas-root gets the TentaNas cell styles in the shadow root', async () => {
  const table = mountInScope('nas-root');
  await settle(table.shadowRoot, 2);

  const polyline = table.shadowRoot.querySelector('.trend polyline');
  assert.ok(polyline, 'the sparkline markup reached the shadow root');

  // Locate a failure precisely: adoption first, then what the cascade made of
  // it. Without these an empty computed value is ambiguous between "no rule
  // matched" and "the sheet never arrived".
  const sheets = Array.from(table.shadowRoot.adoptedStyleSheets || []);
  assert.equal(sheets.length, 2, 'controls.css plus the scoped TentaNas sheet');
  const hasTrendRule = sheets.some((s) => Array.from(s.cssRules || [])
    .some((r) => r.selectorText === '.trend polyline'));
  assert.ok(hasTrendRule, 'an adopted sheet carries the UNPREFIXED .trend polyline rule');

  // The defect: with only controls.css adopted, the polyline falls back to the
  // UA default fill (black) and the chart renders as a filled blob.
  const cs = getComputedStyle(polyline);
  assert.equal(cs.fill, 'none', 'polyline must not be filled');
});

test('.health-dot and .disk-kind are styled inside the shadow root', async () => {
  const table = mountInScope('nas-root');
  await settle(table.shadowRoot, 2);

  const dot = table.shadowRoot.querySelector('.health-dot');
  const kind = table.shadowRoot.querySelector('.disk-kind');
  assert.ok(dot && kind, 'health dot and disk kind reached the shadow root');

  assert.equal(getComputedStyle(dot).width, '10px', 'health dot must have its explicit size');
  assert.equal(
    getComputedStyle(kind).textTransform,
    'uppercase',
    'disk kind badge must be uppercased',
  );
});

test('the typography helpers a cell emits are styled inside .nas-root', async () => {
  const table = mountInScope('nas-root');
  await settle(table.shadowRoot, 2);

  // These two were the measured hole in the first fix: the sheet carried 12
  // classes while the cells emit many more, so fleet share names rendered at
  // normal weight and their captions at the wrong size.
  const name = table.shadowRoot.querySelector('.mono.fw-700');
  const caption = table.shadowRoot.querySelector('.text-xs');
  assert.ok(name && caption, 'name and caption markup reached the shadow root');

  assert.equal(getComputedStyle(name).fontWeight, '700', '.fw-700 must be bold in a cell');
  assert.equal(getComputedStyle(caption).fontSize, '11px', '.text-xs must be 11px in a cell');
});

test('an html cell inside .nas-modal adopts the modal cell sheet', async () => {
  // A TentaNas dialog is a `tf-window.nas-modal` appended to document.body, so
  // the table has no .nas-root ancestor; only `closest('.nas-modal')` resolves.
  // A plain element with the class stands in for the window — the adoption path
  // only ever looks at the host's ancestors.
  const table = mountInScope('nas-modal');
  await settle(table.shadowRoot, 2);

  const sheets = Array.from(table.shadowRoot.adoptedStyleSheets || []);
  assert.equal(sheets.length, 2, 'controls.css plus the scoped TentaNas modal sheet');

  const selectors = sheets.flatMap((s) => Array.from(s.cssRules || []).map((r) => r.selectorText));
  // `.nas-modal .mono.fw-700` is modal-ONLY in tentanas.css. Its presence here
  // and absence from the .nas-root sheet is what forces two sheets instead of
  // one merged copy.
  assert.ok(
    selectors.includes('.mono.fw-700, .mono.fw-800'),
    'the modal sheet carries the modal-only .mono.fw-700 rule',
  );
  // The .nas-root sheet must NOT have been adopted here: it styles .health-dot,
  // which tentanas.css never styles under .nas-modal.
  assert.ok(
    !selectors.some((s) => s === '.health-dot'),
    '.nas-root-scoped rules must not reach a .nas-modal shadow root',
  );

  const name = table.shadowRoot.querySelector('.mono.fw-700');
  assert.equal(getComputedStyle(name).fontWeight, '700', '.fw-700 must be bold in a modal cell');
});

test('a tf-table outside .nas-root does not adopt the TentaNas cell sheet', async () => {
  reset();
  const plain = document.createElement('div');
  plain.className = 'some-other-screen';
  document.body.appendChild(plain);

  const table = mountTable(plain);
  // Wait for the controls sheet, then give a scoped sheet every chance to land.
  await settle(table.shadowRoot, 1);
  await new Promise((r) => setTimeout(r, 30));

  const dot = table.shadowRoot.querySelector('.health-dot');
  assert.ok(dot, 'markup reached the shadow root');
  assert.notEqual(
    getComputedStyle(dot).width,
    '10px',
    'TentaNas cell styles must not leak into other screens',
  );
  assert.equal(
    (table.shadowRoot.adoptedStyleSheets || []).length,
    1,
    'only controls.css is adopted outside .nas-root',
  );
});

// ---- coverage: the scoped sheets must be COMPLETE ---------------------------
//
// The first version of this fix copied 12 classes by hand while the cells emit
// far more, so `.fw-700` and `.text-xs` silently stayed unstyled. A test that
// only compared the two files would have passed. This one derives the class set
// from the SOURCE instead: it finds every `renderer="html"` column, extracts the
// row-object property that fills it, collects the classes that markup emits
// (following one level of helper calls, so `mountDotsHtml` counts), and then
// requires that every such class tentanas.css styles under a scope is also
// styled in that scope's sheet. Nothing here is pinned to a line number or a
// count, so it keeps working as the screens change.

const TENTANAS_DIR = join(WWW_ROOT, 'js/modules/tentanas');
const SOURCES = [readFileSync(join(WWW_ROOT, 'js/modules/tentanas.js'), 'utf8')];
for (const f of readdirSync(TENTANAS_DIR)) {
  if (!f.endsWith('.js') || f.endsWith('.test.js') || f.startsWith('_')) continue;
  SOURCES.push(readFileSync(join(TENTANAS_DIR, f), 'utf8'));
}

const IDENT = /^-?[A-Za-z_][\w-]*$/;

// Reads an expression starting at `start`, stopping at the `,` or `}` that ends
// the property. Quote-, template- and bracket-aware, so a cell built from a
// nested template literal is captured whole.
function valueExpr(text, start) {
  let i = start;
  const stack = [];
  let out = '';
  while (i < text.length) {
    const c = text[i];
    if (stack.length === 0 && (c === ',' || c === '}')) break;
    const top = stack[stack.length - 1];
    if (top === "'" || top === '"') {
      if (c === '\\') { out += text[i] + text[i + 1]; i += 2; continue; }
      if (c === top) stack.pop();
    } else if (top === '`') {
      if (c === '\\') { out += text[i] + text[i + 1]; i += 2; continue; }
      if (c === '`') stack.pop();
      else if (c === '$' && text[i + 1] === '{') { stack.push('${'); out += '${'; i += 2; continue; }
    } else if (c === "'" || c === '"' || c === '`' || c === '(' || c === '[' || c === '{') {
      stack.push(c);
    } else if (c === ')' || c === ']' || c === '}') {
      stack.pop();
    }
    out += c;
    i += 1;
  }
  return out;
}

function bodyFrom(text, start) {
  const open = text.indexOf('{', text.indexOf('(', start));
  if (open < 0) return '';
  let depth = 0;
  for (let i = open; i < text.length; i += 1) {
    if (text[i] === '{') depth += 1;
    else if (text[i] === '}') { depth -= 1; if (depth === 0) return text.slice(open, i + 1); }
  }
  return '';
}

// Helper bodies, so a cell built by `mountDotsHtml(...)` still contributes the
// classes that helper writes.
const HELPERS = new Map();
for (const text of SOURCES) {
  let m;
  const reFn = /(?:export\s+)?function\s+([A-Za-z_$][\w$]*)\s*\(/g;
  while ((m = reFn.exec(text))) if (!HELPERS.has(m[1])) HELPERS.set(m[1], bodyFrom(text, m.index));
  const reArrow = /(?:export\s+)?const\s+([A-Za-z_$][\w$]*)\s*=\s*(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*=>/g;
  while ((m = reArrow.exec(text))) {
    if (!HELPERS.has(m[1])) HELPERS.set(m[1], valueExpr(text, m.index + m[0].length));
  }
}

// `class="md ${cls}"` and `class="${bad ? 'num-err' : ''}"` both occur: take the
// static tokens plus the string literals inside the interpolation.
function classTokens(raw) {
  const out = new Set();
  const interpolations = [];
  const staticPart = raw.replace(/\$\{[^}]*\}/g, (m) => { interpolations.push(m); return ' '; });
  for (const t of staticPart.split(/\s+/)) if (IDENT.test(t)) out.add(t);
  for (const interp of interpolations) {
    for (const lit of interp.matchAll(/'([^']*)'|"([^"]*)"/g)) {
      for (const t of (lit[1] ?? lit[2]).split(/\s+/)) if (IDENT.test(t)) out.add(t);
    }
  }
  return out;
}

function classesIn(expr, seen = new Set(), depth = 0) {
  const out = new Set();
  for (const m of expr.matchAll(/class=(?:"([^"]*)"|'([^']*)')/g)) {
    for (const c of classTokens(m[1] ?? m[2])) out.add(c);
  }
  if (depth >= 3) return out;
  for (const m of expr.matchAll(/\b([A-Za-z_$][\w$]*)\s*\(/g)) {
    if (seen.has(m[1]) || !HELPERS.has(m[1])) continue;
    seen.add(m[1]);
    for (const c of classesIn(HELPERS.get(m[1]), seen, depth + 1)) out.add(c);
  }
  return out;
}

const CELL_CLASSES = new Set();
for (const text of SOURCES) {
  const keys = new Set();
  for (const tag of text.match(/<tf-column\b[^>]*>/g) || []) {
    if (!/renderer="html"/.test(tag)) continue;
    const k = tag.match(/\bkey="([^"]+)"/);
    if (k) keys.add(k[1]);
  }
  for (const key of keys) {
    const re = new RegExp(`(^|[\\s{,])${key}\\s*:`, 'g');
    let m;
    while ((m = re.exec(text))) {
      for (const c of classesIn(valueExpr(text, m.index + m[0].length))) CELL_CLASSES.add(c);
    }
  }
}

const TENTANAS_CSS = readFileSync(join(WWW_ROOT, 'css/tentanas.css'), 'utf8');
const escRe = (s) => s.replace(/[.*+?^${}()|[\]\\-]/g, '\\$&');
const mentionsClass = (selector, cls) => new RegExp(`\\.${escRe(cls)}(?![\\w-])`).test(selector);
const classesOf = (selector) => (selector.match(/\.[-\w]+/g) || []).map((c) => c.slice(1));

// Classes a scope's rules require of a cell. A rule can only ever fire inside a
// cell when EVERY class it asks for is a class the cell markup emits, which
// keeps page-level rules that merely mention a cell class as a descendant of
// some container (`.alert-row .icon`, `.json-pre .k`) out of the requirement.
function requiredClasses(scope) {
  const required = new Set();
  for (const selector of selectorsOf(TENTANAS_CSS)) {
    for (const part of selector.split(',')) {
      const p = part.trim();
      if (!p.startsWith(`.${scope} `)) continue;
      const classes = classesOf(p).filter((c) => c !== scope);
      if (!classes.length || !classes.every((c) => CELL_CLASSES.has(c))) continue;
      for (const c of classes) required.add(c);
    }
  }
  return required;
}

test('the scoped cell sheets cover every class a TentaNas html cell emits', () => {
  // Guard against a silently empty parse: if the extraction ever stops finding
  // cells, this test must fail loudly rather than pass vacuously. The floors are
  // far below the real counts so ordinary screen churn cannot trip them.
  // Measured 2026-09-13: a healthy parse yields 50; a crippled parser (helper
  // body following disabled) yields exactly 25, and reducing the call-follow
  // depth to 1 yields 45. A floor of 25 therefore sat exactly on the boundary
  // and this line alone would not have moved. The per-scope floors below DID
  // catch that regression (required drops 35 -> 14 and 27 -> 7), so the test as
  // a whole was never blind — this floor is tightened to 40 so it fails on its
  // own rather than relying on them.
  assert.ok(CELL_CLASSES.size >= 40, `parsed too few cell classes (${CELL_CLASSES.size})`);

  for (const [scope, sheetPath, floor] of [
    ['nas-root', 'css/tentanas-cells.css', 20],
    ['nas-modal', 'css/tentanas-modal-cells.css', 15],
  ]) {
    const required = requiredClasses(scope);
    assert.ok(
      required.size >= floor,
      `.${scope}: parsed too few required classes (${required.size})`,
    );

    const sheetSelectors = selectorsOf(readFileSync(join(WWW_ROOT, sheetPath), 'utf8'));
    const missing = [...required]
      .filter((c) => !sheetSelectors.some((s) => mentionsClass(s, c)))
      .sort();
    assert.deepEqual(
      missing,
      [],
      `${sheetPath} does not style ${missing.join(', ')} — tentanas.css styles `
        + `${missing.length === 1 ? 'it' : 'them'} under .${scope} and TentaNas cells emit `
        + `${missing.length === 1 ? 'it' : 'them'}, so ${missing.length === 1 ? 'it renders' : 'they render'} unstyled inside the shadow root`,
    );
  }
});

// `empty-message` was passed by 19 call sites across 11 screens and did
// nothing: the attribute was never read and was not in `observedAttributes`,
// so every empty table in the product rendered as blank space and the
// caller's sentence went nowhere. A claim that a given table "does carry an
// empty-message" therefore said nothing about what the user sees.
function mountPlain(attrs = '') {
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  if (attrs) {
    for (const [k, v] of Object.entries(JSON.parse(attrs))) table.setAttribute(k, v);
  }
  table.innerHTML = '<tf-column key="name" label="Nazwa"></tf-column>'
    + '<tf-column key="size" label="Rozmiar"></tf-column>';
  document.body.appendChild(table);
  return table;
}

const emptyCell = (t) => t.shadowRoot.querySelector('.tf-table__empty-row > .tf-table__empty-cell');

test('an empty table renders the caller sentence across every column', () => {
  const table = mountPlain('{"empty-message":"Brak dysków bez przypisania"}');
  table.rows = [];

  const cell = emptyCell(table);
  assert.ok(cell, 'the empty row reached the shadow root');
  assert.equal(cell.textContent, 'Brak dysków bez przypisania');
  const headerCells = table.shadowRoot.querySelector('thead tr').children.length;
  assert.equal(cell.colSpan, headerCells, 'it spans the header, so it cannot drift from the column count');
});

test('the span of an empty row covers the actions column too', () => {
  const table = mountPlain('{"empty-message":"Nic tu nie ma"}');
  table.rowActions = () => document.createElement('span');
  table.rows = [];
  const headerCells = table.shadowRoot.querySelector('thead tr').children.length;
  assert.equal(emptyCell(table).colSpan, headerCells);
  assert.ok(headerCells >= 3, 'the header really did gain the actions column');
});

test('a table with no empty-message stays blank, as before', () => {
  const table = mountPlain();
  table.rows = [];
  assert.equal(table.shadowRoot.querySelector('.tf-table__empty-row'), null);
  assert.equal(table.shadowRoot.querySelector('tbody').children.length, 0);
});

test('an unchanged repaint of an empty table replaces no node', () => {
  const table = mountPlain('{"empty-message":"Brak zadań"}');
  table.rows = [];
  const before = emptyCell(table);
  table.rows = [];
  assert.equal(emptyCell(table), before, 'the empty cell is patched, never rebuilt');
  assert.equal(before.textContent, 'Brak zadań');
});

test('rows arriving drop the empty row instead of being written into it', () => {
  const table = mountPlain('{"empty-message":"Brak dysków"}');
  table.rows = [];
  assert.ok(emptyCell(table));

  table.rows = [{ name: 'sdc', size: '7.3 TiB' }];
  assert.equal(table.shadowRoot.querySelector('.tf-table__empty-row'), null, 'the empty row is gone');
  const body = table.shadowRoot.querySelectorAll('tbody tr');
  assert.equal(body.length, 1, 'exactly the one data row');
  assert.match(body[0].textContent, /sdc/);
  assert.match(body[0].textContent, /7\.3 TiB/, 'the data row is a real row, not a recycled empty one');

  table.rows = [];
  assert.equal(emptyCell(table).textContent, 'Brak dysków', 'and the sentence comes back when the rows go');
});

// ---- write-skipping: compare against the SOURCE string, not td.innerHTML ---
//
// `_writeCell`'s html branch used to gate the write on `td.innerHTML !== next`.
// Once the browser has parsed a cell, `td.innerHTML` is a RE-SERIALIZATION, not
// the string this component wrote: a custom element such as <tf-chip> renders
// into its own light DOM and replaces the markup it was given, and a
// self-closing `<use/>` comes back as `<use></use>`. The comparison was
// therefore true on almost every poll even when nothing changed, and every
// html cell of every table in the product was rewritten on every refresh
// (critic 2026-09-21: mockups n01-n10 M11, n11-n19 MAJOR 9) — tearing down live
// subtrees the owner's hard rule says must only be patched when data changes.
// The tests below pin the fix: compare against `td.__tfHtml`, the source
// string this component itself last wrote into that cell.

function mountHtmlTable(rows) {
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  table.innerHTML = '<tf-column key="cell" label="Komorka" renderer="html"></tf-column>'
    + '<tf-column key="name" label="Nazwa"></tf-column>';
  document.body.appendChild(table);
  table.rows = rows;
  return table;
}

function htmlCell(t, rowIdx = 0) {
  return t.shadowRoot.querySelectorAll('tbody tr')[rowIdx].children[0];
}

const CHIP_HTML = '<tf-chip status="ok" dot>Zdrowy</tf-chip>';
const CHIP_HTML_2 = '<svg viewBox="0 0 10 10"><use href="#i-x"/></svg>';

test('an unchanged tf-chip html cell is not rewritten across two assignments of .rows', () => {
  const table = mountHtmlTable([{ cell: CHIP_HTML, name: 'a' }]);
  const td = htmlCell(table);
  const chip = td.firstElementChild;
  assert.equal(chip.tagName.toLowerCase(), 'tf-chip', 'the chip upgraded in the cell');

  // A poll hands in a BRAND NEW row object (never `===` the previous one) with
  // the exact same source string. Naively comparing `td.innerHTML` (which now
  // reads back as the chip's own light-DOM span, not "<tf-chip ...>") would see
  // this as "changed" and rebuild — destroying the very node under test.
  table.rows = [{ cell: CHIP_HTML, name: 'a' }];

  // Compared as a boolean, not `assert.equal(nodeA, nodeB)` directly: on
  // failure `assert.equal` inspects both operands to build its diff, and
  // inspecting a live (happy-dom) element graph for a failure message is
  // dramatically slower than the DOM operation under test — turning a fast,
  // correct "test fails" into an apparent hang. Comparing the boolean
  // keeps the failure message cheap regardless of what the nodes look like.
  assert.equal(htmlCell(table).firstElementChild === chip, true, 'the same <tf-chip> node survives an unchanged poll');
});

test('an unchanged svg/<use/> html cell is not rewritten across two assignments of .rows', () => {
  const table = mountHtmlTable([{ cell: CHIP_HTML_2, name: 'a' }]);
  const td = htmlCell(table);
  const useEl = td.querySelector('use');
  assert.ok(useEl, 'the <use> element is in the cell');

  // `<use/>` (self-closing) always serializes back as `<use></use>` — a
  // mismatch against the source string on every single poll if the guard
  // compares `td.innerHTML` instead of the cached source.
  assert.notEqual(td.innerHTML, CHIP_HTML_2, 'sanity: the browser DOES re-serialize the markup differently');

  table.rows = [{ cell: CHIP_HTML_2, name: 'a' }];

  // See the comment on the tf-chip test above for why this compares a boolean
  // rather than the two live nodes directly.
  assert.equal(td.querySelector('use') === useEl, true, 'the same <use> node survives an unchanged poll');
});

test('a changed html cell value IS written', () => {
  const table = mountHtmlTable([{ cell: CHIP_HTML, name: 'a' }]);
  const td = htmlCell(table);
  const chip = td.firstElementChild;

  const CHANGED = '<tf-chip status="warn" dot>Ostrzezenie</tf-chip>';
  table.rows = [{ cell: CHANGED, name: 'a' }];

  const newChip = htmlCell(table).firstElementChild;
  // Boolean comparison again (see above) — `assert.notEqual` on two live
  // nodes pays the same inspection cost when it fails.
  assert.equal(newChip === chip, false, 'the cell was rebuilt for the new value');
  assert.equal(newChip.getAttribute('status'), 'warn');
  assert.equal(newChip.textContent, 'Ostrzezenie');
});

test('row recycling: reordering, removing and adding rows leaves every visible cell showing the right data', () => {
  const table = mountHtmlTable([
    { cell: '<tf-chip status="ok" dot>A</tf-chip>', name: 'row-a' },
    { cell: '<tf-chip status="ok" dot>B</tf-chip>', name: 'row-b' },
    { cell: '<tf-chip status="ok" dot>C</tf-chip>', name: 'row-c' },
  ]);
  const rowsText = () => [...table.shadowRoot.querySelectorAll('tbody tr')]
    .map((tr) => [...tr.children].map((td) => td.textContent).join('|'));
  assert.deepEqual(rowsText(), ['A|row-a', 'B|row-b', 'C|row-c']);

  // Reorder + remove b + add a new row d. tf-table recycles <tr>/<td> by
  // INDEX, so this exercises the exact scenario the fix has to get right: a
  // recycled cell now shows a DIFFERENT logical row, and a stale cache must
  // never make it skip that write.
  table.rows = [
    { cell: '<tf-chip status="ok" dot>C</tf-chip>', name: 'row-c' },
    { cell: '<tf-chip status="ok" dot>A</tf-chip>', name: 'row-a' },
    { cell: '<tf-chip status="ok" dot>D</tf-chip>', name: 'row-d' },
    { cell: '<tf-chip status="ok" dot>E</tf-chip>', name: 'row-e' },
  ];
  assert.deepEqual(rowsText(), ['C|row-c', 'A|row-a', 'D|row-d', 'E|row-e']);

  // Shrink back down — the removed trailing <tr>s must not leave stale content
  // behind if rows grow again afterwards.
  table.rows = [
    { cell: '<tf-chip status="ok" dot>F</tf-chip>', name: 'row-f' },
  ];
  assert.deepEqual(rowsText(), ['F|row-f']);

  table.rows = [
    { cell: '<tf-chip status="ok" dot>F</tf-chip>', name: 'row-f' },
    { cell: '<tf-chip status="ok" dot>G</tf-chip>', name: 'row-g' },
  ];
  assert.deepEqual(rowsText(), ['F|row-f', 'G|row-g']);
});

test('a cell another renderer wrote in between is rewritten when its old renderer returns with the old value', () => {
  // Cells are recycled by index and a column change rebuilds only the head,
  // so one <td> can be written by html, then chip, then html again with the
  // very string it held before. A cache left over from the first html write
  // would call that "unchanged" and leave the chip on screen.
  const value = '<span class="probe">A</span>';
  const table = mountHtmlTable([{ cell: value, name: 'r' }]);
  const column = table.querySelector('tf-column[key="cell"]');
  assert.ok(htmlCell(table).querySelector('.probe'));

  column.setAttribute('renderer', 'chip');
  table.rows = [{ cell: { status: 'ok', label: 'B' }, name: 'r' }];
  assert.equal(htmlCell(table).querySelector('.probe'), null, 'the chip replaced the html');

  column.setAttribute('renderer', 'html');
  table.rows = [{ cell: value, name: 'r' }];
  assert.ok(htmlCell(table).querySelector('.probe'), 'the html is written again, not skipped on a stale cache');
});

// ---- multi-select cell 0: the value write must never delete the checkbox --
//
// A `selectable="multi"` row's first cell holds the row <tf-checkbox> PLUS the
// value, built via `_writeCell`'s `keepExisting` path (value goes into a
// holder <span>, checkbox untouched). `_updateRowCells` used to call
// `_writeCell` on the very same td with no `keepExisting`, so a recycled poll
// wrote `td.textContent =` / `td.innerHTML =` straight onto the td — deleting
// the checkbox (critic 2026-09-21 B1: text was a new regression from the
// source-cache change, html was already broken before it). The fix routes the
// update through the same `keepExisting` path the build uses, reusing the
// holder span instead of recreating it, so the checkbox node is never even
// visited.

function mountMultiSelectTable(renderer, rows) {
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  table.setAttribute('selectable', 'multi');
  table.innerHTML = `<tf-column key="name" label="Nazwa" renderer="${renderer}"></tf-column>`
    + '<tf-column key="size" label="Rozmiar"></tf-column>';
  document.body.appendChild(table);
  table.rows = rows;
  return table;
}

function rowCheckbox(t, rowIdx = 0) {
  const td = t.shadowRoot.querySelectorAll('tbody tr')[rowIdx].children[0];
  return td.querySelector('tf-checkbox');
}

function rowFirstCell(t, rowIdx = 0) {
  return t.shadowRoot.querySelectorAll('tbody tr')[rowIdx].children[0];
}

test('multi-select TEXT first column: the row checkbox is the same node across an unchanged poll and its checked state survives', () => {
  const table = mountMultiSelectTable('text', [{ name: 'sdc', size: '7.3 TiB' }]);
  const cb = rowCheckbox(table);
  assert.ok(cb, 'the row checkbox was built into cell 0');
  cb.setAttribute('checked', '');

  // A poll hands in a brand-new row object with the same values.
  table.rows = [{ name: 'sdc', size: '7.3 TiB' }];

  const cbAfter = rowCheckbox(table);
  // Compared as a boolean, never `assert.equal(nodeA, nodeB)` directly — a
  // failing node-identity assert inspects the whole (happy-dom) element graph
  // to build its diff, which reads as a hang rather than a fast failure.
  const sameNode = cbAfter === cb;
  assert.equal(sameNode, true, 'the same <tf-checkbox> node survives an unchanged poll');
  const stillChecked = cbAfter.hasAttribute('checked');
  assert.equal(stillChecked, true, 'the checked state survives the poll');
  assert.match(rowFirstCell(table).textContent, /sdc/, 'the value is still rendered');
});

test('multi-select HTML first column: the row checkbox is the same node across an unchanged poll and its checked state survives', () => {
  const html = '<span class="mono">sdc</span>';
  const table = mountMultiSelectTable('html', [{ name: html, size: '7.3 TiB' }]);
  const cb = rowCheckbox(table);
  assert.ok(cb, 'the row checkbox was built into cell 0');
  cb.setAttribute('checked', '');

  table.rows = [{ name: html, size: '7.3 TiB' }];

  const cbAfter = rowCheckbox(table);
  const sameNode = cbAfter === cb;
  assert.equal(sameNode, true, 'the same <tf-checkbox> node survives an unchanged poll');
  const stillChecked = cbAfter.hasAttribute('checked');
  assert.equal(stillChecked, true, 'the checked state survives the poll');
  assert.ok(rowFirstCell(table).querySelector('.mono'), 'the html value is still rendered');
});

test('multi-select first column: a changed value IS written and the checkbox still survives', () => {
  const table = mountMultiSelectTable('text', [{ name: 'sdc', size: '7.3 TiB' }]);
  const cb = rowCheckbox(table);

  table.rows = [{ name: 'sdd', size: '7.3 TiB' }];

  const text = rowFirstCell(table).textContent;
  assert.match(text, /sdd/, 'the new value was written');
  assert.equal(text.includes('sdc'), false, 'the old value is gone');
  const sameNode = rowCheckbox(table) === cb;
  assert.equal(sameNode, true, 'the checkbox still survives a value change');
});

test('a multi-select first cell that was written plainly in between gets a live value holder back', () => {
  // Leaving multi-select writes cell 0 plainly and detaches the cached value
  // holder; coming back must not write the value into that detached span.
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  table.setAttribute('selectable', 'multi');
  table.innerHTML = '<tf-column key="name" label="Nazwa"></tf-column>';
  document.body.appendChild(table);
  table.rows = [{ name: 'sda' }];
  table.removeAttribute('selectable');
  table.rows = [{ name: 'sdb' }];
  table.setAttribute('selectable', 'multi');
  table.rows = [{ name: 'sdc' }];
  const cell = table.shadowRoot.querySelector('tbody tr').children[0];
  assert.equal(cell.textContent, 'sdc', 'the value is on screen exactly once, not in a detached holder beside stale text');
});

// ---- multi-select cell 0: the checkbox STATE must follow the row ----------
//
// The checkbox node surviving a poll (above) is not enough: `_updateRowCells`
// used to leave `checked` and the `<tr>.selected` class exactly as they were
// on the DOM slot, never re-deriving them from `row._selected` on the recycle
// path (critic 2026-09-22, B1/BLOCKER 1, probes P1-P4). `_buildRow` was the
// only place reading `row._selected`, so:
//   - select-all (P1) updated the data and the count but no box looked checked;
//   - a bulk action clearing the selection (P2) left every box ticked;
//   - a filter/sort (P3) left the tick on whatever row now sits in that slot,
//     not on the row the host still considers selected — the TentaNas bulk
//     SMART run (tentanas.js ~1755/~1833) then tests the wrong disk;
//   - turning `selectable="multi"` on for rows that already exist (P4) never
//     added the checkboxes at all.
// `row._selected` is the single source of truth (the same field `_buildRow`,
// select-all and the host's diskSelection all key off), applied only when the
// row carries the field as ITS OWN key (`tentanas.js` sets it on every row,
// true or false) so a row shape that never uses selection is left alone.

test('P1 select-all: reassigning rows with _selected: true checks every visible box', () => {
  const table = mountMultiSelectTable('text', [
    { name: 'sda', size: '1 TiB', _selected: false },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ]);
  assert.equal(rowCheckbox(table, 0).hasAttribute('checked'), false, 'sanity: starts unchecked');
  assert.equal(rowCheckbox(table, 1).hasAttribute('checked'), false, 'sanity: starts unchecked');

  // Select-all reassigns .rows with _selected: true on every row, exactly as
  // the host does after emitting "select-all" (tentanas.js ~1755-1761).
  table.rows = [
    { name: 'sda', size: '1 TiB', _selected: true },
    { name: 'sdb', size: '2 TiB', _selected: true },
  ];

  assert.equal(rowCheckbox(table, 0).hasAttribute('checked'), true, 'row 0 box is checked');
  assert.equal(rowCheckbox(table, 1).hasAttribute('checked'), true, 'row 1 box is checked');
  assert.equal(
    table.shadowRoot.querySelectorAll('tbody tr.selected').length,
    2,
    'both rows carry the selected class',
  );
});

test('P2 clearing the selection unchecks every box', () => {
  const table = mountMultiSelectTable('text', [
    { name: 'sda', size: '1 TiB', _selected: true },
    { name: 'sdb', size: '2 TiB', _selected: true },
  ]);
  assert.equal(rowCheckbox(table, 0).hasAttribute('checked'), true, 'sanity: starts checked');
  assert.equal(rowCheckbox(table, 1).hasAttribute('checked'), true, 'sanity: starts checked');
  assert.equal(table.shadowRoot.querySelectorAll('tbody tr.selected').length, 2, 'sanity: both selected');

  // A bulk action clears diskSelection then reassigns .rows with
  // _selected: false on every row (tentanas.js ~1833-1834).
  table.rows = [
    { name: 'sda', size: '1 TiB', _selected: false },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ];

  assert.equal(rowCheckbox(table, 0).hasAttribute('checked'), false, 'row 0 box is unchecked');
  assert.equal(rowCheckbox(table, 1).hasAttribute('checked'), false, 'row 1 box is unchecked');
  assert.equal(
    table.shadowRoot.querySelectorAll('tbody tr.selected').length,
    0,
    'no row carries the selected class',
  );
});

test('P3 a filter/reorder keeps the tick on the selected DATA row, not the DOM slot', () => {
  const table = mountMultiSelectTable('text', [
    { name: 'sda', size: '1 TiB', _selected: true },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ]);
  assert.equal(rowCheckbox(table, 0).hasAttribute('checked'), true, 'sanity: sda starts checked in slot 0');
  assert.equal(rowCheckbox(table, 1).hasAttribute('checked'), false, 'sanity: sdb starts unchecked in slot 1');

  // Filter/sort puts sdb in slot 0 and sda in slot 1 — rows are recycled by
  // INDEX, so slot 0's <tr>/<td> now belong to a different logical disk.
  // Selection (kept by the host, keyed on disk identity) still marks sda.
  table.rows = [
    { name: 'sdb', size: '2 TiB', _selected: false },
    { name: 'sda', size: '1 TiB', _selected: true },
  ];

  assert.equal(rowCheckbox(table, 0).hasAttribute('checked'), false, 'slot 0 (now sdb) is unchecked');
  assert.equal(rowCheckbox(table, 1).hasAttribute('checked'), true, 'slot 1 (now sda) is checked');
  assert.match(rowFirstCell(table, 1).textContent, /sda/, 'the checked box really is on the sda row');
  assert.equal(
    table.shadowRoot.querySelectorAll('tbody tr.selected').length,
    1,
    'exactly one row carries the selected class',
  );
});

test('P4 turning selectable="multi" on after rows exist adds a checkbox to every row', () => {
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  table.innerHTML = '<tf-column key="name" label="Nazwa"></tf-column>'
    + '<tf-column key="size" label="Rozmiar"></tf-column>';
  document.body.appendChild(table);
  table.rows = [
    { name: 'sda', size: '1 TiB' },
    { name: 'sdb', size: '2 TiB' },
  ];
  assert.equal(rowCheckbox(table, 0), null, 'sanity: no checkbox before selectable is set');
  assert.equal(rowCheckbox(table, 1), null, 'sanity: no checkbox before selectable is set');

  table.setAttribute('selectable', 'multi');

  assert.ok(rowCheckbox(table, 0), 'row 0 got a checkbox');
  assert.ok(rowCheckbox(table, 1), 'row 1 got a checkbox');
  assert.match(rowFirstCell(table, 0).textContent, /sda/, 'the value is still there beside the new box');
  assert.match(rowFirstCell(table, 1).textContent, /sdb/, 'the value is still there beside the new box');
});

test('the row checkbox node survives an unchanged poll even with a matching _selected field', () => {
  // The node-identity guarantee already pinned above must keep holding once
  // the checkbox's checked state is actively re-derived every render, not
  // just when the row happens to carry no _selected field at all.
  const table = mountMultiSelectTable('text', [{ name: 'sdc', size: '7.3 TiB', _selected: true }]);
  const cb = rowCheckbox(table);
  assert.equal(cb.hasAttribute('checked'), true, 'sanity: starts checked');

  // A poll hands in a brand-new row object with the same _selected value.
  table.rows = [{ name: 'sdc', size: '7.3 TiB', _selected: true }];

  const cbAfter = rowCheckbox(table);
  // Boolean comparison, never `assert.equal(nodeA, nodeB)` (see the comment
  // on the earlier same-node tests for why).
  assert.equal(cbAfter === cb, true, 'the same <tf-checkbox> node survives an unchanged poll');
  assert.equal(cbAfter.hasAttribute('checked'), true, 'still checked');
});

// ---- header select-all: synced FROM the rows on every render (MINOR D) ----
//
// The header select-all box was built once in `_renderThead` and never
// touched again after that, so it kept whatever `checked` a click had last
// left it at. Probe S3 (critic 2026-09-22): after the host cleared the
// selection (a bulk action, a filter change) the header box stayed ticked
// while every row underneath it was unticked. The fix re-derives the box's
// state from the rows on screen on every render: all selected -> checked,
// none -> unchecked, a mix -> indeterminate.

test('MINOR D: header select-all checks when every row is selected and unchecks once the host clears the selection', () => {
  const table = mountMultiSelectTable('text', [
    { name: 'sda', size: '1 TiB', _selected: false },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ]);
  const headerBox = table.shadowRoot.querySelector('.tf-table__select-all');
  assert.ok(headerBox, 'header select-all box exists');
  assert.equal(headerBox.hasAttribute('checked'), false, 'sanity: starts unchecked');

  table.rows = [
    { name: 'sda', size: '1 TiB', _selected: true },
    { name: 'sdb', size: '2 TiB', _selected: true },
  ];
  assert.equal(headerBox.hasAttribute('checked'), true, 'every row selected -> header checked');
  assert.equal(headerBox.hasAttribute('indeterminate'), false);

  table.rows = [
    { name: 'sda', size: '1 TiB', _selected: true },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ];
  assert.equal(headerBox.hasAttribute('checked'), false, 'a mixed selection is not shown fully checked');
  assert.equal(headerBox.hasAttribute('indeterminate'), true, 'a mixed selection shows indeterminate');

  // Host clears the selection entirely (a bulk action, a filter change) and
  // reassigns rows, exactly as tentanas.js / project-studio.js do.
  table.rows = [
    { name: 'sda', size: '1 TiB', _selected: false },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ];
  assert.equal(headerBox.hasAttribute('checked'), false, 'header unchecks after the host clears the selection');
  assert.equal(headerBox.hasAttribute('indeterminate'), false);
});

test('MINOR D: clicking select-all then having the host clear the selection unchecks the header box', () => {
  const table = mountMultiSelectTable('text', [
    { name: 'sda', size: '1 TiB', _selected: false },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ]);
  const headerBox = table.shadowRoot.querySelector('.tf-table__select-all');
  // Simulate what a real <tf-checkbox> does on click: it self-toggles its own
  // `checked` attribute/property BEFORE emitting "change" (tf-checkbox.js is
  // not registered in this harness, so the reflection is done by hand here).
  headerBox.setAttribute('checked', '');
  headerBox.checked = true;
  headerBox.dispatchEvent(new window.Event('change', { bubbles: true, composed: true }));
  assert.ok(headerBox.hasAttribute('checked'), 'sanity: the header shows ticked after select-all');

  // Host reacts to "select-all" by clearing selection right back (e.g. a
  // guard rejected the bulk action) and reassigns rows with _selected: false.
  table.rows = [
    { name: 'sda', size: '1 TiB', _selected: false },
    { name: 'sdb', size: '2 TiB', _selected: false },
  ];
  assert.equal(headerBox.hasAttribute('checked'), false, 'header unchecks once the host clears the selection');
});

// ---- turning `selectable` off removes the row checkbox for good (MINOR E) -
//
// `td.__tfText`/`__tfHtml` describe what a PLAIN write last put directly on
// the td. Multi-select's `keepExisting` path moves the value into a holder
// <span> instead but never invalidated the td's own cache, so it kept the
// value the td held from BEFORE multi-select turned on. Once `selectable`
// turns off again with an unchanged value, that stale cache falsely matched
// and the plain write that must remove the checkbox+holder from the td was
// skipped outright (critic 2026-09-22).

test('MINOR E: turning selectable off after it was on removes the checkbox and shows the value exactly once', () => {
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  table.innerHTML = '<tf-column key="name" label="Nazwa"></tf-column>'
    + '<tf-column key="size" label="Rozmiar"></tf-column>';
  document.body.appendChild(table);
  table.rows = [{ name: 'sda', size: '1 TiB' }];

  table.setAttribute('selectable', 'multi');
  table.rows = [{ name: 'sda', size: '1 TiB' }];
  assert.ok(rowCheckbox(table), 'sanity: checkbox present while multi-select is on');

  table.removeAttribute('selectable');
  table.rows = [{ name: 'sda', size: '1 TiB' }];

  assert.equal(rowCheckbox(table), null, 'the checkbox is gone once multi-select is off again');
  const cell = rowFirstCell(table);
  assert.equal(cell.textContent, 'sda', 'the value shows exactly once');
  assert.equal(cell.children.length, 0, 'no leftover holder/checkbox element remains in the cell');
});

test('a ticked row keeps its tick through a render that happens before the host reassigns rows', () => {
  // Every render syncs the box from `row._selected`; a click that did not
  // write it into the row would be undone by the next render — here a
  // client-side sort — and the tick would vanish.
  document.body.innerHTML = '';
  const table = document.createElement('tf-table');
  table.setAttribute('selectable', 'multi');
  table.setAttribute('sortable', '');
  table.innerHTML = '<tf-column key="name" label="Nazwa" sortable></tf-column>';
  document.body.appendChild(table);
  table.rows = [{ name: 'sdb', _selected: false }, { name: 'sda', _selected: false }];
  const boxOf = (name) => [...table.shadowRoot.querySelectorAll('tbody tr')]
    .find((tr) => tr.textContent.includes(name)).querySelector('.tf-table__row-select');
  const box = boxOf('sdb');
  box.checked = true;
  box.dispatchEvent(new window.Event('change', { bubbles: true, composed: true }));
  table._render();
  assert.ok(boxOf('sdb').hasAttribute('checked'), 'sdb is still ticked after the render');
  assert.ok(!boxOf('sda').hasAttribute('checked'), 'and the tick did not move to sda');
});
