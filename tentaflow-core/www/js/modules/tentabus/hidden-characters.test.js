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
import { HIDDEN_RANGES, hiddenCharacterName, showHidden } from './hidden-characters.js';

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
  assert.equal(showHidden('a‮b'), 'a⟦U+202E⟧b');
  assert.equal(showHidden('​x\u0001\u007F'), '⟦U+200B⟧x⟦U+0001⟧⟦U+007F⟧');
  assert.equal(showHidden('\u{E0041}ᅟ͏᠋ㅤﾠ'), '⟦U+E0041⟧⟦U+115F⟧⟦U+034F⟧⟦U+180B⟧⟦U+3164⟧⟦U+FFA0⟧');
  assert.equal(showHidden(null), '');
});

test('a leading byte-order mark is left alone only where the format allows it', () => {
  assert.equal(showHidden('﻿<xs:schema/>', { leadingBom: true }), '﻿<xs:schema/>');
  assert.equal(showHidden('﻿<xs:schema/>'), '⟦U+FEFF⟧<xs:schema/>');
  assert.equal(showHidden('<a/>﻿', { leadingBom: true }), '<a/>⟦U+FEFF⟧');
});
