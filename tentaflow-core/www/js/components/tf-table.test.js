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
