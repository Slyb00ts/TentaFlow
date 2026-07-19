// =============================================================================
// Plik: sdk-runtime/data-text-renderer.test.js
// Opis: Testy Text/Heading/Paragraph/RichText/MonoBlock/CodeBlock — chunk 3.3d-1.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  TEXT_TAG, HEADING_TAG, PARAGRAPH_TAG, RICH_TEXT_TAG,
  MONO_BLOCK_TAG, CODE_BLOCK_TAG,
} from './data-text-renderer.js';

const results = [];
function test(name, fn) {
  try { fn(); results.push({ name, ok: true }); }
  catch (err) { results.push({ name, ok: false, err }); }
}
function assertEq(a, e, m) {
  const aj = JSON.stringify(a, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  const ej = JSON.stringify(e, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  if (aj !== ej) throw new Error(`${m || 'assertEq'}: expected ${ej}, got ${aj}`);
}
function assert(cond, m) { if (!cond) throw new Error(m || 'assert failed'); }
function assertThrows(fn, m) {
  let t = false; try { fn(); } catch { t = true; }
  if (!t) throw new Error(m || 'expected throw');
}

const PATH = (...segs) => segs.map((s) =>
  typeof s === 'number' ? { kind: 'index', value: s } : { kind: 'key', value: s });

function makeStore() { return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n }); }
function makeEngine(store) {
  return new ComponentRenderer({ store: store || makeStore(), eventDispatcher: { emit() {} }, locale: 'en-US' });
}
function comp(tag, fields, extra = {}) {
  return {
    tag, id: extra.id ?? 'c1', fields,
    handlers: extra.handlers ?? null,
    bind: extra.bind ?? null,
    a11y: extra.a11y ?? null,
    visibility: extra.visibility ?? null,
    test_id: extra.test_id ?? null,
  };
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

// ============================================================================
// Text
// ============================================================================

test('Text renderuje <span> z literal content', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'Hello' }], [1, 'body'],
  ]));
  assertEq(el.tagName, 'SPAN');
  assertEq(el.textContent, 'Hello');
});

test('Text reactive content sync ze store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('msg'), value: 'A' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'bound', path: PATH('msg') }], [1, 'body'],
  ]));
  assertEq(el.textContent, 'A');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('msg'), op: { kind: 'set', value: 'B' } }],
  });
  assertEq(el.textContent, 'B');
});

test('Text tone+align+wrap dodaje klasy', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'caption'],
    [2, 'critical'], [3, 'center'], [4, 'nowrap'],
  ]));
  assert(el.classList.contains('tf-text--tone-critical'));
  assert(el.classList.contains('tf-text--align-center'));
  assert(el.classList.contains('tf-text--wrap-nowrap'));
});

test('Text max_lines=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'body'], [5, 0],
  ])));
});

test('Text streaming=true dodaje klasę caret, false ją zdejmuje', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('stream'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'partial' }], [1, 'body'],
    [7, { kind: 'bound', path: PATH('stream') }],
  ]));
  assert(el.classList.contains('sdk-text--streaming'), 'caret class while streaming');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('stream'), op: { kind: 'set', value: false } }],
  });
  assert(!el.classList.contains('sdk-text--streaming'), 'caret removed when stream ends');
});

test('Text bez streaming nie ma klasy caret', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'x' }], [1, 'body'],
  ]));
  assert(!el.classList.contains('sdk-text--streaming'));
});

test('Text max_lines>0 ustawia CSS var', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'body'], [5, 3],
  ]));
  assert(el.classList.contains('tf-text--clamp'));
  assertEq(el.style.getPropertyValue('--tf-text-max-lines'), '3');
});

test('Text max_lines jako BigInt z dekodera wire akceptowany', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'body'], [5, 3n],
  ]));
  assert(el.classList.contains('tf-text--clamp'));
  assertEq(el.style.getPropertyValue('--tf-text-max-lines'), '3');
});

test('Text max_lines BigInt poza zakresem u8 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'body'], [5, 300n],
  ])));
});

test('Text invalid style throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'huge'],
  ])));
});

test('Text invalid ValueFormat.kind throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'body'],
    [6, { kind: 'nope' }],
  ])));
});

test('Text ValueFormat currency bez code throws (variant-level eager validation)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'body'],
    [6, { kind: 'currency' }],  // brakuje wymaganego .code
  ])));
});

test('Text ValueFormat currency z code akceptowane', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 100 }], [1, 'body'],
    [6, { kind: 'currency', code: 'PLN' }],
  ]));
  assert(el.textContent.length > 0);
});

test('Text unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 'body'], [99, 'x'],
  ])));
});

// ============================================================================
// Heading
// ============================================================================

