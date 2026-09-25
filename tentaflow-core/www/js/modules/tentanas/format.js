// ===== File: modules/tentanas/format.js — labels, numbers and timestamps shared by the TentaNas screen shell and its pools/datasets/snapshots/tasks modules =====
//
// Everything the sub-modules render goes through the same formatters so a
// capacity, a timestamp or a health chip looks identical on the fleet grid,
// the pool cards and the snapshot table. The i18n namespace is fixed here
// (`tentanas.*`) so the modules never repeat the prefix.

import { I18n } from '/js/i18n.js';
import { escapeAttr } from '/js/utils.js';
import { isOpaqueId, isDiskIdShape, scrubIds } from '/js/modules/tentanas/machine-id.js';

export const T = (k, p) => I18n.t('tentanas.' + k, p);

// The wire spells "no channel configured" as `unset` (`elevation::Mode::as_str`,
// and `fleet.rs` for the per-node mode); this UI has always spelled it
// `unarmed`. Every comparison tested only the latter, so a freshly installed
// node read as a WORKING channel: no password was ever asked for, the header
// badge said ok, and `elevation.short_unset` rendered as a raw key. One
// spelling from here on — the i18n keys keep theirs.
export const channelMode = (mode) => (!mode || mode === 'unset' ? 'unarmed' : mode);

// The channel a node REALLY has right now, from its mode and — for mode B —
// until when its password is held (`NasNodeInfo::armed_until`,
// `NasElevation::armed_until`). The mode alone called every mode-B node a
// working channel, so a node whose password had expired read green and its
// "Uzbrój…" was never offered (n16: "tryb B — nieuzbrojony"). A missing or
// past instant is "not armed"; an older node that does not send the field
// therefore reads as not armed, which asks for a password rather than
// promising one is held. Answers 'helper' | 'interactive' |
// 'interactive_unarmed' | 'unarmed'.
export function liveChannelMode(mode, armedUntil, now = Date.now()) {
  const m = channelMode(mode);
  if (m !== 'interactive') return m;
  const until = Date.parse(String(armedUntil || ''));
  return Number.isFinite(until) && until > now ? 'interactive' : 'interactive_unarmed';
}
export const nodeChannelMode = (n, now = Date.now()) => liveChannelMode(n?.elevationMode, n?.armedUntil, now);
// Both "no channel" and "mode B with no password held" leave the node unable
// to run a privileged step without someone typing a password.
export const channelIsUnarmed = (m) => m === 'unarmed' || m === 'interactive_unarmed';
export const sprite = (id) => `<svg class="icon"><use href="#i-${id}"/></svg>`;

// ----- Names, never identifiers ---------------------------------------------
//
// The owner's rule: the screen names a user by display name, a node by
// hostname and a disk by its kernel name, and a machine id (UUID, 64-hex node
// id, `wwn-…`) is never visible text — a tooltip at most. Every surface that
// names a node or a job's author goes through these, so the fallback reads
// the same wherever the name is missing.

// A node's hostname, or "Węzeł bez nazwy". The node sends an EMPTY name when
// neither the peer store nor the sync registry knows one (`fleet.rs`
// `node_name`); it used to send its 64-hex id, and every fleet surface printed
// that as the name. The id stays available as `node.nodeId` for a tooltip.
export const nodeLabel = (node) => String(node?.nodeName || '').trim() || T('node.unnamed');

// The node view's header subline is `nodeT` from node-phrase.js — see that
// file for why a nameless node needs a whole sentence instead of the
// template with `nodeLabel`'s fallback dropped in.

// The two authors a job can have that are not a person: the node restoring
// its arrays at boot, and the scheduler. They reach the wire as the raw
// tokens `startup` / `scheduler` (`scheduler.rs` `STARTED_BY`).
const SYSTEM_AUTHORS = new Set(['startup', 'scheduler']);

// Who started a job, as `{ label }`. The node resolves a user id to the
// account's display name (`dispatch::tentanas::display_names`) and passes an
// id with no account behind it through unchanged — a deleted account, or one
// that exists only on the node that forwarded the request. That UUID is not a
// name: the label says the account is unknown here, and the id is not shown
// at all, not even as a tooltip (owner's rule: no ids anywhere in the GUI).
// An author is always a user id or a system token, never a name that could
// collide with a disk-id prefix, so the opaque rule alone applies.
export function jobAuthor(startedBy) {
  const by = String(startedBy || '').trim();
  if (SYSTEM_AUTHORS.has(by)) return { label: T('jobs.by_' + by) };
  if (isOpaqueId(by)) return { label: T('jobs.by_unknown_account') };
  return { label: by || '—' };
}

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

