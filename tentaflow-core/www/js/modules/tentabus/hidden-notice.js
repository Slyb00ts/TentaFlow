// =============================================================================
// File: hidden-notice.js — refused characters in words: names, places, causes
// =============================================================================
// What the windows and the schema page say about a text that holds an invisible
// or direction-changing character (the list: hidden-characters.js). The common
// characters have a plain Polish name; any other keeps the server's English one.
// =============================================================================

import { escapeHtml } from '/js/utils.js';
import { T } from '/js/modules/tentabus/format.js';
import { findAllHidden, hiddenCharacterName } from '/js/modules/tentabus/hidden-characters.js';

const NAME_KEY = new Map([
  [0x00AD, 'shy'], [0x200B, 'zwsp'], [0x200C, 'zwnj'], [0x200D, 'zwj'], [0x200E, 'lrm'], [0x200F, 'rlm'],
  [0x202A, 'lre'], [0x202B, 'rle'], [0x202C, 'pdf'], [0x202D, 'lro'], [0x202E, 'rlo'], [0x2060, 'wj'],
  [0xFEFF, 'bom'], [0x061C, 'alm'], [0x2066, 'lri'], [0x2067, 'rli'], [0x2068, 'fsi'], [0x2069, 'pdi'],
  [0x0085, 'nel'], [0x2028, 'ls'], [0x2029, 'ps'], [0x034F, 'cgj'], [0x007F, 'del'],
  [0x115F, 'hangul'], [0x1160, 'hangul'], [0x3164, 'hangul'], [0xFFA0, 'hangul'],
  [0x180B, 'mongolian'], [0x180C, 'mongolian'], [0x180D, 'mongolian'], [0x180F, 'mongolian'],
]);

const nameKey = (cp) => {
  if (NAME_KEY.has(cp)) return NAME_KEY.get(cp);
  if (cp < 0x20) return 'control';
  if (cp >= 0x80 && cp <= 0x9F) return 'c1';
  if (cp >= 0xE0000 && cp <= 0xE007F) return 'tag';
  return null;
};

/** The plain name of a refused character; the server's English `fallback` when it has none here. */
export function plainName(codePoint, fallback = '') {
  const key = nameKey(codePoint);
  return key ? T(`schemas.hidden.name.${key}`) : fallback;
}

/** Why the character is there, for the ones that come from somewhere recognisable; `''` otherwise. */
export function causeHint(codePoint) {
  if (codePoint === 0x00AD || codePoint === 0x200B) return T('schemas.hidden.cause_paste');
  if (codePoint === 0x200D) return T('schemas.hidden.cause_zwj');
  return '';
}

const SHOWN_PLACES = 5;

const placeText = (p) => T(p.escape ? 'schemas.hidden.place_escape' : 'schemas.hidden.place', {
  code: p.code, name: plainName(p.codePoint, p.name), line: p.line, column: p.column,
});

/** "U+200B (spacja o zerowej szerokości) w wierszu 3, kolumnie 5; …", the first few and a count of the rest. */
export function placesText(places) {
  const shown = places.slice(0, SHOWN_PLACES).map(placeText);
  const rest = places.length - shown.length;
  return rest > 0 ? `${shown.join('; ')}; ${T('schemas.hidden.more', { count: rest })}` : shown.join('; ');
}

/**
 * The refused characters of `text` and the sentences that place them, or `null` when it holds none. `options` =
 * `{ leadingBom, schemaType }`: with a `schemaType` the characters written as escape sequences of that kind count too.
 * `raw` and `escapes` are the two groups (the button removes only the first), each with its own sentence.
 */
export function hiddenPlaces(text, options) {
  const places = findAllHidden(text, options);
  if (!places.length) return null;
  const raw = places.filter((p) => !p.escape);
  const escapes = places.filter((p) => p.escape);
  return {
    places, raw, escapes, text: placesText(places), rawText: raw.length ? placesText(raw) : '', escapeText: escapes.length ? placesText(escapes) : '',
  };
}

const MARKER = '⟦U\\+[0-9A-F]+⟧';
const MARKED_WORD = new RegExp(`(?:[\\p{L}\\p{N}_.$-]|${MARKER})*${MARKER}(?:[\\p{L}\\p{N}_.$-]|${MARKER})*`, 'gu');

