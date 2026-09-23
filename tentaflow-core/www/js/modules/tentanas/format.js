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

export function jobCanCancel(job) {
  // `disk_wipe` is here for a different reason than the elastic runs: those
  // cannot be interrupted safely, this one cannot be interrupted AT ALL — the
  // helper is already erasing, so the button could only ever lie about what it
  // did. The server refuses it too (`jobs.rs`); this keeps the button away
  // from the admin in the first place.
  return !['elastic_create', 'elastic_restore', 'elastic_sync', 'elastic_scrub', 'elastic_mover', 'disk_wipe'].includes(job.kind);
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