// A wait the node cut to one unit (elastic.rs `coarse_wait_secs`: whole days
// from two days up, whole hours from one hour up, whole minutes below),
// worded in THAT unit only. `fmtDuration` would add a second unit the node
// threw away: 2 d 23 h arrives as 2 days and read "2 d 0 h".
export function fmtCoarseWait(secs) {
  const s = Math.max(0, Math.round(Number(secs) || 0));
  if (s >= 2 * 86400) return T('duration.d', { d: Math.floor(s / 86400) });
  if (s >= 3600) return T('duration.h', { h: Math.floor(s / 3600) });
  return T('duration.m', { m: Math.floor(s / 60) });
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

export function fmtOptionalBytes(n) {
  return n == null || !Number.isFinite(Number(n)) || Number(n) < 0 ? '—' : fmtBytes(n);
}

// THE one rule for a job's Cancel button, used by every job renderer (the
// shared running-job row in tasks.js, which n02 and n15 both draw).
//
// Cancel is offered only where cancelling really stops the work — an
// ALLOWLIST, so a new job kind starts without a button instead of with one
// that lies. Cancelling a job drops its future (`jobs.rs`); what that leaves
// running decides:
// - `pool_scrub` — a guard issues `zpool scrub -s` on drop (`pools.rs`
//   StopScrubOnCancel): the scrub really stops.
// - every `elastic_*` run carries an intent and the server refuses to cancel
//   it at all, so the click could only ever error (A2).
// - `smart_test` — the drive keeps testing; there is no abort command, and
//   the job would read "cancelled" while the test runs on (A3).
// - `disk_wipe` — the helper is already erasing; the server refuses it.
// - every other privileged job (TRIM, pool create/replace/add vdev, dataset
//   destroy, share/target apply, config import, packages, helper
//   provisioning) runs its command through the root helper, which the drop
//   does not stop: cancel would only stop TRACKING the work and still write
//   "cancelled" (A5). That is not an action worth a button.
const CANCELLABLE_KINDS = new Set(['pool_scrub']);

export function jobCanCancel(job) {
  return CANCELLABLE_KINDS.has(job?.kind);
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

// The tail a pool-level detail writes into the shell's one breadcrumb
// ("TentaNas › helios › Pule › tank", n06; "… › Pule › media", n11): the
// "Pule" level walks back through the shell's `pools` crumb action. Shared by
// the ZFS pool detail and the Elastic Array detail, which name the same place.
export function poolCrumbTail(nodeId, name) {
  return [
    { label: T('tabs.pools'), act: 'pools', query: `node=${nodeId}&tab=pools` },
    { label: name },
  ];
}

// ----- Health reasons, in the reader's language ----------------------------
//
// ONE place words every health reason the node sends: the n03 row chip, the
// n04 chip, "why" box and advice, the n02 disk tile, the n03 advice card, the
// pool wizard's disk pickers, the n06 member chips and the n05 pool card.
//
// The node sends a reason as a CODE with parameters (`NasHealthReason`,
// tentaflow-protocol/src/tentanas.rs, which lists every code): disks in
// `healthReasons`, a replacement advice in `reasons`, a pool in
// `healthReasons`. The sentence is composed here from i18n; the node's
// English (`healthReason`, `advice.reason`) is never parsed and never shown
// as text — only as a tooltip. A code this build does not know is left out
// of the text, and when nothing is left the text is the translated grade.
//
// Every numeric parameter must read as a finite number, or the code counts as
// unknown: "NaN realok." is no better than English.

function numParams(params, keys) {
  const out = {};
  for (const key of keys) {
    const raw = params?.[key];
    const n = raw == null || raw === '' ? NaN : Number(raw);
    if (!Number.isFinite(n)) return null;
    out[key] = n;
  }
  return out;
}

// The grades a replacement advice can be "for N days" in.
const UNHEALTHY = new Set(['warning', 'critical']);

// code -> [numeric parameter names, words(numbers, params)]. A Map, not an
// object literal, so a code like `constructor` is unknown and not a
// prototype method.
const DISK_REASON_WORDS = new Map([
  ['smart_failed', [[], () => T('disks.reason_smart_failed')]],
  ['self_test_failed', [[], () => T('disks.reason_self_test_failed')]],
  ['pending_sectors', [['count'], (n) => T('disks.reason_pending', { n: n.count })]],
  ['media_errors', [['count'], (n) => T('disks.reason_media', { n: n.count })]],
  ['reallocated_growing', [['from', 'to'], (n) => T('disks.reason_realloc_growing', { from: n.from, to: n.to })]],
  ['reallocated', [['count'], (n) => T('disks.reason_realloc', { n: n.count })]],
  ['temperature_over_limit', [['celsius', 'limit'], (n) => T('disks.reason_temp_over', { t: n.celsius, limit: n.limit })]],
  ['temperature_high', [['celsius'], (n) => T('disks.reason_temp', { t: n.celsius })]],
  ['crc_errors', [['count'], (n) => T('disks.reason_crc', { n: n.count })]],
  ['wear', [['pct'], (n) => T('disks.reason_wear', { n: n.pct })]],
  ['no_smart_data', [[], () => T('disks.reason_no_smart')]],
  ['zfs_faulted', [[], () => T('disks.reason_zfs_faulted')]],
  ['zfs_unavail', [[], () => T('disks.reason_zfs_unavail')]],
  // The two codes only a replacement advice carries, ahead of the disk's own.
  ['reallocated_grew', [['from', 'to'], (n) => T('replace_advice.reason_grew', { from: n.from, to: n.to })]],
  ['unhealthy_for_days', [['days'], (n, p) => (UNHEALTHY.has(p.health)
    ? T('replace_advice.reason_for_days', { status: T('health.' + p.health), days: n.days })
    : null)]],
]);

// A pool state inside a sentence is the same word the state chip shows; one
// this build has no word for makes the code unknown instead of printing the
// wire spelling.
function poolStateWord(state) {
  const key = 'state.' + String(state || '');
  const label = T(key);
  return label === 'tentanas.' + key ? null : label;
}

const POOL_REASON_WORDS = new Map([
  ['pool_state', [[], (n, p) => {
    const state = poolStateWord(p.state);
    return state ? T('pools.reason_pool_state', { state }) : null;
  }]],
  ['permanent_data_errors', [['count'], (n) => T('pools.reason_permanent_data_errors', { count: n.count })]],
  // `detail` is zpool's own English line: the tooltip has it, the text not.
  ['data_errors_reported', [[], () => T('pools.reason_data_errors_reported')]],
  ['unusable_disks', [['count'], (n) => T('pools.reason_unusable_disks', { count: n.count })]],
  ['degraded_disks', [['count'], (n) => T('pools.reason_degraded_disks', { count: n.count })]],
  ['disks_with_errors', [['count'], (n) => T('pools.reason_disks_with_errors', { count: n.count })]],
  ['scrub_found_errors', [['count'], (n) => T('pools.reason_scrub_found_errors', { count: n.count })]],
  ['resilver_found_errors', [['count'], (n) => T('pools.reason_resilver_found_errors', { count: n.count })]],
  ['scan_found_errors', [['count'], (n) => T('pools.reason_scan_found_errors', { count: n.count })]],
  ['capacity', [['pct'], (n) => T('pools.reason_capacity', { pct: n.pct })]],
]);

function wordOf(table, reason) {
  const entry = table.get(String(reason?.code || ''));
  if (!entry) return null;
  const [keys, words] = entry;
  const params = reason.params || {};
  const numbers = numParams(params, keys);
  return numbers ? words(numbers, params) || null : null;
}

// The words of every known reason, in the node's order (worst first).
function wordsOf(table, reasons) {
  return (Array.isArray(reasons) ? reasons : []).map((r) => wordOf(table, r)).filter(Boolean);
}

// The localized words for ONE disk reason code, or null for a code this
// build does not know.
export function diskReasonWord(reason) {
  return wordOf(DISK_REASON_WORDS, reason);
}

// The short cause a problem chip carries (n03 row, n04 header, n06 member):
// the FIRST reason, which is the one the disk is in that state for. null
// when the first reason is not one this build can word — the chip then
// shows the grade alone rather than skip to a lesser reason.
export function firstDiskReasonWord(disk) {
  const first = Array.isArray(disk?.healthReasons) ? disk.healthReasons[0] : null;
  return first ? diskReasonWord(first) : null;
}

// A disk's whole reason in the reader's language, for the places that show
// every symptom (the n04 "why" box, the n02 disk-health tile, the pool
// wizard): `{ text, title }`. `title` is the node's sentence for the
// tooltip. Both are '' when the node gave no reason at all; with a reason
// but no word this build knows, the text is the translated grade.
export function diskReasonsText(disk) {
  const title = String(disk?.healthReason || '').trim();
  const reasons = Array.isArray(disk?.healthReasons) ? disk.healthReasons : [];
  if (!reasons.length && !title) return { text: '', title: '' };
  const words = wordsOf(DISK_REASON_WORDS, reasons);
  return { text: words.length ? words.join('; ') : T('health.' + (disk?.health || 'unknown')), title };
}

// The chip label "Uwaga: realok. 0 → 3" — or the grade alone when the first
// reason has no word here. `title` is the node's sentence, or null.
export function diskHealthChipLabel(disk) {
  const health = healthChip(disk?.health);
  const word = firstDiskReasonWord(disk);
  return {
    label: word ? T('disk.health_chip', { status: health.label, reason: word }) : health.label,
    title: String(disk?.healthReason || '').trim() || null,
  };
}

// The replacement advice (§5.10) in the reader's language, from the codes
// in `advice.reasons` (`replacement_advice`, tentanas/disks.rs): the growth,
// the days unhealthy, then the disk's own reasons. An advice kind other than
// `urgent` / `advice`, or one whose codes this build cannot word (an older
// node sends none), is `known: false` with the generic recommendation.
// `title` is always the node's sentence.
export const ADVICE_KINDS = new Set(['urgent', 'advice']);

// The advice is worded in whole phrases (`DISK_REASON_SENTENCES`), not in
// the chip abbreviations: it is a sentence ("…: {reason}. {spare}."), and an
// abbreviation ending in a period made it read "3 oczek. sekt.. Wymiana…".
// `known: false` carries no text of its own — WHO the generic words are for
// (a pool disk, an Elastic Array disk) is the caller's to say.
export function replacementAdviceText(advice) {
  const title = String(advice?.reason || '').trim();
  const words = ADVICE_KINDS.has(advice?.severity) ? wordsOf(DISK_REASON_SENTENCES, advice.reasons) : [];
  if (!words.length) return { known: false, text: '', title };
  return { known: true, text: words.join('; '), title };
}

// The same reasons as whole phrases, for the places a reason is part of a
// sentence rather than a chip: the n01/n02 disk alert ("sdd: 3 nowe
// realokowane sektory w 7 dni", mockups n01:298, n02:315) and the
// replacement advice. Same codes, same parameter rules as the chip words.
const DISK_REASON_SENTENCES = new Map([
  ['smart_failed', [[], () => T('alerts.disk_reason.smart_failed')]],
  ['self_test_failed', [[], () => T('alerts.disk_reason.self_test_failed')]],
  ['pending_sectors', [['count'], (n) => T('alerts.disk_reason.pending_sectors', { n: n.count })]],
  ['media_errors', [['count'], (n) => T('alerts.disk_reason.media_errors', { n: n.count })]],
  // The growth is what the mockup says: how many sectors are NEW this week.
  ['reallocated_growing', [['from', 'to'], (n) => (n.to > n.from
    ? T('alerts.disk_reason.reallocated_growing', { n: n.to - n.from })
    : null)]],
  ['reallocated', [['count'], (n) => T('alerts.disk_reason.reallocated', { n: n.count })]],
  ['temperature_over_limit', [['celsius', 'limit'], (n) => T('alerts.disk_reason.temperature_over_limit', { t: n.celsius, limit: n.limit })]],
  // `limit` came later than the code (a row stored before it has none).
  ['temperature_high', [['celsius'], (n, p) => {
    const limit = numParams(p, ['limit']);
    return limit
      ? T('alerts.disk_reason.temperature_high_limit', { t: n.celsius, limit: limit.limit })
      : T('alerts.disk_reason.temperature_high', { t: n.celsius });
  }]],
  ['crc_errors', [['count'], (n) => T('alerts.disk_reason.crc_errors', { n: n.count })]],
  ['wear', [['pct'], (n) => T('alerts.disk_reason.wear', { n: n.pct })]],
  ['no_smart_data', [[], () => T('alerts.disk_reason.no_smart_data')]],
  ['zfs_faulted', [[], () => T('alerts.disk_reason.zfs_faulted')]],
  ['zfs_unavail', [[], () => T('alerts.disk_reason.zfs_unavail')]],
  ['reallocated_grew', [['from', 'to'], (n) => T('replace_advice.reason_grew', { from: n.from, to: n.to })]],
  ['unhealthy_for_days', [['days'], (n, p) => (UNHEALTHY.has(p.health)
    ? T('replace_advice.reason_for_days', { status: T('health.' + p.health), days: n.days })
    : null)]],
]);

// The codes the sentence dictionary words — for the test that holds it to
// the chip dictionary, code for code.
export const DISK_SENTENCE_CODES = [...DISK_REASON_SENTENCES.keys()];
export const DISK_WORD_CODES = [...DISK_REASON_WORDS.keys()];

// A pool's reason for the n05 card, `{ text, title }` as `diskReasonsText`.
export function poolReasonsText(pool) {
  const title = String(pool?.healthReason || '').trim();
  const reasons = Array.isArray(pool?.healthReasons) ? pool.healthReasons : [];
  if (!reasons.length && !title) return { text: '', title: '' };
  const words = wordsOf(POOL_REASON_WORDS, reasons);
  return { text: words.length ? words.join('; ') : T('health.' + (pool?.health || 'unknown')), title };
}

// ----- Alerts, in the reader's language --------------------------------------
//
// ONE place words every alert the node raises: the n01 fleet alert table and
// the n02 alert list. The node sends an alert as a CODE with string
// parameters and coded detail lines (`NasAlert.code` / `params` / `reasons`,
// tentaflow-protocol/src/tentanas.rs, which lists every code); its English
// `title` / `detail` are the tooltip only.
//
// The fallback is truthful rather than clever: an alert whose code this build
// does not know — or one raised by an older node, which sends no code at
// all — or whose parameters do not read as the code needs, is shown as a
// translated generic alert that points at the node's own text — the tooltip
// and the row's "Treść węzła" section. Nothing here parses the English.

// A non-empty string parameter, or null.
function textParam(params, key) {
  const v = params?.[key];
  return typeof v === 'string' && v.trim() ? v : null;
}

// Every named string parameter, or null when one is missing.
function textParams(params, keys) {
  const out = {};
  for (const key of keys) {
    const v = textParam(params, key);
    if (v == null) return null;
    out[key] = v;
  }
  return out;
}

// An approval's operation as the Tasks queue names it (`approvals.op_*`); an
// operation this build does not know reads as the generic "Operacja", the
// same as the queue shows it.
function approvalOperationLabel(op) {
  const key = 'approvals.op_' + String(op || '');
  const label = T(key);
  return label === 'tentanas.' + key ? T('approvals.op_unknown') : label;
}

// Words for the coded lines of a detail that carries them (a conflict's
// files); an unknown line code is left out, like a disk reason. WHERE the
// other version is arrives as a place (`kept_kind` + `kept_disk`), never as
// the helper's path: a quarantined original is named after its operation's
// uuid (wave-4 critic M2).
const CONFLICT_PLACES = new Set(['quarantine', 'data', 'branch']);
const ALERT_LINE_WORDS = new Map([
  ['conflict_file', (p) => {
    const t = textParams(p, ['path', 'visible']);
    const kind = p?.kept_kind;
    if (!t || !CONFLICT_PLACES.has(kind)) return null;
    const disk = textParam(p, 'kept_disk');
    if (kind !== 'branch' && !disk) return null;
    return T('alerts.code.elastic_conflict.file_' + kind, { ...t, disk });
  }],
]);

function alertLines(reasons) {
  return (Array.isArray(reasons) ? reasons : [])
    .map((r) => ALERT_LINE_WORDS.get(String(r?.code || ''))?.(r.params || {}) || null)
    .filter(Boolean);
}

// The other version of each conflicted file, for the row's deliberate "copy
// path" control: `{ path, kept }` — `path` is the file's path in the array
// (what the admin knows it by, the button's label), `kept` the node's path of
// the other version (`kept_path`, elastic.rs `conflict_alert`). A quarantined
// copy is named after its operation's uuid, so `kept` is never rendered: it
// only goes to the clipboard when the admin asks for it. A line without it
// (another array's place, an older node) offers nothing.
function conflictCopies(reasons) {
  return (Array.isArray(reasons) ? reasons : [])
    .filter((r) => r?.code === 'conflict_file')
    .map((r) => ({ path: textParam(r.params, 'path'), kept: textParam(r.params, 'kept_path') }))
    .filter((c) => c.path && c.kept);
}

// The words a detail falls back to when a known code has nothing to word in
// it but the node did write a sentence (a backfilled disk alert has its grade
// and name, no reasons): say where the text is, never print it. The place it
// names is the row's "Treść węzła" section, which every device can open (a
// phone has no tooltip); `nodeText` marks the result so the row offers it.
function detailOrNodeTextHint(words, alert) {
  if (words) return { detail: words, nodeText: false };
  return String(alert?.detail || '').trim()
    ? { detail: T('alerts.detail_in_node_text'), nodeText: true }
    : { detail: '', nodeText: false };
}

const CACHE_STUCK_CAUSES = new Set(['unresolved_operation', 'files_busy', 'schedule_window', 'last_run_moved_nothing', 'not_started']);

// NODE FREE TEXT IS NEVER THE SENTENCE (wave-4 critic M3). A helper's journal
// note, a failed step's error and the kernel's refusal are the node's own
// words — Polish from the helper, English from configfs — and a French reader
// must not get them as the detail line. The detail is a translated sentence
// that says there is more; the raw text is returned as `raw` and goes to the
// tooltip (and the row's touch-reachable node text), never to the line.

// The "unconfirmed step" family: same shape, `{array, error}`.
function unconfirmed(code) {
  return (p) => {
    const t = textParams(p, ['array', 'error']);
    return t
      ? { title: T(`alerts.code.${code}.title`, { array: t.array }), detail: T(`alerts.code.${code}.detail`), raw: t.error }
      : null;
  };
}

// A target's kernel failure: the error is optional — the node leaves it out
// when it names another organisation's target — and the detail says which.
function targetFailure(code) {
  return (p) => {
    const target = textParam(p, 'target');
    if (!target) return null;
    const error = textParam(p, 'error');
    return {
      title: T(`alerts.code.${code}.title`, { target }),
      detail: error ? T(`alerts.code.${code}.detail`) : T(`alerts.code.${code}.detail_hidden`),
      raw: error,
    };
  };
}

// A disk alert reads the way the mockups write it (n01:298, n02:315): the
// disk's name and its FIRST reason as a sentence — "sdd: 3 nowe realokowane
// sektory w 7 dni" — with the other reasons as the detail. The grade is the
// row's severity chip, not a word in the title: "Uwaga" is the disk-health
// vocabulary of n03/n04. With no reason this build can word, the title falls
// back to the grade.
function diskAlertTitle(p, first) {
  const name = textParam(p, 'name');
  const grade = T('health.' + p.health);
  if (p.name_source === 'live' && name) {
    return first ? T('alerts.code.disk_health.title_live_reason', { name, reason: first }) : T('alerts.code.disk_health.title_live', { name, grade });
  }
  if (p.name_source === 'last_known' && name) {
    return first ? T('alerts.code.disk_health.title_last_known_reason', { name, reason: first }) : T('alerts.code.disk_health.title_last_known', { name, grade });
  }
  if (p.name_source === 'unknown') {
    return first ? T('alerts.code.disk_health.title_unnamed_reason', { reason: first }) : T('alerts.code.disk_health.title_unnamed', { grade });
  }
  return null;
}

// What the node put on a disk alert beyond its grade (disks.rs
// `HealthAlertPlace`): the ZFS pool the disk serves with its group's layout —
// n02's sub-line "tank · RAIDZ2" — and whether the node advises replacing
// it — n01's "— zaplanuj wymianę dysku". Neither is ever inferred here: a
// row raised by an older build, or a disk in no pool, simply has none.
function diskAlertPlace(p) {
  const pool = textParam(p, 'pool');
  const layout = textParam(p, 'layout');
  return {
    place: pool ? [pool, layout ? layoutLabel(layout) : null].filter(Boolean).join(' · ') : '',
    advice: p?.advice === 'replace' ? T('alerts.code.disk_health.advice_replace') : '',
  };
}

// code -> (params, reasons, alert) => { title, detail, place?, advice? } |
// null. A Map, so a prototype name is not a code.
const ALERT_WORDS = new Map([
  ['disk_health', (p, reasons, alert) => {
    if (!UNHEALTHY.has(p.health)) return null;
    const [first, ...rest] = wordsOf(DISK_REASON_SENTENCES, reasons);
    const title = diskAlertTitle(p, first);
    if (!title) return null;
    if (first) return { title, detail: rest.join('; '), ...diskAlertPlace(p) };
    return { title, ...detailOrNodeTextHint('', alert), ...diskAlertPlace(p) };
  }],
  // `subject` is optional: a config import from an export the fleet has no
  // node name for arrives without one (dispatch `config_import_subject`),
  // and is then the operation alone — never the node's id.
  ['approval_pending', (p) => {
    const t = textParams(p, ['operation']);
    if (!t) return null;
    const operation = approvalOperationLabel(t.operation);
    const subject = textParam(p, 'subject');
    return {
      title: subject
        ? T('alerts.code.approval_pending.title', { operation, subject })
        : T('alerts.code.approval_pending.title_unnamed', { operation }),
      detail: T('alerts.code.approval_pending.detail'),
    };
  }],
  ['elastic_sync_held', (p) => {
    const t = textParams(p, ['array']);
    if (!t) return null;
    // `cause` 'parity_fault': a fault the helper records with no counts (a
    // scrub whose log broke, a failed repair). Absent on older rows.
    const detail = textParam(p, 'cause') === 'parity_fault' ? 'detail_fault' : 'detail';
    return { title: T('alerts.code.elastic_sync_held.title', t), detail: T('alerts.code.elastic_sync_held.' + detail) };
  }],
  ['elastic_mover_settle_stopped', (p) => {
    const t = textParams(p, ['array']);
    const n = numParams(p, ['runs']);
    return t && n
      ? { title: T('alerts.code.elastic_mover_settle_stopped.title', t), detail: T('alerts.code.elastic_mover_settle_stopped.detail', { runs: n.runs }) }
      : null;
  }],
  ['elastic_cache_stuck', (p) => {
    const t = textParams(p, ['array']);
    const n = numParams(p, ['oldest_secs', 'limit_secs']);
    if (!t || !n || !CACHE_STUCK_CAUSES.has(p.cause)) return null;
    return {
      title: T('alerts.code.elastic_cache_stuck.title', t),
      detail: T('alerts.code.elastic_cache_stuck.detail', {
        oldest: fmtCoarseWait(n.oldest_secs),
        limit: fmtCoarseWait(n.limit_secs),
        cause: T('alerts.code.elastic_cache_stuck.cause_' + p.cause),
      }),
    };
  }],
  ['elastic_conflict', (p, reasons) => {
    const t = textParams(p, ['array']);
    const n = numParams(p, ['count']);
    if (!t || !n) return null;
    const files = alertLines(reasons);
    const lead = T('alerts.code.elastic_conflict.detail');
    return {
      title: T('alerts.code.elastic_conflict.title', { count: n.count, array: t.array }),
      detail: files.length ? `${lead} ${files.join('; ')}` : lead,
      copies: conflictCopies(reasons),
    };
  }],
  ['elastic_needs_attention', (p) => {
    const t = textParams(p, ['array']);
    if (!t) return null;
    // The helper's own text, when it gave one, is its words, not ours: the
    // tooltip, with the sentence saying it is there.
    const helper = textParam(p, 'helper_detail');
    return {
      title: T('alerts.code.elastic_needs_attention.title', t),
      detail: T(helper ? 'alerts.code.elastic_needs_attention.detail_helper' : 'alerts.code.elastic_needs_attention.detail'),
      raw: helper,
    };
  }],
  ['elastic_result_unconfirmed', unconfirmed('elastic_result_unconfirmed')],
  ['elastic_replace_unconfirmed', unconfirmed('elastic_replace_unconfirmed')],
  ['elastic_add_disk_unconfirmed', unconfirmed('elastic_add_disk_unconfirmed')],
  ['elastic_add_disk_abort_unconfirmed', unconfirmed('elastic_add_disk_abort_unconfirmed')],
  ['elastic_restore_waiting', (p) => {
    const t = textParams(p, ['array']);
    return t ? { title: T('alerts.code.elastic_restore_waiting.title', t), detail: T('alerts.code.elastic_restore_waiting.detail') } : null;
  }],
  // Raised by tentanas/targets.rs.
  ['target_portal_moved', (p) => {
    const t = textParams(p, ['target']);
    return t ? { title: T('alerts.code.target_portal_moved.title', t), detail: T('alerts.code.target_portal_moved.detail') } : null;
  }],
  ['target_not_applied', targetFailure('target_not_applied')],
  ['target_still_in_kernel', targetFailure('target_still_in_kernel')],
  ['elevation_unarmed', () => ({ title: T('alerts.code.elevation_unarmed.title'), detail: T('alerts.code.elevation_unarmed.detail') })],
  ['targets_sweep_failing', (p) => {
    const n = numParams(p, ['count', 'alerted']);
    if (!n || !['true', 'false'].includes(p.sweep_failed)) return null;
    const parts = [];
    if (n.count > 0) {
      // Who can read the names: said only as far as it holds (targets.rs
      // `node_sweep_detail`) — every organisation, some, or the log alone.
      const who = n.alerted >= n.count ? T('alerts.code.targets_sweep_failing.who_all')
        : n.alerted > 0 ? T('alerts.code.targets_sweep_failing.who_some', { alerted: n.alerted })
          : T('alerts.code.targets_sweep_failing.who_none');
      parts.push(`${T('alerts.code.targets_sweep_failing.detail_targets', { count: n.count })}; ${who}`);
    }
    if (p.sweep_failed === 'true') parts.push(T('alerts.code.targets_sweep_failing.detail_sweep'));
    return { title: T('alerts.code.targets_sweep_failing.title'), detail: parts.join('; ') };
  }],
  // The open sweep alert as a restart (targets.rs `rewrite_stale_sweep_alert`)
  // and migration 21 leave it: no count this process has measured.
  ['targets_sweep_stale', () => ({ title: T('alerts.code.targets_sweep_stale.title'), detail: T('alerts.code.targets_sweep_stale.detail') })],
]);

// The node's own sentence for the tooltip: title, then detail — and the raw
// text a composer kept off the line, when the node's detail does not already
// carry it. The node's text names what it likes (a transfer path's operation
// uuid, a by-id path, a node id), so every id in it is replaced — by the
// node's name when `nameOf` knows it, else by a neutral word — before it is a
// tooltip or the "Treść węzła" section (`scrubIds`, machine-id.js).
function alertTooltip(alert, raw, nameOf) {
  const title = String(alert?.title || '').trim();
  const detail = String(alert?.detail || '').trim();
  const base = title && detail ? `${title} — ${detail}` : title || detail;
  const extra = String(raw || '').trim();
  const text = !extra || base.includes(extra) ? base : base ? `${base} — ${extra}` : extra;
  return scrubIds(text, T('alerts.id_hidden'), nameOf);
}

// `{ title, detail, place, advice, tooltip, known, nodeText }` for one alert.
// `place` (where the subject sits, "tank · RAIDZ2") and `advice` (what the
// node advises, "zaplanuj wymianę dysku") are '' unless the node sent them;
// `copies` are a conflict's files whose other version can be copied
// (`conflictCopies`). `known` is
// false for the generic fallback (unknown code, no code, or parameters that
// do not read). `nodeText` is true when the detail points at the node's own
// text instead of saying it: a row must then also offer that text where a
// `title` tooltip cannot be reached (a phone, a tablet) — see
// `alertNodeTextHtml`.
//
// `nameOf(nodeId)` is the caller's fleet, for naming a node id the node's
// text carries; without it such an id reads as the neutral placeholder.
export function alertText(alert, { nameOf } = {}) {
  const words = ALERT_WORDS.get(String(alert?.code || ''));
  const worded = words ? words(alert.params || {}, alert.reasons, alert) : null;
  if (worded && worded.title) {
    const tooltip = alertTooltip(alert, worded.raw, nameOf);
    return {
      title: worded.title,
      detail: worded.detail || '',
      place: worded.place || '',
      advice: worded.advice || '',
      copies: worded.copies || [],
      tooltip,
      known: true,
      nodeText: Boolean(tooltip) && (Boolean(worded.nodeText) || Boolean(worded.raw)),
    };
  }
  const tooltip = alertTooltip(alert, '', nameOf);
  return { title: T('alerts.untranslated_title'), detail: T('alerts.untranslated_detail'), place: '', advice: '', copies: [], tooltip, known: false, nodeText: Boolean(tooltip) };
}

// The codes this build words — for the test that holds the node's list to it.
export const ALERT_CODES = [...ALERT_WORDS.keys()];

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

// The media-kind badge shown next to a disk name — n06's topology cells and
// n11's member rows both paint the same badge from the same wire spelling
// ('hdd', 'ssd', 'nvme'), so they share one mapping instead of keeping two
// copies in sync by hand. `[cssClass, label]`; an unknown kind renders no
// badge (the caller treats a missing entry as "no badge").
export const KIND_BADGE = { hdd: ['', 'HDD'], ssd: ['', 'SSD'], nvme: ['nvme', 'NVMe'] };

// Layout names are wire spellings ('raidz2', 'mirror'); the labels are the
// admin-facing forms ("RAIDZ2", "Mirror"). An unknown spelling shows as-is.
export function layoutLabel(layout) {
  const key = 'layout.' + (layout || 'unknown');
  const label = T(key);
  return label === 'tentanas.' + key ? String(layout || '—') : label;
}

// The NFS transport of a share or of one node's mount (§5.5a). RDMA is never
// silent: wherever a share or a mount can run over it, the label says which
// of the two it is.
export function transportLabel(rdma) {
  return T(rdma ? 'shares.transport_rdma' : 'shares.transport_tcp');
}

// `NasMountStatus.transport` / `NasFleetMount.transport`: 'rdma' | 'tcp' | ''
// (a node that is the source, or one that has not mounted anything).
export function transportChipHtml(transport) {
  if (transport !== 'rdma' && transport !== 'tcp') return '';
  return `<tf-chip size="sm" status="${transport === 'rdma' ? 'accent' : 'neutral'}" label="${escapeAttr(transportLabel(transport === 'rdma'))}"></tf-chip>`;
}

// A node sentence that also travels as codes (wave 6, `tentanas::CodedText`
// on the node: an Elastic Array's or a target's state detail, a parked
// request's detail, a kernel-support reason). `reasons` is the wire's
// `[{ code, params }]`; `words` maps a code to `(params) => sentence`, which
// may answer null for parameters that do not read. The parts are joined the
// way the node joins its sentence (" · ").
//
// '' when there are no reasons, or when any of them has no words in this
// build: the caller then shows the node's own sentence as it came — truthful,
// if not translated — rather than half a translation.
export function wordReasons(reasons, words) {
  const list = Array.isArray(reasons) ? reasons : [];
  if (!list.length) return '';
  const parts = list.map((r) => {
    const fn = words.get(String(r?.code || ''));
    return fn ? fn(r?.params || {}) : null;
  });
  return parts.every((p) => typeof p === 'string' && p) ? parts.join(' · ') : '';
}

// The node's own sentence, for a tooltip only: it is one language, and it
// may name what a screen must not show (a by-id path, a WWN, a node id), so
// every such id is replaced by the neutral word first.
export function nodeTextTitle(text) {
  return scrubIds(String(text || '').trim(), T('alerts.id_hidden'));
}

// A refusal the node sends as a CODE rather than a sentence (M1):
// `refusal:<code>` as the whole error message (`SHARE_USER_IN_USE_ELSEWHERE`
// in tentanas/db.rs, `ApprovalError` in tentanas/approvals.rs). Worded here
// from `refusal.<code>`; a code this build has no words for is shown as the
// node sent it — truthful, if not pretty — rather than dropped.
//
// What a screen catches is not the node's message alone: `binary-ws-client.js`
// rejects every error reply as `protocol error <Code>: <message>` (the same
// wrapping `describeError` in agent-accounts.js strips), so the code is looked
// for after that prefix.
const REFUSAL = /^refusal:([a-z0-9_]+)$/;
const WIRE_ERROR_PREFIX = /^protocol error ([A-Za-z]+):\s*/;

// The wire enum of a failed call (`ProtocolErrorCode`: 'NotFound',
// 'PolicyDenied', …), or '' when the error carries none. The real client
// only has it inside its message prefix (see above); a shim or a test may
// set `.code` on the Error instead.
export function errCode(e) {
  if (typeof e?.code === 'string' && e.code) return e.code;
  return WIRE_ERROR_PREFIX.exec(String(e?.message ?? e ?? '').trim())?.[1] || '';
}

// A long hex run is a node id (64 hex), a GUID written without dashes or a
// digest — never words a reader needs, and the owner's rule keeps every id off
// the screen, a toast included.
const HEX_ID = /[0-9a-f]{32,}/gi;

// `nameOf` (node id -> name, `nodeNameOf`) lets an id the fleet knows read as
// that node's name instead of the neutral placeholder.
export function errMessage(e, nameOf = () => '') {
  // "The addressed node did not answer" (dispatch/app_route.rs) is worded
  // here, whatever the forwarder wrote: its sentence names the node by its
  // 64-hex id, and a toast, a banner or a tab body would print it as is.
  if (errCode(e) === 'NodeUnreachable') return T('unreachable.error');
  const message = (e && e.message) ? e.message : String(e);
  const code = REFUSAL.exec(message.trim().replace(WIRE_ERROR_PREFIX, ''))?.[1];
  if (!code) {
    const hidden = T('alerts.id_hidden');
    return scrubIds(message, hidden, nameOf).replace(HEX_ID, (id) => String(nameOf(id) || '').trim() || hidden);
  }
  const key = 'refusal.' + code;
  const words = T(key);
  return words === 'tentanas.' + key ? message : words;
}

// ----- Per-disk batches (SMART "all disks" / SMART "selected") -------------
//
// Both batches send the same request once per disk with one sudo password
// for the whole run. Two very different things can make one of those
// requests fail, and they need OPPOSITE handling:
//   - a refusal specific to THIS disk (busy, a test already runs, the disk
//     rejects the command) — record it, try the next disk.
//   - a privilege/credential failure (a rejected or expired sudo password,
//     an unarmed privilege channel, a helper/core version mismatch) — this
//     will fail identically for every remaining disk, so retrying it per
//     disk only replays the same password (or the same unarmed channel)
//     against sudo once per disk. On a distro with `pam_faillock` that can
//     lock the account the core runs as.
//
// The server tells the two apart with one stable code: `broker_error` in
// dispatch/tentanas.rs maps `BrokerError::Unarmed` (rejected/expired
// password, unarmed channel), `BrokerError::HelperVersion` (helper/core
// version mismatch, `HELPER_VERSION_MARKER`) and `BrokerError::ToolMissing`
// all to `ProtocolErrorCode::NotAvailable` — never left to a guess from the
// (partly Polish, partly English) error text. `api-binary-shim.js` copies
// that code onto the thrown `Error` as `.code`.
const BATCH_HALT_CODE = 'NotAvailable';

/** True for the one error shape that must stop a whole per-disk batch. */
export function isBatchHaltError(e) {
  return Boolean(e) && e.code === BATCH_HALT_CODE;
}

/**
 * Runs `request(disk)` once per disk in `disks`, in order, with one shared
 * sudo password closed over by the caller. A per-disk refusal is recorded in
 * `refused` and the loop continues; a privilege/credential error
 * (`isBatchHaltError`) is rethrown immediately, so the caller's `withSudo`
 * stops the batch and surfaces that one error instead of every disk's copy
 * of it.
 */
export async function runDiskBatch(disks, request) {
  const started = [];
  const refused = [];
  for (const disk of disks) {
    try {
      await request(disk);
      started.push(disk);
    } catch (e) {
      if (isBatchHaltError(e)) throw e;
      refused.push({ disk, error: e });
    }
  }
  return { started, refused };
}

/** The disks a batch refused, named — never a disk id — for one toast. */
export function refusedBatchNames(refused) {
  return refused.map((r) => `${r.disk.name}: ${errMessage(r.error)}`).join(' · ');
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

const SUB_DAILY = new Set(['15m', '30m', '1h', '6h']);

// Human form of a NasSchedule for the sched-pill: "every 15 min",
// "daily 02:00", "Sun 02:00", "1st 01:30".
export function fmtSchedule(s) {
  if (!s || !s.every) return T('schedule.none');
  const every = s.every;
  if (SUB_DAILY.has(every)) return T('schedule.every_' + every);
  const at = timeHm(s.hour, s.minute);
  if (every === 'daily') return T('schedule.daily_at', { at });
  if (every === 'weekly') return T('schedule.weekly_at', { day: T('weekday.' + (Number(s.weekday) || 0)), at });
  if (every === 'monthly') return T('schedule.monthly_at', { day: Number(s.day) || 1, at });
  return String(every);
}

// The GFS "frequent" tier counts snapshots in units of the cadence
// ("96 × 15 min", n10), so it needs the bare unit and not the "every …"
// phrase. Calendar cadences keep their full form — they have no bare unit.
export function fmtScheduleUnit(s) {
  const every = s && s.every;
  return SUB_DAILY.has(every) ? T('schedule.unit_' + every) : fmtSchedule(s);
}

// A ZFS pool leaf as a screen names it (n06 cells, device-action words, the
// destroy dialog, the replace wizard, the Pools tab's spare shelf). A leaf
// `zpool status` can no longer find arrives named by its GUID or by-id link
// — an id, never text (owner's rule). So: the leaf's own kernel name; else
// the kernel name the disk inventory has for it (`inv`); else the name the
// node REMEMBERS for it (`lastKnownName`, pools.rs `last_known_leaf_name`),
// marked as remembered; else "missing disk" with the leaf's 1-based
// `position` in its group (0 when it has none). The id stays `d.name` for
// every request — only the words change.
export function leafDisplayName(d, inv, position = 0) {
  if (!isDiskIdShape(d?.name)) return d.name;
  const kernelName = inv && inv.name && !isDiskIdShape(inv.name) ? inv.name : null;
  if (kernelName) return kernelName;
  const remembered = String(d.lastKnownName || '').trim();
  if (remembered && !isDiskIdShape(remembered)) return T('pool.leaf_last_known', { name: remembered });
  return position ? T('pool.leaf_missing_at', { n: position }) : T('elastic.disk_absent');
}
