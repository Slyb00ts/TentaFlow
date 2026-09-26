// ===== File: modules/tentabus/format.js — TentaBus wording and number formatting shared by the screen shell and its tabs =====
//
// Pure functions only (no DOM, no transport), so every formatter is unit
// tested against the real locale files. Counts are exact with grouping
// ("18 420"), byte sizes keep one decimal from a gigabyte up ("125,5 GB"),
// and every plural goes through the i18n `{n|…}` selector, never string
// concatenation.

import { I18n } from '/js/i18n.js';
import { fmtExact } from '/js/utils.js';

export const T = (key, params) => I18n.t(`tentabus.${key}`, params);

/** Exact count with the locale's grouping; `—` for a missing value. */
export function fmtCount(n) {
  if (n == null) return '—';
  return fmtExact(n, I18n.getLanguage());
}

const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];

/** Binary byte size in the reader's locale: "331 MB", "125,5 GB", "0 B". */
export function fmtBytes(bytes) {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value < 0) return '—';
  let v = value;
  let i = 0;
  while (v >= 1024 && i < BYTE_UNITS.length - 1) {
    v /= 1024;
    i += 1;
  }
  const digits = i >= 3 ? 1 : 0;
  const text = new Intl.NumberFormat(I18n.getLanguage(), {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
    useGrouping: 'always',
  }).format(v);
  return `${text} ${BYTE_UNITS[i]}`;
}

/**
 * Elapsed time as the header and the stale line say it: "3 s", "2 min",
 * "1 godz.". Rounds down, so "sprzed 2 min" never claims fresher data than
 * the reader has.
 */
export function fmtElapsed(ms) {
  const secs = Math.max(0, Math.floor(Number(ms) / 1000) || 0);
  if (secs < 60) return T('fmt.seconds', { count: fmtCount(secs), n: secs });
  const mins = Math.floor(secs / 60);
  if (mins < 60) return T('fmt.minutes', { count: fmtCount(mins), n: mins });
  const hours = Math.floor(mins / 60);
  return T('fmt.hours', { count: fmtCount(hours), n: hours });
}

/**
 * How long something has been going on, for "rośnie od 25 min": minutes up
 * to an hour, then hours and minutes. Never below one minute — the history
 * behind it is sampled once a minute.
 */
export function fmtSince(sinceMs, nowMs) {
  const mins = Math.max(1, Math.floor((Number(nowMs) - Number(sinceMs)) / 60_000) || 0);
  if (mins < 60) return T('fmt.minutes', { count: fmtCount(mins), n: mins });
  const hours = Math.floor(mins / 60);
  const rest = mins % 60;
  if (!rest) return T('fmt.hours', { count: fmtCount(hours), n: hours });
  return T('fmt.hours_minutes', { hours: fmtCount(hours), minutes: fmtCount(rest) });
}

/**
 * How long a topic keeps its messages: whole days as "30 dni", anything
 * shorter or uneven in hours. `—` for no value.
 */
export function fmtRetention(ms) {
  const value = Number(ms);
  if (!Number.isFinite(value) || value <= 0) return '—';
  const day = 86_400_000;
  if (value % day === 0) {
    const days = value / day;
    return T('fmt.days', { count: fmtCount(days), n: days });
  }
  const hours = Math.max(1, Math.round(value / 3_600_000));
  return T('fmt.hours', { count: fmtCount(hours), n: hours });
}

/**
 * When a message was written, the way the preview says it: "dziś 14:09:59",
 * "wczoraj 22:03:11", otherwise the date and the time.
 */
export function fmtWhen(ms, nowMs = Date.now()) {
  const at = new Date(Number(ms));
  if (!Number.isFinite(at.getTime())) return '—';
  const lang = I18n.getLanguage();
  const time = new Intl.DateTimeFormat(lang, { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false }).format(at);
  const startOf = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOf(new Date(nowMs)) - startOf(at)) / 86_400_000);
  if (days === 0) return T('fmt.today', { time });
  if (days === 1) return T('fmt.yesterday', { time });
  const date = new Intl.DateTimeFormat(lang, { day: '2-digit', month: '2-digit', year: 'numeric' }).format(at);
  return `${date} ${time}`;
}

/** Seconds a replica trails its leader, rounded UP: "do 4 s" is a bound. */
export function fmtLagSeconds(ms) {
  const secs = Math.max(1, Math.ceil(Number(ms) / 1000) || 0);
  return T('fmt.seconds', { count: fmtCount(secs), n: secs });
}

const CONTENT_TYPE_KEYS = {
  'application/json': 'json',
  'application/xml': 'xml',
  'text/xml': 'xml',
  'application/hl7-v2': 'hl7v2',
  'x-application/hl7-v2+er7': 'hl7v2',
  'application/octet-stream': 'binary',
};

/**
 * The payload kind a topic declares, in plain words ("HL7 v2", "JSON").
 * Empty for a topic that declares none (or one this screen has no name for):
 * the caller then leaves that part of the line out instead of guessing.
 */
export function contentTypeLabel(contentType) {
  const key = contentKind(contentType);
  return key ? T(`fmt.content.${key}`) : '';
}

/** The payload kind of a content type (`json` / `xml` / `hl7v2` / `binary`), or '' when unknown. */
export function contentKind(contentType) {
  return CONTENT_TYPE_KEYS[String(contentType || '').trim().toLowerCase()] || '';
}

/**
 * Classifies a failed load for the T12 states: `denied` (the matrix or the
 * org role refused), `unavailable` (the instance is not running on this
 * node), `timeout` (no answer in time), `lost` (anything else — the
 * connection itself). Classified on the protocol code first, never on
 * translated text.
 */
export function loadErrorKind(err) {
  const code = String(err?.code || '');
  const message = String(err?.message || err || '');
  if (code === 'PolicyDenied' || code === 'AuthRequired' || /\bbus\.permission_denied\b|PolicyDenied/.test(message)) return 'denied';
  if (code === 'AppUnavailable' || /AppUnavailable/.test(message)) return 'unavailable';
  if (/timed out/i.test(message)) return 'timeout';
  return 'lost';
}