test('Heading renderuje <hN> per level', () => {
  setup();
  const engine = makeEngine();
  for (let lvl = 1; lvl <= 6; lvl++) {
    const el = engine.render(comp(HEADING_TAG, [
      [0, { kind: 'literal', value: `H${lvl}` }], [1, lvl],
    ], { id: `h${lvl}` }));
    assertEq(el.tagName, `H${lvl}`);
  }
});

test('Heading level=0 lub 7 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(HEADING_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 0],
  ])));
  assertThrows(() => engine.render(comp(HEADING_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 7],
  ])));
});

test('Heading tone + align', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(HEADING_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 2],
    [2, 'success'], [3, 'center'],
  ]));
  assert(el.classList.contains('tf-heading--tone-success'));
  assert(el.classList.contains('tf-heading--align-center'));
});

test('Heading unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(HEADING_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, 1], [99, 'x'],
  ])));
});

// ============================================================================
// Paragraph
// ============================================================================

test('Paragraph renderuje <p> z plain text gdy brak allowed_marks', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: 'Hello **world**' }], [3, false],
  ]));
  assertEq(el.tagName, 'P');
  // Brak allowed_marks → plain text (sygnatura **world** zostaje literalnie).
  assertEq(el.textContent, 'Hello **world**');
  assertEq(el.querySelector('strong'), null);
});

test('Paragraph allowed_marks=[bold] renderuje <strong>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: 'Hi **there** all' }],
    [2, ['bold']], [3, false],
  ]));
  const strong = el.querySelector('strong');
  assertEq(strong.textContent, 'there');
});

test('Paragraph allowed_marks=[italic,code] renderuje <em>+<code>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: 'use _foo_ and `bar`' }],
    [2, ['italic', 'code']], [3, false],
  ]));
  assertEq(el.querySelector('em').textContent, 'foo');
  assertEq(el.querySelector('code').textContent, 'bar');
});

test('Paragraph allow_links=true + https link renderuje <a target=_blank>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: 'see [docs](https://example.com)' }],
    [2, ['link']], [3, true],
  ]));
  const a = el.querySelector('a');
  assertEq(a.getAttribute('href'), 'https://example.com');
  assertEq(a.getAttribute('target'), '_blank');
  assertEq(a.getAttribute('rel'), 'noopener noreferrer');
  assertEq(a.textContent, 'docs');
});

test('Paragraph allow_links=false ignoruje [text](url)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: '[click](https://x.com)' }],
    [2, ['link']], [3, false],
  ]));
  assertEq(el.querySelector('a'), null);
  assertEq(el.textContent, '[click](https://x.com)');
});

test('Paragraph link z javascript: scheme jest odrzucony (XSS guard)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: '[bad](javascript:alert(1))' }],
    [2, ['link']], [3, true],
  ]));
  assertEq(el.querySelector('a'), null);
  assertEq(el.textContent, '[bad](javascript:alert(1))');
});

test('Paragraph XSS: <script> w content jest escaped przez textContent', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: '<script>alert(1)</script>' }],
    [3, false],
  ]));
  assertEq(el.querySelector('script'), null);
  assertEq(el.textContent, '<script>alert(1)</script>');
});

test('Paragraph reactive content rerender całego DOM', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('p'), value: 'A **B**' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'bound', path: PATH('p') }],
    [2, ['bold']], [3, false],
  ]));
  assertEq(el.querySelector('strong').textContent, 'B');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('p'), op: { kind: 'set', value: 'X **Y** Z' } }],
  });
  assertEq(el.querySelector('strong').textContent, 'Y');
});

test('Paragraph max_lines=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: 'X' }], [3, false], [4, 0],
  ])));
});

test('Paragraph default style=body gdy nieobecny', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PARAGRAPH_TAG, [
    [0, { kind: 'literal', value: 'X' }], [3, false],
  ]));
  assert(el.classList.contains('tf-paragraph--style-body'));
});

// ============================================================================
// RichText
// ============================================================================

test('RichText heading + paragraph + list', () => {
  setup();
  const engine = makeEngine();
  const md = '# Tytuł\n\nLead text.\n\n- one\n- two';
  const el = engine.render(comp(RICH_TEXT_TAG, [
    [0, { kind: 'literal', value: md }],
    [1, ['heading', 'list']],
    [2, []],
  ]));
  assertEq(el.querySelector('h1').textContent, 'Tytuł');
  assertEq(el.querySelectorAll('p').length, 1);
  assertEq(el.querySelectorAll('li').length, 2);
});

test('RichText code_block z ``` ', () => {
  setup();
  const engine = makeEngine();
  const md = '```rust\nfn main() {}\n```';
  const el = engine.render(comp(RICH_TEXT_TAG, [
    [0, { kind: 'literal', value: md }],
    [1, ['code_block']],
    [2, []],
  ]));
  const code = el.querySelector('.tf-richtext__code');
  assertEq(code.getAttribute('data-language'), 'rust');
  assertEq(code.textContent, 'fn main() {}');
});

