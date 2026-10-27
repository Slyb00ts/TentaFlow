// =============================================================================
// File: lib/chat-transcript.js — timing of the live audio transcript: which
// sentence is spoken now and when each of its words starts. Pure functions, so
// the maths is testable without an AudioContext.
// =============================================================================

export function splitWords(sentence) {
  return String(sentence || '').split(/\s+/).filter(Boolean);
}

/**
 * Start offset (ms from the beginning of the sentence's audio) of every word.
 * The audio chunk's real duration is known, the position of each word inside it
 * is not: it is estimated by weighting words by their length plus a constant
 * for the pause a word costs, which tracks speech rate closely enough for a
 * highlight that moves word by word.
 */
export function wordOffsets(words, durationMs) {
  if (words.length === 0) return [];
  const weights = words.map((w) => w.length + 2);
  const total = weights.reduce((a, b) => a + b, 0);
  let at = 0;
  return weights.map((w) => {
    const start = Math.round(at);
    at += (durationMs * w) / total;
    return start;
  });
}

/**
 * The k-th audio chunk of an answer carries the k-th sentence; when the answer
 * was split into fewer sentences than chunks (the server cut a long sentence)
 * the last sentence stays current instead of the highlight disappearing.
 */
export function sentenceForChunk(sentences, chunkIndex) {
  if (sentences.length === 0) return -1;
  return Math.min(Math.max(0, chunkIndex), sentences.length - 1);
}
