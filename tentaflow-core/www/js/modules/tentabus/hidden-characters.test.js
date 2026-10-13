// =============================================================================
// File: hidden-characters.test.js — the dashboard's list equals the server's
// =============================================================================
// `src/bus/schema_registry/hidden_characters.json` is the table the Rust screen
// reads. Every code point is classified here and must agree with it, name
// included, so the two sides cannot drift apart.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { HIDDEN_RANGES, hiddenCharacterName, showHidden, findHidden, stripHidden } from './hidden-characters.js';

const rows = JSON.parse(readFileSync(new URL('../../../../src/bus/schema_registry/hidden_characters.json', import.meta.url), 'utf8'));

test('the list is the server\'s, range for range', () => {
  assert.deepEqual(
    HIDDEN_RANGES,
    rows.map((r) => [parseInt(r.from, 16), parseInt(r.to, 16), r.name]),
  );
});

test('every code point is classified as the server\'s table classifies it', () => {
  const expected = new Map();
  for (const r of rows) {
    for (let cp = parseInt(r.from, 16); cp <= parseInt(r.to, 16); cp += 1) expected.set(cp, r.name);
  }
  for (let cp = 0; cp <= 0x10FFFF; cp += 1) {
    assert.equal(hiddenCharacterName(cp), expected.get(cp) ?? '', `U+${cp.toString(16).toUpperCase()}`);
  }
});

test('TAB, LF, CR, Polish letters, emoji and the variation selectors are not hidden', () => {
  for (const ch of ['\t', '\n', '\r', 'ą', 'Ż', '😀', '️', '\u{E0100}', ' ']) {
    assert.equal(showHidden(ch), ch, ch);
  }
});

test('a hidden character is written out, wherever it stands in the text', () => {
  assert.equal(showHidden('a\u202Eb'), 'a⟦U+202E⟧b');
  assert.equal(showHidden('\u200Bx\u0001\u007F'), '⟦U+200B⟧x⟦U+0001⟧⟦U+007F⟧');
  assert.equal(showHidden('\u{E0041}\u115F\u034F\u180B\u3164\uFFA0'), '⟦U+E0041⟧⟦U+115F⟧⟦U+034F⟧⟦U+180B⟧⟦U+3164⟧⟦U+FFA0⟧');
  assert.equal(showHidden(null), '');
});

test('a leading byte-order mark is left alone only where the format allows it', () => {
  assert.equal(showHidden('\uFEFF<xs:schema/>', { leadingBom: true }), '\uFEFF<xs:schema/>');
  assert.equal(showHidden('\uFEFF<xs:schema/>'), '⟦U+FEFF⟧<xs:schema/>');
  assert.equal(showHidden('<a/>\uFEFF', { leadingBom: true }), '<a/>⟦U+FEFF⟧');
});

const C = (cp) => String.fromCodePoint(cp);

test('findHidden places each refused character the way the server does: lines end at LF, columns count scalar values', () => {
  const text = `ab\r\nc${C(0x202E)}d😀${C(0x200B)}\n${C(0x00AD)}`;
  assert.deepEqual(
    findHidden(text).map((p) => [p.code, p.name, p.line, p.column]),
    [['U+202E', 'RIGHT-TO-LEFT OVERRIDE', 2, 2], ['U+200B', 'ZERO WIDTH SPACE', 2, 5], ['U+00AD', 'SOFT HYPHEN', 3, 1]],
  );
  assert.deepEqual(findHidden('plain żółć 😀\t\r\n'), []);
  assert.deepEqual(findHidden(`${C(0xFEFF)}x`, { leadingBom: true }), []);
  assert.deepEqual(findHidden(`${C(0xFEFF)}x`).map((p) => p.column), [1]);
  assert.deepEqual(findHidden(`x${C(0xFEFF)}`, { leadingBom: true }).map((p) => p.column), [2]);
});

test('stripHidden removes exactly what findHidden finds and keeps an allowed leading byte-order mark', () => {
  const text = `${C(0xFEFF)}a${C(0x202E)}b${C(0x200D)}c${C(0xE0041)}d\t`;
  assert.equal(stripHidden(text), 'abcd\t');
  assert.equal(stripHidden(text, { leadingBom: true }), `${C(0xFEFF)}abcd\t`);
  assert.deepEqual(findHidden(stripHidden(text)), []);
  assert.deepEqual(findHidden(stripHidden(text, { leadingBom: true }), { leadingBom: true }), []);
  assert.equal(stripHidden(null), '');
});
