// =============================================================================
// Plik: sdk-runtime/bind-resolver.js
// Opis: Resolver bindingów dla addon UI (Faza 6 Krok 3.2). Tłumaczy
// `BindRef` (Literal | Bound) na wartość z `StateStore`, subskrybuje
// reaktywne zmiany dla `Bound`, oraz aplikuje `ValueFormat` do warstwy
// prezentacji (`tentaflow-sdk-spec/src/protocol/ui/bind.rs` §1.4 +
// `value_format.rs` §1.3).
//
// Boundary contract: BindRef i ValueFormat są już-zdekodowanymi JS
// obiektami od dispatcher'a (snake_case + bare arrays per chunk 3.1).
// =============================================================================

import { isPrefixOf } from './state-store.js';

// =============================================================================
// BindRef
// =============================================================================

/// Sprawdza shape `BindRef` — `{ kind: 'literal', value }` lub
/// `{ kind: 'bound', path }`. Wymaga zdekodowanego JS obiektu, nie
/// surowego CBOR.
export function assertBindRef(bindRef, ctx) { return _assertBindRef(bindRef, ctx); }
function _assertBindRef(bindRef, ctx) {
  if (!bindRef || typeof bindRef !== 'object') {
    throw new TypeError(`${ctx}: BindRef must be object`);
  }
  if (bindRef.kind === 'literal') {
    if (!('value' in bindRef)) {
      throw new TypeError(`${ctx}: BindRef.literal missing value`);
    }
    for (const k of Object.keys(bindRef)) {
      if (k !== 'kind' && k !== 'value') {
        throw new TypeError(`${ctx}: BindRef.literal unexpected key '${k}'`);
      }
    }
  } else if (bindRef.kind === 'bound') {
    if (!Array.isArray(bindRef.path)) {
      throw new TypeError(`${ctx}: BindRef.bound.path must be Array<PathSegment>`);
    }
    for (const k of Object.keys(bindRef)) {
      if (k !== 'kind' && k !== 'path') {
        throw new TypeError(`${ctx}: BindRef.bound unexpected key '${k}'`);
      }
    }
  } else {
    throw new TypeError(`${ctx}: BindRef.kind must be 'literal' or 'bound'`);
  }
}

/// Zwraca aktualną wartość dla `BindRef`. Literal → `value` bezpośrednio.
/// Bound → `store.read(path)` (może być `undefined` jeśli ścieżki nie ma).
export function resolveBindRef(bindRef, store) {
  assertBindRef(bindRef, 'resolveBindRef');
  if (bindRef.kind === 'literal') return bindRef.value;
  return store.read(bindRef.path);
}

/// Subskrybuje zmiany pod `BindRef`. Dla Literal: zwraca no-op unsub
/// (literal się nigdy nie zmienia). Dla Bound: deleguje do
/// `store.subscribe(path, callback)`. Callback dostaje
/// `{ path, store }` z wewnętrznego notify storu.
export function subscribeBindRef(bindRef, store, callback) {
  assertBindRef(bindRef, 'subscribeBindRef');
  if (typeof callback !== 'function') {
    throw new TypeError('subscribeBindRef: callback must be function');
  }
  if (bindRef.kind === 'literal') {
    return () => {};
  }
  return store.subscribe(bindRef.path, callback);
}

// =============================================================================
// BindSpec — selektory dla rendererów
// =============================================================================

const BIND_SPEC_KINDS = Object.freeze([
  'text',
  'attr',
  'class_toggle',
  'show',
  'list',
  'two_way',
]);

/// Zwraca listę `StatePath`-ów, które dany `BindSpec` obserwuje. Renderer
/// używa tego do skonfigurowania subskrypcji raz, niezależnie od wariantu
/// BindSpec. Wszystkie warianty w spec'u mają DOKŁADNIE jedną ścieżkę
/// (`path`), więc zwracamy bare array dla spójności z ewentualnymi
/// przyszłymi wariantami multi-path.
export function bindSpecPaths(bindSpec) {
  assertBindSpec(bindSpec, 'bindSpecPaths');
  return [bindSpec.path];
}

