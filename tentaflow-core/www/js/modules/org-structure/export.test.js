// =============================================================================
// File: modules/org-structure/export.test.js
// Description: The document that is printed to PDF: a page per chart, the
//   footer on every page, and titles that cannot inject markup.
// =============================================================================

import '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { printDocument } = await import('./export.js');

test('every page carries its title, its chart and the footer', () => {
  const html = printDocument(
    [{ title: 'Whole', svg: '<svg id="a"></svg>' }, { title: 'IT', svg: '<svg id="b"></svg>' }],
    { title: 'Structure', footer: 'As of 2026-09-30' },
  );
  assert.equal((html.match(/<section class="page">/g) ?? []).length, 2);
  assert.equal((html.match(/<footer>As of 2026-09-30<\/footer>/g) ?? []).length, 2);
  assert.ok(html.includes('<svg id="a"></svg>') && html.includes('<svg id="b"></svg>'));
  assert.match(html, /@page\{size:A4 landscape/);
  assert.match(html, /<title>Structure<\/title>/);
});

test('a unit named like markup is text on its page', () => {
  const html = printDocument([{ title: '<img src=x onerror=alert(1)>', svg: '<svg></svg>' }], { title: 'T&C', footer: '"f"' });
  assert.equal(html.includes('<img src=x'), false);
  assert.match(html, /<h1>&lt;img src=x onerror=alert\(1\)&gt;<\/h1>/);
  assert.match(html, /<title>T&amp;C<\/title>/);
});
