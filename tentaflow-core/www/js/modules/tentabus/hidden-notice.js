// =============================================================================
// File: hidden-notice.js — refused characters in words: names, places, causes
// =============================================================================
// What the windows and the schema page say about a text that holds an invisible
// or direction-changing character (the list: hidden-characters.js). The common
// characters have a plain Polish name; any other keeps the server's English one.
// =============================================================================

import { T } from '/js/modules/tentabus/format.js';
import { findHidden } from '/js/modules/tentabus/hidden-characters.js';

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

/** "U+200B (spacja o zerowej szerokości) w wierszu 3, kolumnie 5; …", the first few and a count of the rest. */
export function placesText(places) {
  const shown = places.slice(0, SHOWN_PLACES).map((p) => T('schemas.hidden.place', {
    code: p.code, name: plainName(p.codePoint, p.name), line: p.line, column: p.column,
  }));
  const rest = places.length - shown.length;
  return rest > 0 ? `${shown.join('; ')}; ${T('schemas.hidden.more', { count: rest })}` : shown.join('; ');
}

/** The refused characters of `text` and the sentence that places them, or `null` when it holds none. */
export function hiddenPlaces(text, options) {
  const places = findHidden(text, options);
  return places.length ? { places, text: placesText(places) } : null;
}