/**
 * A sentence as HTML in which every word that holds a ⟦U+XXXX⟧ marker stays on one line: the marker is punctuation, so
 * a line would otherwise break inside it (`ter⟦U+202E⟧` / `min`).
 */
export function sentenceHtml(sentence) {
  return escapeHtml(sentence).replace(MARKED_WORD, (word) => `<span class="tb-hidden-name" style="white-space:nowrap">${word}</span>`);
}

const PREVIEW_LINES = 5;
const PREVIEW_WIDTH = 120;
const PREVIEW_LEAD = 40;

/** The display cells of one line: a refused character stands as ⟦U+XXXX⟧, an escape sequence as typed and `mark`ed. */
function lineCells(source, { leadingBom = false } = {}, firstLine, escapes) {
  const cells = [];
  let first = -1;
  let column = 0;
  for (const ch of source) {
    column += 1;
    const cp = ch.codePointAt(0);
    const marked = escapes.some((e) => column >= e.column && column < e.column + e.length);
    const refused = !marked && (cp >= 0x7F || cp < 0x20) && hiddenCharacterName(cp) && !(leadingBom && firstLine && column === 1 && cp === 0xFEFF);
    if ((marked || refused) && first < 0) first = cells.length;
    if (refused) {
      for (const c of `⟦U+${cp.toString(16).toUpperCase().padStart(4, '0')}⟧`) cells.push({ ch: c, mark: false });
    } else {
      cells.push({ ch, mark: marked });
    }
  }
  return { cells, first };
}

/**
 * The lines of `text` that hold a refused character, `{ line, text, parts }` each, in order, at most five and a count
 * of the rest (`more`). The characters are written ⟦U+XXXX⟧ and a long line is cut around the first one, so a person can
 * find the place in a box that draws such a character reversed or not at all; `parts` = `[{ text, mark }]` is the
 * same line with the escape sequences marked. `options.schemaType` makes the escape sequences count; the line the server
 * named (`named` = `[{ line, column }]`) is added as it was typed when the scan did not find it.
 */
export function hiddenLines(text, options, named = null) {
  const places = findAllHidden(text, options);
  const numbers = [...new Set([...places.map((p) => p.line), ...(named || []).map((n) => n.line)])].sort((a, b) => a - b);
  const lines = String(text ?? '').split('\n');
  const shown = numbers.slice(0, PREVIEW_LINES).map((line) => {
    const source = (lines[line - 1] ?? '').replace(/\r$/, '');
    const { cells, first } = lineCells(source, options, line === 1, places.filter((p) => p.escape && p.line === line));
    const asked = (named || []).find((n) => n.line === line);
    const centre = first >= 0 ? first : Math.max(0, (asked ? asked.column : 1) - 1);
    // A line that fits is shown whole; a longer one is cut so that the first place stays in view.
    const from = cells.length <= PREVIEW_WIDTH ? 0 : Math.max(0, Math.min(centre - PREVIEW_LEAD, cells.length - PREVIEW_WIDTH));
    const to = Math.min(cells.length, from + PREVIEW_WIDTH);
    const parts = [];
    for (const cell of cells.slice(from, to)) {
      const last = parts[parts.length - 1];
      if (last && last.mark === cell.mark) last.text += cell.ch;
      else parts.push({ text: cell.ch, mark: cell.mark });
    }
    if (from > 0) parts.unshift({ text: '…', mark: false });
    if (to < cells.length) parts.push({ text: '…', mark: false });
    return { line, text: parts.map((p) => p.text).join(''), parts };
  });
  return { lines: shown, more: numbers.length - shown.length };
}

/**
 * What the answer to a read of pattern `subject` (`got`: `{ schema: { version }, schemaText }`) says about the
 * topic that checks with it: `{ version, problem }`, `problem` = `{ subject, version, text }` when that text holds a
 * refused character. The kind of the pattern is not in the answer, so the caller passes it; a kind that is not
 * known means the text cannot be judged (an XSD may start with a byte-order mark), and nothing is claimed.
 */
export function judgedSchema(subject, got, schemaType) {
  const version = got?.schema?.version ?? null;
  const found = schemaType ? hiddenPlaces(got?.schemaText, { leadingBom: schemaType === 'xsd', schemaType }) : null;
  return { version, problem: found ? { subject, version, text: found.text } : null };
}
