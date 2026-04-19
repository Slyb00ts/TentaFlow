// =============================================================================
// Plik: lib/text-measure.js
// Opis: Wrapper na pretext (chenglou/pretext) — szybki, dokladny pomiar
//       wysokosci tekstu wieloliniowego BEZ DOM reflow. Cache per
//       (text, font, maxWidth). Invalidation przy resize (zmiana maxWidth).
//
//       API:
//         measureHeight(text, opts)  → { height, lines }
//         clearCache()              → wyczysc cache
//
//       Wydajnosc: prepare() raz per (text, font), potem layout() to czysta
//       arytmetyka — sub-ms per call. Idealne pod virtual list (chat,
//       audit, logs streaming).
// =============================================================================

import { prepare, layout, clearCache as clearPretextCache } from '/js/vendor/pretext/layout.js';

const DEFAULT_FONT = '14px "Manrope", -apple-system, system-ui, sans-serif';
const DEFAULT_LINE_HEIGHT = 21; // 14px * 1.5

/// Cache: key = `${text}|${font}|${maxWidth}` → { height, lines }
/// LRU-style — gdy przekroczy MAX_CACHE, drop najstarsze 20%.
const CACHE = new Map();
const MAX_CACHE = 5000;

/// Cache prepared (po samym text+font) — niezalezne od maxWidth.
/// Reuse miedzy roznymi szerokosciami (np. resize).
const PREPARED_CACHE = new Map();
const MAX_PREPARED = 2000;

function cacheGet(map, key) {
  if (!map.has(key)) return undefined;
  // touch — przesuwa na koniec (newest)
  const v = map.get(key);
  map.delete(key);
  map.set(key, v);
  return v;
}

function cacheSet(map, key, value, max) {
  if (map.size >= max) {
    // Drop oldest 20%
    const toDrop = Math.floor(max * 0.2);
    const it = map.keys();
    for (let i = 0; i < toDrop; i++) map.delete(it.next().value);
  }
  map.set(key, value);
}

/// Mierzy wysokosc tekstu wewnatrz `maxWidth` z dana fontem i line-height.
/// Zwraca `{ height, lines }`. Cache hit ~0ms, miss ~0.1-2ms (zaleznie od dlugosci).
///
/// opts: { font?, maxWidth, lineHeight? }
export function measureHeight(text, opts) {
  if (!text || !opts || !opts.maxWidth || opts.maxWidth <= 0) {
    return { height: 0, lines: 0 };
  }
  const font = opts.font || DEFAULT_FONT;
  const maxWidth = Math.floor(opts.maxWidth);
  const lineHeight = opts.lineHeight || DEFAULT_LINE_HEIGHT;

  const cacheKey = `${text.length}:${hashStr(text)}|${font}|${maxWidth}|${lineHeight}`;
  const hit = cacheGet(CACHE, cacheKey);
  if (hit) return hit;

  const prepKey = `${text.length}:${hashStr(text)}|${font}`;
  let prepared = cacheGet(PREPARED_CACHE, prepKey);
  if (!prepared) {
    try {
      prepared = prepare(text, font);
      cacheSet(PREPARED_CACHE, prepKey, prepared, MAX_PREPARED);
    } catch (err) {
      console.warn('[text-measure] prepare failed:', err.message);
      return { height: lineHeight, lines: 1 };
    }
  }

  let result;
  try {
    const out = layout(prepared, maxWidth, lineHeight);
    result = { height: out.height, lines: out.lineCount };
  } catch (err) {
    console.warn('[text-measure] layout failed:', err.message);
    result = { height: lineHeight, lines: 1 };
  }

  cacheSet(CACHE, cacheKey, result, MAX_CACHE);
  return result;
}

/// Mierzy tylko height (skrot dla virtual list itemSize callback).
export function measureItemHeight(text, opts) {
  return measureHeight(text, opts).height;
}

/// Czysci cala cache wraz z pretext internal cache.
export function clearCache() {
  CACHE.clear();
  PREPARED_CACHE.clear();
  try { clearPretextCache(); } catch { /* ignore */ }
}

/// Aktualnie uzywany font default — uzywany przez resize observers.
export function getDefaultFont() { return DEFAULT_FONT; }
export function getDefaultLineHeight() { return DEFAULT_LINE_HEIGHT; }

/// Tania hash funkcja (FNV-1a 32-bit) — szybsza od bezposredniego porownania
/// dlugich stringow w cache key.
function hashStr(s) {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = (h * 0x01000193) >>> 0;
  }
  return h.toString(36);
}
