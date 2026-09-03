// ===== File: modules/tentanas/format.js — labels, numbers and timestamps shared by the TentaNas screen shell and its pools/datasets/snapshots/tasks modules =====
//
// Everything the sub-modules render goes through the same formatters so a
// capacity, a timestamp or a health chip looks identical on the fleet grid,
// the pool cards and the snapshot table. The i18n namespace is fixed here
// (`tentanas.*`) so the modules never repeat the prefix.

import { I18n } from '/js/i18n.js';
import { escapeAttr } from '/js/utils.js';

export const T = (k, p) => I18n.t('tentanas.' + k, p);
export const sprite = (id) => `<svg class="icon"><use href="#i-${id}"/></svg>`;

export const POLL_DISKS_MS = 5000;
export const POLL_JOBS_MS = 3000;
export const POLL_OVERVIEW_MS = 5000;
export const POLL_POOLS_MS = 5000;
// Live chart windows (n02): throughput one minute, temperatures half an hour.
export const IO_WINDOW_SECS = 60;
export const TEMP_WINDOW_SECS = 1800;
export const POLL_FLEET_MS = 10000;
export const POLL_JOB_MODAL_MS = 1500;
export const ADMIN_TIMEOUT_MS = 120000;

export function parseServerTs(s) {
  if (!s) return null;
  const str = String(s);
  // Core timestamps are naive UTC "YYYY-MM-DD HH:MM:SS"; RFC3339 passes through.
  const iso = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}/.test(str) ? str.replace(' ', 'T') + 'Z' : str;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d;
}

export function fmtDate(s) {
  const d = parseServerTs(s);
  if (!d) return '—';
  return d.toLocaleString(I18n.getLanguage(), { dateStyle: 'short', timeStyle: 'short' });
}

export function fmtAgo(s) {
  const d = parseServerTs(s);
  if (!d) return '—';
  const secs = Math.max(0, Math.round((Date.now() - d.getTime()) / 1000));
  if (secs < 60) return T('ago_seconds', { n: secs });
  if (secs < 3600) return T('ago_minutes', { n: Math.round(secs / 60) });
  if (secs < 86400) return T('ago_hours', { n: Math.round(secs / 3600) });
  return T('ago_days', { n: Math.round(secs / 86400) });
}

// "in 3 h" style countdown for the next scheduled run; past or unknown
// timestamps read as a plain date so a stale scheduler is visible.
export function fmtIn(s) {
  const d = parseServerTs(s);
  if (!d) return '—';
  const secs = Math.round((d.getTime() - Date.now()) / 1000);
  if (secs < 0) return fmtDate(s);
  if (secs < 60) return T('in_seconds', { n: secs });
  if (secs < 3600) return T('in_minutes', { n: Math.round(secs / 60) });
  if (secs < 86400) return T('in_hours', { n: Math.round(secs / 3600) });
  return T('in_days', { n: Math.round(secs / 86400) });
}

export function fmtDuration(secs) {
  const s = Math.max(0, Math.round(Number(secs) || 0));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return T('duration.dh', { d, h });
  if (h > 0) return T('duration.hm', { h, m });
  return T('duration.m', { m });
}

// A live chart window reads in the unit the mockups label it with ("60 s",
// "30 min"), so sub-two-minute windows stay in seconds instead of collapsing
// to "1 min".
export function fmtWindow(secs) {
  const s = Math.max(0, Math.round(Number(secs) || 0));
  return s < 120 ? T('duration.s', { s }) : fmtDuration(s);
}

export function fmtBytes(n) {
  let v = Number(n) || 0;
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function fmtMBps(bps) {
  const v = (Number(bps) || 0) / 1048576;
  return v >= 100 ? String(Math.round(v)) : v.toFixed(1);
}

export function fmtRatio(r) {
  const v = Number(r);
  return Number.isFinite(v) && v > 0 ? `${v.toFixed(2)}×` : '—';
}

export function pct(used, total) {
  const t = Number(total) || 0;
  return t > 0 ? Math.min(100, Math.round((Number(used) || 0) / t * 100)) : 0;
}

export function healthClass(h) {
  return h === 'ok' ? 'ok' : h === 'warning' ? 'warn' : h === 'critical' ? 'err' : '';
}

export function healthChip(h) {
  const map = { ok: 'ok', warning: 'warn', critical: 'err', unknown: 'info' };
  return { status: map[h] || 'info', label: T('health.' + (h || 'unknown')), dot: true };
}

// zpool device/pool states: only 'online' is healthy, 'degraded' still
// serves data, everything else means data is at risk right now.
export function stateTone(state) {
  return state === 'online' ? 'ok' : state === 'degraded' ? 'warn' : 'err';
}

export function stateLabel(state) {
  const key = 'state.' + (state || 'unknown');
  const label = T(key);
  return label === 'tentanas.' + key ? String(state || '—') : label;
}

export function stateChipHtml(state) {
  return `<tf-chip status="${stateTone(state)}" dot label="${escapeAttr(stateLabel(state))}"></tf-chip>`;
}

// Layout names are wire spellings ('raidz2', 'mirror'); the labels are the
// admin-facing forms ("RAIDZ2", "Mirror"). An unknown spelling shows as-is.
export function layoutLabel(layout) {
  const key = 'layout.' + (layout || 'unknown');
  const label = T(key);
  return label === 'tentanas.' + key ? String(layout || '—') : label;
}

export function errMessage(e) {
  return (e && e.message) ? e.message : String(e);
}

export function jobTone(status) {
  return status === 'succeeded' || status === 'done' ? 'ok' : status === 'failed' || status === 'blocked' ? 'err' : status === 'cancelled' ? 'warn' : status === 'running' ? 'accent' : 'info';
}

// Job kinds are snake_case on the wire ("pool_scrub") and map 1:1 onto
// `jobs.kind_*` keys. A kind without a label shows its wire name so a new
// backend job is still readable in the list.
export function jobKindLabel(kind) {
  const key = 'jobs.kind_' + String(kind || '');
  const label = T(key);
  return label === 'tentanas.' + key ? String(kind || '—') : label;
}

export function timeHm(h, m) {
  return `${String(Number(h) || 0).padStart(2, '0')}:${String(Number(m) || 0).padStart(2, '0')}`;
}

// Human form of a NasSchedule for the sched-pill: "every 15 min",
// "daily 02:00", "Sun 02:00", "1st 01:30".
export function fmtSchedule(s) {
  if (!s || !s.every) return T('schedule.none');
  const every = s.every;
  if (every === '15m' || every === '30m' || every === '1h' || every === '6h') return T('schedule.every_' + every);
  const at = timeHm(s.hour, s.minute);
  if (every === 'daily') return T('schedule.daily_at', { at });
  if (every === 'weekly') return T('schedule.weekly_at', { day: T('weekday.' + (Number(s.weekday) || 0)), at });
  if (every === 'monthly') return T('schedule.monthly_at', { day: Number(s.day) || 1, at });
  return String(every);
}