test('RichText blockquote', () => {
  setup();
  const engine = makeEngine();
  const md = '> Cytat';
  const el = engine.render(comp(RICH_TEXT_TAG, [
    [0, { kind: 'literal', value: md }],
    [1, ['blockquote']],
    [2, []],
  ]));
  assert(el.querySelector('blockquote') != null);
  assertEq(el.querySelector('blockquote p').textContent, 'Cytat');
});

test('RichText inline marks działają w blockach', () => {
  setup();
  const engine = makeEngine();
  const md = '# **Bold** title\n\ntext _italic_';
  const el = engine.render(comp(RICH_TEXT_TAG, [
    [0, { kind: 'literal', value: md }],
    [1, ['heading']], [2, ['bold', 'italic']],
  ]));
  assertEq(el.querySelector('h1 strong').textContent, 'Bold');
  assertEq(el.querySelector('p em').textContent, 'italic');
});

test('RichText max_height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RICH_TEXT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, []], [2, []], [3, 0],
  ])));
});

test('RichText XSS safe: <img onerror> wpisany nie tworzy elementu', () => {
  setup();
  const engine = makeEngine();
  const md = '<img src=x onerror=alert(1)>';
  const el = engine.render(comp(RICH_TEXT_TAG, [
    [0, { kind: 'literal', value: md }],
    [1, []], [2, []],
  ]));
  assertEq(el.querySelector('img'), null);
});

// ============================================================================
// MonoBlock
// ============================================================================

test('MonoBlock renderuje <pre> z content', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MONO_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'line1\nline2' }], [2, false], [3, false],
  ]));
  const pre = el.querySelector('pre');
  assertEq(pre.tagName, 'PRE');
  assertEq(pre.textContent, 'line1\nline2');
});

test('MonoBlock word_wrap=true ustawia klasę', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MONO_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'x' }], [2, true], [3, false],
  ]));
  assert(el.classList.contains('tf-monoblock--wrap'));
});

test('MonoBlock copyable=true renderuje button Copy', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MONO_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'x' }], [2, false], [3, true],
  ]));
  const btn = el.querySelector('.tf-monoblock__copy');
  assertEq(btn.textContent, 'Copy');
});

test('MonoBlock max_height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MONO_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'x' }], [1, 0], [2, false], [3, false],
  ])));
});

// ============================================================================
// CodeBlock
// ============================================================================

test('CodeBlock renderuje <pre> z language data-attr', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CODE_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'const x = 1;' }],
    [1, 'javascript'], [2, false], [3, false],
  ]));
  assertEq(el.getAttribute('data-language'), 'javascript');
  assert(el.classList.contains('tf-codeblock--lang-javascript'));
});

test('CodeBlock invalid language throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CODE_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'x' }],
    [1, 'Java Script!'], [2, false], [3, false],
  ])));
});

test('CodeBlock show_line_numbers=true renderuje gutter z numerami', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CODE_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'a\nb\nc' }],
    [1, 'plain'], [2, true], [3, false],
  ]));
  const gutters = el.querySelectorAll('.tf-codeblock__gutter');
  assertEq(gutters.length, 3);
  assertEq(gutters[0].textContent, '1');
  assertEq(gutters[2].textContent, '3');
});

test('CodeBlock highlight_lines podświetla wskazane linie', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CODE_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'a\nb\nc' }],
    [1, 'plain'], [2, true], [3, false], [5, [2]],
  ]));
  const lines = el.querySelectorAll('.tf-codeblock__line');
  assert(!lines[0].classList.contains('tf-codeblock__line--highlighted'));
  assert(lines[1].classList.contains('tf-codeblock__line--highlighted'));
  assert(!lines[2].classList.contains('tf-codeblock__line--highlighted'));
});

test('CodeBlock copyable=true renderuje button Copy', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CODE_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'x' }],
    [1, 'plain'], [2, false], [3, true],
  ]));
  assert(el.querySelector('.tf-codeblock__copy') != null);
});

test('CodeBlock XSS: <script> w content jest escaped', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CODE_BLOCK_TAG, [
    [0, { kind: 'literal', value: '<script>alert(1)</script>' }],
    [1, 'plain'], [2, false], [3, false],
  ]));
  assertEq(el.querySelector('script'), null);
  assertEq(el.querySelector('code').textContent, '<script>alert(1)</script>');
});

test('CodeBlock unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CODE_BLOCK_TAG, [
    [0, { kind: 'literal', value: 'x' }],
    [1, 'plain'], [2, false], [3, false], [99, 'x'],
  ])));
});

// ---- report ----
function reportResults() {
  let pass = 0, fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) { pass++; lines.push(`✓ ${r.name}`); }
    else { fail++; lines.push(`✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`); }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  return { pass, fail, text: lines.join('\n') };
}
if (typeof process !== 'undefined') {
  const r = reportResults();
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}
export { reportResults };
