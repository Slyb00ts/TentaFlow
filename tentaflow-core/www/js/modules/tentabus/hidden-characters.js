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

// ---------------------------------------------------------------------------
// Characters written as escapes (text_guard.rs `escape_hits`, the same rules)
// ---------------------------------------------------------------------------

/** How each kind of pattern writes a character in another way; Thrift has no such escape. */
const ESCAPE_SYNTAX = { json_schema: 'json', avro: 'json', hl7v2_profile: 'json', xsd: 'xml', protobuf: 'proto' };

/**
 * Below U+0100 an escape is how a format writes an ordinary control or byte (`[^\u0000-\u001F]`, `&#x7F;`,
 * `"\x00"`), so only U+00AD, U+0085 and what lies above are refused as escapes.
 */
const refusedEscape = (cp) => (cp >= 0x100 || cp === 0xAD || cp === 0x85) && hiddenCharacterName(cp) !== '';

const HEX4 = /^[0-9a-fA-F]{4}$/;
const hex4 = (text, at) => {
  const part = text.slice(at, at + 4);
  return HEX4.test(part) ? parseInt(part, 16) : null;
};

function jsonEscapes(text, hit) {
  let i = 0;
  while (i < text.length) {
    if (text[i] !== '\\') {
      i += 1;
    } else if (text[i + 1] !== 'u') {
      i += text[i + 1] === '\\' ? 2 : 1;
    } else {
      const unit = hex4(text, i + 2);
      if (unit == null) {
        i += 2;
        continue;
      }
      const low = unit >= 0xD800 && unit < 0xDC00 && text.startsWith('\\u', i + 6) ? hex4(text, i + 8) : null;
      if (low != null && low >= 0xDC00 && low < 0xE000) {
        hit(i, i + 12, 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00));
        i += 12;
      } else {
        hit(i, i + 6, unit);
        i += 6;
      }
    }
  }
}

const XML_SKIPPED = [['<!--', '-->'], ['<![CDATA[', ']]>'], ['<?', '?>']];

function xmlEscapes(text, hit) {
  let i = 0;
  while (i < text.length) {
    const skipped = XML_SKIPPED.find(([open]) => text.startsWith(open, i));
    if (skipped) {
      const close = text.indexOf(skipped[1], i + skipped[0].length);
      i = close < 0 ? text.length : close + skipped[1].length;
      continue;
    }
    if (text.startsWith('&#', i)) {
      const hex = text[i + 2] === 'x' || text[i + 2] === 'X';
      const start = i + (hex ? 3 : 2);
      const digits = (hex ? /^[0-9a-fA-F]+/ : /^[0-9]+/).exec(text.slice(start))?.[0] || '';
      if (digits && text[start + digits.length] === ';') {
        // Every digit counts: leading zeros do not hide a reference, and a value too big for a code point is none.
        const significant = digits.replace(/^0+/, '');
        if (significant.length <= 8) hit(i, start + digits.length + 1, significant ? parseInt(significant, hex ? 16 : 10) : 0);
        i = start + digits.length + 1;
        continue;
      }
    }
    i += 1;
  }
}

const UTF8 = new TextEncoder();
const STRICT_UTF8 = new TextDecoder('utf-8', { fatal: true });
const PROTO_CONTROL = { a: 7, b: 8, f: 0x0c, n: 0x0a, r: 0x0d, t: 9, v: 0x0b };

