// ===== File: lib/date-format.js — the one place a calendar day is shown, typed and named in the UI language =====
//
// The wire and every stored value carry a day as `YYYY-MM-DD`. Screens never
// print that: they call `formatDay`, and a typed day comes back through
// `parseDay`. English uses the day-first British order so a day reads the same
// way in every locale the product ships (30.09.2026 pl/de, 30/09/2026 en/es/fr).

import { I18n } from '/js/i18n.js';

const ISO_DAY = /^(\d{4})-(\d{2})-(\d{2})$/;
const DAY_MS = 86_400_000;

/** True for a real calendar day in the `YYYY-MM-DD` form. */
export function isIsoDay(value) {
  const m = ISO_DAY.exec(String(value ?? ''));
  if (!m) return false;
  const date = new Date(Date.UTC(+m[1], +m[2] - 1, +m[3]));
  return date.getUTCFullYear() === +m[1] && date.getUTCMonth() === +m[2] - 1 && date.getUTCDate() === +m[3];
}

const utcOf = (iso) => new Date(`${iso}T00:00:00Z`);

/** `iso` moved by `count` days, still as `YYYY-MM-DD`. */
export function addDays(iso, count) {
  return new Date(utcOf(iso).getTime() + count * DAY_MS).toISOString().slice(0, 10);
}

/** Whole days from `fromIso` to `toIso` (negative when `toIso` is earlier). */
export function daysBetween(fromIso, toIso) {
  return Math.round((utcOf(toIso) - utcOf(fromIso)) / DAY_MS);
}

/** The BCP 47 tag the UI language formats with. */
export function uiLocale() {
  const language = I18n.getLanguage();
  return language === 'en' ? 'en-GB' : language;
}

const formatters = new Map();
function formatter(locale, options) {
  const key = `${locale}|${JSON.stringify(options)}`;
  if (!formatters.has(key)) formatters.set(key, new Intl.DateTimeFormat(locale, { ...options, timeZone: 'UTC' }));
  return formatters.get(key);
}

/**
 * A day in the UI language's own order: `30.09.2026`; with `short` the year is
 * left out (`30.09`) for places where width is tight. Text that is not a day
 * comes back as it is.
 */
export function formatDay(iso, { short = false } = {}) {
  if (!isIsoDay(iso)) return String(iso ?? '');
  const options = short ? { day: '2-digit', month: '2-digit' } : { day: '2-digit', month: '2-digit', year: 'numeric' };
  return formatter(uiLocale(), options).format(utcOf(iso));
}

// The order of day, month and year in the locale's numeric date, read off a formatted sample.
function fieldOrder(locale) {
  return formatter(locale, { day: '2-digit', month: '2-digit', year: 'numeric' })
    .formatToParts(Date.UTC(2031, 10, 23))
    .filter((p) => ['day', 'month', 'year'].includes(p.type))
    .map((p) => p.type);
}

/**
 * The `YYYY-MM-DD` of a typed day: the locale's order (`30.09.2026`) with `.`, `/`,
 * `-` or blanks between the parts, or the ISO form itself. Null when it is not a real day.
 */
export function parseDay(text) {
  const raw = String(text ?? '').trim();
  if (isIsoDay(raw)) return raw;
  const parts = raw.split(/[./\-\s]+/).filter(Boolean);
  if (parts.length !== 3 || !parts.every((p) => /^\d+$/.test(p))) return null;
  const order = fieldOrder(uiLocale());
  const byField = Object.fromEntries(order.map((field, i) => [field, parts[i]]));
  if (byField.year.length !== 4) return null;
  const iso = `${byField.year}-${byField.month.padStart(2, '0')}-${byField.day.padStart(2, '0')}`;
  return isIsoDay(iso) ? iso : null;
}

/** How a day is typed in the UI language (`DD.MM.RRRR`), for the sentences that ask for one. */
export function dateFormatHint() {
  return I18n.t('date_field.placeholder');
}

/** 0 = Sunday … 6 = Saturday: the first day of the week in the UI language. */
export function firstWeekday() {
  const locale = new Intl.Locale(uiLocale());
  const info = typeof locale.getWeekInfo === 'function' ? locale.getWeekInfo() : locale.weekInfo;
  return info ? info.firstDay % 7 : 1;
}

/** The seven short weekday names, starting with the locale's first day of the week. */
export function weekdayLabels() {
  const first = firstWeekday();
  // 2031-11-23 is a Sunday.
  return Array.from({ length: 7 }, (_, i) => formatter(uiLocale(), { weekday: 'short' }).format(Date.UTC(2031, 10, 23 + ((first + i) % 7))));
}

/** `September 2026` in the UI language (`month` is 0-based). */
export function monthTitle(year, month) {
  return formatter(uiLocale(), { month: 'long', year: 'numeric' }).format(Date.UTC(year, month, 1));
}
