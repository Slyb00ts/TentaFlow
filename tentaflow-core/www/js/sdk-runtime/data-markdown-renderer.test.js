// =============================================================================
// Plik: sdk-runtime/data-markdown-renderer.test.js
// Opis: Testy Markdown (0x0220) — chunk 3.3d-16.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { MARKDOWN_TAG } from './data-markdown-renderer.js';

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
const LIT = (value) => ({ kind: 'literal', value });
const BOUND = (...segs) => ({ kind: 'bound', path: PATH(...segs) });
const ALL_FEATURES = ['heading', 'list', 'code_block', 'blockquote', 'table', 'link', 'image', 'emphasis', 'strong', 'code_inline'];

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

function mdFields({
  content = LIT('Hello'),
  allowedFeatures = ALL_FEATURES,
  maxHeightPx = null,
  linkTarget = 'self',
} = {}) {
  const f = [[0, content], [1, allowedFeatures]];
  if (maxHeightPx != null) f.push([2, maxHeightPx]);
  f.push([3, linkTarget]);
  return f;
}

// ============================================================================

test('Markdown heading h1..h3', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: LIT('# H1\n## H2\n### H3') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('h1').textContent, 'H1');
  assertEq(el.querySelector('h2').textContent, 'H2');
  assertEq(el.querySelector('h3').textContent, 'H3');
});

test('Markdown heading disabled → plain text', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('# H1'),
    allowedFeatures: [],
  })));
  document.body.appendChild(el);
  assert(el.querySelector('h1') == null);
  assert(el.querySelector('p').textContent.includes('# H1'));
});

test('Markdown code_block renders <pre><code>', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: LIT('```js\nconst x = 1;\n```') })));
  document.body.appendChild(el);
  const code = el.querySelector('code');
  assert(code != null);
  assertEq(code.getAttribute('data-language'), 'js');
  assertEq(code.textContent, 'const x = 1;');
});

test('Markdown unordered list', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: LIT('- A\n- B\n- C') })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('ul li').length, 3);
});

test('Markdown ordered list', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: LIT('1. A\n2. B') })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('ol li').length, 2);
});

test('Markdown blockquote', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: LIT('> Quote text') })));
  document.body.appendChild(el);
  assert(el.querySelector('blockquote') != null);
  assert(el.querySelector('blockquote p').textContent.includes('Quote text'));
});

test('Markdown table', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('| A | B |\n| --- | --- |\n| 1 | 2 |'),
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('table th').length, 2);
  assertEq(el.querySelectorAll('table td').length, 2);
});

test('Markdown link self target', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('[click](https://example.com)'),
  })));
  document.body.appendChild(el);
  const a = el.querySelector('a');
  assert(a != null);
  assertEq(a.textContent, 'click');
  assert(a.href.includes('example.com'));
  assert(a.target !== '_blank');
});

test('Markdown link blank_via_command target', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('[click](https://example.com)'),
    linkTarget: 'blank_via_command',
  })));
  document.body.appendChild(el);
  const a = el.querySelector('a');
  assertEq(a.target, '_blank');
  assertEq(a.rel, 'noopener noreferrer');
});

test('Markdown javascript: link rejected', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('[xss](javascript:alert(1))'),
  })));
  document.body.appendChild(el);
  assert(el.querySelector('a') == null, 'javascript: link should not render');
});

test('Markdown image', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('![alt](https://example.com/img.png)'),
  })));
  document.body.appendChild(el);
  const img = el.querySelector('img');
  assert(img != null);
  assertEq(img.alt, 'alt');
});

test('Markdown javascript: image src rejected', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('![x](javascript:alert(1))'),
  })));
  document.body.appendChild(el);
  assert(el.querySelector('img') == null);
});

test('Markdown inline emphasis + strong + code', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('This is *em* and **strong** and `code`'),
  })));
  document.body.appendChild(el);
  assert(el.querySelector('em') != null);
  assert(el.querySelector('strong') != null);
  assert(el.querySelector('code') != null);
});

test('Markdown max_height_px sets overflow', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: LIT('text'), maxHeightPx: 200 })));
  document.body.appendChild(el);
  assertEq(el.style.maxHeight, '200px');
  assertEq(el.style.overflow, 'auto');
});

test('Markdown reaguje na patch content', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('md'), value: '# Old' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: BOUND('md') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('h1').textContent, 'Old');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('md'), op: { kind: 'set', value: '## New' } }] });
  assert(el.querySelector('h1') == null);
  assertEq(el.querySelector('h2').textContent, 'New');
});

test('Markdown odrzuca unknown feature', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(MARKDOWN_TAG, mdFields({ allowedFeatures: ['unknown'] }))));
});

test('Markdown odrzuca explicit null allowed_features', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(MARKDOWN_TAG, [[0, LIT('x')], [1, null], [3, 'self']])));
});

test('Markdown XSS: HTML tags in content rendered as text', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('<script>alert(1)</script><img src=x onerror=alert(1)>'),
  })));
  document.body.appendChild(el);
  assert(el.querySelector('script') == null);
  assert(el.querySelectorAll('img').length === 0);
  assert(el.textContent.includes('<script>'));
});

test('Markdown table with partial separator does not infinite loop', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('| A | B |\n| --- | nope |\n| 1 | 2 |'),
  })));
  document.body.appendChild(el);
  assert(el.querySelector('table') == null, 'invalid separator should not produce table');
  assert(el.textContent.includes('A'), 'content preserved');
});

test('Markdown table without separator renders as paragraph', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('| A | B |\n| 1 | 2 |'),
  })));
  document.body.appendChild(el);
  assert(el.querySelector('table') == null, 'no table without separator');
  assert(el.querySelector('p') != null, 'should render as paragraph');
});

test('Markdown nested **strong *em*** renders both', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({
    content: LIT('**bold *italic* text**'),
  })));
  document.body.appendChild(el);
  const strong = el.querySelector('strong');
  assert(strong != null);
  assert(strong.querySelector('em') != null, 'em nested inside strong');
});

test('Markdown empty content renders nothing', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MARKDOWN_TAG, mdFields({ content: LIT('') })));
  document.body.appendChild(el);
  assertEq(el.children.length, 0);
});

// ============================================================================
const failed = results.filter((r) => !r.ok);
console.log(`markdown tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
