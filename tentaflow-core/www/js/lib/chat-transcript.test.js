// =============================================================================
// File: lib/chat-transcript.test.js — sentence and word timing of the live transcript
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { splitWords, wordOffsets, sentenceForChunk } from './chat-transcript.js';

test('splitWords ignores repeated whitespace and empty input', () => {
  assert.deepEqual(splitWords('  Ala   ma\nkota '), ['Ala', 'ma', 'kota']);
  assert.deepEqual(splitWords(''), []);
  assert.deepEqual(splitWords(null), []);
});

test('wordOffsets start at zero, grow monotonically and stay inside the chunk', () => {
  const words = splitWords('Gotowe, przypomnienie jest ustawione na jutro');
  const offsets = wordOffsets(words, 3000);
  assert.equal(offsets[0], 0);
  for (let i = 1; i < offsets.length; i += 1) assert.ok(offsets[i] > offsets[i - 1]);
  assert.ok(offsets[offsets.length - 1] < 3000);
});

test('a longer word holds the highlight longer than a short one', () => {
  const [a, b, c] = wordOffsets(['a', 'przypomnienie', 'to'], 1000);
  assert.ok(b - a < c - b, 'the long word occupies the longer span');
});

test('wordOffsets of nothing is nothing', () => {
  assert.deepEqual(wordOffsets([], 1000), []);
});

test('sentenceForChunk clamps to the sentences that exist', () => {
  const s = ['one.', 'two.', 'three.'];
  assert.equal(sentenceForChunk(s, 0), 0);
  assert.equal(sentenceForChunk(s, 2), 2);
  assert.equal(sentenceForChunk(s, 7), 2);
  assert.equal(sentenceForChunk(s, -1), 0);
  assert.equal(sentenceForChunk([], 0), -1);
});
