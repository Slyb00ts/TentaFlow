// =============================================================================
// File: lib/md-lite.test.js
// Description: The dashboard's markdown renderer. It is the one renderer every
// screen uses — chat bubbles, addon mime outputs and the TentaQuant notebook —
// so the block rules (headings, lists, paragraphs) are pinned here together
// with the two features that were always there: code fences and <think> blocks.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';

const { renderMarkdown, extractPlainText } = await import('/js/lib/md-lite.js');

test('ATX headings become headings, not paragraphs that show their hashes', () => {
  const html = renderMarkdown('# Grover 4q\n\n## Plan\n\n###### Notatka');
  assert.equal(html, '<h1>Grover 4q</h1><h2>Plan</h2><h6>Notatka</h6>');
  // Seven hashes is not a level, and a hash without a space is not a heading.
  assert.match(renderMarkdown('####### za dużo'), /^<p>####### za dużo<\/p>$/);
  assert.match(renderMarkdown('#hashtag'), /^<p>#hashtag<\/p>$/);
});

test('bullet and numbered lists become lists and keep their numbering', () => {
  assert.equal(renderMarkdown('- a\n- b'), '<ul><li>a</li><li>b</li></ul>');
  assert.equal(renderMarkdown('1. krok\n2. drugi'), '<ol><li>krok</li><li>drugi</li></ol>');
  // A list that starts at 3 renders as starting at 3; renumbering it would
  // misquote the author.
  assert.equal(renderMarkdown('3. trzeci'), '<ol start="3"><li>trzeci</li></ol>');
  // Two kinds in one chunk are two lists, not one with mixed markers.
  assert.equal(renderMarkdown('- a\n1. b'), '<ul><li>a</li></ul><ol><li>b</li></ol>');
  // "-5 °C" is a temperature; a bullet needs the space.
  assert.equal(renderMarkdown('-5 °C'), '<p>-5 °C</p>');
});

test('inline markup still runs inside the new blocks', () => {
  assert.equal(renderMarkdown('## **Plan** na `q[0]`'),
    '<h2><strong>Plan</strong> na <code class="inline">q[0]</code></h2>');
  assert.equal(renderMarkdown('- *raz*'), '<ul><li><em>raz</em></li></ul>');
  // A whole line in italics is not a bullet: the inline pass already turned it
  // into an element, which is why the block rules run after it.
  assert.equal(renderMarkdown('*całość*'), '<p><em>całość</em></p>');
});

test('paragraphs still join their lines and split on blank ones', () => {
  assert.equal(renderMarkdown('a\nb\n\nc'), '<p>a<br>b</p><p>c</p>');
  assert.equal(renderMarkdown(''), '');
  assert.equal(renderMarkdown('   \n  '), '');
});

test('text is escaped, so markdown never smuggles markup through', () => {
  assert.equal(renderMarkdown('# <img src=x onerror=alert(1)>'),
    '<h1>&lt;img src=x onerror=alert(1)&gt;</h1>');
  assert.match(renderMarkdown('- <script>alert(1)</script>'), /&lt;script&gt;/);
});

test('code fences and thinking blocks stay whole blocks of their own', () => {
  const fenced = renderMarkdown('tekst\n\n```python\nprint(1)\n```');
  assert.match(fenced, /^<p>tekst<\/p><div class="code-block">/);
  assert.match(fenced, /<pre><code>print\(1\)\n<\/code><\/pre>/);
  // A fence line is never read as a list item or a heading on the way out.
  assert.doesNotMatch(fenced, /<li>|<h1>/);

  const thought = renderMarkdown('<think>waham się</think>\n\nodpowiedź');
  assert.match(thought, /^<details class="thinking"/);
  assert.match(thought, /<\/details><p>odpowiedź<\/p>$/);
});

test('the sidebar preview strips markup rather than rendering it', () => {
  assert.equal(extractPlainText('**Grover** i `q[0]`'), 'Grover i q[0]');
  assert.equal(extractPlainText('```py\nprint(1)\n```'), '[code]');
});