function assertBindSpec(bindSpec, ctx) {
  if (!bindSpec || typeof bindSpec !== 'object') {
    throw new TypeError(`${ctx}: BindSpec must be object`);
  }
  if (!BIND_SPEC_KINDS.includes(bindSpec.kind)) {
    throw new TypeError(
      `${ctx}: BindSpec.kind must be one of ${BIND_SPEC_KINDS.join('/')}`
    );
  }
  if (!Array.isArray(bindSpec.path)) {
    throw new TypeError(`${ctx}: BindSpec.path must be Array<PathSegment>`);
  }
}

/// Subskrybuje zmiany pod ścieżką `BindSpec`. Zwraca unsub. Callback
/// dostaje aktualną wartość pod ścieżką (po zmianie); dla wygody renderery
/// dostają ją od razu, bez konieczności drugiego `store.read()`.
export function subscribeBindSpec(bindSpec, store, callback) {
  assertBindSpec(bindSpec, 'subscribeBindSpec');
  if (typeof callback !== 'function') {
    throw new TypeError('subscribeBindSpec: callback must be function');
  }
  return store.subscribe(bindSpec.path, () => {
    callback(store.read(bindSpec.path));
  });
}

/// Synchroniczny odczyt aktualnej wartości pod `BindSpec.path`. Renderer
/// woła to przy pierwszym mount'cie i potem polega na subscribe'ie.
export function readBindSpec(bindSpec, store) {
  assertBindSpec(bindSpec, 'readBindSpec');
  return store.read(bindSpec.path);
}

// =============================================================================
// ValueFormat applier
// =============================================================================

const VALUE_FORMAT_KINDS = Object.freeze([
  'number',
  'currency',
  'percent',
  'bytes',
  'duration',
  'date',
  'time',
  'datetime',
  'relative',
  'plain',
]);

function assertValueFormat(fmt, ctx) {
  if (!fmt || typeof fmt !== 'object') {
    throw new TypeError(`${ctx}: ValueFormat must be object`);
  }
  if (!VALUE_FORMAT_KINDS.includes(fmt.kind)) {
    throw new TypeError(
      `${ctx}: ValueFormat.kind must be one of ${VALUE_FORMAT_KINDS.join('/')}`
    );
  }
}

/// Aplikuje `ValueFormat` do wartości i zwraca string gotowy do
/// wyświetlenia. `locale` domyślnie z `navigator.language`
/// (lub 'en' w środowiskach non-browser jak Node test runner).
///
/// Konwencje:
///   - `null` / `undefined` → pusty string (renderer wstawia placeholder).
///   - Liczbowe formaty (number/currency/percent/bytes/duration) akceptują
///     każdy finite `Number` (włącznie z floatami) oraz `BigInt` w safe-
///     integer range. BigInt poza safe range jest odrzucany RangeError —
///     bez tego konwersja do Number gubiłaby precyzję.
///   - Formaty dat (date/time/datetime/relative) akceptują Unix-millis
///     jako `Number` lub `BigInt`, w zakresie ±8.64e15 ms (Date TimeClip).
///     Poza zakresem → RangeError.
export function formatValue(value, valueFormat, locale) {
  assertValueFormat(valueFormat, 'formatValue');
  if (value === null || value === undefined) return '';
  const lc = locale ? canonicalLocale(locale) : pickDefaultLocale();
  switch (valueFormat.kind) {
    case 'plain':
      return formatPlain(value);
    case 'number':
      return formatNumber(value, valueFormat, lc);
    case 'currency':
      return formatCurrency(value, valueFormat, lc);
    case 'percent':
      return formatPercent(value, valueFormat, lc);
    case 'bytes':
      return formatBytes(value, valueFormat);
    case 'duration':
      return formatDuration(value, valueFormat);
    case 'date':
      return formatDate(value, valueFormat, lc);
    case 'time':
      return formatTime(value, valueFormat, lc);
    case 'datetime':
      return formatDateTime(value, valueFormat, lc);
    case 'relative':
      return formatRelative(value, lc);
  }
  // Unreachable — assertValueFormat() już to odsiało.
  return '';
}

// JS Date akceptuje TimeClip ±8.64e15 ms. BigInt/Number'y poza tym zakresem
// powodują RangeError w Intl.DateTimeFormat.format() — walidujemy zawczasu.
const MAX_TIME_CLIP_MS = 8_640_000_000_000_000;

function pickDefaultLocale() {
  if (typeof navigator !== 'undefined' && navigator.language) {
    return canonicalLocale(navigator.language);
  }
  return 'en';
}

