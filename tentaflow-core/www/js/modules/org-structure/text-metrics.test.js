// =============================================================================
// File: modules/org-structure/text-metrics.test.js
// Description: Wrapping of chart text: every word survives, lines fit the width,
//   a hyphenated surname breaks at its hyphen, and only a piece wider than a
//   whole line is broken by letters.
// =============================================================================

import '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { textWidth, wrapLines } = await import('./text-metrics.js');

test('text that fits stays on one line', () => {
  assert.deepEqual(wrapLines('Anna Nowak', 500, 12), ['Anna Nowak']);
  assert.deepEqual(wrapLines('', 100, 12), ['']);
});

test('lines break at spaces and every line fits', () => {
  const lines = wrapLines('Starszy Specjalista do spraw Rozwoju Współpracy', 120, 12);
  assert.ok(lines.length > 1);
  assert.equal(lines.join(' '), 'Starszy Specjalista do spraw Rozwoju Współpracy');
  for (const line of lines) assert.ok(textWidth(line, 12) <= 120 + 1e-9, line);
});

test('a hyphenated surname breaks after the hyphen, not in the middle of a piece', () => {
  const lines = wrapLines('Wiśniewski-Kowalczykowski-Żółkiewski', 150, 12);
  assert.ok(lines.length > 1);
  assert.equal(lines.join('').replace(/-$/gm, '-'), 'Wiśniewski-Kowalczykowski-Żółkiewski');
  assert.ok(lines.slice(0, -1).every((l) => l.endsWith('-')));
});

test('a single piece wider than a line is broken by letters and loses nothing', () => {
  const word = 'Nieprzeciwdziałającemu';
  const lines = wrapLines(word, 60, 12);
  assert.ok(lines.length > 1);
  assert.equal(lines.join(''), word);
});
