// =============================================================================
// File: hidden-characters.js — the characters a message pattern may not contain
// =============================================================================
// The registry refuses a schema text that holds an invisible or
// direction-changing character ("Trojan Source", `text_guard.rs`). A version
// stored before that refusal can still hold one, and this screen reads stored
// texts as they are, so wherever it shows such a text or a name taken from it,
// it writes the character out as ⟦U+202E⟧ instead of drawing it.
//
// The list below is the same one, range for range and name for name, as
// `src/bus/schema_registry/hidden_characters.json` (the file the Rust side
// reads): `hidden-characters.test.js` compares them for every code point.
// =============================================================================

/** [first, last, name] — inclusive code point ranges, sorted. */
export const HIDDEN_RANGES = [
  [0x0, 0x8, 'CONTROL CHARACTER'],
  [0xB, 0xC, 'CONTROL CHARACTER'],
  [0xE, 0x1F, 'CONTROL CHARACTER'],
  [0x7F, 0x7F, 'DELETE'],
  [0x80, 0x84, 'C1 CONTROL CHARACTER'],
  [0x85, 0x85, 'NEXT LINE'],
  [0x86, 0x9F, 'C1 CONTROL CHARACTER'],
  [0xAD, 0xAD, 'SOFT HYPHEN'],
  [0x34F, 0x34F, 'COMBINING GRAPHEME JOINER'],
  [0x600, 0x605, 'INVISIBLE FORMAT CHARACTER'],
  [0x61C, 0x61C, 'ARABIC LETTER MARK'],
  [0x6DD, 0x6DD, 'INVISIBLE FORMAT CHARACTER'],
  [0x70F, 0x70F, 'INVISIBLE FORMAT CHARACTER'],
  [0x890, 0x891, 'INVISIBLE FORMAT CHARACTER'],
  [0x8E2, 0x8E2, 'INVISIBLE FORMAT CHARACTER'],
  [0x115F, 0x115F, 'HANGUL CHOSEONG FILLER'],
  [0x1160, 0x1160, 'HANGUL JUNGSEONG FILLER'],
  [0x180B, 0x180B, 'MONGOLIAN FREE VARIATION SELECTOR ONE'],
  [0x180C, 0x180C, 'MONGOLIAN FREE VARIATION SELECTOR TWO'],
  [0x180D, 0x180D, 'MONGOLIAN FREE VARIATION SELECTOR THREE'],
  [0x180E, 0x180E, 'MONGOLIAN VOWEL SEPARATOR'],
  [0x180F, 0x180F, 'MONGOLIAN FREE VARIATION SELECTOR FOUR'],
  [0x200B, 0x200B, 'ZERO WIDTH SPACE'],
  [0x200C, 0x200C, 'ZERO WIDTH NON-JOINER'],
  [0x200D, 0x200D, 'ZERO WIDTH JOINER'],
  [0x200E, 0x200E, 'LEFT-TO-RIGHT MARK'],
  [0x200F, 0x200F, 'RIGHT-TO-LEFT MARK'],
  [0x2028, 0x2028, 'LINE SEPARATOR'],
  [0x2029, 0x2029, 'PARAGRAPH SEPARATOR'],
  [0x202A, 0x202A, 'LEFT-TO-RIGHT EMBEDDING'],
  [0x202B, 0x202B, 'RIGHT-TO-LEFT EMBEDDING'],
  [0x202C, 0x202C, 'POP DIRECTIONAL FORMATTING'],
  [0x202D, 0x202D, 'LEFT-TO-RIGHT OVERRIDE'],
  [0x202E, 0x202E, 'RIGHT-TO-LEFT OVERRIDE'],
  [0x2060, 0x2060, 'WORD JOINER'],
  [0x2061, 0x2061, 'FUNCTION APPLICATION'],
  [0x2062, 0x2062, 'INVISIBLE TIMES'],
  [0x2063, 0x2063, 'INVISIBLE SEPARATOR'],
  [0x2064, 0x2064, 'INVISIBLE PLUS'],
  [0x2065, 0x2065, 'INVISIBLE FORMAT CHARACTER'],
  [0x2066, 0x2066, 'LEFT-TO-RIGHT ISOLATE'],
  [0x2067, 0x2067, 'RIGHT-TO-LEFT ISOLATE'],
  [0x2068, 0x2068, 'FIRST STRONG ISOLATE'],
  [0x2069, 0x2069, 'POP DIRECTIONAL ISOLATE'],
  [0x206A, 0x206F, 'INVISIBLE FORMAT CHARACTER'],
  [0x3164, 0x3164, 'HANGUL FILLER'],
  [0xFEFF, 0xFEFF, 'ZERO WIDTH NO-BREAK SPACE'],
  [0xFFA0, 0xFFA0, 'HALFWIDTH HANGUL FILLER'],
  [0xFFF9, 0xFFFB, 'INVISIBLE FORMAT CHARACTER'],
  [0x110BD, 0x110BD, 'INVISIBLE FORMAT CHARACTER'],
  [0x110CD, 0x110CD, 'INVISIBLE FORMAT CHARACTER'],
  [0x13430, 0x1343F, 'INVISIBLE FORMAT CHARACTER'],
  [0x1BCA0, 0x1BCA3, 'INVISIBLE FORMAT CHARACTER'],
  [0x1D173, 0x1D17A, 'INVISIBLE FORMAT CHARACTER'],
  [0xE0000, 0xE007F, 'TAG CHARACTER'],
];