// Normalizuje BCP 47 tag przez `Intl.getCanonicalLocales`; fallback do
// 'en' przy niepoprawnym tagu. Zabezpiecza Intl.NumberFormat /
// DateTimeFormat / RelativeTimeFormat przed RangeError z user/addon input.
function canonicalLocale(tag) {
  if (typeof tag !== 'string' || tag.length === 0) return 'en';
  try {
    const canonical = Intl.getCanonicalLocales(tag);
    return canonical[0] || 'en';
  } catch {
    return 'en';
  }
}

function toNumber(value, ctx) {
  if (typeof value === 'bigint') {
    // BigInt → Number tylko gdy mieści się w safe-integer range. Inaczej
    // formatowanie ułatwiłoby ukrytą utratę precyzji.
    if (value > BigInt(Number.MAX_SAFE_INTEGER) || value < BigInt(Number.MIN_SAFE_INTEGER)) {
      throw new RangeError(`${ctx}: BigInt out of safe Number range`);
    }
    return Number(value);
  }
  // Number (włącznie z floatami f64) jest dozwolony — formaty number /
  // currency / percent / bytes / duration mają sensowne fp inputy
  // (np. 0.25 dla percent, 1.5 dla bytes/MB).
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  throw new TypeError(`${ctx}: expected Number or BigInt, got ${typeof value}`);
}

function formatPlain(value) {
  if (typeof value === 'bigint') return value.toString();
  if (value instanceof Uint8Array) return `[${value.length} bytes]`;
  if (typeof value === 'object') {
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }
  return String(value);
}

function formatNumber(value, fmt, locale) {
  const n = toNumber(value, 'number');
  const decimals = clampDecimals(fmt.decimals);
  return new Intl.NumberFormat(locale, {
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
    useGrouping: !!fmt.thousands_sep,
  }).format(n);
}

function formatCurrency(value, fmt, locale) {
  const n = toNumber(value, 'currency');
  if (typeof fmt.code !== 'string' || fmt.code.length === 0) {
    throw new TypeError('currency.code must be non-empty ISO 4217 string');
  }
  return new Intl.NumberFormat(locale, {
    style: 'currency',
    currency: fmt.code,
  }).format(n);
}

function formatPercent(value, fmt, locale) {
  const n = toNumber(value, 'percent');
  const decimals = clampDecimals(fmt.decimals);
  return new Intl.NumberFormat(locale, {
    style: 'percent',
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
  }).format(n);
}

function clampDecimals(d) {
  if (typeof d === 'bigint') {
    if (d < 0n || d > 20n) {
      throw new TypeError(`decimals must be integer in [0,20], got ${d}`);
    }
    return Number(d);
  }
  if (!Number.isInteger(d) || d < 0 || d > 20) {
    // Intl.NumberFormat wymaga 0..20 zakresu — odrzucamy z TypeError
    // żeby addon natychmiast widział błąd, zamiast ciemnego throw'a z
    // engine'a.
    throw new TypeError(`decimals must be integer in [0,20], got ${d}`);
  }
  return d;
}

const SI_UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB'];
const BIN_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB', 'EiB'];

function formatBytes(value, fmt) {
  const n = toNumber(value, 'bytes');
  const isBinary = fmt.base === '1024';
  if (fmt.base !== '1000' && fmt.base !== '1024') {
    throw new TypeError(`bytes.base must be '1000' or '1024', got ${fmt.base}`);
  }
  const factor = isBinary ? 1024 : 1000;
  const units = isBinary ? BIN_UNITS : SI_UNITS;
  let v = n;
  let u = 0;
  while (Math.abs(v) >= factor && u < units.length - 1) {
    v /= factor;
    u++;
  }
  const formatted = u === 0 ? v.toFixed(0) : v.toFixed(1);
  return `${formatted} ${units[u]}`;
}

