// =============================================================================
// File: modules/org-structure/text-metrics.js
// Description: Width of a piece of text and its wrapping into lines, for the
//   org chart. SVG has no text layout, so cards are sized from the text they
//   hold: measured with a canvas in the browser (the real font), estimated where
//   there is no canvas (tests). Nothing is ever cut with an ellipsis — text
//   that does not fit on one line wraps.
// =============================================================================

const FONT = 'Manrope, Inter, system-ui, -apple-system, "Segoe UI", sans-serif';
// Average glyph width of the UI face in em, used only when no canvas exists. A little wide on
// purpose: an estimate that is too small would cut text, one that is too large only pads a card.
const ESTIMATE_EM = 0.62;

let context;
const cache = new Map();

function canvasContext() {
  if (context !== undefined) return context;
  context = null;
  try {
    const canvas = typeof document !== 'undefined' ? document.createElement('canvas') : null;
    const ctx = canvas?.getContext?.('2d');
    if (ctx && typeof ctx.measureText === 'function') context = ctx;
  } catch {
    context = null;
  }
  return context;
}

/** Width in px of `text` set at `size` px and `weight`. */
export function textWidth(text, size, weight = 400) {
  const key = `${weight}|${size}|${text}`;
  const hit = cache.get(key);
  if (hit !== undefined) return hit;
  const ctx = canvasContext();
  let width;
  if (ctx) {
    ctx.font = `${weight} ${size}px ${FONT}`;
    width = ctx.measureText(text).width;
  } else {
    width = Array.from(text).length * size * ESTIMATE_EM;
  }
  if (cache.size > 20000) cache.clear();
  cache.set(key, width);
  return width;
}

/**
 * Breaks `text` into lines no wider than `maxWidth`: at spaces, then after a hyphen (a double-barrelled
 * surname wraps at its hyphen), and only a piece wider than a whole line is broken by letters.
 */
export function wrapLines(text, maxWidth, size, weight = 400) {
  const tokens = [];
  for (const word of String(text ?? '').split(/\s+/).filter(Boolean)) {
    (word.match(/[^-]+-?|-/g) ?? [word]).forEach((piece, i) => tokens.push({ piece, spaced: i === 0 }));
  }
  if (!tokens.length) return [''];
  const lines = [];
  let line = '';
  const fits = (candidate) => textWidth(candidate, size, weight) <= maxWidth;
  const flush = () => { if (line) lines.push(line); line = ''; };
  for (const { piece, spaced } of tokens) {
    const candidate = line ? `${line}${spaced ? ' ' : ''}${piece}` : piece;
    if (fits(candidate)) {
      line = candidate;
      continue;
    }
    flush();
    if (fits(piece)) {
      line = piece;
      continue;
    }
    let chunk = '';
    for (const ch of Array.from(piece)) {
      if (chunk && !fits(chunk + ch)) {
        lines.push(chunk);
        chunk = ch;
      } else {
        chunk += ch;
      }
    }
    line = chunk;
  }
  flush();
  return lines;
}