const hex = (cp) => cp.toString(16).toUpperCase().padStart(4, '0');

/** The name of a refused code point, or `''` for one that is allowed. */
export function hiddenCharacterName(codePoint) {
  let low = 0;
  let high = HIDDEN_RANGES.length - 1;
  while (low <= high) {
    const mid = (low + high) >> 1;
    const [from, to, name] = HIDDEN_RANGES[mid];
    if (codePoint < from) high = mid - 1;
    else if (codePoint > to) low = mid + 1;
    else return name;
  }
  return '';
}


/**
 * `text` with every refused character written as ⟦U+XXXX⟧. An XSD may start
 * with a byte-order mark (`leadingBom`), which is left alone there.
 */
export function showHidden(text, { leadingBom = false } = {}) {
  const value = String(text ?? '');
  let out = '';
  let index = 0;
  for (const ch of value) {
    const cp = ch.codePointAt(0);
    const refused = (cp >= 0x7F || cp < 0x20) && hiddenCharacterName(cp);
    out += refused && !(leadingBom && index === 0 && cp === 0xFEFF) ? `⟦U+${hex(cp)}⟧` : ch;
    index += 1;
  }
  return out;
}

/**
 * Every refused character of `text` with where it stands, in the order it
 * occurs: `{ codePoint, code: 'U+202E', name, line, column }` — the same
 * 1-based line (LF ends it) and column (scalar values) the server reports.
 * An XSD's leading byte-order mark is not one (`leadingBom`).
 */
export function findHidden(text, { leadingBom = false } = {}) {
  const found = [];
  let line = 1;
  let column = 1;
  let index = 0;
  for (const ch of String(text ?? '')) {
    const cp = ch.codePointAt(0);
    const name = (cp >= 0x7F || cp < 0x20) ? hiddenCharacterName(cp) : '';
    if (name && !(leadingBom && index === 0 && cp === 0xFEFF)) found.push({ codePoint: cp, code: `U+${hex(cp)}`, name, line, column });
    if (cp === 0x0A) {
      line += 1;
      column = 1;
    } else {
      column += 1;
    }
    index += 1;
  }
  return found;
}

/** `text` without its refused characters (an XSD keeps its leading byte-order mark). */
export function stripHidden(text, { leadingBom = false } = {}) {
  let out = '';
  let index = 0;
  for (const ch of String(text ?? '')) {
    const cp = ch.codePointAt(0);
    const refused = (cp >= 0x7F || cp < 0x20) && hiddenCharacterName(cp);
    if (!refused || (leadingBom && index === 0 && cp === 0xFEFF)) out += ch;
    index += 1;
  }
  return out;
}

const WORD_PUNCTUATION = '_-.:$';
const isWordCharacter = (ch) => /[\p{L}\p{N}]/u.test(ch) || WORD_PUNCTUATION.includes(ch);

/**
 * The words of `text` that hold a refused character, `{ shown, clean }` each
 * (`pac⟦U+202E⟧jent` → `pacjent`), once and in order of appearance: the names
 * and values a correction respells. Words are what the server's own list
 * (`text_guard::hidden_spellings`) splits on.
 */
export function hiddenWords(text, { leadingBom = false } = {}) {
  const out = new Map();
  let word = '';
  const flush = () => {
    const clean = stripHidden(word);
    if (clean !== word && /[\p{L}\p{N}]/u.test(clean)) out.set(word, { shown: showHidden(word), clean });
    word = '';
  };
  let index = 0;
  for (const ch of String(text ?? '')) {
    const cp = ch.codePointAt(0);
    const bom = leadingBom && index === 0 && cp === 0xFEFF;
    if (!bom && (isWordCharacter(ch) || ((cp >= 0x7F || cp < 0x20) && hiddenCharacterName(cp)))) word += ch;
    else flush();
    index += 1;
  }
  flush();
  return [...out.values()];
}