/** The reading of a protobuf string literal from its opening quote; returns where it ends. */
function protoLiteral(text, open, hit) {
  const quote = text[open];
  const bytes = [];
  const pushChar = (at, len, escaped) => {
    for (const value of UTF8.encode(text.slice(at, at + len))) bytes.push({ value, start: at, end: at + len, escaped });
  };
  let i = open + 1;
  while (i < text.length && text[i] !== quote && text[i] !== '\n') {
    if (text[i] !== '\\') {
      const len = text.codePointAt(i) > 0xFFFF ? 2 : 1;
      pushChar(i, len, false);
      i += len;
      continue;
    }
    const next = text[i + 1];
    if (next === undefined) return text.length;
    let value = null;
    let len;
    if (next in PROTO_CONTROL) {
      value = PROTO_CONTROL[next];
      len = 2;
    } else if (next === 'x') {
      const part = text.slice(i + 2, i + 4);
      if (/^[0-9a-fA-F]{2}$/.test(part)) {
        value = parseInt(part, 16);
        len = 4;
      } else {
        len = 2;
      }
    } else if (next >= '0' && next <= '7') {
      // The protobuf lexer takes any decimal digit after the first octal one and lets the byte wrap: `\428` is 0x18.
      const more = /^[0-9]{0,2}/.exec(text.slice(i + 2))[0];
      value = [...next + more].reduce((acc, d) => ((acc << 3) + Number(d)) & 0xFF, 0);
      len = 2 + more.length;
    } else {
      len = 1 + (text.codePointAt(i + 1) > 0xFFFF ? 2 : 1);
    }
    if (value != null) bytes.push({ value, start: i, end: i + len, escaped: true });
    else pushChar(i + 1, len - 1, false);
    i += len;
  }
  let at = 0;
  while (at < bytes.length) {
    const lead = bytes[at].value;
    const want = lead >= 0xF0 && lead <= 0xF7 ? 4 : lead >= 0xE0 && lead <= 0xEF ? 3 : lead >= 0xC0 && lead <= 0xDF ? 2 : 1;
    const group = bytes.slice(at, at + want);
    let decoded = null;
    if (group.length === want) {
      try {
        decoded = STRICT_UTF8.decode(Uint8Array.from(group.map((b) => b.value)));
      } catch {
        decoded = null;
      }
    }
    if (decoded && [...decoded].length === 1) {
      if (group.some((b) => b.escaped)) hit(group[0].start, group[want - 1].end, decoded.codePointAt(0));
      at += want;
    } else {
      at += 1;
    }
  }
  return i + 1;
}

function protoEscapes(text, hit) {
  let i = 0;
  while (i < text.length) {
    if (text.startsWith('//', i)) {
      const end = text.indexOf('\n', i);
      i = end < 0 ? text.length : end + 1;
    } else if (text.startsWith('/*', i)) {
      const end = text.indexOf('*/', i + 2);
      i = end < 0 ? text.length : end + 2;
    } else if (text[i] === '"' || text[i] === "'") {
      i = protoLiteral(text, i, hit);
    } else {
      i += 1;
    }
  }
}

/**
 * The refused characters `text` writes as escape sequences of its format, in order: `{ codePoint, code, name, line,
 * column, escape: true, length }` (`length` = characters of the sequence). `schemaType` is the pattern's kind; one
 * with no such escape (Thrift) or an unknown one has none.
 */
export function findEscapes(text, schemaType) {
  const syntax = ESCAPE_SYNTAX[schemaType];
  const value = String(text ?? '');
  if (!syntax) return [];
  const hits = [];
  const hit = (start, end, cp) => {
    if (refusedEscape(cp)) hits.push({ start, end, cp });
  };
  ({ json: jsonEscapes, xml: xmlEscapes, proto: protoEscapes })[syntax](value, hit);
  hits.sort((a, b) => a.start - b.start);
  let line = 1;
  let column = 1;
  let cursor = 0;
  return hits.map(({ start, end, cp }) => {
    for (const ch of value.slice(cursor, start)) {
      if (ch === '\n') {
        line += 1;
        column = 1;
      } else {
        column += 1;
      }
    }
    cursor = start;
    return { codePoint: cp, code: `U+${hex(cp)}`, name: hiddenCharacterName(cp), line, column, escape: true, length: [...value.slice(start, end)].length };
  });
}

/**
 * Every refused character of `text`, standing in it (`escape: false`) or written as an escape sequence of the pattern's
 * kind (`escape: true`, `options.schemaType`), in the order of the text.
 */
export function findAllHidden(text, { leadingBom = false, schemaType = '' } = {}) {
  const raw = findHidden(text, { leadingBom }).map((p) => ({ ...p, escape: false }));
  return [...raw, ...findEscapes(text, schemaType)].sort((a, b) => a.line - b.line || a.column - b.column);
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
 * and values a correction respells. This is a reading of the text the author
 * is correcting, in every position; the server's list of what stored records keep
 * (`text_guard::hidden_spellings`) is limited to names and values and leaves out
 * titles, descriptions and comments.
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