function formatDuration(value, fmt) {
  const ms = toNumber(value, 'duration');
  const total = Math.abs(Math.round(ms));
  const sign = ms < 0 ? '-' : '';
  const sec = Math.floor(total / 1000);
  const hh = Math.floor(sec / 3600);
  const mm = Math.floor((sec % 3600) / 60);
  const ss = sec % 60;
  switch (fmt.style) {
    case 'stopwatch':
      return `${sign}${pad2(hh)}:${pad2(mm)}:${pad2(ss)}`;
    case 'short': {
      if (hh > 0) return `${sign}${hh}h ${mm}m`;
      if (mm > 0) return `${sign}${mm}m ${ss}s`;
      return `${sign}${ss}s`;
    }
    case 'long': {
      const parts = [];
      if (hh > 0) parts.push(`${hh} ${plural(hh, 'hour')}`);
      if (mm > 0) parts.push(`${mm} ${plural(mm, 'minute')}`);
      if (ss > 0 || parts.length === 0) parts.push(`${ss} ${plural(ss, 'second')}`);
      return `${sign}${parts.join(' ')}`;
    }
    default:
      throw new TypeError(
        `duration.style must be short/long/stopwatch, got ${fmt.style}`
      );
  }
}

function pad2(n) {
  return n < 10 ? `0${n}` : String(n);
}

function plural(n, root) {
  return n === 1 ? root : `${root}s`;
}

function toDate(value, ctx) {
  let ms;
  if (typeof value === 'bigint') {
    if (
      value > BigInt(MAX_TIME_CLIP_MS) ||
      value < BigInt(-MAX_TIME_CLIP_MS)
    ) {
      throw new RangeError(`${ctx}: BigInt timestamp out of Date TimeClip range`);
    }
    ms = Number(value);
  } else if (typeof value === 'number' && Number.isFinite(value)) {
    if (Math.abs(value) > MAX_TIME_CLIP_MS) {
      throw new RangeError(`${ctx}: Number timestamp out of Date TimeClip range`);
    }
    ms = value;
  } else {
    throw new TypeError(`${ctx}: expected Number/BigInt timestamp, got ${typeof value}`);
  }
  const d = new Date(ms);
  if (!Number.isFinite(d.getTime())) {
    throw new RangeError(`${ctx}: invalid Date after TimeClip`);
  }
  return d;
}

const DATE_STYLES = new Set(['short', 'medium', 'long', 'full']);
const TIME_STYLES = new Set(['short', 'medium', 'long']);

function formatDate(value, fmt, locale) {
  if (!DATE_STYLES.has(fmt.style)) {
    throw new TypeError(`date.style must be short/medium/long/full, got ${fmt.style}`);
  }
  const d = toDate(value, 'date');
  return new Intl.DateTimeFormat(locale, { dateStyle: fmt.style }).format(d);
}

function formatTime(value, fmt, locale) {
  if (!TIME_STYLES.has(fmt.style)) {
    throw new TypeError(`time.style must be short/medium/long, got ${fmt.style}`);
  }
  const d = toDate(value, 'time');
  return new Intl.DateTimeFormat(locale, { timeStyle: fmt.style }).format(d);
}

function formatDateTime(value, fmt, locale) {
  if (!DATE_STYLES.has(fmt.style)) {
    throw new TypeError(
      `datetime.style must be short/medium/long/full, got ${fmt.style}`
    );
  }
  const d = toDate(value, 'datetime');
  return new Intl.DateTimeFormat(locale, {
    dateStyle: fmt.style,
    timeStyle: fmt.style === 'full' ? 'long' : fmt.style,
  }).format(d);
}

function formatRelative(value, locale) {
  // Relative format: różnica między teraz a wartością (ms timestamp).
  // Wybieramy największą sensowną jednostkę — sekundy/minuty/godziny/dni.
  const d = toDate(value, 'relative');
  const diffMs = d.getTime() - Date.now();
  const absMs = Math.abs(diffMs);
  const sec = Math.round(diffMs / 1000);
  const min = Math.round(diffMs / 60_000);
  const hr = Math.round(diffMs / 3_600_000);
  const day = Math.round(diffMs / 86_400_000);
  const month = Math.round(diffMs / (30 * 86_400_000));
  const year = Math.round(diffMs / (365 * 86_400_000));
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
  if (absMs < 60_000) return rtf.format(sec, 'second');
  if (absMs < 3_600_000) return rtf.format(min, 'minute');
  if (absMs < 86_400_000) return rtf.format(hr, 'hour');
  if (absMs < 30 * 86_400_000) return rtf.format(day, 'day');
  if (absMs < 365 * 86_400_000) return rtf.format(month, 'month');
  return rtf.format(year, 'year');
}

// =============================================================================
// Eksporty pomocnicze
// =============================================================================

export {
  BIND_SPEC_KINDS,
  VALUE_FORMAT_KINDS,
  isPrefixOf, // re-eksport dla rendererów
};
