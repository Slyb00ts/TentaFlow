// ===== File: modules/tentanas/format.js — labels, numbers and timestamps shared by the TentaNas screen shell and its pools/datasets/snapshots/tasks modules =====
//
// Everything the sub-modules render goes through the same formatters so a
// capacity, a timestamp or a health chip looks identical on the fleet grid,
// the pool cards and the snapshot table. The i18n namespace is fixed here
// (`tentanas.*`) so the modules never repeat the prefix.

import { I18n } from '/js/i18n.js';
import { escapeAttr } from '/js/utils.js';
import { isOpaqueId } from '/js/modules/tentanas/machine-id.js';

export const T = (k, p) => I18n.t('tentanas.' + k, p);

// The wire spells "no channel configured" as `unset` (`elevation::Mode::as_str`,
// and `fleet.rs` for the per-node mode); this UI has always spelled it
// `unarmed`. Every comparison tested only the latter, so a freshly installed
// node read as a WORKING channel: no password was ever asked for, the header
// badge said ok, and `elevation.short_unset` rendered as a raw key. One
// spelling from here on — the i18n keys keep theirs.
export const channelMode = (mode) => (!mode || mode === 'unset' ? 'unarmed' : mode);
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

// Who started a job, as `{ label, title }`. The node resolves a user id to the
// account's display name (`dispatch::tentanas::display_names`) and passes an
// id with no account behind it through unchanged — a deleted account, or one
// that exists only on the node that forwarded the request. That UUID is not a
// name: the label says the account is unknown here and the id becomes the
// tooltip. An author is always a user id or a system token, never a name
// that could collide with a disk-id prefix, so the opaque rule alone applies.
export function jobAuthor(startedBy) {
  const by = String(startedBy || '').trim();
  if (SYSTEM_AUTHORS.has(by)) return { label: T('jobs.by_' + by), title: '' };
  if (isOpaqueId(by)) return { label: T('jobs.by_unknown_account'), title: by };
  return { label: by || '—', title: '' };
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

export function replacementAdviceText(advice) {
  const title = String(advice?.reason || '').trim();
  const words = ADVICE_KINDS.has(advice?.severity) ? wordsOf(DISK_REASON_WORDS, advice.reasons) : [];
  if (!words.length) return { known: false, text: T('replace_advice.reason_other'), title };
  return { known: true, text: words.join('; '), title };
}

// A pool's reason for the n05 card, `{ text, title }` as `diskReasonsText`.
export function poolReasonsText(pool) {
  const title = String(pool?.healthReason || '').trim();
  const reasons = Array.isArray(pool?.healthReasons) ? pool.healthReasons : [];
  if (!reasons.length && !title) return { text: '', title: '' };
  const words = wordsOf(POOL_REASON_WORDS, reasons);
  return { text: words.length ? words.join('; ') : T('health.' + (pool?.health || 'unknown')), title };
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

export function errMessage(e) {
  return (e && e.message) ? e.message : String(e);
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
